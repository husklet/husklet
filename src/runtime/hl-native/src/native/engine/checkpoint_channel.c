#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "checkpoint_channel.h"

#include "../linux_abi/container/pidmap.h" /* the identity registry's reserved region in the trigger object */

/* The checkpoint transport is not a protocol that happens to use POSIX; it IS four POSIX mechanisms.
 * The broker is an AF_UNIX socketpair, engine processes announce themselves by passing a live descriptor
 * over SCM_RIGHTS, the per-process channel is re-created after fork() because a shared stream socket would
 * interleave two processes' frames, and the trigger is a memfd mapped MAP_SHARED so every fork descendant
 * polls one counter with a plain load. Windows has none of the four, and the descriptor-passing half is
 * already declared absent for this host: fork_wire.c, which sends and receives those descriptors, is left
 * out of the Windows host archive for exactly that reason.
 *
 * So the feature is guarded whole rather than emulated. Every entry point still exists, and each reports
 * the absence in its own already-defined failure channel -- the -1 that callers handle when no broker was
 * published or a connect failed -- so nothing here can be mistaken for a checkpoint that was taken. A named
 * pipe plus DuplicateHandle could carry this protocol one day; that is a transport to design, not a spelling
 * to shim, and inventing half of it here would produce a channel that accepts requests and captures nothing. */
#if defined(_WIN32)

void hl_ckpt_channel_publish(int broker) {
    (void)broker;
}

int hl_ckpt_channel_broker(void) {
    return -1;
}

int hl_ckpt_channel_owns_descriptor(int descriptor) {
    (void)descriptor;
    return 0;
}

const char *hl_ckpt_channel_failure(void) {
    return "reach a checkpoint broker: this host has no checkpoint transport";
}

int hl_ckpt_channel_adopt(const char *broker, const char *trigger) {
    (void)broker;
    (void)trigger;
    return -1;
}

int hl_ckpt_channel_acquire(void) {
    return -1;
}

int hl_ckpt_channel_authenticate_peer(int descriptor, uint64_t claimed_pid, uint64_t *authenticated_pid) {
    (void)descriptor;
    (void)claimed_pid;
    if (authenticated_pid != NULL) *authenticated_pid = 0;
    return -1;
}

int hl_ckpt_channel_call(hl_ckpt_request *request, const char *name, const void *payload, hl_ckpt_reply *reply,
                         void *out, size_t capacity) {
    (void)request;
    (void)name;
    (void)payload;
    (void)reply;
    (void)out;
    (void)capacity;
    return -1;
}

int hl_ckpt_channel_call_receive_descriptor(hl_ckpt_request *request, const void *payload, hl_ckpt_reply *reply,
                                            int *out_descriptor) {
    (void)request;
    (void)payload;
    (void)reply;
    if (out_descriptor != NULL) *out_descriptor = -1;
    return -1;
}

void hl_ckpt_trigger_publish(int descriptor) {
    (void)descriptor;
}

int hl_ckpt_trigger_descriptor(void) {
    return -1;
}

int hl_ckpt_broker_pair(hl_activation_descriptor *out_parent, hl_activation_descriptor *out_child) {
    if (out_parent != NULL) *out_parent = HL_ACTIVATION_DESCRIPTOR_NONE;
    if (out_child != NULL) *out_child = HL_ACTIVATION_DESCRIPTOR_NONE;
    return -1;
}

hl_activation_descriptor hl_ckpt_broker_accept(hl_activation_descriptor broker, int timeout_ms,
                                               uint64_t *out_host_pid) {
    (void)broker;
    (void)timeout_ms;
    (void)out_host_pid;
    return HL_ACTIVATION_DESCRIPTOR_NONE;
}

int hl_ckpt_trigger_create(hl_activation_descriptor *out_descriptor, void **out_mapping) {
    if (out_descriptor != NULL) *out_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
    if (out_mapping != NULL) *out_mapping = NULL;
    return -1;
}

/* A trigger that was never created has no generation to advance. Zero is what a NULL mapping already
 * returns on every host, so a caller that skipped the failed create() sees one answer everywhere. */
