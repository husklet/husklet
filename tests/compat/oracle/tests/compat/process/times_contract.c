// times(2)/sysconf contract is fixed-ABI, not a host measurement: the Linux user-space clock
// tick is always 100 Hz (_SC_CLK_TCK == 100 == CLOCKS_PER_SEC as the kernel exposes it to
// glibc), times() with a valid buffer returns a non-(clock_t)-1 tick count and fills tms fields
// that are non-negative, and times(NULL) also returns successfully. No wall-clock value is
// asserted, only the fixed tick rate and success classes the engine must emulate to Linux.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/times.h>
#include <unistd.h>

int main(void) {
    long clk = sysconf(_SC_CLK_TCK);
    struct tms tb;
    clock_t r = times(&tb);
    int valid = r != (clock_t)-1;
    int fields_ok = tb.tms_utime >= 0 && tb.tms_stime >= 0
                 && tb.tms_cutime >= 0 && tb.tms_cstime >= 0;
    clock_t rn = times(NULL);
    int nullok = rn != (clock_t)-1;

    printf("times-contract clk=%ld valid=%d fields_ok=%d nullok=%d\n",
           clk, valid, fields_ok, nullok);
    return 0;
}
