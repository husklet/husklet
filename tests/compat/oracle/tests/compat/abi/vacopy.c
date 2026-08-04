/* Variadic argument handling: many mixed int/double args past the register-save area, va_copy for a
   second pass, and nested variadic forwarding. aarch64 and x86_64 have very different varargs ABIs
   (separate GP/FP save areas, overflow area layout), so this differentially exercises the register-
   spill logic. The summed result is arch-neutral. */
#include <stdio.h>
#include <stdarg.h>

/* alternating: count pairs of (int, double) */
static double mix(int count, ...) {
    va_list ap, ap2;
    va_start(ap, count);
    va_copy(ap2, ap);
    long isum = 0;
    double dsum = 0;
    for (int i = 0; i < count; i++) {
        isum += va_arg(ap, int);
        dsum += va_arg(ap, double);
    }
    /* second pass over the copy: only the ints */
    long isum2 = 0;
    for (int i = 0; i < count; i++) {
        isum2 += va_arg(ap2, int);
        (void)va_arg(ap2, double);
    }
    va_end(ap);
    va_end(ap2);
    return (double)isum + dsum + (double)(isum2 == isum ? 1 : 0);
}

static long isum_only(int count, ...) {
    va_list ap; va_start(ap, count);
    long s = 0;
    for (int i = 0; i < count; i++) s += va_arg(ap, long);
    va_end(ap);
    return s;
}

int main(void) {
    double r = mix(10,
        1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5, 5.5,
        6, 6.5, 7, 7.5, 8, 8.5, 9, 9.5, 10, 10.5);
    printf("mix=%.3f\n", r);
    printf("ints=%ld\n", isum_only(12, 1L, 2L, 3L, 4L, 5L, 6L, 7L, 8L, 9L, 10L, 11L, 12L));
    /* many FP args to overflow the FP register save area */
    double r2 = mix(1, 100, 0.25);
    printf("mix1=%.3f\n", r2);
    return 0;
}
