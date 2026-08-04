// The AdvSIMD "vector x indexed element" box -- baseline Armv8.0, and the encoding group every compiled
// `vmla_lane_*` / `vmul_lane_*` / autovectorised inner loop lands in. Three things here are invisible to a
// careless test and are checked deliberately:
//   * the index split differs by element width (H:L:M for 16-bit, H:L for 32-bit, H for 64-bit), and a
//     16-bit element leaves Rm only 4 bits -- so every index is exercised, and one case pins Vm into V0..V15.
//   * FMLA/FMLS are FUSED: fused_ne_split() picks inputs where one rounding and two disagree.
//   * the saturating doubling forms must set FPSR.QC, and INT_MIN * INT_MIN is the input that catches a
//     doubling done in too narrow a type. FPSR is read inside the SAME asm block as the instruction: a
//     separate `mrs` lets the compiler hoist the NEON op out of the window and the check reads a stale bit.
// Every value is cross-checked against a C transcription of the ARM ARM pseudocode, so on an aarch64 host
// this is a differential test against silicon.
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// Make a buffer's contents unknown to the optimiser: without this GCC constant-folds every case here at
// -O2 and the fixture tests the compiler instead of the backend.
static void escape(const void *p) {
    __asm__ __volatile__("" ::"r"(p) : "memory");
}

// SignedSat(), used by the doubling references.
static int64_t ref_sat(__int128 value, unsigned bits) {
    __int128 max = ((__int128)1 << (bits - 1)) - 1, min = -max - 1;
    return (int64_t)(value > max ? max : (value < min ? min : value));
}

// ---- integer, non-widening: MUL/MLA/MLS and SQDMULH/SQRDMULH ----
// `op`: 0 MUL, 1 MLA, 2 MLS, 3 SQDMULH, 4 SQRDMULH. `b` is Vm[index]; the caller has already picked the lane.
static int32_t ref_mul(int op, unsigned esize, int32_t d, int32_t a, int32_t b) {
    if (op < 3) {
        uint32_t product = (uint32_t)a * (uint32_t)b;
        return (int32_t)(op == 0 ? product : (op == 1 ? (uint32_t)d + product : (uint32_t)d - product));
    }
    __int128 product = (__int128)2 * a * b;
    if (op == 4) product += (__int128)1 << (esize - 1u);
    return (int32_t)ref_sat(product >> esize, esize);
}

static void check16(int op, int16x8_t got, const int16_t *n, int16_t b, int16_t acc, int *ok) {
    int16_t out[8];
    vst1q_s16(out, got);
    for (int lane = 0; lane < 8; lane++)
        if (out[lane] != (int16_t)ref_mul(op, 16, acc, n[lane], b)) *ok = 0;
}

static void check32(int op, int32x4_t got, const int32_t *n, int32_t b, int32_t acc, int *ok) {
    int32_t out[4];
    vst1q_s32(out, got);
    for (int lane = 0; lane < 4; lane++)
        if (out[lane] != ref_mul(op, 32, acc, n[lane], b)) *ok = 0;
}

// Every index of the 16-bit form, so a decoder that reads H:L instead of H:L:M is caught.
static int integer_16(int32_t *sum) {
    int16_t n[8], m[8];
    for (int i = 0; i < 8; i++) {
        n[i] = (int16_t)(i * 4111 - 9000);
        m[i] = (int16_t)(30011 - i * 7919);
    }
    escape(n);
    escape(m);
    int16x8_t vn = vld1q_s16(n), vm = vld1q_s16(m);
    int ok = 1;
#define IDX16(index)                                                                                                   \
    do {                                                                                                               \
        check16(0, vmulq_laneq_s16(vn, vm, index), n, m[index], 0, &ok);                                               \
        check16(1, vmlaq_laneq_s16(vdupq_n_s16(77), vn, vm, index), n, m[index], 77, &ok);                             \
        check16(2, vmlsq_laneq_s16(vdupq_n_s16(-31), vn, vm, index), n, m[index], -31, &ok);                           \
        check16(3, vqdmulhq_laneq_s16(vn, vm, index), n, m[index], 0, &ok);                                            \
        check16(4, vqrdmulhq_laneq_s16(vn, vm, index), n, m[index], 0, &ok);                                           \
        *sum += vgetq_lane_s16(vmulq_laneq_s16(vn, vm, index), 1);                                                     \
    } while (0)
    IDX16(0);
    IDX16(1);
    IDX16(2);
    IDX16(3);
    IDX16(4);
    IDX16(5);
    IDX16(6);
    IDX16(7);
#undef IDX16
    // The D-form (Q=0) must ZERO the upper 64 bits, and the "x" constraint pins Vm into V0..V15 -- the only
    // registers a 16-bit-element by-element form can name.
    int16x8_t half = vcombine_s16(vld1_s16(n), vdup_n_s16(-1));
    __asm__("mul %0.4h, %0.4h, %1.h[5]" : "+w"(half) : "x"(vm));
    int16_t low[8];
    vst1q_s16(low, half);
    for (int lane = 0; lane < 4; lane++)
        if (low[lane] != (int16_t)ref_mul(0, 16, 0, n[lane], m[5])) ok = 0;
    for (int lane = 4; lane < 8; lane++)
        if (low[lane] != 0) ok = 0;
    return ok;
}

