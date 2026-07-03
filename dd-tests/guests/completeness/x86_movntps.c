// Non-temporal packed stores: MOVNTPS (0F 2B, #190), MOVNTPD (66 0F 2B), MOVNTDQ (66 0F E7). Each is a
// plain aligned 128-bit store to memory on ARM. Uses explicit inline asm so the exact 0F 2B encoding is
// exercised (not a compiler-chosen substitute). Oracle: qemu-x86_64.
#include <stdio.h>
#include <stdint.h>

int main(void) {
    float in_ps[4] __attribute__((aligned(16))) = {1.5f, 2.25f, 3.75f, 4.5f};
    float out_ps[4] __attribute__((aligned(16))) = {0};
    double in_pd[2] __attribute__((aligned(16))) = {6.25, 7.5};
    double out_pd[2] __attribute__((aligned(16))) = {0};
    int32_t in_dq[4] __attribute__((aligned(16))) = {0x11111111, 0x22222222, 0x33333333, 0x44444444};
    int32_t out_dq[4] __attribute__((aligned(16))) = {0};

    __asm__ volatile("movaps %[i], %%xmm3\n\t movntps %%xmm3, %[o]\n\t sfence\n\t"
                     : [o] "=m"(out_ps) : [i] "m"(in_ps) : "xmm3", "memory");
    __asm__ volatile("movapd %[i], %%xmm4\n\t movntpd %%xmm4, %[o]\n\t sfence\n\t"
                     : [o] "=m"(out_pd) : [i] "m"(in_pd) : "xmm4", "memory");
    __asm__ volatile("movdqa %[i], %%xmm5\n\t movntdq %%xmm5, %[o]\n\t sfence\n\t"
                     : [o] "=m"(out_dq) : [i] "m"(in_dq) : "xmm5", "memory");

    // Compare via raw bit patterns (no float rounding), so the checksum is exact and deterministic.
    uint32_t *ps = (uint32_t *)out_ps;
    uint64_t *pd = (uint64_t *)out_pd;
    unsigned long long s = 0;
    for (int i = 0; i < 4; i++) s = s * 131 + ps[i];
    for (int i = 0; i < 2; i++) s = s * 131 + pd[i];
    for (int i = 0; i < 4; i++) s = s * 131 + (uint32_t)out_dq[i];
    printf("movntps s=%llx\n", s);
    return 0;
}