uint32_t hl_ckpt_trigger_bump(void *mapping) {
    (void)mapping;
    return 0;
}

void hl_ckpt_trigger_destroy(void *mapping, hl_activation_descriptor descriptor) {
    (void)mapping;
    (void)descriptor;
}

#if defined(HL_NATIVE_TEST_HOOKS)
/* The header declares these three whenever the hooks are compiled in, and checkpoint/image.c's own hook
 * blocks CALL two of them, so the Windows arm has to answer as well -- otherwise the feature build is an
 * undefined-reference at link, which is where this arm was found. There is no cached channel to forget on
 * a host with no channel, so forgetting is a no-op and `current` reports the -1 every caller already
 * handles as "this process has none". */
void hl_ckpt_channel_test_claimed_pid(uint64_t claimed_pid) {
    (void)claimed_pid;
}

void hl_ckpt_channel_forget_for_test(void) {}

int hl_ckpt_channel_current_for_test(void) {
    return -1;
}
#endif

#else

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/types.h>
#if defined(__APPLE__)
#include <sys/un.h>
#endif
#include <unistd.h>

#include "../host/fork_wire.h"
#include "../host/system.h"
#include "backend.h"

static int checkpoint_broker = -1;
static int checkpoint_trigger = -1;
static int checkpoint_channel = -1;
static long checkpoint_channel_owner; /* getpid() that created `checkpoint_channel` */
/* This process's own reference to the broker end of its channel: the SAME socket the broker will read,
 * held here from the moment the hello is sent until the broker answers. See
 * checkpoint_channel_receipt_release. */
static int checkpoint_channel_receipt = -1;
static long checkpoint_channel_receipt_owner;
/* The step at which this process's last round trip failed, and the errno it failed with. Diagnostic
 * only; see the header. */
static char checkpoint_channel_failure[192];

/* Records `step`, and the errno IF THERE IS ONE.
 *
 * Three of this file's failures are not syscall results and set no errno at all: an absent broker, a
 * request that fails the protocol's own size bounds, and a peer that closed the connection cleanly (a
 * zero-length read, which is not an error and touches nothing). This used to append `strerror(errno)`
 * unconditionally, so those three reported whatever unrelated syscall had last set it -- and what the
 * checkpoint coordinator leaves there is the ENOTTY from its own `tcgetattr` on a non-tty, so every
 * socket-topology refusal in this engine read "Inappropriate ioctl for device" and sent its reader
 * looking for a terminal that was never involved. Callers on those three paths clear errno first, and
 * the parenthetical is omitted rather than fabricated. */
static int checkpoint_channel_failed(const char *step) {
    int saved = errno;
    if (saved == 0)
        snprintf(checkpoint_channel_failure, sizeof checkpoint_channel_failure, "%s", step);
    else
        snprintf(checkpoint_channel_failure, sizeof checkpoint_channel_failure, "%s (%s)", step, strerror(saved));
    errno = saved;
    return -1;
}

const char *hl_ckpt_channel_failure(void) {
    return checkpoint_channel_failure[0] != 0 ? checkpoint_channel_failure : NULL;
}

int hl_ckpt_channel_owns_descriptor(int descriptor) {
    /* Bounded to the engine-private band, and that bound is load-bearing rather than defensive. These
     * three statics are plain numbers: a restore re-forks a member and hands the guest its captured
     * descriptor table, so a number one of them still names can legitimately BE a guest descriptor in
     * the new process. Claiming ownership of it would make the guest's own close_range and its
     * exec CLOEXEC sweep skip a real guest fd -- which stopped a restored process tree from making
     * progress at all. Nothing the transport owns is ever below the private floor, so the band excludes
     * every guest number by construction while still covering the fork window this answer exists for. */
    int floor = hl_host_process_fd_private_floor();
    if (descriptor < 0 || floor < 0 || descriptor < floor) return 0;
    if (descriptor == checkpoint_broker || descriptor == checkpoint_trigger) return 1;
    if (descriptor == checkpoint_channel_receipt && checkpoint_channel_receipt_owner == (long)getpid()) return 1;
    return descriptor == checkpoint_channel && checkpoint_channel_owner == (long)getpid();
}