static int integer_32(int32_t *sum) {
    int32_t n[4] = {INT32_MIN, INT32_MAX, -70001, 123456789};
    int32_t m[4] = {INT32_MIN, 3, -5, 65537};
    escape(n);
    escape(m);
    int32x4_t vn = vld1q_s32(n), vm = vld1q_s32(m);
    int ok = 1;
#define IDX32(index)                                                                                                   \
    do {                                                                                                               \
        check32(0, vmulq_laneq_s32(vn, vm, index), n, m[index], 0, &ok);                                               \
        check32(1, vmlaq_laneq_s32(vdupq_n_s32(9), vn, vm, index), n, m[index], 9, &ok);                               \
        check32(2, vmlsq_laneq_s32(vdupq_n_s32(-9), vn, vm, index), n, m[index], -9, &ok);                             \
        check32(3, vqdmulhq_laneq_s32(vn, vm, index), n, m[index], 0, &ok);                                            \
        check32(4, vqrdmulhq_laneq_s32(vn, vm, index), n, m[index], 0, &ok);                                           \
        *sum += vgetq_lane_s32(vqrdmulhq_laneq_s32(vn, vm, index), 0);                                                 \
    } while (0)
    IDX32(0);
    IDX32(1);
    IDX32(2);
    IDX32(3);
#undef IDX32
    return ok;
}

// ---- widening: S/UMLAL, S/UMLSL, S/UMULL and the saturating doubling SQDML{A,S}L / SQDMULL ----
// `op`: 0 MULL, 1 MLAL, 2 MLSL, 3 SQDMULL, 4 SQDMLAL, 5 SQDMLSL.
static int64_t ref_widen(int op, unsigned esize, int64_t d, int64_t a, int64_t b, int is_signed) {
    if (op < 3) {
        uint64_t product = (uint64_t)a * (uint64_t)b;
        if (is_signed) product = (uint64_t)(a * b);
        return (int64_t)(op == 0 ? product : (op == 1 ? (uint64_t)d + product : (uint64_t)d - product));
    }
    int64_t product = ref_sat((__int128)2 * a * b, 2u * esize);
    if (op == 3) return product;
    return ref_sat(op == 4 ? (__int128)d + product : (__int128)d - product, 2u * esize);
}

static int widen_16(int32_t *sum) {
    int16_t n[8];
    uint16_t un[8];
    for (int i = 0; i < 8; i++) {
        n[i] = (int16_t)(i == 0 ? INT16_MIN : i * 5003 - 15000);
        un[i] = (uint16_t)(60013 - i * 4001);
    }
    escape(n);
    escape(un);
    int16x8_t vn = vld1q_s16(n);
    uint16x8_t vun = vld1q_u16(un);
    int ok = 1;
    for (int index = 0; index < 8; index++) {
        int32_t got[4], want[4];
        int32x4_t acc = vdupq_n_s32(1 << 20);
        switch (index) { // the laneq intrinsics need a literal lane, so the loop index becomes a switch
#define W16(index)                                                                                                     \
    case index: {                                                                                                      \
        vst1q_s32(got, vmull_laneq_s16(vget_low_s16(vn), vn, index));                                                  \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(0, 16, 0, n[l], n[index], 1);                                                 \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vmull_high_laneq_s16(vn, vn, index));                                                           \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(0, 16, 0, n[l + 4], n[index], 1);                                             \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vmlal_laneq_s16(acc, vget_low_s16(vn), vn, index));                                             \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(1, 16, 1 << 20, n[l], n[index], 1);                                           \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vmlsl_high_laneq_s16(acc, vn, vn, index));                                                      \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(2, 16, 1 << 20, n[l + 4], n[index], 1);                                       \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_u32((uint32_t *)got, vmull_laneq_u16(vget_low_u16(vun), vun, index));                                    \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(0, 16, 0, un[l], un[index], 0);                                               \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_u32((uint32_t *)got, vmlal_high_laneq_u16(vreinterpretq_u32_s32(acc), vun, vun, index));                 \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(1, 16, 1 << 20, un[l + 4], un[index], 0);                                     \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vqdmull_laneq_s16(vget_low_s16(vn), vn, index));                                                \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(3, 16, 0, n[l], n[index], 1);                                                 \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vqdmlal_high_laneq_s16(acc, vn, vn, index));                                                    \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(4, 16, 1 << 20, n[l + 4], n[index], 1);                                       \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s32(got, vqdmlsl_laneq_s16(acc, vget_low_s16(vn), vn, index));                                           \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = (int32_t)ref_widen(5, 16, 1 << 20, n[l], n[index], 1);                                           \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        *sum += got[0] + got[3];                                                                                       \
        break;                                                                                                         \
    }
            W16(0)
            W16(1) W16(2) W16(3) W16(4) W16(5) W16(6) W16(7)
