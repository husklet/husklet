// times() populates tms fields and its return (clock ticks since an arbitrary epoch) advances under
// CPU work; clock() (process CPU time in CLOCKS_PER_SEC units) advances under CPU work. tms_utime
// accumulates for a busy loop.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/times.h>
#include <time.h>
#include <unistd.h>

static void burn(long iters) {
    volatile double x = 1.0;
    for (long i = 1; i <= iters; i++) x = x * 1.0000001 + 1.0;
    (void)x;
}

int main(void) {
    struct tms a, b;
    clock_t r0 = times(&a);
    clock_t c0 = clock();
    burn(80L * 1000 * 1000);
    clock_t r1 = times(&b);
    clock_t c1 = clock();

    int ret_valid = r0 != (clock_t)-1 && r1 != (clock_t)-1;
    int ret_adv = r1 >= r0;                // real-time ticks never go backwards
    int utime_adv = b.tms_utime >= a.tms_utime && (b.tms_utime > a.tms_utime); // user CPU grew
    int clock_adv = c1 > c0;
    int clock_pos = c1 > 0;

    printf("timesclock ret_valid=%d ret_adv=%d utime_adv=%d clock_adv=%d clock_pos=%d\n", ret_valid,
           ret_adv, utime_adv, clock_adv, clock_pos);
    return 0;
}
