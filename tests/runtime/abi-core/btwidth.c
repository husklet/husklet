// BT/BTS/BTR/BTC register-destination width preservation (x86-only). A 16-bit destination must leave the
// upper 48 bits (63:16) of the underlying 64-bit register untouched; a 32-bit destination zero-extends
// (clears 63:32); a 64-bit destination writes all 64. A prior lowering bug wrote the 16-bit result with a
// 32-bit host op straight into the guest home register, zeroing bits 63:32 that x86 preserves. Also checks
// CF (the tested bit) via SETC, plus the register-offset addressing (offset masked mod operand size).
#include <stdint.h>
#include <stdio.h>

#define GEN(name, insn, sfx)                                                                                \
    static uint64_t name(uint64_t r) {                                                                      \
        __asm__ volatile(insn " $3, %" sfx "0" : "+r"(r)::"cc");                                             \
        return r;                                                                                            \
    }
GEN(bts16, "btsw", "w")
GEN(btr16, "btrw", "w")
GEN(btc16, "btcw", "w")
GEN(bts32, "btsl", "k")
GEN(btr32, "btrl", "k")
GEN(bts64, "btsq", "q")
GEN(btr64, "btrq", "q")

// register bit-offset (offset taken modulo operand size); returns result and CF.
static uint64_t bts16_reg(uint64_t r, uint64_t off, int *cf) {
    unsigned char c;
    __asm__ volatile("btsw %w2, %w0\n\tsetc %1" : "+r"(r), "=q"(c) : "r"(off) : "cc");
    *cf = c;
    return r;
}

int main(void) {
    int cf;
    printf("bts16 %016lx\n", bts16(0xDEADBEEF12340000UL));
    printf("btr16 %016lx\n", btr16(0xDEADBEEF1234FFFFUL));
    printf("btc16 %016lx\n", btc16(0xDEADBEEF12340000UL));
    printf("bts32 %016lx\n", bts32(0xDEADBEEF12340000UL)); // 32-bit: upper cleared
    printf("btr32 %016lx\n", btr32(0xDEADBEEF1234FFFFUL));
    printf("bts64 %016lx\n", bts64(0xDEAD000000000000UL));
    printf("btr64 %016lx\n", btr64(0xDEADFFFFFFFFFFFFUL));
    // register offset 19 masked mod 16 -> bit 3; CF = old bit 3 (0 here)
    uint64_t v = bts16_reg(0xCAFEF00D00000000UL, 19, &cf);
    printf("btsr  %016lx cf=%d\n", v, cf);
    // offset 35 masked mod 16 -> bit 3 of 0xFFFF -> CF=1
    v = bts16_reg(0xCAFEF00D0000FFFFUL, 35, &cf);
    printf("btsr2 %016lx cf=%d\n", v, cf);
    return 0;
}
