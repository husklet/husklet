// fxsave/fxrstor must round-trip MXCSR (and FCW). The old engine saved/restored only the XMM lanes, so a
// context that saved state, changed the SSE rounding mode, then fxrstor'd kept the WRONG rounding mode.
// This saves MXCSR with RC=round-down, switches to nearest, fxrstor's, and checks (a) the restored MXCSR RC
// via stmxcsr and (b) that a subsequent cvtsd2si rounds down again. Oracle-diffed vs qemu.
#include <stdint.h>
#include <stdio.h>

static uint32_t getmxcsr(void) {
    uint32_t v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    return v;
}
static void setmxcsr(uint32_t v) { __asm__ volatile("ldmxcsr %0" ::"m"(v)); }
static int cvt(volatile double *x) {
    int r;
    __asm__ volatile("cvtsd2si %1,%0" : "=r"(r) : "x"(*x));
    return r;
}

int main(void) {
    _Alignas(16) unsigned char buf[512] = {0};
    volatile double v = 2.7;
    uint32_t base = getmxcsr() & ~0x6000u;

    setmxcsr(base | 0x2000u); // RC=01 round-down (toward -inf)
    __asm__ volatile("fxsave %0" ::"m"(buf) : "memory");
    setmxcsr(base); // RC=00 nearest
    int near = cvt(&v);
    __asm__ volatile("fxrstor %0" ::"m"(buf) : "memory"); // MXCSR RC should be round-down again
    int rc = (getmxcsr() >> 13) & 3;
    int down = cvt(&v);

    printf("near=%d rc_restored=%d down=%d\n", near, rc, down);
    return 0;
}
