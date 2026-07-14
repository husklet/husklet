// #187 — CPUID feature-flag completeness guard (x86-64 only). Executes the real CPUID instruction and
// asserts that (a) every feature hl's translator actually implements is ADVERTISED as present, and (b) the
// families hl CANNOT translate (AVX/AVX2/AVX512/FMA/XSAVE) stay OFF — advertising a VEX/EVEX feature hl
// can't lower would crash any guest that then used it. Verdict-style (`ok=1`), golden on the hl engine:
// qemu-x86_64 advertises its OWN model (a limited qemu64 CPU that lacks sse4.2/aes/bmi/sha), so the exact
// bits can't be oracle-diffed — we self-check the required set instead.
#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static int bit(unsigned v, int b) { return (int)((v >> b) & 1u); }

int main(void) {
    unsigned a, b, c, d;
    int ok = 1;

    // ---- leaf 0: max standard leaf + vendor string ----
    __cpuid(0, a, b, c, d);
    unsigned maxstd = a;
    char vendor[13];
    memcpy(vendor + 0, &b, 4);
    memcpy(vendor + 4, &d, 4);
    memcpy(vendor + 8, &c, 4);
    vendor[12] = 0;
    ok &= (maxstd >= 7);
    ok &= (strcmp(vendor, "GenuineIntel") == 0);

    // ---- leaf 1: baseline EDX + ECX feature bits ----
    __cpuid(1, a, b, c, d);
    // EDX: FPU TSC CX8 CMOV CLFSH MMX FXSR SSE SSE2
    ok &= bit(d, 0) & bit(d, 4) & bit(d, 8) & bit(d, 15) & bit(d, 19) & bit(d, 23) & bit(d, 24) & bit(d, 25) &
          bit(d, 26);
    // ECX: SSE3 PCLMULQDQ SSSE3 CX16 SSE4.1 SSE4.2 POPCNT AES
    ok &= bit(c, 0) & bit(c, 1) & bit(c, 9) & bit(c, 13) & bit(c, 19) & bit(c, 20) & bit(c, 23) & bit(c, 25);
    // NOT advertised (hl cannot translate VEX/EVEX or provide YMM/XSAVE state): FMA(12), XSAVE(26),
    // OSXSAVE(27), AVX(28). Any of these set would be a bug that crashes AVX-using guests.
    ok &= !bit(c, 12) & !bit(c, 26) & !bit(c, 27) & !bit(c, 28);

    // ---- leaf 7 subleaf 0: structured extended features ----
    __cpuid_count(7, 0, a, b, c, d);
    ok &= bit(b, 3) & bit(b, 8) & bit(b, 9) & bit(b, 29); // BMI1 BMI2 ERMS SHA
    ok &= bit(d, 4);                                      // FSRM
    ok &= !bit(b, 5) & !bit(b, 16);                       // AVX2(EBX5) / AVX512F(EBX16) must be OFF

    // ---- extended leaves ----
    __cpuid(0x80000000, a, b, c, d);
    ok &= (a >= 0x80000004); // brand-string leaves reachable
    __cpuid(0x80000001, a, b, c, d);
    ok &= bit(d, 11) & bit(d, 20) & bit(d, 27) & bit(d, 29); // SYSCALL NX RDTSCP LM
    ok &= bit(c, 0);                                          // LAHF/SAHF in long mode

    // ---- brand string (0x80000002..4), 48 bytes ----
    char brand[49];
    unsigned *bp = (unsigned *)brand;
    __cpuid(0x80000002, bp[0], bp[1], bp[2], bp[3]);
    __cpuid(0x80000003, bp[4], bp[5], bp[6], bp[7]);
    __cpuid(0x80000004, bp[8], bp[9], bp[10], bp[11]);
    brand[48] = 0;
    ok &= (strcmp(brand, "hl JIT x86-64 processor") == 0);

    printf("cpuid ok=%d\n", ok ? 1 : 0);
    return 0;
}
