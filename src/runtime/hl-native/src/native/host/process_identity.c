#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "system.h"
#include "hl/base.h"

#include <errno.h>
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
 * The memo covers the CALLING process only. It records the pid it was taken under and compares that
 * against getpid() on every hit, so a fork invalidates it with no hook: the child observes a different
 * pid and re-reads. A live process's own pid is by construction not available for reuse, so the
 * pid-reuse hazard this token exists to detect cannot alias a self observation.
 *
 * Peer pids are deliberately NOT memoized. A recycled peer pid paired with a remembered start time is
 * precisely the stale-identity failure the token guards against, and every peer caller here uses it to
 * decide membership, privacy or teardown. Those must stay fresh observations.
 *
 * Threads share the process, so several may fill the memo concurrently with the same value. The pid is
 * published last with release ordering and read first with acquire ordering, so a reader that observes
 * the pid also observes the start time stored before it. */
static _Atomic int64_t g_self_identity_pid;
static _Atomic uint64_t g_self_identity_start_ns;

int hl_host_process_start_time_ns(int64_t pid, uint64_t *start_time_ns) {
    hl_host_process_info info;
    int64_t self;
    if (start_time_ns == NULL || pid <= 0) return 0;
    self = (int64_t)getpid();
    if (pid == self && atomic_load_explicit(&g_self_identity_pid, memory_order_acquire) == self) {
        *start_time_ns = atomic_load_explicit(&g_self_identity_start_ns, memory_order_acquire);
        return 1;
    }
    if (!hl_host_process_read(pid, &info)) return 0;
    if (pid == self) {
        atomic_store_explicit(&g_self_identity_start_ns, info.start_time_ns, memory_order_release);
        atomic_store_explicit(&g_self_identity_pid, self, memory_order_release);
    }
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
 * from the memo, even immediately after a self call primed it. Scenario 4: an absent pid fails. */
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
            if (read(pipefd[0], &child_token, sizeof child_token) == (ssize_t)sizeof child_token &&
                child_token != 0 && child_token != token)
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
        if (!hl_host_process_read(peer, &record)) return 0; /* peer unreadable here; nothing to pin */
        if (!hl_host_process_start_time_ns(peer, &peer_token)) return -1;
        return peer_token == record.start_time_ns && peer_token != token ? 0 : -1;
    }
    if (scenario == 4) return hl_host_process_start_time_ns(-1, &token) == 0 &&
                                      hl_host_process_start_time_ns(self, NULL) == 0
                                  ? 0
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
