// time() contract: time(NULL) equals time(&t) and both fill the same value; the returned wall-clock
// second matches clock_gettime(CLOCK_REALTIME).tv_sec; and time() advances by ~1 across a 1.1s
// sleep (relationship, not an absolute value).
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    time_t t = 0;
    time_t r = time(&t);
    int consistent = r == t;

    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    long d = (long)ts.tv_sec - (long)t;
    if (d < 0) d = -d;
    int matches_realtime = d <= 1;

    time_t a = time(NULL);
    usleep(1100 * 1000); // 1.1s
    time_t b = time(NULL);
    long adv = (long)(b - a);
    int advances = adv >= 1 && adv <= 3;

    int post_epoch = t > 946684800;

    printf("timefunc consistent=%d matches=%d advances=%d post_epoch=%d\n", consistent,
           matches_realtime, advances, post_epoch);
    return 0;
}
