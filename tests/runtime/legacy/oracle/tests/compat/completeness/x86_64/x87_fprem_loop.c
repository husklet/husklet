// FPREM / FPREM1 as an ITERATIVE instruction. C2 = 1 means "partial remainder, call me again"; hardware
// genuinely iterates when the operand exponents differ by 64 or more, and glibc's remainderl/fmodl loop on
// it. A model that always reports C2 = 0 hands the loop a partial remainder and calls it the answer.
//
// The other half: FPREM is EXACT by definition -- the remainder is a subset of ST0's bits -- so it raises
// NO exception, not even #P. Deriving the C0/C1/C3 quotient bits through a host division used to raise a
// spurious one.
//
// The per-step partial remainder is NOT asserted: hardware reduces "up to 63 quotient bits" per step by an
// unspecified quantum, so the step COUNT is implementation detail. What is architectural, and what this
// checks, is: C2 clears eventually, C2 was set at least once when the exponents are far apart, the final
// remainder is exact, |Q| mod 8 lands in C0:C3:C1, and nothing was raised.
#include <stdint.h>
#include <stdio.h>

static unsigned fsw(void) {
    unsigned short s;
    __asm__ volatile("fnstsw %0" : "=am"(s));
    return s;
}
static void init(void) { __asm__ volatile("fninit"); }

struct out {
    int iterated; // C2 was set on some step
    int done;     // C2 finally clear
    unsigned q3;  // C0:C3:C1 = |Q| mod 8
    unsigned exc;
    double r;
};

static struct out run(double a, double b, int ieee) {
    struct out o = {0, 0, 0, 0, 0.0};
    unsigned s;
    int steps = 0;
    init();
    __asm__ volatile("fldl %0\n\tfldl %1" : : "m"(b), "m"(a));
    do {
        if (ieee)
            __asm__ volatile("fprem1");
        else
            __asm__ volatile("fprem");
        s = fsw();
        if (s & 0x0400) o.iterated = 1;
    } while ((s & 0x0400) && ++steps < 4096);
    o.done = !(s & 0x0400);
    o.q3 = (unsigned)(((s >> 8) & 1) << 2 | ((s >> 14) & 1) << 1 | ((s >> 9) & 1));
    o.exc = s & 0x3f;
    __asm__ volatile("fstpl %0" : "=m"(o.r));
    return o;
}

static void show(const char *tag, double a, double b) {
    struct out t = run(a, b, 0);
    struct out i = run(a, b, 1);
    printf("%-16s fprem  done=%d iter=%d q3=%u exc=%02x r=%.17g\n", tag, t.done, t.iterated, t.q3, t.exc, t.r);
    printf("%-16s fprem1 done=%d iter=%d q3=%u exc=%02x r=%.17g\n", tag, i.done, i.iterated, i.q3, i.exc, i.r);
}

int main(void) {
    // Close exponents: one step, and the quotient's low three bits are the point.
    show("10/3", 10.0, 3.0);
    show("13/4", 13.0, 4.0);
    show("-13/4", -13.0, 4.0);
    show("13/-4", 13.0, -4.0);
    show("1/3", 1.0, 3.0);
    show("2.5/1", 2.5, 1.0);
    show("100/7", 100.0, 7.0);
    show("255/2", 255.0, 2.0);
    show("7/7", 7.0, 7.0);
    show("0.5/1", 0.5, 1.0);
    show("1.5/1", 1.5, 1.0);
    // Every |Q| mod 8 residue, so C0/C3/C1 are each exercised in both positions.
    for (int q = 0; q < 9; q++) {
        char tag[24];
        snprintf(tag, sizeof tag, "q=%d", q);
        show(tag, (double)q + 0.25, 1.0);
    }
    // Exponent spread >= 64: hardware MUST iterate. The final remainder is still exact.
    show("2^70/1", 1180591620717411303424.0, 1.0);
    show("2^70/3", 1180591620717411303424.0, 3.0);
    show("1e300/3", 1e300, 3.0);
    show("1e300/1e-300", 1e300, 1e-300);
    show("1/1e-300", 1.0, 1e-300);
    show("1e30/1e-30", 1e30, 1e-30);
    show("2^100/7", 1267650600228229401496703205376.0, 7.0);
    // Above 2^53 quotients the low three bits are still exact on hardware.
    show("2^60+1/2", 1152921504606846977.0, 2.0);
    show("1e18/3", 1e18, 3.0);
    // Degenerate operands: #IA is legitimate here, unlike #P.
    show("1/0", 1.0, 0.0);
    show("inf/2", 1.0 / 0.0, 2.0);
    show("2/inf", 2.0, 1.0 / 0.0);
    show("nan/2", 0.0 / 0.0, 2.0);
    show("0/2", 0.0, 2.0);
    show("-0/2", -0.0, 2.0);
    show("denorm/2", 5e-324, 2.0);
    show("2/denorm", 2.0, 5e-324);
    return 0;
}