/* Forgets a channel whose round trip failed, so the NEXT call mints a fresh one.
 *
 * A stream socket that failed mid-frame is not merely unlucky, it is DESYNCHRONIZED: a request whose
 * header went out and whose payload did not leaves the broker reading this process's next request as that
 * payload. Keeping it cached made the failure absorbing -- every later call on this process, including the
 * one that reports WHY the capture must be refused, failed on the same dead descriptor, which is why a
 * member that had already decided it could not contribute still let the coordinator burn its whole
 * rendezvous budget before anyone was told.
 *
 * A fresh connection is not a way around any gate: the broker admits a connection to publish capture bytes
 * only after REGISTER_READY on that connection (broker.rs `publishes_capture_bytes`), so a reconnecting
 * process starts unregistered and can publish nothing it had not already proven. */

/* Releases this process's own reference to the broker end of its channel.
 *
 * A descriptor sent over a unix socket is IN FLIGHT until the receiver takes it out of the message, and
 * while it is in flight the kernel accounts for it separately from every file table. On Darwin the
 * unix-domain rights collector may then treat a socket whose only remaining reference is that in-flight
 * message as unreachable and flush its receive state: the connection survives -- LOCAL_PEERPID still
 * names the announcing process and the peer link is intact -- but the broker end reads EOF forever, so
 * the announcing process gets no answer to its FIRST request and reports "read the broker's reply: this
 * channel ended before one arrived". A restoring member announces and reads proc.<gpid>/meta in the same
 * breath, which is why the restore is where it surfaced. Measured on macOS 26.3 (Darwin 25.3.0, arm64):
 * 5 flushed channels in 40,000 announcements with this reference dropped at send time, 0 in 20,000 with
 * it held to the first reply, and 0 in 20,000 on a socketpair that was never passed as a right at all.
 *
 * Holding our own reference for the whole flight keeps the socket reachable from a real file table,
 * which is the property the collector reasons about. It costs one descriptor for one round trip and
 * nothing after, and it cannot leave a channel un-EOFed: process death closes this reference exactly as
 * it closes every other one. */
static void checkpoint_channel_receipt_release(void) {
    if (checkpoint_channel_receipt < 0) return;
    hl_host_process_fd_private_remove(checkpoint_channel_receipt);
    (void)close(checkpoint_channel_receipt);
    checkpoint_channel_receipt = -1;
    checkpoint_channel_receipt_owner = 0;
}

static void checkpoint_channel_poison(void) {
    checkpoint_channel_receipt_release();
    if (checkpoint_channel < 0) return;
    hl_host_process_fd_private_remove(checkpoint_channel);
    (void)close(checkpoint_channel);
    checkpoint_channel = -1;
    checkpoint_channel_owner = 0;
}
#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t checkpoint_test_claimed_pid;
void hl_ckpt_channel_test_claimed_pid(uint64_t claimed_pid) { checkpoint_test_claimed_pid = claimed_pid; }

/* Forgets this process's cached channel WITHOUT closing it.
 *
 * hl_ckpt_channel_acquire caches one channel per process and returns the same descriptor forever
 * after, which is right for the engine: a process has exactly one channel for its whole life. The
 * test hook that mints channels does not want that -- it hands each descriptor to its caller, who
 * closes it -- so a second call would return an already-closed number and the caller would close it
 * twice. Forgetting rather than closing is what keeps ownership with the caller who already has it. */
void hl_ckpt_channel_forget_for_test(void) {
    checkpoint_channel_receipt_release();
    checkpoint_channel = -1;
    checkpoint_channel_owner = 0;
}

/* This process's cached channel, or -1 when it has none. Unlike hl_ckpt_channel_acquire it never MINTS
 * one, which is what a test's cleanup path needs: a fixture that reclaims its channel through `acquire`
 * silently opens a fresh connection on the paths where the code under test never sent a request. */
int hl_ckpt_channel_current_for_test(void) {
    return checkpoint_channel_owner == (long)getpid() ? checkpoint_channel : -1;
}
#endif

