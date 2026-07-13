// RCL/RCR through-carry, BY-CL runtime-count form (group2 D2/D3 /2,/3) across all four operand widths,
// plus a memory-operand case. Exercises the do_rcl (R_RCL) C helper: the count MOD (width+1) reduction
// (mod 9/17 for byte/word), CF carry-in/out and the single-bit OF. FNV-mixes (result, CF) for every
// (v, cl, cfin) and OF only when the masked count == 1 (OF is x86-undefined otherwise). Oracle: qemu.
#include <stdio.h>
#include <stdint.h>

static unsigned long long acc = 1469598103934665603ULL;
static void mix(unsigned long long x) { acc ^= x; acc *= 1099511628211ULL; }

// One (v, cl, cfin) probe for a given width; captures result + CF, and OF for the single-bit case.
#define DEF(W, TY, CON)                                                                                    \
    static void rcl_##W(TY v, unsigned char cl, int cfin) {                                                \
        TY r1 = v, r2 = v;                                                                                 \
        unsigned char c1, c2, o1, o2;                                                                      \
        __asm__ volatile("bt $0,%4\n\t rcl %%cl,%0\n\t setc %1\n\t seto %2\n\t"                            \
                         : CON(r1), "=q"(c1), "=q"(o1) : "c"(cl), "r"((int)cfin) : "cc");                  \
        __asm__ volatile("bt $0,%4\n\t rcr %%cl,%0\n\t setc %1\n\t seto %2\n\t"                            \
                         : CON(r2), "=q"(c2), "=q"(o2) : "c"(cl), "r"((int)cfin) : "cc");                  \
        mix((unsigned long long)(TY)r1); mix(c1);                                                          \
        mix((unsigned long long)(TY)r2); mix(c2);                                                          \
        if ((cl & (W == 64 ? 63 : 31)) == 1) { mix(o1); mix(o2); }                                         \
    }
DEF(8, uint8_t, "+q")
DEF(16, uint16_t, "+r")
DEF(32, uint32_t, "+r")
DEF(64, uint64_t, "+r")

int main(void) {
    static const unsigned char cls[] = {0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 24, 31, 32, 33, 48, 63, 64, 65, 255};
    for (unsigned i = 0; i < sizeof cls / sizeof cls[0]; i++) {
        unsigned char cl = cls[i];
        for (int cf = 0; cf < 2; cf++) {
            rcl_8((uint8_t)0x93, cl, cf);
            rcl_16((uint16_t)0x93C7, cl, cf);
            rcl_32((uint32_t)0x93C7A5F1u, cl, cf);
            rcl_64((uint64_t)0x93C7A5F10E2B4D69ULL, cl, cf);
        }
    }
    // memory-operand form: rcl/rcr %cl, m64 and m8
    for (int cf = 0; cf < 2; cf++)
        for (unsigned char cl = 0; cl <= 5; cl++) {
            uint64_t m = 0xF0E1D2C3B4A59687ULL;
            unsigned char c;
            __asm__ volatile("bt $0,%3\n\t rclq %%cl,%0\n\t setc %1\n\t"
                             : "+m"(m), "=q"(c) : "c"(cl), "r"((int)cf) : "cc");
            mix(m); mix(c);
            uint8_t mb = 0x87;
            __asm__ volatile("bt $0,%3\n\t rcrb %%cl,%0\n\t setc %1\n\t"
                             : "+m"(mb), "=q"(c) : "c"(cl), "r"((int)cf) : "cc");
            mix(mb); mix(c);
        }
    printf("rcl acc=%llx\n", acc);
    return 0;
}
