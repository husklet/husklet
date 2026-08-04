/* C99 _Complex double arithmetic: add/mul/div/conj/cabs/carg. IEEE binary64 real+imag on both
   targets; printed as raw bit patterns of the real/imag components to catch any single-ULP codegen
   divergence in the complex multiply/divide lowering. Arch-neutral. */
#include <stdio.h>
#include <complex.h>
#include <string.h>

static void emit(const char *tag, double complex z) {
    double re = creal(z), im = cimag(z);
    unsigned long long ur, ui;
    memcpy(&ur, &re, 8);
    memcpy(&ui, &im, 8);
    printf("%s re=%016llx im=%016llx\n", tag, ur, ui);
}

int main(void) {
    double complex a = 3.0 + 4.0 * I;
    double complex b = 1.0 - 2.0 * I;
    emit("add", a + b);
    emit("mul", a * b);
    emit("div", a / b);
    emit("conj", conj(a));
    double m = cabs(a);
    double g = carg(b);
    unsigned long long um, ug;
    memcpy(&um, &m, 8);
    memcpy(&ug, &g, 8);
    printf("cabs=%016llx carg=%016llx\n", um, ug);
    double complex acc = 0;
    for (int k = 1; k <= 8; k++) acc += (double)k / (a + (double)k);
    emit("acc", acc);
    return 0;
}