#undef W16
                default : break;
        }
    }
    return ok;
}

static int widen_32(int32_t *sum) {
    int32_t n[4] = {INT32_MIN, INT32_MAX, -1000003, 77777};
    uint32_t un[4] = {4000000001u, 3u, 65537u, 2147483649u};
    escape(n);
    escape(un);
    int32x4_t vn = vld1q_s32(n);
    uint32x4_t vun = vld1q_u32(un);
    int ok = 1;
    for (int index = 0; index < 4; index++) {
        int64_t got[2], want[2];
        int64x2_t acc = vdupq_n_s64(-((int64_t)1 << 40));
        switch (index) {
#define W32(index)                                                                                                     \
    case index: {                                                                                                      \
        vst1q_s64(got, vmull_laneq_s32(vget_low_s32(vn), vn, index));                                                  \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(0, 32, 0, n[l], n[index], 1);                                                          \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s64(got, vmlal_high_laneq_s32(acc, vn, vn, index));                                                      \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(1, 32, -((int64_t)1 << 40), n[l + 2], n[index], 1);                                    \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s64(got, vmlsl_laneq_s32(acc, vget_low_s32(vn), vn, index));                                             \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(2, 32, -((int64_t)1 << 40), n[l], n[index], 1);                                        \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_u64((uint64_t *)got, vmull_high_laneq_u32(vun, vun, index));                                             \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(0, 32, 0, (int64_t)un[l + 2], (int64_t)un[index], 0);                                  \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s64(got, vqdmull_laneq_s32(vget_low_s32(vn), vn, index));                                                \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(3, 32, 0, n[l], n[index], 1);                                                          \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_s64(got, vqdmlsl_high_laneq_s32(acc, vn, vn, index));                                                    \
        for (int l = 0; l < 2; l++)                                                                                    \
            want[l] = ref_widen(5, 32, -((int64_t)1 << 40), n[l + 2], n[index], 1);                                    \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        *sum += (int32_t)got[0];                                                                                       \
        break;                                                                                                         \
    }
            W32(0)
            W32(1) W32(2) W32(3)
#undef W32
                default : break;
        }
    }
    // SQDMULL scalar: one element wide, upper bits zeroed. 2 * INT32_MIN * INT32_MIN is 2^63, so it saturates.
    int64_t scalar = vqdmulls_laneq_s32(INT32_MIN, vn, 0);
    if (scalar != ref_widen(3, 32, 0, INT32_MIN, INT32_MIN, 1)) ok = 0;
    return ok;
}

