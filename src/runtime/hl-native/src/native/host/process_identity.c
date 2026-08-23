#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "system.h"
#include "hl/base.h"

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <unistd.h>

/* A process's start time is fixed for its whole lifetime, so the engine's own identity token never
 * changes while its pid does not. The callers that need only that token -- the fdvis owner stamp, the
 * engine-private descriptor band, the descriptor-privacy classification run per inspected descriptor,
 * and the container registry birth records -- otherwise open, read and parse /proc/<pid>/stat on every
 * guest open(), close() and descriptor inspection. That read and its field parse dominate the cost of a
 * guest open().
 *
 * The memo covers the CALLING process only, and it memoizes the PID as well as the start time. An
 * earlier revision re-read getpid() on every hit to detect fork; that recheck alone was measured at 59
 * of the 68 getpid() calls the engine issues per guest open(), the single largest syscall source in the
 * open path. glibc dropped its own getpid() cache because a raw clone() can create a process without
 * the library learning of it, and that hazard is real -- so the memo is not merely "cached forever".
 * It is keyed on a FORK EPOCH: a pthread_atfork() child handler bumps hl_identity_epoch before fork()
 * returns in the child, and a memo stamped with a different epoch is a miss. The child therefore
 * re-reads its own /proc record exactly once and never answers with its parent's identity.
 *
 * The engine creates every host process through hl_host_process_clone_current(), which is fork(); it
 * has no vfork() and no raw clone() call site, so pthread_atfork() covers all of them. Two further
 * defences make that a belt rather than an assumption: fork_child_hooks() in linux_abi/syscall/proc.c
 * calls hl_host_process_identity_after_fork() explicitly beside the sibling pid caches it already
 * drops, and the memo is only armed if pthread_atfork() actually accepted the handler -- if it did
 * not, hl_identity_armed stays zero and every call re-reads, which is exactly the previous behaviour.
 *
 * Peer pids are deliberately NOT memoized. A recycled peer pid paired with a remembered start time is
 * precisely the stale-identity failure the token guards against, and every peer caller here uses it to
 * decide membership, privacy or teardown. Those must stay fresh observations.
 *
 * Threads share the process, so several may fill the memo concurrently with the same value. The epoch
 * is published last with release ordering and read first with acquire ordering, so a reader that
 * observes the epoch also observes the pid and start time stored before it. */
static _Atomic uint64_t hl_identity_epoch = 1;
static _Atomic uint64_t hl_identity_memo_epoch;
static _Atomic int64_t hl_identity_memo_pid;
static _Atomic uint64_t hl_identity_memo_start_ns;
static _Atomic int hl_identity_armed;
static pthread_once_t hl_identity_atfork_once = PTHREAD_ONCE_INIT;

void hl_host_process_identity_after_fork(void) {
    /* Runs in the fork child before fork() returns, so it must not allocate, lock or log. A single
     * relaxed increment retires every memo stamped by the parent. */
    (void)atomic_fetch_add_explicit(&hl_identity_epoch, 1, memory_order_relaxed);
}

static void hl_identity_arm(void) {
    if (pthread_atfork(NULL, NULL, hl_host_process_identity_after_fork) == 0)
        atomic_store_explicit(&hl_identity_armed, 1, memory_order_release);
}

int hl_host_process_self_identity(int64_t *pid, uint64_t *start_time_ns) {
    hl_host_process_info info;
    uint64_t epoch;
    int64_t self;
    if (pid == NULL || start_time_ns == NULL) return 0;
    (void)pthread_once(&hl_identity_atfork_once, hl_identity_arm);
    epoch = atomic_load_explicit(&hl_identity_epoch, memory_order_relaxed);
    if (atomic_load_explicit(&hl_identity_armed, memory_order_acquire) &&
        atomic_load_explicit(&hl_identity_memo_epoch, memory_order_acquire) == epoch) {
        *pid = atomic_load_explicit(&hl_identity_memo_pid, memory_order_acquire);
        *start_time_ns = atomic_load_explicit(&hl_identity_memo_start_ns, memory_order_acquire);
        return 1;
    }
    self = (int64_t)getpid();
    if (self <= 0 || !hl_host_process_read(self, &info)) return 0;
    if (atomic_load_explicit(&hl_identity_armed, memory_order_acquire)) {
        atomic_store_explicit(&hl_identity_memo_pid, self, memory_order_release);
        atomic_store_explicit(&hl_identity_memo_start_ns, info.start_time_ns, memory_order_release);
        atomic_store_explicit(&hl_identity_memo_epoch, epoch, memory_order_release);
    }
    *pid = self;
    *start_time_ns = info.start_time_ns;
    return 1;
}

