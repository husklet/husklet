// x87 FSW (FPU status word) sticky exception flags. An x87 arithmetic op that raises a floating-point
// exception must SET the corresponding sticky flag in the status word (IE0/DE1/ZE2/OE3/UE4/PE5), readable
// via FNSTSW; the flag accumulates until FNCLEX/FNINIT clears it, and is set INDEPENDENTLY of the FCW
// exception mask (a masked exception still sets its flag, it just does not trap). ES(7) is the error
// summary: set iff any raised exception is UNMASKED per FCW[0..5]. A translator that synthesizes FSW
// without projecting the host FP status (the pre-fix engine) reports the flags CLEAR after e.g. 1.0/0.0
// or 0.0/0.0 -- a silent divergence from real x87 hardware and qemu.
//
// x86 drives real x87; aarch64 (no x87) drives the same operations on host binary64 FP and projects its
// FPSR into the identical FSW bit layout, so the golden is byte-identical across ISAs. Only the
// exponent-range-INDEPENDENT flags are asserted (IE invalid, ZE divide-by-zero, PE inexact, plus ES and
// FNCLEX clearing): OE/UE depend on the exponent range and the engine carries ST(i) as binary64, not
// 80-bit extended (known limitation H11), so overflow/underflow cannot match qemu's 80-bit x87 and are
// deliberately not asserted here.
#include <stdint.h>
#include <stdio.h>
#include <math.h>

// FSW/MXCSR exception bit positions (shared layout).
#define FSW_IE 0x01 // invalid
#define FSW_ZE 0x04 // divide-by-zero
#define FSW_PE 0x20 // precision/inexact
#define FSW_ES 0x80 // error summary
#define FCW_DEFAULT 0x037f    // all exceptions masked, round-nearest, 64-bit precision
#define FCW_ZE_UNMASK 0x037b  // clear the ZE(bit2) mask -> divide-by-zero is unmasked

#if defined(__x86_64__)
// Run an x87 divide under control word `cw` and return the resulting FSW. FNSTSW/FNINIT are the no-wait
// variants, so a pending UNMASKED exception is read out (ES visible) and then cleared BEFORE any waiting
// x87 instruction executes -- no #MF/SIGFPE is ever delivered.
static uint16_t x87_div(double a, double b, uint16_t cw) {
    uint16_t fsw;
    __asm__ volatile("fninit\n\t fldcw %1\n\t fldl %2\n\t fdivl %3\n\t fnstsw %%ax\n\t fninit\n\t"
                     : "=a"(fsw)
                     : "m"(cw), "m"(a), "m"(b)
                     : "st");
    return fsw;
}
static uint16_t x87_sqrt(double a) {
    uint16_t fsw, cw = FCW_DEFAULT;
    __asm__ volatile("fninit\n\t fldcw %1\n\t fldl %2\n\t fsqrt\n\t fnstsw %%ax\n\t fninit\n\t"
                     : "=a"(fsw)
                     : "m"(cw), "m"(a)
                     : "st");
    return fsw;
}
// Divide-by-zero, then FNCLEX, then read FSW: the sticky flags must be cleared.
static uint16_t x87_div_then_clex(double a, double b) {
    uint16_t fsw, cw = FCW_DEFAULT;
    __asm__ volatile("fninit\n\t fldcw %1\n\t fldl %2\n\t fdivl %3\n\t fnclex\n\t fnstsw %%ax\n\t fninit\n\t"
                     : "=a"(fsw)
                     : "m"(cw), "m"(a), "m"(b)
                     : "st");
    return fsw;
}
#else
// aarch64 reference: run the op on binary64, project the FPSR cumulative flags into the FSW layout, and
// compute ES from the same FCW mask an x87 would apply.
static uint32_t fpsr_clear_read(void) {
    uint64_t v;
    __asm__ volatile("mrs %0, fpsr" : "=r"(v));
    return (uint32_t)v;
}
static void fpsr_clear(void) {
    uint64_t v;
    __asm__ volatile("mrs %0, fpsr" : "=r"(v));
    v &= ~(uint64_t)0x9f; // clear IOC/DZC/OFC/UFC/IXC/IDC
    __asm__ volatile("msr fpsr, %0" ::"r"(v));
}
static uint16_t fpsr_to_fsw(uint32_t fpsr, uint16_t cw) {
    uint16_t fsw = 0;
    if (fpsr & (1u << 0)) fsw |= FSW_IE;   // IOC -> IE
    if (fpsr & (1u << 1)) fsw |= FSW_ZE;   // DZC -> ZE
    if (fpsr & (1u << 2)) fsw |= 0x08;     // OFC -> OE
    if (fpsr & (1u << 3)) fsw |= 0x10;     // UFC -> UE
    if (fpsr & (1u << 4)) fsw |= FSW_PE;   // IXC -> PE
    if (fpsr & (1u << 7)) fsw |= 0x02;     // IDC -> DE
    if (fsw & (uint16_t)(~cw & 0x3f)) fsw |= FSW_ES;
    return fsw;
}
static uint16_t x87_div(double a, double b, uint16_t cw) {
    fpsr_clear();
    volatile double r = a / b;
    (void)r;
    return fpsr_to_fsw(fpsr_clear_read(), cw);
}
static uint16_t x87_sqrt(double a) {
    fpsr_clear();
    volatile double r = sqrt(a);
    (void)r;
    return fpsr_to_fsw(fpsr_clear_read(), FCW_DEFAULT);
}
static uint16_t x87_div_then_clex(double a, double b) {
    fpsr_clear();
    volatile double r = a / b;
    (void)r;
    fpsr_clear(); // model FNCLEX
    return fpsr_to_fsw(fpsr_clear_read(), FCW_DEFAULT);
}
#endif

int main(void) {
    puts("x87 fsw exception flags:");
    printf("  1.0/0.0   ZE=%d\n", (x87_div(1.0, 0.0, FCW_DEFAULT) & FSW_ZE) != 0);
    printf("  0.0/0.0   IE=%d\n", (x87_div(0.0, 0.0, FCW_DEFAULT) & FSW_IE) != 0);
    printf("  sqrt(-1)  IE=%d\n", (x87_sqrt(-1.0) & FSW_IE) != 0);
    printf("  1.0/3.0   PE=%d\n", (x87_div(1.0, 3.0, FCW_DEFAULT) & FSW_PE) != 0);
    printf("  1.0/2.0   PE=%d\n", (x87_div(1.0, 2.0, FCW_DEFAULT) & FSW_PE) != 0);

    puts("fnclex clears:");
    printf("  after 1/0 then fnclex, exc=%02x\n", x87_div_then_clex(1.0, 0.0) & 0xbf);

    puts("error-summary (masked vs unmasked ZE):");
    {
        uint16_t masked = x87_div(1.0, 0.0, FCW_DEFAULT);
        uint16_t unmasked = x87_div(1.0, 0.0, FCW_ZE_UNMASK);
        printf("  masked   ZE=%d ES=%d\n", (masked & FSW_ZE) != 0, (masked & FSW_ES) != 0);
        printf("  unmasked ZE=%d ES=%d\n", (unmasked & FSW_ZE) != 0, (unmasked & FSW_ES) != 0);
    }
    return 0;
}