// ---- the spellings intrinsics do not produce ----
// GCC lowers vmls*_lane to MUL+SUB, offers no umlsl-by-element, and never emits a SCALAR by-element form, so
// those encodings reach the decoder only if they are written out.
static int asm_spelled(void) {
    int ok = 1;
    int16_t n[8], m[8];
    uint16_t un[8];
    for (int i = 0; i < 8; i++) {
        n[i] = (int16_t)(i * 3001 - 12000);
        m[i] = (int16_t)(i * 4999 - 8000);
        un[i] = (uint16_t)(50021 - i * 3001);
    }
    int32_t n32[4] = {-70001, 123456789, INT32_MIN, 5}, m32[4] = {3, -5, 65537, INT32_MAX};
    escape(n);
    escape(m);
    escape(un);
    escape(n32);
    escape(m32);
    int16x8_t vn = vld1q_s16(n), vm = vld1q_s16(m);
    uint16x8_t vun = vld1q_u16(un);
    int32x4_t wn = vld1q_s32(n32), wm = vld1q_s32(m32);

    int16x8_t narrow = vdupq_n_s16(1234);
    int16_t got16[8];
    __asm__("mls %0.8h, %1.8h, %2.h[6]" : "+w"(narrow) : "w"(vn), "x"(vm));
    vst1q_s16(got16, narrow);
    for (int l = 0; l < 8; l++)
        if (got16[l] != (int16_t)ref_mul(2, 16, 1234, n[l], m[6])) ok = 0;

    int32x4_t word = vdupq_n_s32(-9999);
    int32_t got32[4];
    __asm__("mls %0.4s, %1.4s, %2.s[2]" : "+w"(word) : "w"(wn), "w"(wm));
    vst1q_s32(got32, word);
    for (int l = 0; l < 4; l++)
        if (got32[l] != ref_mul(2, 32, -9999, n32[l], m32[2])) ok = 0;

    uint32x4_t unsigned_acc = vdupq_n_u32(1u << 28);
    __asm__("umlsl %0.4s, %1.4h, %2.h[3]" : "+w"(unsigned_acc) : "w"(vun), "x"(vun));
    vst1q_u32((uint32_t *)got32, unsigned_acc);
    for (int l = 0; l < 4; l++)
        if (got32[l] != (int32_t)ref_widen(2, 16, 1u << 28, un[l], un[3], 0)) ok = 0;
    unsigned_acc = vdupq_n_u32(1u << 28);
    __asm__("umlsl2 %0.4s, %1.8h, %2.h[3]" : "+w"(unsigned_acc) : "w"(vun), "x"(vun));
    vst1q_u32((uint32_t *)got32, unsigned_acc);
    for (int l = 0; l < 4; l++)
        if (got32[l] != (int32_t)ref_widen(2, 16, 1u << 28, un[l + 4], un[3], 0)) ok = 0;

    int64x2_t wide = vdupq_n_s64(1 << 20);
    int64_t got64[2];
    __asm__("sqdmlal %0.2d, %1.2s, %2.s[3]" : "+w"(wide) : "w"(wn), "w"(wm));
    vst1q_s64(got64, wide);
    for (int l = 0; l < 2; l++)
        if (got64[l] != ref_widen(4, 32, 1 << 20, n32[l], m32[3], 1)) ok = 0;

    // Scalar spellings: ONE element, and everything above it must be zeroed.
    int16x8_t scalar16;
    __asm__("sqdmulh %h0, %h1, %2.h[7]" : "=w"(scalar16) : "w"(vn), "x"(vm));
    vst1q_s16(got16, scalar16);
    if (got16[0] != (int16_t)ref_mul(3, 16, 0, n[0], m[7])) ok = 0;
    for (int l = 1; l < 8; l++)
        if (got16[l] != 0) ok = 0;
    int32x4_t scalar32;
    __asm__("sqdmull %s0, %h1, %2.h[7]" : "=w"(scalar32) : "w"(vn), "x"(vm));
    vst1q_s32(got32, scalar32);
    if (got32[0] != (int32_t)ref_widen(3, 16, 0, n[0], m[7], 1)) ok = 0;
    for (int l = 1; l < 4; l++)
        if (got32[l] != 0) ok = 0;
    float fscalar[4] = {2.5f, 0, 0, 0}, fother[4] = {1.0f, -3.0f, 0.25f, 8.0f};
    escape(fscalar);
    escape(fother);
    float32x4_t fs = vld1q_f32(fscalar), fo = vld1q_f32(fother);
    __asm__("fmla %s0, %s1, %2.s[3]" : "+w"(fs) : "w"(fo), "w"(fo));
    float fgot[4];
    vst1q_f32(fgot, fs);
    if (fgot[0] != 2.5f + fother[0] * fother[3]) ok = 0;
    for (int l = 1; l < 4; l++)
        if (fgot[l] != 0.0f) ok = 0;
    return ok;
}

// ---- FPSR.QC ----
// The instruction and both FPSR accesses live in one asm block; splitting them lets the compiler schedule
// the NEON op outside the window, which reads a stale QC and passes for the wrong reason.
// %0 status, %1 destination (read-modify-write), %2 Vn, %3 Vm -- "x" keeps Vm inside V0..V15 so the same
// macro can spell a 16-bit-element form.
#define QC_CASE(text, status, dst, src, other)                                                                         \
    __asm__ __volatile__("msr fpsr, xzr\n\t" text "\n\tmrs %0, fpsr"                                                   \
                         : "=r"(status), "+w"(dst)                                                                     \
                         : "w"(src), "x"(other)                                                                        \
                         : "memory")
