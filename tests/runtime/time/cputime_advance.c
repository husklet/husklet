// PROCESS_CPUTIME_ID and THREAD_CPUTIME_ID must advance under a busy loop, and must advance by
// less than the elapsed MONOTONIC wall time is unreliable to assert, so we only assert forward
// progress and that a pure sleep does NOT advance thread CPU time appreciably (CPU time tracks
// on-CPU work, not wall clock).
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <unistd.h>

static long long ns(clockid_t c) {
    struct timespec ts;
    if (clock_gettime(c, &ts) != 0) return -1;
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static void burn(long iters) {
    volatile double x = 1.0;
    for (long i = 1; i <= iters; i++) x = x * 1.0000001 + 1.0;
    (void)x;
}

int main(void) {
    long long p0 = ns(CLOCK_PROCESS_CPUTIME_ID);
    long long t0 = ns(CLOCK_THREAD_CPUTIME_ID);
    burn(50L * 1000 * 1000);
    long long p1 = ns(CLOCK_PROCESS_CPUTIME_ID);
    long long t1 = ns(CLOCK_THREAD_CPUTIME_ID);
    int proc_adv = p1 > p0;
    int thr_adv = t1 > t0;

    // A pure sleep should add little CPU time relative to the ~50ms wall sleep.
    long long t2 = ns(CLOCK_THREAD_CPUTIME_ID);
    usleep(50 * 1000);
    long long t3 = ns(CLOCK_THREAD_CPUTIME_ID);
    int sleep_cheap = (t3 - t2) < 30 * 1000 * 1000; // under 30ms cpu for a 50ms sleep

    // Values are non-negative.
    int nonneg = p1 >= 0 && t1 >= 0;
    printf("cputime proc_adv=%d thr_adv=%d sleep_cheap=%d nonneg=%d\n", proc_adv, thr_adv, sleep_cheap,
           nonneg);
    return 0;
}
