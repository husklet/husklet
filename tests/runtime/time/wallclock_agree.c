// Cross-check the wall-clock sources against each other: gettimeofday vs clock_gettime(REALTIME),
// time() vs gettimeofday, and CLOCK_REALTIME_COARSE vs CLOCK_REALTIME. All read the same wall
// clock and must agree within a small tolerance.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/time.h>
#include <time.h>

int main(void) {
    struct timeval tv;
    struct timespec ts;
    gettimeofday(&tv, NULL);
    clock_gettime(CLOCK_REALTIME, &ts);
    long long gtod = tv.tv_sec * 1000000000LL + tv.tv_usec * 1000LL;
    long long crt = ts.tv_sec * 1000000000LL + ts.tv_nsec;
    long long d1 = gtod > crt ? gtod - crt : crt - gtod;
    int gtod_agree = d1 < 50LL * 1000 * 1000; // within 50ms

    time_t t = time(NULL);
    struct timeval tv2;
    gettimeofday(&tv2, NULL);
    long long dt = t - tv2.tv_sec;
    if (dt < 0) dt = -dt;
    int time_agree = dt <= 1; // seconds resolution, within 1s

    struct timespec fine, coarse;
    clock_gettime(CLOCK_REALTIME, &fine);
    clock_gettime(CLOCK_REALTIME_COARSE, &coarse);
    long long fn = fine.tv_sec * 1000000000LL + fine.tv_nsec;
    long long cn = coarse.tv_sec * 1000000000LL + coarse.tv_nsec;
    long long dc = fn > cn ? fn - cn : cn - fn;
    int coarse_agree = dc < 100LL * 1000 * 1000; // coarse is within ~1 tick, allow 100ms

    // All wall sources are well past the year-2000 epoch (positive sanity).
    int post_epoch = t > 946684800; // 2000-01-01

    printf("wallagree gtod=%d time=%d coarse=%d post_epoch=%d\n", gtod_agree, time_agree, coarse_agree,
           post_epoch);
    return 0;
}
