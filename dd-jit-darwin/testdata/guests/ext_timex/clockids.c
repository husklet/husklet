// Linux-specific clock ids: BOOTTIME/MONOTONIC_RAW/MONOTONIC_COARSE/REALTIME_COARSE all read and
// advance sensibly. No portable form (macOS lacks these ids) -> Linux-only, native oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <unistd.h>

static long long ns(clockid_t c) {
    struct timespec ts;
    if (clock_gettime(c, &ts) != 0) return -1;
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main(void) {
    int boot = ns(CLOCK_BOOTTIME) > 0;
    int raw0 = ns(CLOCK_MONOTONIC_RAW) > 0;
    int mcoarse = ns(CLOCK_MONOTONIC_COARSE) > 0;
    int rcoarse = ns(CLOCK_REALTIME_COARSE) > 0;
    long long r0 = ns(CLOCK_MONOTONIC_RAW);
    usleep(20000);
    long long r1 = ns(CLOCK_MONOTONIC_RAW);
    int raw_advances = r1 >= r0;
    printf("clockids boot=%d raw=%d mcoarse=%d rcoarse=%d advances=%d\n",
           boot, raw0, mcoarse, rcoarse, raw_advances);
    return 0;
}
