// clock_getres for every supported clock id must report a sane resolution: strictly > 0 and
// <= 1 second. COARSE clocks have a coarser (larger) resolution than their fine counterparts,
// but still <= 1s. A bad clock id -> EINVAL from clock_getres.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <time.h>

static int res_sane(clockid_t c) {
    struct timespec r;
    if (clock_getres(c, &r) != 0) return 0;
    long long n = r.tv_sec * 1000000000LL + r.tv_nsec;
    return n > 0 && n <= 1000000000LL;
}

int main(void) {
    int real = res_sane(CLOCK_REALTIME);
    int mono = res_sane(CLOCK_MONOTONIC);
    int raw = res_sane(CLOCK_MONOTONIC_RAW);
    int boot = res_sane(CLOCK_BOOTTIME);
    int proc = res_sane(CLOCK_PROCESS_CPUTIME_ID);
    int thr = res_sane(CLOCK_THREAD_CPUTIME_ID);
    int rc = res_sane(CLOCK_REALTIME_COARSE);
    int mc = res_sane(CLOCK_MONOTONIC_COARSE);

    // COARSE resolution should be >= fine resolution (i.e. numerically larger or equal).
    struct timespec fine, coarse;
    clock_getres(CLOCK_MONOTONIC, &fine);
    clock_getres(CLOCK_MONOTONIC_COARSE, &coarse);
    long long fn = fine.tv_sec * 1000000000LL + fine.tv_nsec;
    long long cn = coarse.tv_sec * 1000000000LL + coarse.tv_nsec;
    int coarser = cn >= fn;

    errno = 0;
    struct timespec junk;
    int bad = clock_getres((clockid_t)0x7fff, &junk) == -1 && errno == EINVAL;

    printf("getres real=%d mono=%d raw=%d boot=%d proc=%d thr=%d rc=%d mc=%d coarser=%d bad=%d\n", real,
           mono, raw, boot, proc, thr, rc, mc, coarser, bad);
    return 0;
}