void hl_ckpt_channel_publish(int broker) {
    checkpoint_broker = broker;
}

int hl_ckpt_channel_broker(void) {
    return checkpoint_broker;
}

static int checkpoint_parse_descriptor(const char *text) {
    long value;
    char *end;
    if (text == NULL || text[0] == 0) return -1;
    value = strtol(text, &end, 10);
    if (*end != 0 || value < 0 || value > INT32_MAX) return -1;
    return (int)value;
}

static void checkpoint_private_descriptor_close(int descriptor) {
    if (descriptor < 0) return;
    hl_host_process_fd_private_remove(descriptor);
    (void)close(descriptor);
}

int hl_ckpt_channel_adopt(const char *broker, const char *trigger) {
    int broker_descriptor = checkpoint_parse_descriptor(broker);
    int trigger_descriptor = checkpoint_parse_descriptor(trigger);
    if (broker_descriptor < 0 || trigger_descriptor < 0) return -1;
    if (fcntl(broker_descriptor, F_GETFD) < 0 || fcntl(trigger_descriptor, F_GETFD) < 0) return -1;
    hl_host_private_init();
    /* Move both into the engine-private range, exactly as activation does with the descriptors it is
     * handed: the guest descriptor scan must never see them. */
    broker_descriptor = hl_host_process_fd_private_adopt(broker_descriptor);
    trigger_descriptor = hl_host_process_fd_private_adopt(trigger_descriptor);
    if (broker_descriptor < 0 || trigger_descriptor < 0) {
        checkpoint_private_descriptor_close(broker_descriptor);
        checkpoint_private_descriptor_close(trigger_descriptor);
        return -1;
    }
    checkpoint_channel_receipt_release();
    checkpoint_private_descriptor_close(checkpoint_channel);
    checkpoint_channel = -1;
    checkpoint_channel_owner = 0;
    checkpoint_private_descriptor_close(checkpoint_broker);
    checkpoint_private_descriptor_close(checkpoint_trigger);
    checkpoint_broker = broker_descriptor;
    checkpoint_trigger = trigger_descriptor;
    return 0;
}

void hl_ckpt_trigger_publish(int descriptor) {
    checkpoint_trigger = descriptor;
}

int hl_ckpt_trigger_descriptor(void) {
    return checkpoint_trigger;
}

