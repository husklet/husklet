// clock_nanosleep boundaries: an absolute deadline already in the past returns 0 at once,
// out-of-range nsec is EINVAL, a negative relative time is EINVAL, CLOCK_THREAD_CPUTIME_ID
// is not permitted as a sleep clock, and a bogus clockid is EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    struct timespec past = {now.tv_sec - 10, now.tv_nsec};
    int a = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &past, NULL);
    struct timespec badns = {0, 1000000000};
    int b = clock_nanosleep(CLOCK_MONOTONIC, 0, &badns, NULL);
    struct timespec neg = {-1, 0};
    int c = clock_nanosleep(CLOCK_MONOTONIC, 0, &neg, NULL);
    struct timespec z = {0, 0};
    int d = clock_nanosleep(CLOCK_THREAD_CPUTIME_ID, 0, &z, NULL);
    int e = clock_nanosleep(99, 0, &z, NULL);
    int f = clock_nanosleep(CLOCK_REALTIME, 0, &z, NULL);
    struct timespec rem = {7, 7};
    int g = clock_nanosleep(CLOCK_MONOTONIC, 0, &z, &rem);
    printf("a=%d b=%d c=%d d=%d e=%d f=%d g=%d rem=%ld.%ld\n", a, b, c, d, e, f, g,
           (long)rem.tv_sec, (long)rem.tv_nsec);
    return 0;
}
