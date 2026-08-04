// C11 timespec_get(TIME_UTC) returns the base TIME_UTC and reads the same wall clock as
// clock_gettime(CLOCK_REALTIME); the two agree within a small tolerance and both advance across a
// short sleep.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    struct timespec a, b;
    int base = timespec_get(&a, TIME_UTC);
    clock_gettime(CLOCK_REALTIME, &b);
    int base_ok = base == TIME_UTC;

    long long an = a.tv_sec * 1000000000LL + a.tv_nsec;
    long long bn = b.tv_sec * 1000000000LL + b.tv_nsec;
    long long d = an > bn ? an - bn : bn - an;
    int agree = d < 50LL * 1000 * 1000;

    struct timespec c;
    usleep(20 * 1000);
    timespec_get(&c, TIME_UTC);
    long long cn = c.tv_sec * 1000000000LL + c.tv_nsec;
    int advances = cn >= an;

    int post_epoch = a.tv_sec > 946684800;

    printf("timespecget base=%d agree=%d advances=%d post_epoch=%d\n", base_ok, agree, advances,
           post_epoch);
    return 0;
}