static int checkpoint_write_all(int descriptor, const void *data, size_t size) {
    const char *bytes = data;
    size_t done = 0;
    while (done < size) {
        ssize_t count = write(descriptor, bytes + done, size - done);
        if (count < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        /* A zero-length write is the peer gone, not a syscall error: errno is untouched, so clear it
           rather than let the caller's diagnostic print an unrelated leftover. */
        if (count == 0) {
            errno = 0;
            return -1;
        }
        done += (size_t)count;
    }
    return 0;
}

static int checkpoint_read_all(int descriptor, void *data, size_t size) {
    char *bytes = data;
    size_t done = 0;
    while (done < size) {
        ssize_t count = read(descriptor, bytes + done, size - done);
        if (count < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        /* The server went away mid-image: the capture must fail, not truncate. A clean EOF sets no
           errno, so clear it -- see checkpoint_channel_failed. */
        if (count == 0) {
            errno = 0;
            return -1;
        }
        done += (size_t)count;
    }
    return 0;
}

int hl_ckpt_channel_acquire(void) {
    hl_ckpt_hello hello;
    int pair[2];
    if (checkpoint_broker < 0) {
        errno = 0; /* no syscall was made; see checkpoint_channel_failed */
        return checkpoint_channel_failed("find a published checkpoint broker");
    }
    /* Inherited across fork() like the channel below, and dropped for the same reason: it belongs to the
     * parent's announcement, and this process is about to make its own. */
    if (checkpoint_channel_receipt >= 0 && checkpoint_channel_receipt_owner != (long)getpid())
        checkpoint_channel_receipt_release();
    if (checkpoint_channel >= 0) {
        if (checkpoint_channel_owner == (long)getpid()) return checkpoint_channel;
        /* Inherited across fork(). Drop the parent's channel rather than sharing it: two processes issuing
         * requests on one stream socket would interleave frames and mismatch replies. */
        (void)close(checkpoint_channel);
        checkpoint_channel = -1;
    }
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) return checkpoint_channel_failed("create its channel socket pair");
    hello.magic = HL_CKPT_STREAM_MAGIC_HELLO;
    hello.abi = HL_CKPT_STREAM_ABI;
    hello.host_pid = (uint64_t)getpid();
#if defined(HL_NATIVE_TEST_HOOKS)
    if (checkpoint_test_claimed_pid != 0) hello.host_pid = checkpoint_test_claimed_pid;
#endif
    if (hl_fork_wire_send_descriptors(checkpoint_broker, &hello, sizeof hello, &pair[1], 1) != 0) {
        (void)close(pair[0]);
        (void)close(pair[1]);
        return checkpoint_channel_failed("announce its channel to the broker");
    }
    /* NOT closed here: see checkpoint_channel_receipt_release. The broker end must stay referenced by a
     * real file table for as long as it is in flight, or the collector may flush it. */
    {
        int retained = hl_host_process_fd_private_adopt(pair[1]);
        if (retained < 0) {
            (void)close(pair[1]);
        } else {
            checkpoint_channel_receipt = retained;
            checkpoint_channel_receipt_owner = (long)getpid();
        }
    }
    /* The channel is engine control state, not a guest socket. Move it into the private descriptor range so
     * the checkpoint writer's own descriptor scan never mistakes it for something the guest owns -- the
     * coordinator opens its channel BEFORE it dumps itself. */
    {
        int adopted = hl_host_process_fd_private_adopt(pair[0]);
        if (adopted < 0) {
            (void)close(pair[0]);
            return checkpoint_channel_failed("move its channel into the engine-private descriptor range");
        }
        pair[0] = adopted;
    }
    checkpoint_channel = pair[0];
    checkpoint_channel_owner = (long)getpid();
    return checkpoint_channel;
}

int hl_ckpt_channel_call(hl_ckpt_request *request, const char *name, const void *payload, hl_ckpt_reply *reply,
                         void *out, size_t capacity) {
    int descriptor = hl_ckpt_channel_acquire();
    size_t name_size = name != NULL ? strlen(name) + 1 : 0;
    if (descriptor < 0) return -1; /* acquire already named the step */
    if (name_size > HL_CKPT_STREAM_NAME_MAX || request->length > HL_CKPT_STREAM_PAYLOAD_MAX) {
        errno = 0; /* a bounds check, not a syscall; see checkpoint_channel_failed */
        return checkpoint_channel_failed("frame a request within the protocol's size bounds");
    }
    request->magic = HL_CKPT_STREAM_MAGIC_REQUEST;
    request->abi = HL_CKPT_STREAM_ABI;
    request->name_size = (uint32_t)name_size;
    if (checkpoint_write_all(descriptor, request, sizeof *request) != 0) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("send its request header: the broker closed this channel");
    }
    if (name_size != 0 && checkpoint_write_all(descriptor, name, name_size) != 0) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("send its request name: the broker closed this channel");
    }
    /* A NULL payload with a non-zero length is a REQUESTED length (SOURCE_READ), not bytes to send. */
    if (payload != NULL && request->length != 0 &&
        checkpoint_write_all(descriptor, payload, (size_t)request->length) != 0) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("send its request payload: the broker closed this channel");
    }
    if (checkpoint_read_all(descriptor, reply, sizeof *reply) != 0) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("read the broker's reply: this channel ended before one arrived");
    }
    checkpoint_channel_receipt_release(); /* answered, so the broker holds its own reference now */
    if (reply->magic != HL_CKPT_STREAM_MAGIC_REPLY || reply->abi != HL_CKPT_STREAM_ABI) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("recognize the broker's reply framing");
    }
    if (reply->length > capacity || reply->length > HL_CKPT_STREAM_PAYLOAD_MAX) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("accept the broker's reply payload length");
    }
    if (reply->length != 0 && checkpoint_read_all(descriptor, out, (size_t)reply->length) != 0) {
        checkpoint_channel_poison();
        return checkpoint_channel_failed("read the broker's reply payload");
    }
    return 0;
}

