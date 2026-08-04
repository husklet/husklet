/* IEEE-754 rounding-mode control via fesetround. double arithmetic is IEEE binary64 on both targets,
   so directed-rounding results are arch-neutral. Exercises nearest/down/up/toward-zero on divisions
   and sqrt whose exact results are not representable. */
#include <stdio.h>
#include <fenv.h>
#include <math.h>

#pragma STDC FENV_ACCESS ON

static void round_report(const char *name, int mode) {
    fesetround(mode);
    volatile double a = 1.0, b = 3.0, c = 10.0, d = 7.0;
    double q1 = a / b;
    double q2 = c / d;
    double s = sqrt((double)2.0);
    double f = fma(0.1, 0.2, 0.3);
    /* print the raw bit patterns to catch a single-ULP rounding divergence */
    unsigned long long u1, u2, us, uf;
    __builtin_memcpy(&u1, &q1, 8);
    __builtin_memcpy(&u2, &q2, 8);
    __builtin_memcpy(&us, &s, 8);
    __builtin_memcpy(&uf, &f, 8);
    printf("%s 1/3=%016llx 10/7=%016llx sqrt2=%016llx fma=%016llx\n",
           name, u1, u2, us, uf);
}

int main(void) {
    round_report("nearest", FE_TONEAREST);
    round_report("down", FE_DOWNWARD);
    round_report("up", FE_UPWARD);
    round_report("zero", FE_TOWARDZERO);
    fesetround(FE_TONEAREST);
    /* exception flags */
    feclearexcept(FE_ALL_EXCEPT);
    volatile double z = 0.0, one = 1.0;
    volatile double inf = one / z;
    (void)inf;
    int raised = fetestexcept(FE_DIVBYZERO) ? 1 : 0;
    printf("divbyzero_flag=%d\n", raised);
    return 0;
}
