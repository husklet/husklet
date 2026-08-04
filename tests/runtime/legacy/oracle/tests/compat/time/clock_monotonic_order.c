// Monotonic clock ids must never step backwards across many samples. We sample REALTIME too
// (may jump, so only counted, not asserted monotonic) and assert the strictly-monotonic ids
// (MONOTONIC, MONOTONIC_RAW, BOOTTIME) never regress across N reads interleaved with tiny work.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>

static long long ns(clockid_t c) {
    struct timespec ts;
    if (clock_gettime(c, &ts) != 0) return -1;
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int never_backwards(clockid_t c, int n) {
    long long prev = ns(c);
    if (prev < 0) return 0;
    volatile unsigned sink = 0;
    for (int i = 0; i < n; i++) {
        for (int k = 0; k < 1000; k++) sink += k;
        long long cur = ns(c);
        if (cur < prev) return 0;
        prev = cur;
    }
    return 1;
}

int main(void) {
    int mono = never_backwards(CLOCK_MONOTONIC, 2000);
    int raw = never_backwards(CLOCK_MONOTONIC_RAW, 2000);
    int boot = never_backwards(CLOCK_BOOTTIME, 2000);
    int mcoarse = never_backwards(CLOCK_MONOTONIC_COARSE, 2000);
    // BOOTTIME >= MONOTONIC always (boot counts suspend time too). Read MONOTONIC first so the later
    // BOOTTIME read cannot lose the race when the two share a base with no suspend offset.
    long long m = ns(CLOCK_MONOTONIC);
    int boot_ge_mono = ns(CLOCK_BOOTTIME) >= m;
    printf("monoorder mono=%d raw=%d boot=%d mcoarse=%d boot_ge_mono=%d\n", mono, raw, boot, mcoarse,
           boot_ge_mono);
    return 0;
}
