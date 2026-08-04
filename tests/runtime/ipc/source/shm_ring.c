// multi-process application out-of-process service command-buffer transport, faithfully modeled at multi-process application's regime — the Wall-7
// "multi-proc content white" reproducer.
//
// WHY THIS GATE EXISTS. Multi-process multi-process application content is white because the worker's queued
// command stream never executes in the service process RasterDecoder (executor trace: 1913/1913 frames bind
// ONLY the present surface, off_draws=0 — the service process composites but never processes content). The
// worker→service out-of-process command buffer is a SHARED-MEMORY ring: the worker writes GL commands into a
// memfd "transfer buffer", advances a "put" offset in a small memfd "shared state" region, and signals an
// eventfd (AsyncFlush over the IPC PeerChannel); the service process reads [get..put] from the ring, executes
// it, and advances "get". EVERY existing cross-process micro-gate passes (scm-recv-epoll, scm-futex,
// pump-*, xproc-*, zygote-inbound, epoll-shared-xthread), and SharedImage ALLOCATION (a pure IPC message)
// works — so the wall is NOT a missing primitive. What NO prior gate tests is the exact command-buffer
// COMBINATION: a memfd created by one process, transferred by SCM_RIGHTS, mapped MAP_SHARED by BOTH, then
// used as a producer/consumer RING with a separate tiny partial-host-page STATE region, under ring-wrap
// and a >1024-fd interest set.
//
// This isolates the residual into exactly ONE of three verdicts, each with distinct output:
//   * "ring-incoherence"  — the consumer wakes and reads the ring, but the bytes the producer wrote are
//                           STALE/ZERO (a cross-process MAP_SHARED memfd coherence bug — the transfer
//                           buffer). Emitted with the first bad (offset, expected-seq, got-magic).
//   * "state-incoherence" — the consumer wakes but the producer's "put" advance in the small (<16 KB host
//                           page) STATE memfd is NOT visible (a partial-page coherence bug — the put/get
//                           offsets). Emitted with (seen_put, real commands pending).
//   * exit 7 (watchdog)   — the producer advanced put + signaled, but the consumer never woke (a lost
//                           cross-process eventfd wakeup = command-channel pump dormancy).
//   * "shm_ring ok"    — all N commands crossed coherently and every flush woke the consumer: the
//                           engine's command-buffer transport is correct; the live wall is multi-process application-internal
//                           command-buffer scheduling, not a hl primitive.
//
// FIX LOCUS if it reproduces: hl-jit-darwin/src/runtime/os/linux/syscall/mem.c (MAP_SHARED memfd host
// mapping / partial 16 KB-vs-4 KB page coherence) for the *-incoherence verdicts, or event.c/eventfd
// cross-process wakeup for the watchdog verdict.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// memfd_create is glibc >= 2.27; call the syscall directly to avoid a musl/glibc header gap.
#include <sys/syscall.h>
static int memfd_create_(const char *name, unsigned flags) {
    return (int)syscall(SYS_memfd_create, name, flags);
}

#define CMD_MAGIC 0xC0DEu
#define CMD_WORDS 32u              // 128-byte command record (magic, seq, + 30 payload words)
#define CMD_SIZE (CMD_WORDS * 4u)  // 128 bytes
#define RING_BYTES 65536u          // small ring -> wraps ~N*CMD_SIZE/RING_BYTES times (forces wrap)
#define RING_SLOTS (RING_BYTES / CMD_SIZE)
#define N_CMDS 200000u             // ~390 ring wraps; thousands of flush/wake cycles
#define IDLE_FDS 1500              // >1024: multi-process application's high-fd interest-set regime

// The command-buffer SHARED STATE: put/get offsets in COMMAND units (not bytes). Deliberately tiny so the
// memfd is far smaller than one 16 KB host page — the partial-page coherence case.
struct cbstate {
    _Atomic uint32_t put; // producer-owned: next command index to be produced (published with release)
    _Atomic uint32_t get; // consumer-owned: next command index to be consumed (published with release)
};

static volatile int g_done = 0;
_Atomic unsigned g_spurious_eagain = 0; // spurious EAGAIN on the blocking eventfd (cross-proc O_NONBLOCK race)
static _Atomic uint32_t g_next = 0;  // consumer progress (commands consumed so far)
static _Atomic uint32_t g_wakes = 0; // eventfd reads that returned (AsyncFlush wakes received)
static void *watchdog(void *arg) {
    (void)arg;
    // Generous bound: 200k commands is trivial work; anything past this is a real stall.
    uint32_t last = 0, stuck = 0;
    for (int i = 0; i < 300; i++) { // 300 * 100ms = 30s
        struct timespec ts = {0, 100 * 1000 * 1000};
        nanosleep(&ts, NULL);
        if (g_done) return NULL;
        uint32_t now = atomic_load(&g_next);
        stuck = (now == last) ? stuck + 1 : 0;
        last = now;
    }
    (void)stuck;
    // A pump parked with work pending = the dormant worker. Sub-classify by whether wakes were still
    // arriving: wakes received but progress stuck => the producer's `put` advance in the STATE memfd is
    // not visible (partial-page STATE incoherence); no wakes => the eventfd AsyncFlush wake was lost.
    char b[160];
    uint32_t w = atomic_load(&g_wakes), n = atomic_load(&g_next);
    const char *kind = (w > n + 4) ? "state-incoherence (put advance not visible)" : "lost-wakeup (pump dormancy)";
    int k = snprintf(b, sizeof b, "shm_ring WATCHDOG stall: consumed=%u wakes=%u -> %s\n", n, w, kind);
    write(2, b, (size_t)k);
    _exit(7);
}