#define QC_SET(status) (((status) >> 27) & 1u)

static int saturation(uint32_t *seen) {
    int ok = 1;
    uint64_t status;
    *seen = 0;
    int32x4_t min32 = vdupq_n_s32(INT32_MIN), acc32 = vdupq_n_s32(0);
    // 2 * INT32_MIN * INT32_MIN >> 32 is 2^31, which saturates to INT32_MAX. Doubling inside int64 wraps to
    // INT32_MIN instead and leaves QC clear -- the exact wrong answer this case exists to reject.
    QC_CASE("sqdmulh %1.4s, %2.4s, %3.s[0]", status, acc32, min32, min32);
    if (vgetq_lane_s32(acc32, 0) != INT32_MAX || !QC_SET(status)) ok = 0;
    *seen |= 1u;
    QC_CASE("sqrdmulh %1.4s, %2.4s, %3.s[0]", status, acc32, min32, min32);
    if (vgetq_lane_s32(acc32, 0) != INT32_MAX || !QC_SET(status)) ok = 0;
    *seen |= 2u;
    int64x2_t wide = vdupq_n_s64(0);
    QC_CASE("sqdmull %1.2d, %2.2s, %3.s[0]", status, wide, min32, min32);
    if (vgetq_lane_s64(wide, 0) != INT64_MAX || !QC_SET(status)) ok = 0;
    *seen |= 4u;
    // The accumulate saturates on its own, after the product has already been clamped.
    wide = vdupq_n_s64(INT64_MAX);
    QC_CASE("sqdmlal %1.2d, %2.2s, %3.s[0]", status, wide, min32, min32);
    if (vgetq_lane_s64(wide, 0) != INT64_MAX || !QC_SET(status)) ok = 0;
    *seen |= 8u;
    // An in-range doubling must leave QC alone.
    int32x4_t small = vdupq_n_s32(3), out = vdupq_n_s32(0);
    QC_CASE("sqdmulh %1.4s, %2.4s, %3.s[0]", status, out, small, small);
    if (vgetq_lane_s32(out, 0) != 0 || QC_SET(status)) ok = 0;
    *seen |= 16u;
    // 16-bit elements, index 7 -- the H:L:M corner, and the narrow doubling that saturates at 32 bits.
    int16x8_t min16 = vdupq_n_s16(INT16_MIN);
    int32x4_t narrow = vdupq_n_s32(0);
    QC_CASE("sqdmull %1.4s, %2.4h, %3.h[7]", status, narrow, min16, min16);
    if (vgetq_lane_s32(narrow, 0) != INT32_MAX || !QC_SET(status)) ok = 0;
    *seen |= 32u;
    return ok;
}

// ---- floating point: FMLA/FMLS/FMUL and FMULX by element ----
// One rounding, not two: a*b is inexact here and rounding it before the add discards what the fused form keeps.
static int fused_ne_split(uint32_t *fused_bits, uint32_t *split_bits) {
    const uint32_t a_bits = 0x3F800001u, b_bits = 0x3F800003u, c_bits = 0xBF800000u;
    float a, b, c;
    memcpy(&a, &a_bits, 4);
    memcpy(&b, &b_bits, 4);
    memcpy(&c, &c_bits, 4);
    escape(&a);
    escape(&b);
    escape(&c);
    float32x4_t acc = vdupq_n_f32(c);
    acc = vfmaq_laneq_f32(acc, vdupq_n_f32(a), vdupq_n_f32(b), 2);
    float fused = vgetq_lane_f32(acc, 0);
    volatile float product = a * b; // volatile: the round trip through single is the point
    float split = product + c;
    memcpy(fused_bits, &fused, 4);
    memcpy(split_bits, &split, 4);
    return *fused_bits != *split_bits;
}

