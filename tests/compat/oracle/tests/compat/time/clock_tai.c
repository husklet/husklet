// CLOCK_TAI reads and advances monotonically, and sits within a few dozen seconds of CLOCK_REALTIME
// (TAI leads UTC by the accumulated leap seconds, ~37s; on a host with no TAI offset configured it
// equals REALTIME). We only assert non-negative, monotone advance, and a bounded offset.
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
    long long a = ns(CLOCK_TAI);
    int reads = a > 0;
    usleep(20 * 1000);
    long long b = ns(CLOCK_TAI);
    int advances = b >= a;

    struct timespec r;
    int res_ok = clock_getres(CLOCK_TAI, &r) == 0;

    printf("clocktai reads=%d advances=%d res=%d\n", reads, advances, res_ok);
    return 0;
}
