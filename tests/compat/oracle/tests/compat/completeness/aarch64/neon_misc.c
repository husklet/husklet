// The AdvSIMD encodings an exhaustive interpreter-vs-qemu sweep of the whole 0x7 top-level group found
// SILENTLY mis-executed or computed-and-discarded -- every one baseline Armv8.0 except the FP16 block,
// which was being decoded as INS. Each case is checked against a C transcription of the ARM ARM
// pseudocode, so on an aarch64 host this is a differential test against silicon rather than a golden.
//
// Two traps this file exists to avoid re-learning:
//   * GCC constant-folds NEON at -O2; escape() on every seed buffer keeps the instruction in the binary.
//   * FPSR must be read in the SAME asm block as the instruction, or the scheduler hoists the operation
//     out of the window and the flag assertions pass for the wrong reason.
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static void escape(const void *p) {
    __asm__ __volatile__("" ::"r"(p) : "memory");
}

// One instruction with FPSR cleared before and read after, inside the same block.
#define FPSR_CASE(text, status, dst, a, b)                                                                             \
    __asm__ __volatile__("msr fpsr, xzr\n\t" text "\n\tmrs %0, fpsr"                                                   \
                         : "=r"(status), "+w"(dst)                                                                     \
                         : "w"(a), "w"(b)                                                                              \
                         : "memory")
#define FPSR_CASE1(text, status, dst, a)                                                                               \
    __asm__ __volatile__("msr fpsr, xzr\n\t" text "\n\tmrs %0, fpsr" : "=r"(status), "+w"(dst) : "w"(a) : "memory")
#define QC(status) (((status) >> 27) & 1u)
#define IOC(status) ((status) & 1u)
#define IXC(status) (((status) >> 4) & 1u)

// ---- DUP (element), scalar spelling -------------------------------------------------------------
// MOV Bd/Hd/Sd, Vn.T[i] writes ONE element and zeroes [127:esize]. Filling the whole 64-bit half instead
// is invisible through a float read and visible the moment the register is read as a D or a Q.
static int dup_scalar(uint64_t *seen) {
    uint8_t bytes[16];
    for (int i = 0; i < 16; i++) bytes[i] = (uint8_t)(0x11u * (unsigned)(i + 1));
    escape(bytes);
    uint8x16_t v = vld1q_u8(bytes);
    int ok = 1;
    uint64_t out[2];
    uint8x16_t d = vdupq_n_u8(0xFF);

    __asm__ __volatile__("mov %b0, %1.b[5]" : "=w"(d) : "w"(v));
    vst1q_u64(out, vreinterpretq_u64_u8(d));
    if (out[0] != bytes[5] || out[1] != 0) ok = 0;
    *seen |= out[0];

    __asm__ __volatile__("mov %h0, %1.h[3]" : "=w"(d) : "w"(v));
    vst1q_u64(out, vreinterpretq_u64_u8(d));
    uint16_t h;
    memcpy(&h, bytes + 6, 2);
    if (out[0] != h || out[1] != 0) ok = 0;
    *seen |= out[0] << 8;

    __asm__ __volatile__("mov %s0, %1.s[2]" : "=w"(d) : "w"(v));
    vst1q_u64(out, vreinterpretq_u64_u8(d));
    uint32_t s;
    memcpy(&s, bytes + 8, 4);
    if (out[0] != s || out[1] != 0) ok = 0;

    // The D form fills the same 64 bits either way, so it pins the reference rather than the bug.
    __asm__ __volatile__("mov %d0, %1.d[1]" : "=w"(d) : "w"(v));
    vst1q_u64(out, vreinterpretq_u64_u8(d));
    uint64_t g;
    memcpy(&g, bytes + 8, 8);
    if (out[0] != g || out[1] != 0) ok = 0;
    return ok;
}

