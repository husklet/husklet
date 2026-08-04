// strftime formatting of a fixed UTC broken-down time. TZ=UTC. Portable verdicts.
#include <stdio.h>
#include <time.h>
#include <string.h>

int main(void) {
    time_t t = 1700000000; // 2023-11-14T22:13:20Z
    struct tm tmv; gmtime_r(&t, &tmv);
    char buf[64];
    strftime(buf, sizeof buf, "%Y-%m-%d %H:%M:%S", &tmv);
    int d1 = strcmp(buf, "2023-11-14 22:13:20") == 0;
    strftime(buf, sizeof buf, "%A %B", &tmv);
    int d2 = strcmp(buf, "Tuesday November") == 0;
    strftime(buf, sizeof buf, "%j %u %p", &tmv);
    int d3 = strcmp(buf, "318 2 PM") == 0; // day-of-year 318, weekday 2 (Tue), PM
    printf("strftime_utc d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