// Send two fds (state, ring) + the flush eventfd over a unix socket via SCM_RIGHTS.
static int send_fds(int sock, int fds[3]) {
    char dummy = 'x';
    struct iovec io = {&dummy, 1};
    char cbuf[CMSG_SPACE(sizeof(int) * 3)];
    memset(cbuf, 0, sizeof(cbuf));
    struct msghdr msg = {0};
    msg.msg_iov = &io;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf;
    msg.msg_controllen = sizeof(cbuf);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int) * 3);
    memcpy(CMSG_DATA(cm), fds, sizeof(int) * 3);
    return sendmsg(sock, &msg, 0) < 0 ? -1 : 0;
}
static int recv_fds(int sock, int fds[3]) {
    char dummy;
    struct iovec io = {&dummy, 1};
    char cbuf[CMSG_SPACE(sizeof(int) * 3)];
    memset(cbuf, 0, sizeof(cbuf));
    struct msghdr msg = {0};
    msg.msg_iov = &io;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf;
    msg.msg_controllen = sizeof(cbuf);
    if (recvmsg(sock, &msg, 0) < 0) return -1;
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    if (!cm || cm->cmsg_type != SCM_RIGHTS) return -1;
    memcpy(fds, CMSG_DATA(cm), sizeof(int) * 3);
    return 0;
}

// The WORKER (child, producer): write commands into the ring, advance put in the shared state, signal
// the flush eventfd. Backpressure on the ring capacity (respect RING_SLOTS-1 so we never overwrite an
// unconsumed slot — the correctness this reproducer's earlier draft got wrong).
static int run_producer(struct cbstate *st, uint32_t *ring, int flush_efd) {
    for (uint32_t seq = 0; seq < N_CMDS; seq++) {
        // Wait for ring space: keep at most RING_SLOTS-1 in flight so put never laps get.
        uint32_t g;
        while ((seq - (g = atomic_load_explicit(&st->get, memory_order_acquire))) >= RING_SLOTS - 1) {
            struct timespec ts = {0, 50 * 1000}; // 50us; consumer is fast
            nanosleep(&ts, NULL);
        }
        uint32_t slot = seq % RING_SLOTS;
        uint32_t *rec = ring + slot * CMD_WORDS;
        // Write payload first, header LAST — but the consumer gates on the STATE put offset (release),
        // so ordering across the ring is provided by that single release/acquire, exactly like a real
        // command buffer (put is the publish point).
        for (uint32_t w = 2; w < CMD_WORDS; w++) rec[w] = seq * 2654435761u + w;
        rec[1] = seq;                        // seq
        rec[0] = CMD_MAGIC;                  // magic (header)
        atomic_store_explicit(&st->put, seq + 1, memory_order_release); // publish (put advance)
        // AsyncFlush: signal the service process. Coalescing is fine (eventfd counter), like IPC.
        uint64_t one = 1;
        if (write(flush_efd, &one, sizeof one) != sizeof one && errno != EAGAIN) return 2;
    }
    return 0;
}

