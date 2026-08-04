// clock_nanosleep TIMER_ABSTIME: sleeps until an absolute monotonic deadline; past deadline returns immediately.
#include <time.h>
#include <stdio.h>

static long elapsed_ms(const struct timespec *a, const struct timespec *b) {
    return (b->tv_sec - a->tv_sec) * 1000 + (b->tv_nsec - a->tv_nsec) / 1000000;
}

int main(void) {
    struct timespec start, target, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    target = start;
    target.tv_nsec += 80 * 1000 * 1000; // +80ms
    if (target.tv_nsec >= 1000000000L) { target.tv_nsec -= 1000000000L; target.tv_sec++; }
    int rc = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &target, NULL);
    clock_gettime(CLOCK_MONOTONIC, &end);
    long slept = elapsed_ms(&start, &end);
    int slept_enough = rc == 0 && slept >= 70;

    // A deadline already in the past returns immediately (still success).
    struct timespec past = {0, 0};
    clock_gettime(CLOCK_MONOTONIC, &past);
    if (past.tv_nsec == 0) past.tv_nsec = 1;
    past.tv_nsec -= 1;
    int rc2 = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &past, NULL);
    printf("clocknano slept_enough=%d past_immediate=%d\n", slept_enough, rc2 == 0);
    return 0;
}