// ---- MOVI / MVNI with MSL, and FMOV Vd.8H, #imm -------------------------------------------------
// cmode 110x is MOVI/MVNI shifting ONES in, not the ORR/BIC-immediate form: testing cmode<3:1> != 111
// instead of cmode<3:2> != 11 made 1101 read-modify the destination.
static int modified_immediate(void) {
    int ok = 1;
    uint32_t out[4];
    uint32x4_t d = vdupq_n_u32(0xDEADBEEFu);
    __asm__ __volatile__("movi %0.4s, #0x01, msl #16" : "=w"(d));
    vst1q_u32(out, d);
    for (int i = 0; i < 4; i++)
        if (out[i] != 0x0001FFFFu) ok = 0;
    d = vdupq_n_u32(0xDEADBEEFu);
    __asm__ __volatile__("movi %0.4s, #0x81, msl #8" : "=w"(d));
    vst1q_u32(out, d);
    for (int i = 0; i < 4; i++)
        if (out[i] != 0x000081FFu) ok = 0;
    d = vdupq_n_u32(0xDEADBEEFu);
    __asm__ __volatile__("mvni %0.4s, #0x01, msl #16" : "=w"(d));
    vst1q_u32(out, d);
    for (int i = 0; i < 4; i++)
        if (out[i] != ~0x0001FFFFu) ok = 0;
    // ORR/BIC immediate DOES read-modify -- the arm the MSL forms were wrongly sharing.
    d = vdupq_n_u32(0x00FF0000u);
    __asm__ __volatile__("orr %0.4s, #0x0f, lsl #8" : "+w"(d));
    vst1q_u32(out, d);
    for (int i = 0; i < 4; i++)
        if (out[i] != 0x00FF0F00u) ok = 0;

    // FMOV Vd.8H, #2.125 (cmode 1111, o2 1): the half expansion, not the single one replicated.
    uint16_t halves[8];
    uint16x8_t hv = vdupq_n_u16(0);
    __asm__ __volatile__(".inst 0x4f00fc20\n\tmov %0.16b, v0.16b" : "=w"(hv) : : "v0");
    vst1q_u16(halves, hv);
    for (int i = 0; i < 8; i++)
        if (halves[i] != 0x4040u) ok = 0;
    return ok;
}

// ---- UQSHL / UQRSHL at 64-bit elements ----------------------------------------------------------
// A shift of 0 needs its own arm: the overflow test shifts by esize, which the host masks to a no-op at
// esize 64 and so saturated every nonzero input on an instruction that changes nothing.
static int qshl_64(void) {
    int ok = 1;
    uint64_t status;
    uint64_t src[2] = {0xFFF0000000000000ull, 0x0000000000000001ull};
    escape(src);
    uint64x2_t n = vld1q_u64(src), amount = vdupq_n_u64(0), r = vdupq_n_u64(0);
    uint64_t out[2];

    FPSR_CASE("uqshl %1.2d, %2.2d, %3.2d", status, r, n, amount);
    vst1q_u64(out, r);
    if (out[0] != src[0] || out[1] != src[1] || QC(status)) ok = 0;
    FPSR_CASE("uqrshl %1.2d, %2.2d, %3.2d", status, r, n, amount);
    vst1q_u64(out, r);
    if (out[0] != src[0] || out[1] != src[1] || QC(status)) ok = 0;

    // A shift that really does overflow must still saturate and set QC.
    amount = vdupq_n_u64(4);
    FPSR_CASE("uqshl %1.2d, %2.2d, %3.2d", status, r, n, amount);
    vst1q_u64(out, r);
    if (out[0] != 0xFFFFFFFFFFFFFFFFull || out[1] != 16 || !QC(status)) ok = 0;
    return ok;
}

// ---- UQRSHRN / SQRSHRN: the rounding constant carries out of the source element ------------------
// 0xFFFFFFFFFFFFFFFF + 2^31 does not fit 64 bits; adding in the source width wrapped to a small value and
// produced 0 with QC CLEAR where the answer is the maximum with QC SET.
static int rshrn_carry(uint64_t *seen) {
    int ok = 1;
    uint64_t status;
    uint64_t src[2] = {0xFFFFFFFFFFFFFFFFull, 0x7FFFFFFFFFFFFFFFull};
    escape(src);
    uint64x2_t n = vld1q_u64(src);
    uint32x2_t narrow = vdup_n_u32(0);
    uint32_t out32[2];

    FPSR_CASE1("uqrshrn %1.2s, %2.2d, #32", status, narrow, n);
    vst1_u32(out32, narrow);
    if (out32[0] != 0xFFFFFFFFu || out32[1] != 0x80000000u || !QC(status)) ok = 0;
    *seen |= 1u;

    // The signed source overflows int64 on the same add.
    int64x2_t sn = vreinterpretq_s64_u64(n);
    int32x2_t snarrow = vdup_n_s32(0);
    int32_t outs32[2];
    FPSR_CASE1("sqrshrn %1.2s, %2.2d, #32", status, snarrow, sn);
    vst1_s32(outs32, snarrow);
    if (outs32[0] != 0 || outs32[1] != INT32_MAX || !QC(status)) ok = 0;
    *seen |= 2u;

    // 16-bit sources, where the same add cannot carry: the arm that must not change.
    uint16_t s16[8];
    for (int i = 0; i < 8; i++) s16[i] = (uint16_t)(0xFF00u + (unsigned)i * 37u);
    escape(s16);
    uint16x8_t w = vld1q_u16(s16);
    uint8x8_t b = vdup_n_u8(0);
    uint8_t out8[8];
    FPSR_CASE1("uqrshrn %1.8b, %2.8h, #8", status, b, w);
    vst1_u8(out8, b);
    for (int i = 0; i < 8; i++) {
        uint32_t want = ((uint32_t)s16[i] + 0x80u) >> 8;
        if (out8[i] != (want > 0xFFu ? 0xFFu : want)) ok = 0;
    }
    *seen |= 4u;
    return ok;
}

