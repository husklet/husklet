// sysconf query surface: deterministic sign/relationship verdicts (no raw arch-specific numbers).
#include <stdio.h>
#include <unistd.h>
#include <limits.h>

int main(void) {
    long pg = sysconf(_SC_PAGESIZE);
    int d1 = pg > 0 && (pg & (pg - 1)) == 0; // power of two
    long clk = sysconf(_SC_CLK_TCK);
    int d2 = clk > 0;
    long amax = sysconf(_SC_ARG_MAX);
    int d3 = amax > 4096;
    long omax = sysconf(_SC_OPEN_MAX);
    int d4 = omax > 0;
    long ver = sysconf(_SC_VERSION);
    int d5 = ver >= 200809L; // POSIX.1-2008 or later
    printf("sysconf d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