int hl_host_process_start_time_ns(int64_t pid, uint64_t *start_time_ns) {
    hl_host_process_info info;
    int64_t self = 0;
    uint64_t self_start = 0;
    if (start_time_ns == NULL || pid <= 0) return 0;
    if (hl_host_process_self_identity(&self, &self_start) && pid == self) {
        *start_time_ns = self_start;
        return 1;
    }
    if (!hl_host_process_read(pid, &info)) return 0;
    *start_time_ns = info.start_time_ns;
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
#include <sys/wait.h>
#include <time.h>

HL_API int hl_c_backend_process_identity_token_test(uint32_t scenario);

/* Pins the two properties the memo must never lose. Scenario 1: the token a process reports for itself
 * equals the unmemoized /proc record and is stable across repeated calls. Scenario 2: a forked child
 * reports ITS OWN token, not the parent's -- the memo must be keyed on the pid it was taken under, so a
 * child that inherits the parent's memoized bytes still re-reads. Scenario 3: a peer pid is never served
 * from the memo, even immediately after a self call primed it. Scenario 4: an absent pid fails. Scenario 5:
 * a forked child's hl_host_process_self_identity() reports the CHILD's pid and start time, which is the
 * property the fork epoch exists to hold and the one scenario 2 cannot reach. */
HL_API int hl_c_backend_process_identity_token_test(uint32_t scenario) {
    hl_host_process_info record;
    uint64_t token = 0;
    uint64_t again = 0;
    int64_t self = (int64_t)getpid();
    if (scenario == 1) {
        if (!hl_host_process_read(self, &record)) return -1;
        if (!hl_host_process_start_time_ns(self, &token) || token != record.start_time_ns) return -1;
        if (!hl_host_process_start_time_ns(self, &again) || again != token) return -1;
        return 0;
    }
    if (scenario == 2) {
        int pipefd[2];
        pid_t child;
        int outcome = -1;
        uint64_t child_token = 0;
        struct timespec settle = {0, 150 * 1000 * 1000};
        if (!hl_host_process_start_time_ns(self, &token)) return -1; /* prime the memo before forking */
        /* start_time_ns is derived from /proc field 22, which the kernel reports in clock ticks -- 10 ms
         * here. A child forked immediately shares its parent's tick and would report the same token
         * whether the memo was rechecked or not, so the assertion below would hold vacuously. Settling
         * well past one tick makes the child's own start time genuinely distinct, which is what turns
         * this into a test of the pid recheck rather than of arithmetic. */
        (void)nanosleep(&settle, NULL);
        if (pipe(pipefd) != 0) return -1;
        child = fork();
        if (child == 0) {
            uint64_t mine = 0;
            hl_host_process_info direct;
            int ok = hl_host_process_start_time_ns((int64_t)getpid(), &mine) &&
                     hl_host_process_read((int64_t)getpid(), &direct) && mine == direct.start_time_ns;
            ssize_t sent = write(pipefd[1], ok ? &mine : &child_token, sizeof mine);
            (void)sent;
            _exit(0);
        }
        (void)close(pipefd[1]);
        if (child > 0) {
            int status = 0;
            if (read(pipefd[0], &child_token, sizeof child_token) == (ssize_t)sizeof child_token && child_token != 0 &&
                child_token != token)
                outcome = 0;
            (void)waitpid(child, &status, 0);
        }
        (void)close(pipefd[0]);
        return outcome;
    }
    if (scenario == 3) {
        int64_t peer = 1; /* pid 1 always exists and is never this process */
        uint64_t peer_token = 0;
        if (peer == self) return 0;
        if (!hl_host_process_start_time_ns(self, &token)) return -1; /* prime the memo */
        if (!hl_host_process_read(peer, &record)) return 0;          /* peer unreadable here; nothing to pin */
        if (!hl_host_process_start_time_ns(peer, &peer_token)) return -1;
        return peer_token == record.start_time_ns && peer_token != token ? 0 : -1;
    }
    if (scenario == 5) {
        /* The fork epoch. A child must report ITS OWN identity from hl_host_process_self_identity(),
         * which unlike scenario 2 cannot fall back on a caller-supplied pid: with the atfork handler
         * absent the child hits the parent's memo and answers with the parent's pid outright, which is
         * exactly the row-ownership defect the epoch prevents. The parent primes the memo and settles
         * past one /proc clock tick first, so the child's start time is genuinely distinct too. */
        int pipefd[2];
        pid_t child;
        int outcome = -1;
        uint64_t message[2] = {0, 0};
        int64_t primed_pid = 0;
        struct timespec settle = {0, 150 * 1000 * 1000};
        if (!hl_host_process_self_identity(&primed_pid, &token) || primed_pid != self) return -1;
        (void)nanosleep(&settle, NULL);
        if (pipe(pipefd) != 0) return -1;
        child = fork();
        if (child == 0) {
            int64_t mine_pid = 0;
            uint64_t mine_start = 0;
            hl_host_process_info direct;
            uint64_t reply[2] = {0, 0};
            if (hl_host_process_self_identity(&mine_pid, &mine_start) &&
                hl_host_process_read((int64_t)getpid(), &direct) && mine_pid == (int64_t)getpid() &&
                mine_start == direct.start_time_ns) {
                reply[0] = (uint64_t)mine_pid;
                reply[1] = mine_start;
            }
            ssize_t sent = write(pipefd[1], reply, sizeof reply);
            (void)sent;
            _exit(0);
        }
        (void)close(pipefd[1]);
        if (child > 0) {
            int status = 0;
            if (read(pipefd[0], message, sizeof message) == (ssize_t)sizeof message && message[0] != 0 &&
                (int64_t)message[0] == (int64_t)child && message[1] != 0 && message[1] != token)
                outcome = 0;
            (void)waitpid(child, &status, 0);
        }
        (void)close(pipefd[0]);
        return outcome;
    }
    if (scenario == 4)
        return hl_host_process_start_time_ns(-1, &token) == 0 && hl_host_process_start_time_ns(self, NULL) == 0 ? 0
                                                                                                                : -1;
    errno = EINVAL;
    return -1;
}
#elif defined(HL_NATIVE_TEST_HOOKS)
/* The scenarios above fork and wait; the Windows target has neither, and the loader still resolves
 * every exported test symbol, so the hook must exist and refuse. */
HL_API int hl_c_backend_process_identity_token_test(uint32_t scenario);

HL_API int hl_c_backend_process_identity_token_test(uint32_t scenario) {
    (void)scenario;
    errno = ENOTSUP;
    return -1;
}
#endif
