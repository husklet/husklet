// clockelapsed.c — wall-clock RATE correctness (#384). CLOCK_REALTIME, CLOCK_MONOTONIC and gettimeofday
// must all advance at the REAL host rate across a known nanosleep — not a scaled/truncated rate. This is
// the regression guard for the x86 vDSO fast-syscall timebase: a mis-scaled REALTIME read that advanced
// ~40x too slow (a 150ms sleep reading as ~4ms) would fail `real_ok` AND the `agree` cross-check below,
// while CLOCK_MONOTONIC stayed correct — exactly the reported divergence. Boolean verdicts so the output is
// golden across engines (raw wall-clock ns are not reproducible). The bands are deliberately wide (0.4x..20x
// of the request) to never flake on a coarse timer or a loaded CI host, yet still catch a 40x/frozen clock.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>

#define REQ_MS 150

static long long ns(clockid_t c) {
    struct timespec ts;
    clock_gettime(c, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}
static int band(long long elapsed_ns) {
    double ms = elapsed_ns / 1e6;
    return ms >= REQ_MS * 0.4 && ms <= REQ_MS * 20.0;
}

int main(void) {
    struct timespec req = {.tv_sec = 0, .tv_nsec = (long)REQ_MS * 1000000L};
    long long r0 = ns(CLOCK_REALTIME), m0 = ns(CLOCK_MONOTONIC);
    struct timeval g0;
    gettimeofday(&g0, 0);
    nanosleep(&req, NULL);
    long long r1 = ns(CLOCK_REALTIME), m1 = ns(CLOCK_MONOTONIC);
    struct timeval g1;
    gettimeofday(&g1, 0);

    long long re = r1 - r0, me = m1 - m0;
    long long gd = ((long long)(g1.tv_sec - g0.tv_sec) * 1000000LL + (g1.tv_usec - g0.tv_usec)) * 1000LL;
    // REALTIME and MONOTONIC share the host timebase, so their measured elapsed must agree closely; a
    // per-clock scaling bug (REALTIME slow while MONOTONIC correct) shows up as a large divergence here.
    int agree = llabs(re - me) < 50 * 1000000LL; // < 50ms
    printf("clockelapsed real_ok=%d mono_ok=%d gtod_ok=%d mono_fwd=%d agree=%d\n",
           band(re), band(me), band(gd), me >= 0, agree);
    return 0;
}
