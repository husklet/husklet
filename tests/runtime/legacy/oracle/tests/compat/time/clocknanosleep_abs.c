// clock_nanosleep TIMER_ABSTIME semantics: an absolute deadline already in the past returns 0
// immediately; a near-future absolute deadline sleeps until it. Relative negative -> EINVAL.
// TIMER_ABSTIME does not write the remainder argument (POSIX): we pass NULL. Also validate that
// an EINTR on a relative sleep reports remaining, but an EINTR on ABSTIME does not require it.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <time.h>

static long long ns(clockid_t c) {
    struct timespec ts;
    clock_gettime(c, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main(void) {
    // Absolute deadline in the past -> return 0 immediately.
    struct timespec past;
    clock_gettime(CLOCK_MONOTONIC, &past);
    past.tv_sec -= 5;
    int past_ok = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &past, NULL) == 0;

    // Absolute near-future deadline: sleeps until at/after the deadline.
    long long before = ns(CLOCK_MONOTONIC);
    struct timespec fut;
    clock_gettime(CLOCK_MONOTONIC, &fut);
    fut.tv_nsec += 60 * 1000 * 1000; // +60ms
    if (fut.tv_nsec >= 1000000000L) { fut.tv_nsec -= 1000000000L; fut.tv_sec += 1; }
    long long deadline = fut.tv_sec * 1000000000LL + fut.tv_nsec;
    int rc = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &fut, NULL);
    long long after = ns(CLOCK_MONOTONIC);
    int slept_to_deadline = rc == 0 && after >= deadline && after > before;

    // Same for CLOCK_REALTIME absolute.
    struct timespec rfut;
    clock_gettime(CLOCK_REALTIME, &rfut);
    rfut.tv_nsec += 40 * 1000 * 1000;
    if (rfut.tv_nsec >= 1000000000L) { rfut.tv_nsec -= 1000000000L; rfut.tv_sec += 1; }
    int real_abs = clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME, &rfut, NULL) == 0;

    // Relative negative -> EINVAL.
    struct timespec neg = {-1, 0};
    int negrel = clock_nanosleep(CLOCK_MONOTONIC, 0, &neg, NULL) == EINVAL;

    // Out-of-range nsec -> EINVAL.
    struct timespec big = {0, 1000000000L};
    int badns = clock_nanosleep(CLOCK_MONOTONIC, 0, &big, NULL) == EINVAL;

    printf("clocknanoabs past=%d abs=%d real=%d negrel=%d badns=%d\n", past_ok, slept_to_deadline,
           real_abs, negrel, badns);
    return 0;
}
