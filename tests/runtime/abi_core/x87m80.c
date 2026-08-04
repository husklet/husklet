// x87 80-bit (m80) FLD/FSTP round-trip differential (H11 area). The ST stack is emulated at double
// precision, so long-double ARITHMETIC drifts in the low mantissa bits and is NOT tested here. What IS
// tested (and is now byte-exact vs qemu): the m80 <-> double CONVERTERS on the value classes that must be
// bit-exact regardless of the carrier width -- +/-0, +/-Inf, NaN, and values exactly representable in
// double. The Inf/NaN paths in particular were miscompiled (FSTP m80 of a double Inf/NaN wrote a rebiased
// FINITE ext80 exponent 0x43FF instead of Inf's 0x7FFF; FLD m80 of an ext80 NaN silently became Inf).
// Values flow through a volatile double so the store really executes `fstpt` (x87_fstp_m80), not a
// compile-time constant emit.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static volatile double g_d;
static double VD(double x) { g_d = x; return g_d; }
static double bits_d(uint64_t u) { volatile uint64_t v = u; uint64_t w = v; double f; memcpy(&f, &w, 8); return f; }

// Store a runtime double (widened to long double) to memory and print its 10-byte ext80 image.
static void st80(const char *tag, double x) {
    long double y = (long double)VD(x); // FLD m64 -> ST, then the memcpy forces FSTP m80
    unsigned char b[16];
    memcpy(b, &y, sizeof y);
    printf("%s", tag);
    for (int i = 0; i < 10; i++) printf("%02x", b[i]);
    printf("\n");
}

// Load a 10-byte ext80 image (FLD m80), narrow to double, print the double bit pattern.
static void ld80(const char *tag, uint64_t sig, uint16_t se) {
    unsigned char b[16] = {0};
    memcpy(b, &sig, 8);
    memcpy(b + 8, &se, 2);
    long double y;
    memcpy(&y, b, sizeof y);
    volatile double d = (double)y; // FLD m80 -> ST, narrow (FSTP m64) to double
    uint64_t u;
    double dd = d;
    memcpy(&u, &dd, 8);
    printf("%s%016llx\n", tag, (unsigned long long)u);
}

int main(void) {
    // ---- FSTP m80: double -> ext80 (10-byte image) ----
    st80("st +0    ", VD(0.0));
    st80("st -0    ", VD(-0.0));
    st80("st 1.0   ", VD(1.0));
    st80("st 2.0   ", VD(2.0));
    st80("st 0.5   ", VD(0.5));
    st80("st -3.0  ", VD(-3.0));
    st80("st 1024  ", VD(1024.0));
    st80("st +Inf  ", bits_d(0x7ff0000000000000ull)); // was 0x43FF finite -> now 0x7FFF Inf
    st80("st -Inf  ", bits_d(0xfff0000000000000ull));
    st80("st NaN   ", bits_d(0x7ff8000000000000ull)); // quiet NaN
    // ---- FLD m80: ext80 (10-byte image) -> double ----
    ld80("ld +0    ", 0x0000000000000000ull, 0x0000);
    ld80("ld +Inf  ", 0x8000000000000000ull, 0x7fff);
    ld80("ld -Inf  ", 0x8000000000000000ull, 0xffff);
    ld80("ld NaN   ", 0xc000000000000000ull, 0x7fff); // was flattened to Inf -> now a double NaN
    ld80("ld 1.5   ", 0xc000000000000000ull, 0x3fff); // exact in double
    ld80("ld 2.0   ", 0x8000000000000000ull, 0x4000);
    return 0;
}
