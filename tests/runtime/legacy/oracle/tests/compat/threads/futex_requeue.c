// FUTEX_CMP_REQUEUE (the primitive behind glibc pthread_cond_broadcast): waiters parked on futex A are
// atomically requeued onto futex B without a thundering-herd wake, then woken on B. We park 6 waiters
// on A, requeue up to 5 of them to B (waking at most 1 on A), then FUTEX_WAKE all on B. The derived
// facts -- every waiter eventually returns, and the requeue syscall reported a non-negative count --
// are timing-independent because we only wake after all six have parked (barrier) and drain to zero.
#define _GNU_SOURCE
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define WAITERS 6
static int fa = 0, fb = 0;
static atomic_int parked = 0;
static atomic_int returned = 0;

static long futex(int *a, int op, int val, void *ts, int *a2, int val3) {
    return syscall(SYS_futex, a, op, val, ts, a2, val3);
}

static void *waiter(void *_) {
    (void)_;
    atomic_fetch_add(&parked, 1);
    // Wait on A; may be woken from A or requeued to B and woken there. Loop until B is signalled (fb!=0).
    while (atomic_load((atomic_int *)&fb) == 0) {
        futex(&fa, FUTEX_WAIT, 0, NULL, NULL, 0);
        if (atomic_load((atomic_int *)&fb) != 0) break;
    }
    atomic_fetch_add(&returned, 1);
    return 0;
}

int main(void) {
    pthread_t t[WAITERS];
    for (int i = 0; i < WAITERS; i++) pthread_create(&t[i], 0, waiter, 0);
    // wait until all six are parked on A
    while (atomic_load(&parked) < WAITERS) { struct timespec ts = {0, 200000}; nanosleep(&ts, 0); }
    struct timespec ts = {0, 20 * 1000000}; nanosleep(&ts, 0); // ensure they are blocked in FUTEX_WAIT

    // Set B's condition, then CMP_REQUEUE: wake up to 1 on A, requeue the rest onto B.
    atomic_store((atomic_int *)&fb, 1);
    long rq = futex(&fa, FUTEX_CMP_REQUEUE, 1, (void *)(long)(WAITERS), &fb, 0);
    int requeue_ok = rq >= 0;
    // Wake everything now parked on B (and belt-and-suspenders also wake A for any woken-on-A waiter).
    futex(&fb, FUTEX_WAKE, WAITERS, NULL, NULL, 0);
    futex(&fa, FUTEX_WAKE, WAITERS, NULL, NULL, 0);

    for (int i = 0; i < WAITERS; i++) pthread_join(t[i], 0);
    printf("futex_requeue requeue_ok=%d returned=%d\n", requeue_ok, atomic_load(&returned));
    return 0;
}
