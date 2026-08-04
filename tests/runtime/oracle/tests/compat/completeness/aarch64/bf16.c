// FEAT_BF16. The CPU model does NOT set HWCAP2_BF16, so the BFDOT half is behind the auxv check a conforming
// guest makes -- flipping the bit without finishing BFDOT would make this case start exercising it and fail
// loudly, which is the point. BFCVT is exercised unconditionally: both backends implement it, and it is where
// the subtlety lives. bf16 IS the top half of the binary32 encoding, so the whole instruction is a rounding of
// the discarded low 16 bits: ties-to-EVEN (never toward zero), and every NaN becomes the default BF16 NaN.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <arm_neon.h>

#ifndef HWCAP2_BF16
#define HWCAP2_BF16 (1UL << 14)
#endif

__attribute__((target("+bf16"))) static long dot(void) {
    const float af[8] = {1, 2, 3, 4, 5, 6, 7, 8}, bf[8] = {2, -1, .5f, 2, -2, 3, 4, .25f};
    bfloat16_t ah[8], bh[8];
    for (int i = 0; i < 8; i++) {
        ah[i] = vcvth_bf16_f32(af[i]);
        bh[i] = vcvth_bf16_f32(bf[i]);
    }
    bfloat16x8_t a = vld1q_bf16(ah), b = vld1q_bf16(bh);
    float32x4_t acc = vbfdotq_f32(vdupq_n_f32(0.0f), a, b);
    long r = 0;
    float32_t o[4];
    vst1q_f32(o, acc);
    for (int i = 0; i < 4; i++)
        r += (long)(o[i] * 4);
    return r;
}

static const uint32_t kSource[] = {
    0x3f808000u,              // tie, kept bit even -> stays down
    0x3f818000u,              // tie, kept bit odd  -> rounds up
    0x3f807fffu, 0x3f808001u, // either side of the tie
    0xbf808000u, 0xbf818000u, // the same two ties, negative: the tie-break is on the bit, not the magnitude
    0x7f800001u,              // signalling NaN whose payload is entirely below bit 16
    0x7fbfffffu, 0x7fc00000u, 0xffc00001u, // sNaN with a high payload, and quiet NaNs
    0x7f800000u, 0xff800000u,              // infinities pass through
    0x00000000u, 0x80000000u,              // zeroes keep their sign
    0x7f7fffffu, 0xff7fffffu,              // largest finite: the rounding carries it to infinity
    0x00000001u, 0x00008000u, 0x00018000u, // subnormals, including both tie parities
};

__attribute__((target("+bf16"))) static void convert(uint16_t out[]) {
    for (unsigned i = 0; i < sizeof kSource / sizeof kSource[0]; i++) {
        float f;
        memcpy(&f, &kSource[i], 4);
        bfloat16_t h = vcvth_bf16_f32(f);
        memcpy(&out[i], &h, 2);
    }
}

int main(void) {
    unsigned n = sizeof kSource / sizeof kSource[0];
    uint16_t out[sizeof kSource / sizeof kSource[0]];
    uint64_t fpsr = 0;
    __asm__ volatile("msr fpsr, %0" : : "r"(fpsr));
    convert(out);
    __asm__ volatile("mrs %0, fpsr" : "=r"(fpsr));
    printf("bf16 cvt");
    for (unsigned i = 0; i < n; i++)
        printf(" %04x", out[i]);
    putchar('\n');
    printf("bf16 fpsr=%lx\n", (unsigned long)fpsr);
    if (getauxval(AT_HWCAP2) & HWCAP2_BF16)
        printf("bf16 dot=%ld\n", dot());
    else
        printf("bf16 dot=unadvertised\n");
    return 0;
}