int hl_ckpt_channel_call_receive_descriptor(hl_ckpt_request *request, const void *payload, hl_ckpt_reply *reply,
                                            int *out_descriptor) {
    int descriptor = hl_ckpt_channel_acquire();
    int received[8];
    int count = 0;
    int read_bytes;
    if (out_descriptor == NULL) return -1;
    *out_descriptor = -1;
    if (descriptor < 0 || request->length > HL_CKPT_STREAM_PAYLOAD_MAX) return -1;
    request->magic = HL_CKPT_STREAM_MAGIC_REQUEST;
    request->abi = HL_CKPT_STREAM_ABI;
    request->name_size = 0;
    if (checkpoint_write_all(descriptor, request, sizeof *request) != 0) return -1;
    if (payload != NULL && request->length != 0 &&
        checkpoint_write_all(descriptor, payload, (size_t)request->length) != 0)
        return -1;
    /* recvmsg, not read: the rights are attached to the message carrying the header, and a read() that
     * takes the header drops them irrecoverably. The server sends the header in one sendmsg and appends
     * no payload, so one receive is the whole reply. */
    read_bytes = hl_fork_wire_receive_descriptors(descriptor, reply, sizeof *reply, received, &count);
    if (read_bytes != (int)sizeof *reply || reply->magic != HL_CKPT_STREAM_MAGIC_REPLY ||
        reply->abi != HL_CKPT_STREAM_ABI || reply->length != 0 || count > 1) {
        while (count > 0)
            (void)close(received[--count]);
        return -1;
    }
    checkpoint_channel_receipt_release(); /* answered, so the broker holds its own reference now */
    if (count == 1) *out_descriptor = received[0];
    return 0;
}

/* HL_ACTIVATION_DESCRIPTOR_NONE is 0, so a descriptor that landed on 0 -- which every allocator here will
 * hand out if this process closed its standard input -- is a live descriptor indistinguishable from
 * "absent". Move it off zero at the source rather than teaching each caller to second-guess the sentinel.
 * Returns the (possibly new) descriptor, or -1 after closing the original when it cannot be moved. */
static int checkpoint_reserve_descriptor(int descriptor) {
    int moved;
    if (descriptor != 0) return descriptor;
    moved = fcntl(descriptor, F_DUPFD_CLOEXEC, STDERR_FILENO + 1);
    (void)close(descriptor);
    return moved;
}

/* The hello PID is framing only.  The stream endpoint is created by the
 * announcing process after fork, so its kernel-owned peer credential is the
 * authority.  Refuse platforms without an exact peer-PID query instead of
 * silently falling back to attacker-controlled bytes. */
int hl_ckpt_channel_authenticate_peer(int descriptor, uint64_t claimed_pid, uint64_t *out) {
    uint64_t authenticated = 0;
    if (out == NULL) return -1;
    *out = 0;
#if defined(__linux__)
    struct ucred credentials;
    socklen_t size = (socklen_t)sizeof credentials;
    memset(&credentials, 0, sizeof credentials);
    if (getsockopt(descriptor, SOL_SOCKET, SO_PEERCRED, &credentials, &size) != 0 ||
        size != (socklen_t)sizeof credentials || credentials.pid <= 0)
        return -1;
    authenticated = (uint64_t)credentials.pid;
#elif defined(__APPLE__)
    pid_t pid = 0;
    socklen_t size = (socklen_t)sizeof pid;
    if (getsockopt(descriptor, SOL_LOCAL, LOCAL_PEERPID, &pid, &size) != 0 ||
        size != (socklen_t)sizeof pid || pid <= 0)
        return -1;
    authenticated = (uint64_t)pid;
#else
    (void)descriptor;
    (void)out;
    return -1;
#endif
    if (authenticated != claimed_pid) return -1;
    *out = authenticated;
    return 0;
}

