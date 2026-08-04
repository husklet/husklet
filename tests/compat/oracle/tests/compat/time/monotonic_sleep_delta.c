// Relationship check: sleeping for a bounded interval advances CLOCK_MONOTONIC by at least (most
// of) the requested duration and by no more than a generous ceiling. This asserts the clock is
// wired to real elapsed time, not a stalled or free-running counter. We keep sleeps <= 200ms.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>

static long long ns(clockid_t c) {
    struct timespec ts;
    clock_gettime(c, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int slept_ok(clockid_t c, long ms) {
    long long t0 = ns(c);
    struct timespec req = {ms / 1000, (ms % 1000) * 1000000L};
    struct timespec rem;
    while (nanosleep(&req, &rem) == -1) req = rem; // finish the full sleep
    long long d = ns(c) - t0;
    long long want = (long long)ms * 1000000LL;
    return d >= want - 5 * 1000000LL && d < want + 500LL * 1000000LL;
}

int main(void) {
    int mono = slept_ok(CLOCK_MONOTONIC, 100);
    int raw = slept_ok(CLOCK_MONOTONIC_RAW, 100);
    int boot = slept_ok(CLOCK_BOOTTIME, 100);
    int real = slept_ok(CLOCK_REALTIME, 100);
    printf("monosleep mono=%d raw=%d boot=%d real=%d\n", mono, raw, boot, real);
    return 0;
}
