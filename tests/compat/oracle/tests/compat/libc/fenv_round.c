// fesetround rounding-mode control affects nearbyint/rint. Portable verdicts.
#include <stdio.h>
#include <fenv.h>
#include <math.h>
#pragma STDC FENV_ACCESS ON

int main(void) {
    fesetround(FE_TONEAREST);
    int d1 = nearbyint(2.5) == 2.0 && nearbyint(3.5) == 4.0; // ties to even
    fesetround(FE_UPWARD);
    int d2 = nearbyint(2.1) == 3.0 && nearbyint(-2.1) == -2.0;
    fesetround(FE_DOWNWARD);
    int d3 = nearbyint(2.9) == 2.0 && nearbyint(-2.1) == -3.0;
    fesetround(FE_TOWARDZERO);
    int d4 = nearbyint(2.9) == 2.0 && nearbyint(-2.9) == -2.0;
    fesetround(FE_TONEAREST);
    int d5 = fegetround() == FE_TONEAREST;
    printf("fenv_round d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
