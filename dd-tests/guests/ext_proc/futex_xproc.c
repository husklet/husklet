// Cross-process futex on a MAP_SHARED page across fork() (the LTP tst_checkpoint / fork04 pattern).
// A FUTEX_WAKE in one process must wake a FUTEX_WAIT in another on the SAME shared physical page:
//   A. child FUTEX_WAITs, parent stores + FUTEX_WAKEs   -> child wakes
//   B. parent FUTEX_WAITs, child stores + FUTEX_WAKEs   -> parent wakes (reverse direction)
//   C. FUTEX_WAIT with a relative timeout on an unchanged word returns -1/ETIMEDOUT
//   D. FUTEX_WAKE(n) wakes N distinct waiters camped on one shared address
// All waits are UNTIMED (except C): if a cross-process wake is lost the waiter blocks forever and the
// harness times out -> a hard failure that reproduces the bug. Linux-only (raw futex(2)) -> oracle.
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long fwait(int *addr, int expected, const struct timespec *to) {
    return syscall(SYS_futex, addr, FUTEX_WAIT, expected, to, NULL, 0);
}
static long fwake(int *addr, int n) { return syscall(SYS_futex, addr, FUTEX_WAKE, n, NULL, NULL, 0); }

// Block until the shared word leaves 0, re-checking on every (possibly spurious) wakeup.
static void wait_until_set(int *w) {
    while (atomic_load((_Atomic int *)w) == 0)
        fwait(w, 0, NULL);
}

int main(void) {
    // One shared, fork-inherited page holds every futex word + the cross-process counters.
    int *sh = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (sh == MAP_FAILED) {
        printf("xproc-futex mmap-failed\n");
        return 1;
    }
    int *wa = &sh[0];              // A: parent -> child
    int *wb = &sh[1];              // B: child -> parent
    int *wd = &sh[2];              // D: parent -> N children
    _Atomic int *woke = (_Atomic int *)&sh[3]; // D: waiters that returned
    *wa = *wb = *wd = 0;
    atomic_store(woke, 0);

    struct timespec nap = {0, 120 * 1000 * 1000}; // 120ms: let the waiter park in FUTEX_WAIT first

    // ---- A: child waits, parent wakes ----
    pid_t a = fork();
    if (a == 0) {
        wait_until_set(wa);
        _exit(42);
    }
    nanosleep(&nap, NULL);
    atomic_store((_Atomic int *)wa, 1);
    long a_rc = fwake(wa, INT_MAX); // LTP tst_checkpoint_wake pattern: WAKE(INT_MAX) must return 1, not INT_MAX
    int st = 0;
    waitpid(a, &st, 0);
    int a_wake = (WIFEXITED(st) && WEXITSTATUS(st) == 42) && a_rc == 1;

    // ---- B: parent waits, child wakes (reverse direction) ----
    pid_t b = fork();
    if (b == 0) {
        nanosleep(&nap, NULL);
        atomic_store((_Atomic int *)wb, 1);
        fwake(wb, 1);
        _exit(0);
    }
    wait_until_set(wb); // parent parks here; only the child's cross-process WAKE releases it
    waitpid(b, &st, 0);
    int b_wake = 1;

    // ---- C: FUTEX_WAIT relative timeout on an unchanged word ----
    int local = 0;
    struct timespec to = {0, 100 * 1000 * 1000};
    long rc = fwait(&local, 0, &to);
    int timeout_ok = rc == -1 && errno == ETIMEDOUT;

    // ---- D: FUTEX_WAKE(N) wakes N distinct cross-process waiters ----
    const int N = 4;
    pid_t kids[4];
    for (int i = 0; i < N; i++) {
        kids[i] = fork();
        if (kids[i] == 0) {
            wait_until_set(wd);
            atomic_fetch_add(woke, 1);
            _exit(0);
        }
    }
    struct timespec longer = {0, 250 * 1000 * 1000};
    nanosleep(&longer, NULL);          // let all N park
    // WAKE with no waiter on a different word must report 0.
    int none = 0;
    long zero_rc = fwake(&none, INT_MAX);
    atomic_store((_Atomic int *)wd, 1);
    long d_rc = fwake(wd, INT_MAX);    // one WAKE releases all N -> returns N
    for (int i = 0; i < N; i++) waitpid(kids[i], &st, 0);
    int nwake = atomic_load(woke) == N && d_rc == N && zero_rc == 0;

    printf("xproc-futex a_wake=%d b_wake=%d timeout=%d nwake=%d\n", a_wake, b_wake, timeout_ok, nwake);
    return 0;
}
