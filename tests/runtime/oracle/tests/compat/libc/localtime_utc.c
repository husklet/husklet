// With TZ=UTC, localtime_r must equal gmtime_r; gmtime day-of-week correct. TZ=UTC.
#include <stdio.h>
#include <time.h>

int main(void) {
    time_t t = 1609459200; // 2021-01-01T00:00:00Z, a Friday (wday=5)
    struct tm g, l;
    gmtime_r(&t, &g);
    localtime_r(&t, &l);
    int d1 = g.tm_year == l.tm_year && g.tm_mon == l.tm_mon && g.tm_mday == l.tm_mday
          && g.tm_hour == l.tm_hour && g.tm_min == l.tm_min && g.tm_sec == l.tm_sec;
    int d2 = g.tm_wday == 5 && g.tm_yday == 0;
    int d3 = l.tm_gmtoff == 0;
    printf("localtime_utc d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