int hl_ckpt_broker_pair(hl_activation_descriptor *out_parent, hl_activation_descriptor *out_child) {
    int pair[2];
    if (out_parent == NULL || out_child == NULL) return -1;
    *out_parent = HL_ACTIVATION_DESCRIPTOR_NONE;
    *out_child = HL_ACTIVATION_DESCRIPTOR_NONE;
    /* Datagram framing: an arbitrary number of engine processes announce themselves concurrently and each
     * sendmsg is one indivisible record carrying one descriptor. */
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) return -1;
    /* Close-on-exec on BOTH ends. The child end reaches the engine through SCM_RIGHTS, which exec does not
     * affect; without this both ends also leak into the spawned engine as ordinary inherited descriptors,
     * where the guest descriptor scan sees two anonymous sockets it cannot account for and refuses to
     * checkpoint at all. */
    if (fcntl(pair[0], F_SETFD, FD_CLOEXEC) != 0 || fcntl(pair[1], F_SETFD, FD_CLOEXEC) != 0) {
        (void)close(pair[0]);
        (void)close(pair[1]);
        return -1;
    }
    hl_host_private_init();
    pair[0] = hl_host_process_fd_private_adopt(pair[0]);
    pair[1] = hl_host_process_fd_private_adopt(pair[1]);
    if (pair[0] < 0 || pair[1] < 0) {
        if (pair[0] >= 0) (void)close(pair[0]);
        if (pair[1] >= 0) (void)close(pair[1]);
        return -1;
    }
    *out_parent = (hl_activation_descriptor)pair[0];
    *out_child = (hl_activation_descriptor)pair[1];
    if (hl_engine_checkpoint_descriptors_register(pair[0], pair[1]) != 0) {
        (void)close(pair[0]);
        (void)close(pair[1]);
        *out_parent = HL_ACTIVATION_DESCRIPTOR_NONE;
        *out_child = HL_ACTIVATION_DESCRIPTOR_NONE;
        return -1;
    }
    return 0;
}

hl_activation_descriptor hl_ckpt_broker_accept(hl_activation_descriptor broker, int timeout_ms,
                                               uint64_t *out_host_pid) {
    struct pollfd waiting;
    hl_ckpt_hello hello;
    int descriptors[8];
    int count = 0;
    int ready;
    int channel;
    uint64_t authenticated_pid = 0;
    if (broker == HL_ACTIVATION_DESCRIPTOR_NONE || broker > (hl_activation_descriptor)INT32_MAX)
        return HL_ACTIVATION_DESCRIPTOR_NONE;
    waiting = (struct pollfd){.fd = (int)broker, .events = POLLIN};
    do {
        ready = poll(&waiting, 1, timeout_ms);
    } while (ready < 0 && errno == EINTR);
    if (ready <= 0) return HL_ACTIVATION_DESCRIPTOR_NONE;
    if (hl_fork_wire_receive_descriptors(waiting.fd, &hello, sizeof hello, descriptors, &count) != (int)sizeof hello) {
        while (count > 0)
            (void)close(descriptors[--count]);
        return HL_ACTIVATION_DESCRIPTOR_NONE;
    }
    if (count != 1 || hello.magic != HL_CKPT_STREAM_MAGIC_HELLO || hello.abi != HL_CKPT_STREAM_ABI) {
        while (count > 0)
            (void)close(descriptors[--count]);
        return HL_ACTIVATION_DESCRIPTOR_NONE;
    }
    channel = checkpoint_reserve_descriptor(descriptors[0]);
    if (channel < 0) return HL_ACTIVATION_DESCRIPTOR_NONE;
    if (hl_ckpt_channel_authenticate_peer(channel, hello.host_pid, &authenticated_pid) != 0) {
        (void)close(channel);
        return HL_ACTIVATION_DESCRIPTOR_NONE;
    }
    if (hl_engine_checkpoint_descriptors_register(channel, -1) != 0) {
        (void)close(channel);
        return HL_ACTIVATION_DESCRIPTOR_NONE;
    }
    if (out_host_pid != NULL) *out_host_pid = authenticated_pid;
    return (hl_activation_descriptor)channel;
}

