// strptime parses a date string into broken-down time. TZ=UTC. Portable verdicts.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <string.h>

int main(void) {
    struct tm tmv; memset(&tmv, 0, sizeof tmv);
    char *end = strptime("2021-06-15 08:30:45", "%Y-%m-%d %H:%M:%S", &tmv);
    int d1 = end != NULL && *end == '\0';
    int d2 = tmv.tm_year == 121 && tmv.tm_mon == 5 && tmv.tm_mday == 15;
    int d3 = tmv.tm_hour == 8 && tmv.tm_min == 30 && tmv.tm_sec == 45;
    time_t t = timegm(&tmv);
    int d4 = t == 1623745845;
    printf("strptime d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
