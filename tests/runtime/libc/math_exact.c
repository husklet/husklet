// remquo / lgamma_r / nextafter / scalbn exact IEEE results. Portable verdicts.
#include <stdio.h>
#include <math.h>

int main(void) {
    int q;
    double r = remquo(10.0, 3.0, &q);
    int d1 = r == 1.0 && (q & 7) == 3;
    int sign;
    double lg = lgamma_r(5.0, &sign); // ln(4!) = ln24
    int d2 = sign == 1 && fabs(lg - log(24.0)) < 1e-12;
    double na = nextafter(0.0, 1.0);
    int d3 = na > 0.0 && na < 1e-300; // smallest subnormal
    int d4 = scalbn(1.0, 10) == 1024.0;
    double ip;
    double fr = modf(3.75, &ip);
    int d5 = ip == 3.0 && fr == 0.75;
    int d6 = remainder(5.0, 2.0) == 1.0 && remainder(7.0, 2.0) == -1.0;
    printf("math_exact d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d\n", d1, d2, d3, d4, d5, d6);
    return 0;
}
