// VEX vmovss/vmovsd register-source scalar merge: dst[es-1:0]=src2(r/m),
// dst[127:es]=src1(vvvv), dst[255:128]=0. The upper low-lane bits must merge
// from vvvv, not be zeroed. Oracle-diffed vs qemu.
#include <stdint.h>
#include <stdio.h>

__attribute__((target("avx"))) static void go(void) {
    // src1 (vvvv) dwords low->high; src2 (r/m) supplies only the low element.
    uint32_t s1[4] = {0x11111111u, 0x22222222u, 0x33333333u, 0x44444444u};
    uint32_t s2[4] = {0x41100000u, 0x55555555u, 0x66666666u, 0x77777777u};
    uint32_t out[4] = {0, 0, 0, 0};
    __asm__ volatile("vmovdqu %1, %%xmm1\n\t"
                     "vmovdqu %2, %%xmm2\n\t"
                     "vmovss %%xmm2, %%xmm1, %%xmm0\n\t" // xmm0 = merge(src2 low, src1 upper)
                     "vmovdqu %%xmm0, %0\n\t"
                     : "=m"(out)
                     : "m"(s1[0]), "m"(s2[0])
                     : "xmm0", "xmm1", "xmm2");
    printf("vmovss %08x %08x %08x %08x\n", out[0], out[1], out[2], out[3]);

    uint64_t d1[2] = {0x1111111122222222ull, 0x3333333344444444ull};
    uint64_t d2[2] = {0x4110000000000000ull, 0x5555555566666666ull};
    uint64_t dout[2] = {0, 0};
    __asm__ volatile("vmovdqu %1, %%xmm1\n\t"
                     "vmovdqu %2, %%xmm2\n\t"
                     "vmovsd %%xmm2, %%xmm1, %%xmm0\n\t"
                     "vmovdqu %%xmm0, %0\n\t"
                     : "=m"(dout)
                     : "m"(d1[0]), "m"(d2[0])
                     : "xmm0", "xmm1", "xmm2");
    printf("vmovsd %016llx %016llx\n", (unsigned long long)dout[0], (unsigned long long)dout[1]);
}

int main(void) {
    go();
    return 0;
}