// ---- FPCompareGE/GT raise Invalid for a QUIET NaN; FPCompareEQ does not --------------------------
static int compare_invalid(uint32_t *mask) {
    int ok = 1;
    uint64_t status;
    float pair[4] = {0.0f, 1.0f, 2.0f, 3.0f};
    escape(pair);
    float32x4_t a = vld1q_f32(pair), nan = vdupq_n_f32(__builtin_nanf("")), r = vdupq_n_f32(0.0f);

    FPSR_CASE("fcmge %1.4s, %2.4s, %3.4s", status, r, a, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 1u;
    FPSR_CASE("fcmgt %1.4s, %2.4s, %3.4s", status, r, a, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 2u;
    FPSR_CASE("facge %1.4s, %2.4s, %3.4s", status, r, a, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 4u;
    FPSR_CASE("facgt %1.4s, %2.4s, %3.4s", status, r, a, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 8u;
    // FCMEQ is FPCompareEQ: quiet in, quiet out.
    FPSR_CASE("fcmeq %1.4s, %2.4s, %3.4s", status, r, a, nan);
    if (IOC(status)) ok = 0;
    *mask |= 16u;
    // Compare-against-zero takes the same split.
    FPSR_CASE1("fcmge %1.4s, %2.4s, #0.0", status, r, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 32u;
    FPSR_CASE1("fcmlt %1.4s, %2.4s, #0.0", status, r, nan);
    if (!IOC(status)) ok = 0;
    *mask |= 64u;
    FPSR_CASE1("fcmeq %1.4s, %2.4s, #0.0", status, r, nan);
    if (IOC(status)) ok = 0;
    *mask |= 128u;
    // The scalar spellings share the helper.
    float64x2_t da = vdupq_n_f64(1.0), dn = vdupq_n_f64(__builtin_nan("")), dr = vdupq_n_f64(0.0);
    FPSR_CASE("facgt %d1, %d2, %d3", status, dr, da, dn);
    if (!IOC(status)) ok = 0;
    *mask |= 256u;
    return ok;
}

// ---- FCVT*: a scaled conversion landing exactly at 2^0, and the Invalid/Inexact split -------------
static int convert_flags(void) {
    int ok = 1;
    uint64_t status;
    // 2^-7 scaled by 2^59 is exactly 2^52: exponent 0 in the interpreter's scaling, where a shift by the
    // full 64 bits wrongly declared the value too large and saturated it.
    double dv[2] = {0x1p-7, 0x1p-7};
    escape(dv);
    float64x2_t d = vld1q_f64(dv);
    int64x2_t got = vdupq_n_s64(0);
    int64_t out[2];
    FPSR_CASE1("fcvtzs %1.2d, %2.2d, #59", status, got, d);
    vst1q_s64(out, got);
    if (out[0] != ((int64_t)1 << 52) || out[1] != ((int64_t)1 << 52)) ok = 0;

    float fv[4] = {0x1p-5f, 0x1p-5f, 0x1p-5f, 0x1p-5f};
    escape(fv);
    float32x4_t f = vld1q_f32(fv);
    int32x4_t g32 = vdupq_n_s32(0);
    int32_t o32[4];
    FPSR_CASE1("fcvtzs %1.4s, %2.4s, #27", status, g32, f);
    vst1q_s32(o32, g32);
    for (int i = 0; i < 4; i++)
        if (o32[i] != (1 << 22)) ok = 0;

    // FPToFixed's exceptions are an if/elsif: -0.5 rounded toward -inf is -1, which an unsigned
    // destination cannot hold, so Invalid fires ALONE and Inexact is suppressed.
    double neg = -0.5;
    escape(&neg);
    uint64_t w;
    __asm__ __volatile__("msr fpsr, xzr\n\tfcvtmu %w0, %d2\n\tmrs %1, fpsr"
                         : "=r"(w), "=r"(status)
                         : "w"(neg)
                         : "memory");
    if ((uint32_t)w != 0 || !IOC(status) || IXC(status)) ok = 0;
    // The in-range neighbour still reports Inexact and nothing else.
    __asm__ __volatile__("msr fpsr, xzr\n\tfcvtzu %w0, %d2\n\tmrs %1, fpsr"
                         : "=r"(w), "=r"(status)
                         : "w"(neg)
                         : "memory");
    if ((uint32_t)w != 0 || IOC(status) || !IXC(status)) ok = 0;
    return ok;
}

// ---- CLS/CLZ and SQABS/SQNEG: computed, then dropped by a `break` into the next switch ------------
static int misc_twoop(int64_t *sum) {
    int ok = 1;
    uint64_t status;
    int8_t src[16];
    for (int i = 0; i < 16; i++) src[i] = (int8_t)(i * 17 - 128);
    escape(src);
    int8x16_t v = vld1q_s8(src);
    int8_t out[16];

    vst1q_s8(out, vclsq_s8(v));
    for (int i = 0; i < 16; i++) {
        unsigned x = (uint8_t)src[i], n = 0;
        while (n < 7 && (((x >> (7 - n)) ^ (x >> (6 - n))) & 1u) == 0) n++;
        if (out[i] != (int8_t)n) ok = 0;
        *sum += out[i];
    }
    vst1q_u8((uint8_t *)out, vclzq_u8(vreinterpretq_u8_s8(v)));
    for (int i = 0; i < 16; i++) {
        unsigned x = (uint8_t)src[i], n = 0;
        while (n < 8 && ((x >> (7 - n)) & 1u) == 0) n++;
        if ((uint8_t)out[i] != n) ok = 0;
        *sum += (uint8_t)out[i];
    }
    // SQABS/SQNEG saturate on the one input whose negation does not fit.
    int8x16_t r = vdupq_n_s8(0);
    int8x16_t mins = vdupq_n_s8(INT8_MIN);
    FPSR_CASE1("sqabs %1.16b, %2.16b", status, r, mins);
    vst1q_s8(out, r);
    if (out[0] != INT8_MAX || !QC(status)) ok = 0;
    FPSR_CASE1("sqneg %1.16b, %2.16b", status, r, mins);
    vst1q_s8(out, r);
    if (out[0] != INT8_MAX || !QC(status)) ok = 0;
    FPSR_CASE1("sqneg %1.16b, %2.16b", status, r, v);
    vst1q_s8(out, r);
    for (int i = 0; i < 16; i++) {
        int want = src[i] == INT8_MIN ? INT8_MAX : -src[i];
        if (out[i] != (int8_t)want) ok = 0;
    }

    // SUQADD/USQADD: the accumulator's signedness differs from the operand's, so neither SQADD nor UQADD
    // is the reference.
    int16_t acc[8], op[8];
    for (int i = 0; i < 8; i++) {
        acc[i] = (int16_t)(i * 4093 - 16000);
        op[i] = (int16_t)(0x7000 + i * 999);
    }
    escape(acc);
    escape(op);
    int16x8_t a = vld1q_s16(acc);
    uint16x8_t b = vreinterpretq_u16_s16(vld1q_s16(op));
    int16x8_t sres = a;
    FPSR_CASE1("suqadd %1.8h, %2.8h", status, sres, b);
    int16_t so[8];
    vst1q_s16(so, sres);
    unsigned qc_suq = QC(status);
    unsigned want_qc = 0;
    for (int i = 0; i < 8; i++) {
        int32_t t = (int32_t)acc[i] + (int32_t)(uint16_t)op[i];
        if (t > INT16_MAX) {
            t = INT16_MAX;
            want_qc = 1;
        }
        if (so[i] != (int16_t)t) ok = 0;
    }
    if (qc_suq != want_qc) ok = 0;

    uint16x8_t ures = vreinterpretq_u16_s16(a);
    FPSR_CASE1("usqadd %1.8h, %2.8h", status, ures, vld1q_s16(op));
    uint16_t uo[8];
    vst1q_u16(uo, ures);
    want_qc = 0;
    for (int i = 0; i < 8; i++) {
        int32_t t = (int32_t)(uint16_t)acc[i] + (int32_t)op[i];
        if (t > 0xFFFF) {
            t = 0xFFFF;
            want_qc = 1;
        } else if (t < 0) {
            t = 0;
            want_qc = 1;
        }
        if (uo[i] != (uint16_t)t) ok = 0;
    }
    if (QC(status) != want_qc) ok = 0;
    return ok;
}

// ---- three-same FP16, which the copy class was swallowing and running as INS ---------------------
// Spelled with .inst so the fixture needs no +fp16 in its arch string: FADD v0.8h, v1.8h, v2.8h and
// FCMGE v0.4h, v1.4h, v2.4h. Half-precision values are built as bit patterns for the same reason.
static int fp16_three_same(uint64_t *seen) {
    uint16_t lhs[8] = {0x3C00, 0x4000, 0x4200, 0x4400, 0xBC00, 0x7BFF, 0x0001, 0x0000};
    uint16_t rhs[8] = {0x3C00, 0x3C00, 0x4000, 0xC000, 0x3C00, 0x3C00, 0x0001, 0x3C00};
    uint16_t got[8];
    escape(lhs);
    escape(rhs);
    uint16x8_t a = vld1q_u16(lhs), b = vld1q_u16(rhs), r = vdupq_n_u16(0);
    __asm__ __volatile__("mov v1.16b, %1.16b\n\tmov v2.16b, %2.16b\n\t"
                         ".inst 0x4e421420\n\t" // fadd v0.8h, v1.8h, v2.8h
                         "mov %0.16b, v0.16b"
                         : "=w"(r)
                         : "w"(a), "w"(b)
                         : "v0", "v1", "v2");
    vst1q_u16(got, r);
    // 1+1=2, 2+1=3, 3+2=5, 4+(-2)=2, -1+1=0, 65504+1=65504, denorm+denorm, 0+1=1.
    static const uint16_t want[8] = {0x4000, 0x4200, 0x4500, 0x4000, 0x0000, 0x7BFF, 0x0002, 0x3C00};
    int ok = 1;
    for (int i = 0; i < 8; i++)
        if (got[i] != want[i]) ok = 0;
    *seen = ((uint64_t)got[0] << 16) | got[2];

    __asm__ __volatile__("mov v1.16b, %1.16b\n\tmov v2.16b, %2.16b\n\t"
                         ".inst 0x2e422420\n\t" // fcmge v0.4h, v1.4h, v2.4h
                         "mov %0.16b, v0.16b"
                         : "=w"(r)
                         : "w"(a), "w"(b)
                         : "v0", "v1", "v2");
    vst1q_u16(got, r);
    static const uint16_t cmp[8] = {0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0, 0, 0, 0};
    for (int i = 0; i < 8; i++)
        if (got[i] != cmp[i]) ok = 0;
    return ok;
}

int main(void) {
    uint64_t dup_bits = 0, rshrn_seen = 0, fp16_bits = 0;
    uint32_t cmp_mask = 0;
    int64_t clsum = 0;
    int d = dup_scalar(&dup_bits);
    int mi = modified_immediate();
    int qs = qshl_64();
    int rc = rshrn_carry(&rshrn_seen);
    int ci = compare_invalid(&cmp_mask);
    int cf = convert_flags();
    int mt = misc_twoop(&clsum);
    int fh = fp16_three_same(&fp16_bits);
    printf("neon-misc dup=%d movi=%d uqshl=%d rshrn=%d cmpinv=%d cvt=%d misc=%d fp16=%d\n", d, mi, qs, rc, ci, cf, mt,
           fh);
    printf("neon-misc bits dup=%llx rshrn=%llx cmp=%x cls=%lld fp16=%llx\n", (unsigned long long)dup_bits,
           (unsigned long long)rshrn_seen, cmp_mask, (long long)clsum, (unsigned long long)fp16_bits);
    return 0;
}
