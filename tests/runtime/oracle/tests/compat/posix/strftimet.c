// strftime/strptime round-trip in UTC across conversion specifiers; locale-independent.
#define _GNU_SOURCE
#include <time.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    setenv("TZ", "UTC", 1);
    tzset();

    // A fixed epoch: 2021-03-04 05:06:07 UTC (Thursday, day-of-year 063).
    time_t t = 1614834367;
    struct tm tmv;
    gmtime_r(&t, &tmv);

    char buf[128];
    strftime(buf, sizeof buf, "%Y-%m-%dT%H:%M:%S", &tmv);
    int iso = strcmp(buf, "2021-03-04T05:06:07") == 0;

    char wd[16];
    strftime(wd, sizeof wd, "%a %A", &tmv);
    int weekday = strcmp(wd, "Thu Thursday") == 0;

    char misc[64];
    strftime(misc, sizeof misc, "%j %p %I %m %b", &tmv);
    int derived = strcmp(misc, "063 AM 05 03 Mar") == 0;

    // strptime parses back to the same broken-down time.
    struct tm parsed;
    memset(&parsed, 0, sizeof parsed);
    char *end = strptime("2021-03-04T05:06:07", "%Y-%m-%dT%H:%M:%S", &parsed);
    int roundtrip = end && *end == 0 &&
                    parsed.tm_year == 121 && parsed.tm_mon == 2 && parsed.tm_mday == 4 &&
                    parsed.tm_hour == 5 && parsed.tm_min == 6 && parsed.tm_sec == 7;
    time_t back = timegm(&parsed);
    int epoch_ok = back == t;

    printf("strftimet iso=%d weekday=%d derived=%d roundtrip=%d epoch=%d\n",
           iso, weekday, derived, roundtrip, epoch_ok);
    return 0;
}