static int floating(uint32_t *mulx_bits) {
    int ok = 1;
    float fn[4] = {1.5f, -2.25f, 0.5f, 3.0f}, fm[4] = {2.0f, 4.0f, -8.0f, 0.25f};
    escape(fn);
    escape(fm);
    float32x4_t vn = vld1q_f32(fn), vm = vld1q_f32(fm);
    for (int index = 0; index < 4; index++) {
        float got[4], want[4];
        switch (index) {
#define F32(index)                                                                                                     \
    case index: {                                                                                                      \
        vst1q_f32(got, vmulq_laneq_f32(vn, vm, index));                                                                \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = fn[l] * fm[index];                                                                               \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_f32(got, vfmaq_laneq_f32(vdupq_n_f32(0.125f), vn, vm, index));                                           \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = 0.125f + fn[l] * fm[index];                                                                      \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        vst1q_f32(got, vfmsq_laneq_f32(vdupq_n_f32(0.125f), vn, vm, index));                                           \
        for (int l = 0; l < 4; l++)                                                                                    \
            want[l] = 0.125f - fn[l] * fm[index];                                                                      \
        if (memcmp(got, want, sizeof got)) ok = 0;                                                                     \
        break;                                                                                                         \
    }
            F32(0)
            F32(1) F32(2) F32(3)
#undef F32
                default : break;
        }
    }
    // 64-bit elements index with H alone, so only [0] and [1] exist and Q must be 1.
    double dn[2] = {1.25, -6.5}, dm[2] = {8.0, -0.5};
    escape(dn);
    escape(dm);
    float64x2_t wn = vld1q_f64(dn), wm = vld1q_f64(dm);
    double dgot[2];
    vst1q_f64(dgot, vmulq_laneq_f64(wn, wm, 1));
    if (dgot[0] != dn[0] * dm[1] || dgot[1] != dn[1] * dm[1]) ok = 0;
    vst1q_f64(dgot, vfmaq_laneq_f64(vdupq_n_f64(0.5), wn, wm, 0));
    if (dgot[0] != 0.5 + dn[0] * dm[0] || dgot[1] != 0.5 + dn[1] * dm[0]) ok = 0;
    vst1q_f64(dgot, vfmsq_laneq_f64(vdupq_n_f64(0.5), wn, wm, 1));
    if (dgot[0] != 0.5 - dn[0] * dm[1] || dgot[1] != 0.5 - dn[1] * dm[1]) ok = 0;
    // FMULX differs from FMUL on 0 * inf ALONE: +-2.0 with no Invalid, which is the whole reason it exists.
    float32x4_t zero_inf = vsetq_lane_f32(__builtin_inff(), vdupq_n_f32(0.0f), 1);
    float32x4_t inf_zero = vsetq_lane_f32(0.0f, vdupq_n_f32(__builtin_inff()), 1);
    float32x4_t mulx = vdupq_n_f32(0.0f);
    uint64_t status;
    QC_CASE("fmulx %1.4s, %2.4s, %3.s[1]", status, mulx, zero_inf, zero_inf);
    float taken = vgetq_lane_f32(mulx, 0);
    memcpy(mulx_bits, &taken, 4);
    if (taken != 2.0f || (status & 1u)) ok = 0; // FPSR.IOC must stay clear
    float32x4_t plain = vdupq_n_f32(0.0f);
    QC_CASE("fmul %1.4s, %2.4s, %3.s[1]", status, plain, zero_inf, zero_inf);
    if (!__builtin_isnan(vgetq_lane_f32(plain, 0)) || !(status & 1u)) ok = 0;
    // FMULX (vector) shares the helper, so exercise it here too: 0*inf and inf*0, one per lane.
    float32x4_t vec = vdupq_n_f32(0.0f);
    QC_CASE("fmulx %1.4s, %2.4s, %3.4s", status, vec, zero_inf, inf_zero);
    if (vgetq_lane_f32(vec, 0) != 2.0f || vgetq_lane_f32(vec, 1) != 2.0f || (status & 1u)) ok = 0;
    return ok;
}

int main(void) {
    int32_t sum16 = 0, sum32 = 0, wsum16 = 0, wsum32 = 0;
    uint32_t fused = 0, split = 0, mulx = 0, seen = 0;
    int i16 = integer_16(&sum16), i32 = integer_32(&sum32);
    int w16 = widen_16(&wsum16), w32 = widen_32(&wsum32);
    int sat = saturation(&seen), spelled = asm_spelled();
    int differs = fused_ne_split(&fused, &split), fp = floating(&mulx);
    printf("neon-indexed i16=%d i32=%d w16=%d w32=%d asm=%d sat=%d qc_cases=%u fp=%d fused_differs=%d\n", i16, i32, w16,
           w32, spelled, sat, seen, fp, differs);
    printf("neon-indexed sums %d %d %d %d fused=%08x split=%08x mulx=%08x\n", sum16, sum32, wsum16, wsum32, fused,
           split, mulx);
    return 0;
}
