// Direct futex(2) FUTEX_WAIT/FUTEX_WAKE between two threads (no pthread mutex): a waiter blocks on a
// shared word; the main thread stores the new value and wakes exactly one waiter. Linux-only -> oracle.
#define _GNU_SOURCE
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static atomic_int word = 0;
static volatile int woke = 0;

// FUTEX_WAKE_OP (glibc pthread_cond_signal/broadcast uses this): a condition word woken like a plain WAKE,
// plus an atomic op applied to a SECOND word (uaddr2). cw is the condition word; wv is the arithmetic target.
static atomic_int cw = 0;
static atomic_int wv = 5;
static volatile int woke2 = 0;

static long fwait(int *addr, int expected) {
    return syscall(SYS_futex, addr, FUTEX_WAIT, expected, NULL, NULL, 0);
}
static long fwake(int *addr, int n) {
    return syscall(SYS_futex, addr, FUTEX_WAKE, n, NULL, NULL, 0);
}
// futex(uaddr, FUTEX_WAKE_OP, nr_wake, nr_wake2, uaddr2, val3): wake nr_wake on uaddr, atomically apply the
// val3-encoded op to *uaddr2, and (if the pre-op value satisfies the encoded cmp) wake nr_wake2 on uaddr2.
static long fwakeop(int *addr, int n, int *addr2, int n2, int val3) {
    return syscall(SYS_futex, addr, FUTEX_WAKE_OP, n, n2, addr2, val3);
}

static void *waiter(void *arg) {
    (void)arg;
    // spin until the value flips to 1 via a futex wait; re-check on spurious wakeups
    while (atomic_load(&word) == 0) {
        fwait((int *)&word, 0);
    }
    woke = 1;
    return NULL;
}

static void *waiter2(void *arg) {
    (void)arg;
    while (atomic_load(&cw) == 0) {
        fwait((int *)&cw, 0);
    }
    woke2 = 1;
    return NULL;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, NULL, waiter, NULL);
    struct timespec s = { .tv_sec = 0, .tv_nsec = 50000000 };
    nanosleep(&s, NULL);            // let the waiter block in FUTEX_WAIT
    atomic_store(&word, 1);
    long n = fwake((int *)&word, 1);
    pthread_join(t, NULL);
    int woke_one = n >= 0;          // number actually woken (0 or 1 depending on timing)
    printf("futex woke=%d wake_rc_ok=%d\n", woke, woke_one);

    // FUTEX_WAKE_OP: park a waiter on cw, then wake it AND mutate wv atomically in the same syscall.
    // Encoding: ADD 3 to wv, then compare the pre-op value (5) > 4 -> true. wv MUST become 8 (deterministic,
    // timing-independent) regardless of whether the wake races; the wake of cw mirrors the plain-WAKE test.
    pthread_t t2;
    pthread_create(&t2, NULL, waiter2, NULL);
    nanosleep(&s, NULL);            // let waiter2 block in FUTEX_WAIT on cw
    atomic_store(&cw, 1);
    int v3 = FUTEX_OP(FUTEX_OP_ADD, 3, FUTEX_OP_CMP_GT, 4);
    long n2 = fwakeop((int *)&cw, 1, (int *)&wv, 0, v3);
    pthread_join(t2, NULL);
    printf("wakeop woke2=%d wv=%d wakeop_rc_ok=%d\n", woke2, atomic_load(&wv), n2 >= 0);
    return 0;
}
