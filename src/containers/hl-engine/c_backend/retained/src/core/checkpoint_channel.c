#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "checkpoint_channel.h"

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

int hl_ckpt_channel_adopt(const char *broker, const char *trigger) {
    (void)broker;
    (void)trigger;
    return -1;
}

int hl_ckpt_channel_acquire(void) {
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
#include <unistd.h>

#include "../host/fork_wire.h"
#include "../host/system.h"

static int checkpoint_broker = -1;
static int checkpoint_trigger = -1;
static int checkpoint_channel = -1;
static long checkpoint_channel_owner; /* getpid() that created `checkpoint_channel` */

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
    if (*end != 0 || value < 0 || value > 65535) return -1;
    return (int)value;
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
    if (broker_descriptor < 0 || trigger_descriptor < 0) return -1;
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
        if (count == 0) return -1;
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
        if (count == 0) return -1; /* the server went away mid-image: the capture must fail, not truncate */
        done += (size_t)count;
    }
    return 0;
}

int hl_ckpt_channel_acquire(void) {
    hl_ckpt_hello hello;
    int pair[2];
    if (checkpoint_broker < 0) return -1;
    if (checkpoint_channel >= 0) {
        if (checkpoint_channel_owner == (long)getpid()) return checkpoint_channel;
        /* Inherited across fork(). Drop the parent's channel rather than sharing it: two processes issuing
         * requests on one stream socket would interleave frames and mismatch replies. */
        (void)close(checkpoint_channel);
        checkpoint_channel = -1;
    }
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) return -1;
    hello.magic = HL_CKPT_STREAM_MAGIC_HELLO;
    hello.abi = HL_CKPT_STREAM_ABI;
    hello.host_pid = (uint64_t)getpid();
    if (hl_fork_wire_send_descriptors(checkpoint_broker, &hello, sizeof hello, &pair[1], 1) != 0) {
        (void)close(pair[0]);
        (void)close(pair[1]);
        return -1;
    }
    (void)close(pair[1]);
    /* The channel is engine control state, not a guest socket. Move it into the private descriptor range so
     * the checkpoint writer's own descriptor scan never mistakes it for something the guest owns -- the
     * coordinator opens its channel BEFORE it dumps itself. */
    {
        int adopted = hl_host_process_fd_private_adopt(pair[0]);
        if (adopted < 0) {
            (void)close(pair[0]);
            return -1;
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
    if (descriptor < 0 || name_size > HL_CKPT_STREAM_NAME_MAX || request->length > HL_CKPT_STREAM_PAYLOAD_MAX)
        return -1;
    request->magic = HL_CKPT_STREAM_MAGIC_REQUEST;
    request->abi = HL_CKPT_STREAM_ABI;
    request->name_size = (uint32_t)name_size;
    request->reserved = 0;
    if (checkpoint_write_all(descriptor, request, sizeof *request) != 0) return -1;
    if (name_size != 0 && checkpoint_write_all(descriptor, name, name_size) != 0) return -1;
    /* A NULL payload with a non-zero length is a REQUESTED length (SOURCE_READ), not bytes to send. */
    if (payload != NULL && request->length != 0 &&
        checkpoint_write_all(descriptor, payload, (size_t)request->length) != 0)
        return -1;
    if (checkpoint_read_all(descriptor, reply, sizeof *reply) != 0) return -1;
    if (reply->magic != HL_CKPT_STREAM_MAGIC_REPLY || reply->abi != HL_CKPT_STREAM_ABI) return -1;
    if (reply->length > capacity || reply->length > HL_CKPT_STREAM_PAYLOAD_MAX) return -1;
    if (reply->length != 0 && checkpoint_read_all(descriptor, out, (size_t)reply->length) != 0) return -1;
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
    pair[0] = checkpoint_reserve_descriptor(pair[0]);
    pair[1] = checkpoint_reserve_descriptor(pair[1]);
    if (pair[0] < 0 || pair[1] < 0) {
        if (pair[0] >= 0) (void)close(pair[0]);
        if (pair[1] >= 0) (void)close(pair[1]);
        return -1;
    }
    *out_parent = (hl_activation_descriptor)pair[0];
    *out_child = (hl_activation_descriptor)pair[1];
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
    if (out_host_pid != NULL) *out_host_pid = hello.host_pid;
    return (hl_activation_descriptor)channel;
}

static int checkpoint_anonymous_descriptor(void) {
#if defined(__linux__)
    return memfd_create("hl-checkpoint-trigger", MFD_CLOEXEC);
#else
    /* macOS has no memfd. A POSIX shared segment unlinked immediately after creation is the same thing:
     * the name is gone before anything else can observe it, and the descriptor keeps the object alive. */
    char name[64];
    int descriptor;
    snprintf(name, sizeof name, "/hl-ckpt-%d-%u", (int)getpid(), (unsigned)clock());
    descriptor = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
    if (descriptor < 0) return -1;
    (void)shm_unlink(name);
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
    descriptor = checkpoint_reserve_descriptor(checkpoint_anonymous_descriptor());
    if (descriptor < 0) return -1;
    if (ftruncate(descriptor, (off_t)sizeof(uint32_t)) != 0) {
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
    return 0;
}

uint32_t hl_ckpt_trigger_bump(void *mapping) {
    volatile uint32_t *generation = mapping;
    uint32_t next;
    if (mapping == NULL) return 0;
    next = *generation + 1u;
    *generation = next;
    return next;
}

void hl_ckpt_trigger_destroy(void *mapping, hl_activation_descriptor descriptor) {
    if (mapping != NULL) (void)munmap(mapping, sizeof(uint32_t));
    if (descriptor != HL_ACTIVATION_DESCRIPTOR_NONE && descriptor <= (hl_activation_descriptor)INT32_MAX)
        (void)close((int)descriptor);
}

#endif
