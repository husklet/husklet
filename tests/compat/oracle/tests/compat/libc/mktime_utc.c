// mktime/timegm round-trip and normalisation. TZ=UTC. Portable verdicts.
#include <stdio.h>
#include <time.h>
#include <string.h>

int main(void) {
    struct tm tmv; memset(&tmv, 0, sizeof tmv);
    tmv.tm_year = 2000 - 1900; tmv.tm_mon = 0; tmv.tm_mday = 1;
    tmv.tm_hour = 0; tmv.tm_min = 0; tmv.tm_sec = 0; tmv.tm_isdst = 0;
    time_t t = mktime(&tmv); // with TZ=UTC equals timegm
    int d1 = t == 946684800;
    // Normalisation: month 13 -> next year Jan; day 32 of Jan -> Feb 1.
    struct tm n; memset(&n, 0, sizeof n);
    n.tm_year = 2021 - 1900; n.tm_mon = 0; n.tm_mday = 32; n.tm_isdst = 0;
    mktime(&n);
    int d2 = n.tm_mon == 1 && n.tm_mday == 1;
    struct tm g; memset(&g, 0, sizeof g);
    g.tm_year = 2010 - 1900; g.tm_mon = 5; g.tm_mday = 20;
    time_t tg = timegm(&g);
    int d3 = tg == 1276992000;
    printf("mktime_utc d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