static int checkpoint_anonymous_descriptor(void) {
#if defined(__linux__)
    return memfd_create("hl-checkpoint-trigger", MFD_CLOEXEC);
#else
    /* macOS has no memfd. An ORDINARY file unlinked immediately after creation is the closest thing: the
     * name is gone before anything else can observe it and the descriptor keeps the object alive, exactly
     * as an unlinked POSIX shared segment would -- but unlike a shared segment it is a real inode, and this
     * object carries the container's identity registry, whose one-time seeding and whose every allocation
     * are serialized by a POSIX record lock on that inode. Measured on macOS 26.3 (Darwin 25.3.0, arm64):
     * fcntl(F_SETLKW) on a shm_open() descriptor fails EBADF, while the same call on an unlinked mkstemp()
     * descriptor succeeds. Under the shared segment the lock failed, prepare_shared_descriptor failed with
     * it, and each launch quietly fell back to a private registry that re-issued guest 1, 2, 3, 4. */
    char path[] = "/tmp/hl-ckpt-trigger-XXXXXX";
    int descriptor = mkstemp(path);
    int flags;
    if (descriptor < 0) return -1;
    (void)unlink(path);
    flags = fcntl(descriptor, F_GETFD);
    if (flags < 0 || fcntl(descriptor, F_SETFD, flags | FD_CLOEXEC) != 0) {
        (void)close(descriptor);
        return -1;
    }
    return descriptor;
#endif
}

int hl_ckpt_trigger_create(hl_activation_descriptor *out_descriptor, void **out_mapping) {
    int descriptor;
    void *mapping;
    if (out_descriptor == NULL || out_mapping == NULL) return -1;
    *out_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
    *out_mapping = NULL;
    /* An anonymous shared file: no name in any namespace the guest or the filesystem can see. */
    hl_host_private_init();
    descriptor = hl_host_process_fd_private_adopt(checkpoint_anonymous_descriptor());
    if (descriptor < 0) return -1;
    /* The same object also carries this container's identity registry, one page in. It is the one shared
     * mapping the spec tree's launch and every exec session's launch all inherit, which is precisely the
     * set of processes that must agree on one pid namespace; a per-launch registry gave each of them guest
     * pid 1 and then the same 2, 3, 4. The trigger word keeps offset 0 and its own four-byte mapping. */
    if (ftruncate(descriptor,
                  (off_t)(hl_linux_identity_registry_offset() + (uint64_t)HL_LINUX_IDENTITY_REGISTRY_BYTES)) != 0) {
        (void)close(descriptor);
        return -1;
    }
    mapping = mmap(NULL, sizeof(uint32_t), PROT_READ | PROT_WRITE, MAP_SHARED, descriptor, 0);
    if (mapping == MAP_FAILED) {
        (void)close(descriptor);
        return -1;
    }
    *out_descriptor = (hl_activation_descriptor)descriptor;
    *out_mapping = mapping;
    if (hl_engine_checkpoint_descriptors_register(descriptor, -1) != 0) {
        (void)munmap(mapping, sizeof(uint32_t));
        (void)close(descriptor);
        *out_descriptor = HL_ACTIVATION_DESCRIPTOR_NONE;
        *out_mapping = NULL;
        return -1;
    }
    return 0;
}

uint32_t hl_ckpt_trigger_bump(void *mapping) {
    uint32_t *generation = mapping;
    uint32_t current;
    uint32_t next;
    if (mapping == NULL) return 0;
    current = __atomic_load_n(generation, __ATOMIC_ACQUIRE);
    do {
        next = current + 1u;
        if (next == 0u) next = 1u;
    } while (!__atomic_compare_exchange_n(generation, &current, next, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE));
    return next;
}

void hl_ckpt_trigger_destroy(void *mapping, hl_activation_descriptor descriptor) {
    if (mapping != NULL) (void)munmap(mapping, sizeof(uint32_t));
    if (descriptor != HL_ACTIVATION_DESCRIPTOR_NONE && descriptor <= (hl_activation_descriptor)INT32_MAX)
        (void)close((int)descriptor);
}

#endif
