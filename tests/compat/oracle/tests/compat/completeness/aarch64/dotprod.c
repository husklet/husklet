// FEAT_DotProd (AT_HWCAP bit 20, ASIMDDP) -- the one extension in this group the CPU model advertises, so a
// conforming guest reaches these instructions and both backends must run them. The by-element forms matter as
// much as the vector ones: they are what a real int8 kernel emits, and they broadcast ONE 4-byte group of Vm
// to every lane, which is the part an implementation gets wrong. Each result is cross-checked against a plain
// C transcription of the ARM ARM pseudocode, so on an aarch64 host this is a differential test against silicon.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <arm_neon.h>

#ifndef HWCAP_ASIMDDP
#define HWCAP_ASIMDDP (1UL << 20)
#endif

__attribute__((target("+dotprod"))) static long go(void) {
    int8_t va[16], vb[16];
    for (int i = 0; i < 16; i++) {
        va[i] = i - 8;
        vb[i] = (i & 3) + 1;
    }
    int8x16_t a = vld1q_s8(va), b = vld1q_s8(vb);
    int32x4_t acc = vdupq_n_s32(0);
    acc = vdotq_s32(acc, a, b);
    uint8_t ua[16], ub[16];
    for (int i = 0; i < 16; i++) {
        ua[i] = i + 1;
        ub[i] = 2;
    }
    uint32x4_t uacc = vdupq_n_u32(0);
    uacc = vdotq_u32(uacc, vld1q_u8(ua), vld1q_u8(ub));
    long r = 0;
    int32_t o[4];
    uint32_t uo[4];
    vst1q_s32(o, acc);
    for (int i = 0; i < 4; i++)
        r += o[i];
    vst1q_u32(uo, uacc);
    for (int i = 0; i < 4; i++)
        r += uo[i];
    return r;
}

static void seed(int8_t s[16], uint8_t u[16]) {
    for (int i = 0; i < 16; i++) {
        s[i] = (int8_t)(i * 13 - 70);
        u[i] = (uint8_t)(i * 11 + 3);
    }
}

// ARM ARM: res = Elem[Vd,e,32]; for i in 0..3: res = res + Int(Elem[Vn,4e+i,8]) * Int(Elem[Vm,4m+i,8]),
// where m is the lane index for the by-element forms and e for the vector ones. All arithmetic is 32-bit
// modular. `sig` names how each source's bytes are read.
static void dot_ref(int32_t acc[4], const uint8_t *n, const uint8_t *m, int lanes, int idx, int nsig, int msig) {
    for (int e = 0; e < lanes; e++) {
        int32_t s = acc[e];
        int mbase = (idx < 0 ? e : idx) * 4;
        for (int i = 0; i < 4; i++) {
            int32_t x = nsig ? (int32_t)(int8_t)n[4 * e + i] : (int32_t)n[4 * e + i];
            int32_t y = msig ? (int32_t)(int8_t)m[mbase + i] : (int32_t)m[mbase + i];
            s += x * y;
        }
        acc[e] = s;
    }
}

__attribute__((target("+dotprod"))) static void go_idx(int32_t out[12]) {
    int8_t s[16];
    uint8_t u[16];
    seed(s, u);
    int8x16_t a = vld1q_s8(s);
    uint8x16_t b = vld1q_u8(u);
    // Every index 0..3 of the broadcast group, both signednesses, and the 64-bit (Q=0) form.
    int32x4_t sq = vdupq_n_s32(1);
    sq = vdotq_laneq_s32(sq, a, a, 0);
    sq = vdotq_laneq_s32(sq, a, a, 3);
    vst1q_s32(out, sq);
    uint32x4_t uq = vdupq_n_u32(2);
    uq = vdotq_laneq_u32(uq, b, b, 1);
    uq = vdotq_laneq_u32(uq, b, b, 2);
    vst1q_u32((uint32_t *)(out + 4), uq);
    int32x2_t sd = vdup_n_s32(-3);
    sd = vdot_laneq_s32(sd, vget_low_s8(a), a, 2);
    vst1_s32(out + 8, sd);
    out[10] = out[11] = 0;
}

static void ref_idx(int32_t out[12]) {
    int8_t s[16];
    uint8_t u[16];
    seed(s, u);
    int32_t sq[4] = {1, 1, 1, 1};
    dot_ref(sq, (const uint8_t *)s, (const uint8_t *)s, 4, 0, 1, 1);
    dot_ref(sq, (const uint8_t *)s, (const uint8_t *)s, 4, 3, 1, 1);
    memcpy(out, sq, sizeof sq);
    int32_t uq[4] = {2, 2, 2, 2};
    dot_ref(uq, u, u, 4, 1, 0, 0);
    dot_ref(uq, u, u, 4, 2, 0, 0);
    memcpy(out + 4, uq, sizeof uq);
    int32_t sd[4] = {-3, -3, 0, 0};
    dot_ref(sd, (const uint8_t *)s, (const uint8_t *)s, 2, 2, 1, 1);
    out[8] = sd[0];
    out[9] = sd[1];
    out[10] = out[11] = 0;
}

int main(void) {
    int32_t got[12], want[12];
    go_idx(got);
    ref_idx(want);
    printf("dotprod r=%ld\n", go());
    printf("dotprod hwcap_dp=%d idx_ok=%d", !!(getauxval(AT_HWCAP) & HWCAP_ASIMDDP), memcmp(got, want, 40) == 0);
    for (int i = 0; i < 10; i++)
        printf(" %d", got[i]);
    putchar('\n');
    return 0;
}