// The SERVICE PROCESS (parent, consumer): block on the flush eventfd, then drain [get..put], verifying each
// command's magic+seq is COHERENT (i.e. the producer's cross-process writes are visible), advancing get.
static int run_consumer(struct cbstate *st, uint32_t *ring, int flush_efd, char *err, size_t errsz) {
    uint32_t next = 0; // expected seq
    while (next < N_CMDS) {
        uint64_t cnt;
        ssize_t r = read(flush_efd, &cnt, sizeof cnt); // AsyncFlush wake (blocks; watchdog guards a lost wake)
        if (r != sizeof cnt) {
            if (errno == EINTR) continue;
            if (errno == EAGAIN) { // spurious EAGAIN on a BLOCKING eventfd — count + retry (see cross-proc race)
                extern _Atomic unsigned g_spurious_eagain;
                atomic_fetch_add(&g_spurious_eagain, 1u);
                struct timespec ts = {0, 20 * 1000};
                nanosleep(&ts, NULL);
                continue;
            }
            snprintf(err, errsz, "shm_ring FAIL: eventfd read errno=%d\n", errno);
            return 3;
        }
        atomic_fetch_add(&g_wakes, 1u); // an AsyncFlush wake was delivered (distinguishes lost-wake vs stale-put)
        // Drain everything the producer has PUBLISHED (put, acquire). Ring reads strictly FOLLOW the put
        // acquire-load — the command-buffer publish protocol — so a coherent ring never reads ahead of put.
        for (;;) {
            uint32_t put = atomic_load_explicit(&st->put, memory_order_acquire);
            if (put == next) break; // nothing new published; re-block on the next flush
            while (next < put) {
                uint32_t slot = next % RING_SLOTS;
                uint32_t *rec = ring + slot * CMD_WORDS;
                uint32_t magic = rec[0], gotseq = rec[1];
                if (magic != CMD_MAGIC || gotseq != next) {
                    snprintf(err, errsz,
                             "shm_ring FAIL ring-incoherence: at seq=%u slot=%u got magic=0x%x seq=%u "
                             "(TRANSFER-BUFFER memfd not coherent cross-process)\n",
                             next, slot, magic, gotseq);
                    return 5;
                }
                // Verify a payload word too (catches a torn / partial-page write mid-record).
                if (rec[CMD_WORDS - 1] != next * 2654435761u + (CMD_WORDS - 1)) {
                    snprintf(err, errsz, "shm_ring FAIL ring-incoherence: torn payload at seq=%u\n", next);
                    return 5;
                }
                next++;
            }
            atomic_store_explicit(&st->get, next, memory_order_release); // publish get (unblock producer)
            atomic_store(&g_next, next);                                 // watchdog progress
        }
    }
    return 0;
}

int main(void) {
    // multi-process application's regime: a large fd interest set. Burn >1024 fds so any fd-count-sensitive path is exercised.
    for (int i = 0; i < IDLE_FDS; i++)
        if (dup(2) < 0) break;

    // Create the two shared regions as memfds (exactly like multi-process application's base::UnsafeSharedMemoryRegion).
    int state_fd = memfd_create_("cbstate", 0);
    int ring_fd = memfd_create_("cbring", 0);
    if (state_fd < 0 || ring_fd < 0) {
        printf("shm_ring FAIL: memfd_create errno=%d\n", errno);
        return 1;
    }
    if (ftruncate(state_fd, sizeof(struct cbstate)) != 0 || ftruncate(ring_fd, RING_BYTES) != 0) {
        printf("shm_ring FAIL: ftruncate errno=%d\n", errno);
        return 1;
    }
    int flush_efd = eventfd(0, 0); // blocking eventfd = the AsyncFlush signal
    if (flush_efd < 0) {
        printf("shm_ring FAIL: eventfd errno=%d\n", errno);
        return 1;
    }

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        printf("shm_ring FAIL: socketpair\n");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        printf("shm_ring FAIL: fork\n");
        return 1;
    }
    if (pid == 0) {
        // Child = WORKER (producer). Receives the fds over SCM_RIGHTS (multi-process application transfers the memfds via
        // IPC/SCM_RIGHTS; the child maps the RECEIVED fds, not the fork-inherited ones — the faithful path).
        close(sv[0]);
        int fds[3];
        if (recv_fds(sv[1], fds) != 0) _exit(11);
        int rstate = fds[0], rring = fds[1], reff = fds[2];
        struct cbstate *st = mmap(NULL, sizeof(struct cbstate), PROT_READ | PROT_WRITE, MAP_SHARED, rstate, 0);
        uint32_t *ring = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, rring, 0);
        if (st == MAP_FAILED || ring == MAP_FAILED) _exit(12);
        int rc = run_producer(st, ring, reff);
        _exit(rc);
    }

    // Parent = SERVICE PROCESS (consumer). Send the fds, then map + drain.
    close(sv[1]);
    int fds[3] = {state_fd, ring_fd, flush_efd};
    if (send_fds(sv[0], fds) != 0) {
        printf("shm_ring FAIL: send_fds\n");
        return 1;
    }
    struct cbstate *st = mmap(NULL, sizeof(struct cbstate), PROT_READ | PROT_WRITE, MAP_SHARED, state_fd, 0);
    uint32_t *ring = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, ring_fd, 0);
    if (st == MAP_FAILED || ring == MAP_FAILED) {
        printf("shm_ring FAIL: parent mmap\n");
        return 1;
    }

    pthread_t wd;
    pthread_create(&wd, NULL, watchdog, NULL);

    char err[256] = {0};
    int rc = run_consumer(st, ring, flush_efd, err, sizeof err);
    g_done = 1;

    if (rc != 0) kill(pid, SIGKILL); // consumer bailed: the producer may be blocked on backpressure — don't hang
    int status = 0;
    waitpid(pid, &status, 0);
    int cexit = WIFEXITED(status) ? WEXITSTATUS(status) : -1;

    if (rc != 0) {
        fputs(err, stdout);
        return rc;
    }
    if (cexit != 0) {
        printf("shm_ring FAIL: producer exit=%d\n", cexit);
        return 6;
    }
    printf("shm_ring ok cmds=%u wraps=%u child_exit=0 spurious_eagain=%u\n", N_CMDS, N_CMDS / RING_SLOTS,
           atomic_load(&g_spurious_eagain));
    return 0;
}
