// Calendar conversions under a fixed UTC timezone: gmtime/timegm and localtime/mktime must round-
// trip a known epoch, gmtime decodes a fixed timestamp to the exact broken-down fields, difftime
// computes the correct delta, and mktime normalizes out-of-range fields.
#define _GNU_SOURCE
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

int main(void) {
    setenv("TZ", "UTC", 1);
    tzset();

    // 2021-03-04 05:06:07 UTC == 1614834367.
    time_t fixed = 1614834367;
    struct tm g;
    gmtime_r(&fixed, &g);
    int fields_ok = g.tm_year == 121 && g.tm_mon == 2 && g.tm_mday == 4 && g.tm_hour == 5 &&
                    g.tm_min == 6 && g.tm_sec == 7 && g.tm_wday == 4; // Thursday

    // timegm round-trips back to the same epoch.
    time_t back = timegm(&g);
    int timegm_ok = back == fixed;

    // Under TZ=UTC, localtime == gmtime and mktime == timegm.
    struct tm l;
    localtime_r(&fixed, &l);
    time_t lback = mktime(&l);
    int local_ok = lback == fixed && l.tm_hour == 5;

    // difftime of two epochs one hour apart.
    int diff_ok = difftime(fixed + 3600, fixed) == 3600.0;

    // mktime normalizes: day 32 of January -> Feb 1.
    struct tm over;
    memset(&over, 0, sizeof over);
    over.tm_year = 121; // 2021
    over.tm_mon = 0;    // January
    over.tm_mday = 32;  // overflow -> Feb 1
    over.tm_hour = 0;
    mktime(&over);
    int norm_ok = over.tm_mon == 1 && over.tm_mday == 1;

    printf("calendar fields=%d timegm=%d local=%d diff=%d norm=%d\n", fields_ok, timegm_ok, local_ok,
           diff_ok, norm_ok);
    return 0;
}
