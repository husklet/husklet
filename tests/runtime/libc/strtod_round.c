// strtod round-trip + ERANGE on overflow/underflow + hex float parse. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <math.h>

int main(void) {
    char *end;
    double v = strtod("3.14159265358979", &end);
    int d1 = v > 3.1415926 && v < 3.1415927 && *end == '\0';
    errno = 0;
    double big = strtod("1e400", &end);
    int d2 = errno == ERANGE && big == HUGE_VAL;
    errno = 0;
    double tiny = strtod("1e-400", &end);
    int d3 = errno == ERANGE && tiny >= 0.0 && tiny < 1e-300;
    double hex = strtod("0x1.8p3", &end); // 1.5 * 8 = 12
    int d4 = hex == 12.0;
    double inf = strtod("inf", &end);
    int d5 = isinf(inf);
    printf("strtod_round d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
