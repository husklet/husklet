// FRECPE / FRSQRTE / FRECPS / FRSQRTS / FRECPX / URECPE / URSQRTE -- 535 baseline Armv8.0 encodings the
// interpreter used to report rather than execute -- plus FCVTXN/FCVTXN2's round-to-odd narrowing.
//
// x86's RCPPS is an implementation-defined approximation, and this engine answers it exactly. AArch64's
// estimates are the opposite: the ARM ARM specifies an 8-bit-mantissa TABLE and the exact extraction, so
// two conforming implementations agree bit for bit and a close approximation is a wrong answer. Every case
// here is checked against a C transcription of the ARM ARM pseudocode -- RecipEstimate, RecipSqrtEstimate,
// FPRecipEstimate, FPRSqrtEstimate, FPRecpX, FPRecipStepFused, FPRSqrtStepFused, UnsignedRecipEstimate,
// UnsignedRSqrtEstimate -- so on an aarch64 host this is a differential against silicon, not a golden.
// tables() walks the COMPLETE domain of both tables: 256 + 384 entries, nothing sampled.
//
// Two traps this file exists to avoid re-learning:
//   * GCC constant-folds NEON at -O2; escape() on every seed buffer keeps the instruction in the binary.
//   * FPSR must be read in the SAME asm block as the instruction, or the scheduler hoists the operation
//     out of the window and the flag assertions pass for the wrong reason.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static void escape(const void *p) {
    __asm__ __volatile__("" ::"r"(p) : "memory");
}

typedef struct {
    uint64_t lo, hi;
} q_t;

static q_t in, in2, out;
static uint64_t fpcr, status;

// One instruction with FPCR installed and FPSR cleared before it and read after it, all in the same block.
// Operands arrive through memory so nothing folds; v0 is preloaded so FCVTXN2 sees its own destination.
// FPCR goes back to zero inside the block, so a mode cannot leak into the C between cases.
#define RUN1(text)                                                                                                     \
    __asm__ __volatile__("msr fpcr, %3\n\tmsr fpsr, xzr\n\tldr q1, [%1]\n\tldr q0, [%2]\n\t" text                       \
                         "\n\tmrs %0, fpsr\n\tstr q0, [%2]\n\tmsr fpcr, xzr"                                           \
                         : "=&r"(status)                                                                               \
                         : "r"(&in), "r"(&out), "r"(fpcr)                                                              \
                         : "v0", "v1", "memory")
#define RUN2(text)                                                                                                     \
    __asm__ __volatile__("msr fpcr, %4\n\tmsr fpsr, xzr\n\tldr q1, [%1]\n\tldr q2, [%2]\n\tldr q0, [%3]\n\t" text       \
                         "\n\tmrs %0, fpsr\n\tstr q0, [%3]\n\tmsr fpcr, xzr"                                           \
                         : "=&r"(status)                                                                               \
                         : "r"(&in), "r"(&in2), "r"(&out), "r"(fpcr)                                                   \
                         : "v0", "v1", "v2", "memory")

// ---- ARM ARM shared pseudocode ------------------------------------------------------------------
// RecipEstimate(a): a is 256..511 for 0.5 <= x < 1.0; result 256..511 for 1.0 <= 1/x <= 511/256.
static unsigned RecipEstimate(unsigned a) {
    a = a * 2u + 1u; // to nearest, in units of 1/512
    unsigned b = (1u << 19) / a;
    return (b + 1u) / 2u; // to nearest
}

// RecipSqrtEstimate(a): 128..255 covers 0.25 <= x < 0.5, 256..511 covers 0.5 <= x < 1.0.
static unsigned RecipSqrtEstimate(unsigned a) {
    a = a < 256u ? a * 2u + 1u : (((a >> 1) << 1) + 1u) * 2u;
    uint64_t b = 512;
    while ((uint64_t)a * (b + 1u) * (b + 1u) < (UINT64_C(1) << 28)) b++;
    return (unsigned)((b + 1u) / 2u);
}

#define FMT_H 0
#define FMT_S 1
#define FMT_D 2
#define MANT(f) ((f) == FMT_H ? 10u : (f) == FMT_S ? 23u : 52u)
#define BIAS(f) ((f) == FMT_H ? 15 : (f) == FMT_S ? 127 : 1023)
#define INFEXP(f) ((f) == FMT_H ? 0x1Fu : (f) == FMT_S ? 0xFFu : 0x7FFu)
#define FRACM(f) ((UINT64_C(1) << MANT(f)) - 1u)
#define SIGNM(f) (UINT64_C(1) << ((f) == FMT_H ? 15u : (f) == FMT_S ? 31u : 63u))
#define FRAC52 ((UINT64_C(1) << 52) - 1u)
#define IOC 0x01u
#define DZC 0x02u
#define OFC 0x04u
#define UFC 0x08u
#define IXC 0x10u
#define IDC 0x80u
#define FPSR_MASK 0x9Fu
#define RMODE(c) (((c) >> 22) & 3u)
#define FZ(c) (((c) >> 24) & 1u)
#define DN(c) (((c) >> 25) & 1u)
#define FZ16(c) (((c) >> 19) & 1u)
#define RN 0u
#define RP 1u
#define RM 2u
#define RZ 3u

// 0 zero, 1 denormal, 2 normal, 3 infinity, 4 quiet NaN, 5 signalling NaN.
static unsigned cls(uint64_t b, int f) {
    unsigned e = (unsigned)((b >> MANT(f)) & INFEXP(f));
    uint64_t frac = b & FRACM(f);
    if (e == 0) return frac ? 1u : 0u;
    if (e != INFEXP(f)) return 2u;
    if (!frac) return 3u;
    return ((frac >> (MANT(f) - 1u)) & 1u) ? 4u : 5u;
}

static uint64_t default_nan(int f) {
    return ((uint64_t)INFEXP(f) << MANT(f)) | (UINT64_C(1) << (MANT(f) - 1u));
}

// FPProcessNaN: quiet it, or substitute the default NaN under FPCR.DN; a signalling NaN raises Invalid.
static uint64_t process_nan(uint64_t b, int f, unsigned *fpsr) {
    if (cls(b, f) == 5u) *fpsr |= IOC;
    if (DN(fpcr)) return default_nan(f);
    return b | (UINT64_C(1) << (MANT(f) - 1u));
}

// FPUnpack's flush-to-zero. FZ governs single and double, FZ16 half -- and a half flush is SILENT: the
// N == 16 branch carries no InputDenorm exception, so FPSR.IDC stays clear where FZ would have set it.
static uint64_t flush_in(uint64_t b, int f, unsigned *fpsr) {
    unsigned on = f == FMT_H ? FZ16(fpcr) : FZ(fpcr);
    if (!on || cls(b, f) != 1u) return b;
    if (f != FMT_H) *fpsr |= IDC;
    return b & SIGNM(f);
}

// FPRecipEstimate.
static uint64_t model_recpe(uint64_t a, int f, unsigned *fpsr) {
    a = flush_in(a, f, fpsr);
    unsigned c = cls(a, f), mant = MANT(f), inf = INFEXP(f);
    int bias = BIAS(f);
    uint64_t sign = a & SIGNM(f);
    if (c >= 4u) return process_nan(a, f, fpsr);
    if (c == 3u) return sign;
    if (c == 0u) {
        *fpsr |= DZC;
        return sign | ((uint64_t)inf << mant);
    }
    uint64_t frac = a & FRACM(f);
    int exp = (int)((a >> mant) & inf);
    if (exp == 0 && (frac >> (mant - 2u)) == 0) { // |x| < 2^-(bias+1): the reciprocal overflows
        unsigned r = RMODE(fpcr);
        int to_inf = r == RN || (r == RP && !sign) || (r == RM && sign);
        *fpsr |= OFC | IXC;
        return sign | (to_inf ? ((uint64_t)inf << mant) : (((uint64_t)(inf - 1u) << mant) | FRACM(f)));
    }
    if ((f == FMT_H ? FZ16(fpcr) : FZ(fpcr)) && exp >= 2 * bias - 1) { // the reciprocal would be denormal
        *fpsr |= UFC;
        return sign;
    }
    uint64_t fr = frac << (52u - mant);
    if (exp == 0) { // at most two shifts: a third would have been the overflow case above
        if ((fr >> 51) == 0) {
            exp = -1;
            fr = (fr << 2) & FRAC52;
        } else {
            fr = (fr << 1) & FRAC52;
        }
    }
    unsigned est = RecipEstimate(256u + (unsigned)((fr >> 44) & 0xFFu));
    int rexp = 2 * bias - 1 - exp;
    fr = (uint64_t)(est & 0xFFu) << 44;
    if (rexp == 0) { // pushed out of the normal range: the leading one becomes explicit
        fr = (UINT64_C(1) << 51) | (fr >> 1);
    } else if (rexp == -1) {
        fr = (UINT64_C(1) << 50) | (fr >> 2);
        rexp = 0;
    }
    return sign | ((uint64_t)(unsigned)rexp << mant) | (fr >> (52u - mant));
}

// FPRSqrtEstimate.
static uint64_t model_rsqrte(uint64_t a, int f, unsigned *fpsr) {
    a = flush_in(a, f, fpsr);
    unsigned c = cls(a, f), mant = MANT(f), inf = INFEXP(f);
    uint64_t sign = a & SIGNM(f);
    if (c >= 4u) return process_nan(a, f, fpsr);
    if (c == 0u) {
        *fpsr |= DZC;
        return sign | ((uint64_t)inf << mant);
    }
    if (sign) { // -0 returned -inf above; every other negative is Invalid, not a signed result
        *fpsr |= IOC;
        return default_nan(f);
    }
    if (c == 3u) return 0;
    uint64_t fr = (a & FRACM(f)) << (52u - mant);
    int exp = (int)((a >> mant) & inf);
    if (exp == 0) { // a denormal always has a set bit to normalise onto
        while ((fr >> 51) == 0) {
            fr = (fr << 1) & FRAC52;
            exp--;
        }
        fr = (fr << 1) & FRAC52;
    }
    // A square root halves the exponent, so its PARITY survives: an odd one scales into 0.25..0.5 and
    // indexes the table's lower half.
    unsigned scaled =
        ((unsigned)exp & 1u) ? 128u + (unsigned)((fr >> 45) & 0x7Fu) : 256u + (unsigned)((fr >> 44) & 0xFFu);
    int rexp = (3 * BIAS(f) - 1 - exp) / 2;
    return ((uint64_t)(unsigned)rexp << mant) | ((uint64_t)(RecipSqrtEstimate(scaled) & 0xFFu) << (mant - 8u));
}

// FPRecpX: not an estimate at all. The exponent is reflected exactly and the mantissa forced to zero, so
// a range reduction is invertible. FZ never changes the answer -- the exponent comes from the raw
// operand -- but flushing a denormal input still reports it at single and double.
static uint64_t model_recpx(uint64_t a, int f, unsigned *fpsr) {
    unsigned mant = MANT(f), inf = INFEXP(f);
    if (cls(a, f) >= 4u) return process_nan(a, f, fpsr);
    (void)flush_in(a, f, fpsr);
    unsigned e = (unsigned)((a >> mant) & inf);
    return (a & SIGNM(f)) | ((uint64_t)(e == 0 ? inf - 1u : (~e & inf)) << mant);
}

// UnsignedRecipEstimate / UnsignedRSqrtEstimate: the same tables read as unsigned Q0.32 fixed point.
// No FPCR, no FPSR, and everything at or below 0.5 (0.25 for the square root) saturates.
static uint32_t model_uest(uint32_t v, int sqrt_form) {
    if (sqrt_form ? (v >> 30) == 0 : (v >> 31) == 0) return 0xFFFFFFFFu;
    return (sqrt_form ? RecipSqrtEstimate(v >> 23) : RecipEstimate(v >> 23)) << 23;
}

// FPRecipStepFused / FPRSqrtStepFused, special cases only -- returns 0 when the arithmetic decides.
// Three things here do not follow from 2 - a*b or (3 - a*b)/2: op1 is NEGATED before the unpack, so a NaN
// operand propagates with its sign FLIPPED; 0*inf yields exactly 2.0 or 1.5 rather than the Invalid a bare
// multiply would raise; and inf*finite yields a signed infinity outright.
static int model_step_special(uint64_t a, uint64_t b, int f, int sqrt_form, unsigned *fpsr, uint64_t *res) {
    a ^= SIGNM(f);
    a = flush_in(a, f, fpsr);
    b = flush_in(b, f, fpsr);
    unsigned ca = cls(a, f), cb = cls(b, f);
    if (ca == 5u || cb == 5u) { // FPProcessNaNs: signalling first, op1 before op2
        *res = process_nan(ca == 5u ? a : b, f, fpsr);
        return 1;
    }
    if (ca == 4u || cb == 4u) {
        *res = process_nan(ca == 4u ? a : b, f, fpsr);
        return 1;
    }
    if ((ca == 3u && cb == 0u) || (ca == 0u && cb == 3u)) {
        *res = sqrt_form ? (((uint64_t)BIAS(f) << MANT(f)) | (UINT64_C(1) << (MANT(f) - 1u)))  // 1.5
                         : ((uint64_t)(BIAS(f) + 1) << MANT(f));                               // 2.0
        return 1;
    }
    if (ca == 3u || cb == 3u) {
        *res = ((a ^ b) & SIGNM(f)) | ((uint64_t)INFEXP(f) << MANT(f));
        return 1;
    }
    return 0;
}

// ---- bookkeeping --------------------------------------------------------------------------------
static unsigned long checks, fails;
static uint64_t digest;

static void note(int ok, uint64_t got, uint64_t st) {
    checks++;
    if (!ok) fails++;
    digest = (digest ^ got ^ (st << 40)) * UINT64_C(0x100000001B3);
}

// ---- inputs -------------------------------------------------------------------------------------
static const uint32_t SIN[] = {0x00000000, 0x80000000, 0x7F800000, 0xFF800000, 0x7FC00000, 0xFFC00000,
                               0x7F800001, 0x7FFFFFFF, 0x00800000, 0x007FFFFF, 0x00000001, 0x00400000,
                               0x00200000, 0x001FFFFF, 0x7F7FFFFF, 0xFF7FFFFF, 0x3F800000, 0xBF800000,
                               0x40000000, 0x7E800000, 0x7E7FFFFF, 0x01000000, 0x3F000000, 0xC0490FDB};
static const uint64_t DIN[] = {
    UINT64_C(0x0000000000000000), UINT64_C(0x8000000000000000), UINT64_C(0x7FF0000000000000),
    UINT64_C(0xFFF0000000000000), UINT64_C(0x7FF8000000000000), UINT64_C(0xFFF8000000000000),
    UINT64_C(0x7FF0000000000001), UINT64_C(0x0010000000000000), UINT64_C(0x000FFFFFFFFFFFFF),
    UINT64_C(0x0000000000000001), UINT64_C(0x0008000000000000), UINT64_C(0x0003FFFFFFFFFFFF),
    UINT64_C(0x7FEFFFFFFFFFFFFF), UINT64_C(0x3FF0000000000000), UINT64_C(0xBFF0000000000000),
    UINT64_C(0x4000000000000000), UINT64_C(0x7FD0000000000000), UINT64_C(0x3FE0000000000000)};
static const uint16_t HIN[] = {0x0000, 0x8000, 0x7C00, 0xFC00, 0x7E00, 0xFE00, 0x7C01, 0x0400, 0x03FF, 0x0001,
                               0x0200, 0x00FF, 0x7BFF, 0x3C00, 0xBC00, 0x4000, 0x6800, 0x67FF, 0x3800, 0xC248};
static const uint32_t UIN[] = {0x00000000, 0x00000001, 0x3FFFFFFF, 0x40000000, 0x7FFFFFFF,
                               0x80000000, 0xFFFFFFFF, 0xC0000000, 0xAAAAAAAA, 0x90000000};
static const uint64_t FPCRS[] = {0,
                                 (uint64_t)RP << 22,
                                 (uint64_t)RM << 22,
                                 (uint64_t)RZ << 22,
                                 UINT64_C(1) << 24,
                                 UINT64_C(1) << 25,
                                 UINT64_C(1) << 19,
                                 (UINT64_C(1) << 24) | (UINT64_C(1) << 25) | (UINT64_C(1) << 19) |
                                     ((uint64_t)RZ << 22)};
#define NS (sizeof SIN / sizeof *SIN)
#define ND (sizeof DIN / sizeof *DIN)
#define NH (sizeof HIN / sizeof *HIN)
#define NU (sizeof UIN / sizeof *UIN)
#define NC (sizeof FPCRS / sizeof *FPCRS)

// Load the four/two/eight lanes of `in` from `set` starting at `i`, so each lane holds a DIFFERENT value
// and a lane-placement error cannot hide behind a broadcast.
static void fill(const void *set, unsigned n, unsigned i, unsigned width, q_t *dst) {
    unsigned per = 16u / width;
    memset(dst, 0, sizeof *dst);
    for (unsigned l = 0; l < per; l++)
        memcpy((unsigned char *)dst + l * width, (const unsigned char *)set + ((i + l) % n) * width, width);
}

// A scalar write must zero [127:esize]; `width` is a parameter so the 64-bit case is not a shift by 64.
static int upper_clear(const q_t *v, unsigned width) {
    if (v->hi) return 0;
    return width >= 8 ? 1 : (v->lo >> (8u * width)) == 0;
}

static uint64_t lane(const q_t *v, unsigned width, unsigned l) {
    uint64_t x = 0;
    memcpy(&x, (const unsigned char *)v + l * width, width);
    return x;
}

// ---- RecipEstimate / RecipSqrtEstimate: every entry of both tables ------------------------------
static int tables(void) {
    int ok = 1;
    for (unsigned k = 0; k < 256; k++) {
        fpcr = 0;
        uint32_t w = (127u << 23) | (k << 15); // 1.0 <= x < 2.0, so the estimate lands on a normal
        in.lo = ((uint64_t)w << 32) | w;
        in.hi = in.lo;
        escape(&in);
        out.lo = out.hi = 0;
        RUN1("frecpe v0.4s, v1.4s");
        if (256u + (unsigned)((out.lo >> 15) & 0xFFu) != RecipEstimate(256u + k)) ok = 0;
        note(ok, out.lo, status);

        w = (126u << 23) | (k << 15); // an EVEN exponent selects the table's upper half
        in.lo = ((uint64_t)w << 32) | w;
        in.hi = in.lo;
        escape(&in);
        RUN1("frsqrte v0.4s, v1.4s");
        if (256u + (unsigned)((out.lo >> 15) & 0xFFu) != RecipSqrtEstimate(256u + k)) ok = 0;
        note(ok, out.lo, status);
    }
    for (unsigned j = 0; j < 128; j++) {
        fpcr = 0;
        uint32_t w = (127u << 23) | (j << 16); // an ODD exponent selects the lower half
        in.lo = ((uint64_t)w << 32) | w;
        in.hi = in.lo;
        escape(&in);
        out.lo = out.hi = 0;
        RUN1("frsqrte v0.4s, v1.4s");
        if (256u + (unsigned)((out.lo >> 15) & 0xFFu) != RecipSqrtEstimate(128u + j)) ok = 0;
        note(ok, out.lo, status);
    }
    return ok;
}

// ---- FRECPE / FRSQRTE / FRECPX ------------------------------------------------------------------
#define EST_FORM(text, set, n, width, fmt, modelfn)                                                                    \
    do {                                                                                                               \
        unsigned per = 16u / (width);                                                                                  \
        for (unsigned c = 0; c < NC; c++)                                                                              \
            for (unsigned i = 0; i < (n); i++) {                                                                       \
                fpcr = FPCRS[c];                                                                                       \
                fill((set), (n), i, (width), &in);                                                                     \
                escape(&in);                                                                                           \
                out.lo = out.hi = 0;                                                                                   \
                RUN1(text);                                                                                            \
                unsigned want_fpsr = 0;                                                                                \
                int good = 1;                                                                                          \
                for (unsigned l = 0; l < per; l++) {                                                                   \
                    uint64_t w = modelfn(lane(&in, (width), l), (fmt), &want_fpsr);                                    \
                    if (lane(&out, (width), l) != w) good = 0;                                                         \
                }                                                                                                      \
                if ((status & FPSR_MASK) != want_fpsr) good = 0;                                                        \
                if (!good) ok = 0;                                                                                     \
                note(good, out.lo ^ out.hi, status);                                                                   \
            }                                                                                                          \
    } while (0)

// The scalar spellings run one lane and must ZERO [127:esize]; a vector-shaped write would leave the
// upper lanes of the seed behind.
#define EST_SCALAR(text, set, n, width, fmt, modelfn)                                                                  \
    do {                                                                                                               \
        for (unsigned c = 0; c < NC; c++)                                                                              \
            for (unsigned i = 0; i < (n); i++) {                                                                       \
                fpcr = FPCRS[c];                                                                                       \
                fill((set), (n), i, (width), &in);                                                                     \
                escape(&in);                                                                                           \
                out.lo = UINT64_C(0xA5A5A5A5A5A5A5A5);                                                                 \
                out.hi = UINT64_C(0x5A5A5A5A5A5A5A5A);                                                                 \
                RUN1(text);                                                                                            \
                unsigned want_fpsr = 0;                                                                                \
                uint64_t w = modelfn(lane(&in, (width), 0), (fmt), &want_fpsr);                                        \
                int good = lane(&out, (width), 0) == w && upper_clear(&out, (width)) &&                                \
                           (status & FPSR_MASK) == want_fpsr;                                                          \
                if (!good) ok = 0;                                                                                     \
                note(good, out.lo ^ out.hi, status);                                                                   \
            }                                                                                                          \
    } while (0)

static int estimates(void) {
    int ok = 1;
    EST_FORM("frecpe v0.4s, v1.4s", SIN, NS, 4, FMT_S, model_recpe);
    EST_FORM("frecpe v0.2d, v1.2d", DIN, ND, 8, FMT_D, model_recpe);
    EST_FORM(".inst 0x4ef9d820", HIN, NH, 2, FMT_H, model_recpe); // frecpe v0.8h, v1.8h
    EST_FORM("frsqrte v0.4s, v1.4s", SIN, NS, 4, FMT_S, model_rsqrte);
    EST_FORM("frsqrte v0.2d, v1.2d", DIN, ND, 8, FMT_D, model_rsqrte);
    EST_FORM(".inst 0x6ef9d820", HIN, NH, 2, FMT_H, model_rsqrte); // frsqrte v0.8h, v1.8h
    EST_SCALAR("frecpe s0, s1", SIN, NS, 4, FMT_S, model_recpe);
    EST_SCALAR("frecpe d0, d1", DIN, ND, 8, FMT_D, model_recpe);
    EST_SCALAR(".inst 0x5ef9d820", HIN, NH, 2, FMT_H, model_recpe); // frecpe h0, h1
    EST_SCALAR("frsqrte s0, s1", SIN, NS, 4, FMT_S, model_rsqrte);
    EST_SCALAR("frsqrte d0, d1", DIN, ND, 8, FMT_D, model_rsqrte);
    EST_SCALAR(".inst 0x7ef9d820", HIN, NH, 2, FMT_H, model_rsqrte); // frsqrte h0, h1
    EST_SCALAR("frecpx s0, s1", SIN, NS, 4, FMT_S, model_recpx);
    EST_SCALAR("frecpx d0, d1", DIN, ND, 8, FMT_D, model_recpx);
    EST_SCALAR(".inst 0x5ef9f820", HIN, NH, 2, FMT_H, model_recpx); // frecpx h0, h1
    return ok;
}

// ---- URECPE / URSQRTE ---------------------------------------------------------------------------
static int uint_estimates(void) {
    int ok = 1;
    for (unsigned i = 0; i < NU; i++) {
        fpcr = 0;
        fill(UIN, NU, i, 4, &in);
        escape(&in);
        out.lo = out.hi = 0;
        RUN1(".inst 0x4ea1c820"); // urecpe v0.4s, v1.4s
        int good = (status & FPSR_MASK) == 0;
        for (unsigned l = 0; l < 4; l++)
            if ((uint32_t)lane(&out, 4, l) != model_uest((uint32_t)lane(&in, 4, l), 0)) good = 0;
        if (!good) ok = 0;
        note(good, out.lo ^ out.hi, status);

        out.lo = out.hi = 0;
        RUN1(".inst 0x6ea1c820"); // ursqrte v0.4s, v1.4s
        good = (status & FPSR_MASK) == 0;
        for (unsigned l = 0; l < 4; l++)
            if ((uint32_t)lane(&out, 4, l) != model_uest((uint32_t)lane(&in, 4, l), 1)) good = 0;
        if (!good) ok = 0;
        note(good, out.lo ^ out.hi, status);
    }
    return ok;
}

// ---- FRECPS / FRSQRTS ---------------------------------------------------------------------------
// The special cases come straight from the pseudocode. The arithmetic case is checked against FMADD,
// which is the same fused single rounding the pseudocode calls for: 2 + (-a)*b, and for the square-root
// step 1.5 + (-a/2)*b, halving a by decrementing its exponent -- exact, and so restricted to the inputs
// where that decrement cannot leave the normal range. An implementation that rounded twice, or that used
// a non-fused multiply-then-add, disagrees here.
#define STEP_FORM(text, fmadd, set, n, width, fmt, sqrt_form, scalar)                                                  \
    do {                                                                                                               \
        unsigned per = (scalar) ? 1u : 16u / (width);                                                                  \
        for (unsigned c = 0; c < NC; c++)                                                                              \
            for (unsigned i = 0; i < (n); i++)                                                                         \
                for (unsigned j = 1; j < (n); j += 3) {                                                                \
                    fpcr = FPCRS[c];                                                                                   \
                    fill((set), (n), i, (width), &in);                                                                 \
                    fill((set), (n), (i + j) % (n), (width), &in2);                                                    \
                    escape(&in);                                                                                       \
                    escape(&in2);                                                                                      \
                    out.lo = UINT64_C(0xA5A5A5A5A5A5A5A5);                                                             \
                    out.hi = UINT64_C(0x5A5A5A5A5A5A5A5A);                                                             \
                    RUN2(text);                                                                                        \
                    q_t got = out;                                                                                     \
                    uint64_t got_status = status;                                                                      \
                    unsigned want_fpsr = 0;                                                                            \
                    int all_special = 1, good = !(scalar) || upper_clear(&got, (width));                               \
                    for (unsigned l = 0; l < per; l++) {                                                               \
                        uint64_t res;                                                                                  \
                        if (model_step_special(lane(&in, (width), l), lane(&in2, (width), l), (fmt), (sqrt_form),      \
                                               &want_fpsr, &res)) {                                                    \
                            if (lane(&got, (width), l) != res) good = 0;                                               \
                        } else {                                                                                       \
                            all_special = 0;                                                                           \
                        }                                                                                              \
                    }                                                                                                  \
                    if (all_special && (got_status & FPSR_MASK) != want_fpsr) good = 0;                                \
                    if (!all_special) good &= step_fmadd_agrees(fmadd, (width), (fmt), (sqrt_form), per, &got);         \
                    if (!good) ok = 0;                                                                                 \
                    note(good, got.lo ^ got.hi, got_status);                                                           \
                }                                                                                                      \
    } while (0)

// Rebuild the step with FMADD over the same operands and compare lane by lane, skipping the lanes the
// special cases own and the ones where halving `a` would leave the normal range.
static int step_fmadd_agrees(int which, unsigned width, int fmt, int sqrt_form, unsigned check, const q_t *got) {
    q_t alt = in, konst, res;
    unsigned per = 16u / width;
    int usable[8], any = 0;
    for (unsigned l = 0; l < per; l++) {
        uint64_t a = lane(&in, width, l), b = lane(&in2, width, l);
        unsigned ca = cls(a, fmt), cb = cls(b, fmt);
        uint64_t neg = a ^ SIGNM(fmt);
        usable[l] = ca == 2u && cb >= 1u && cb <= 2u;
        if (sqrt_form) {
            if (((neg >> MANT(fmt)) & INFEXP(fmt)) < 2u) usable[l] = 0;
            neg -= UINT64_C(1) << MANT(fmt); // exact halving
        }
        if (usable[l]) any = 1;
        memcpy((unsigned char *)&alt + l * width, &neg, width);
        uint64_t k = sqrt_form ? (((uint64_t)BIAS(fmt) << MANT(fmt)) | (UINT64_C(1) << (MANT(fmt) - 1u)))
                               : ((uint64_t)(BIAS(fmt) + 1) << MANT(fmt));
        memcpy((unsigned char *)&konst + l * width, &k, width);
    }
    if (!any) return 1;
    escape(&alt);
    escape(&konst);
    res = konst;
    uint64_t st;
    // fmla accumulates into v0, which already holds the constant: v0 += alt * in2.
    switch (which) {
    case 0:
        __asm__ __volatile__("msr fpcr, %4\n\tmsr fpsr, xzr\n\tldr q1,[%1]\n\tldr q2,[%2]\n\tldr q0,[%3]\n\t"
                             "fmla v0.4s, v1.4s, v2.4s\n\tmrs %0, fpsr\n\tstr q0,[%3]\n\tmsr fpcr, xzr"
                             : "=&r"(st) : "r"(&alt), "r"(&in2), "r"(&res), "r"(fpcr) : "v0", "v1", "v2", "memory");
        break;
    case 1:
        __asm__ __volatile__("msr fpcr, %4\n\tmsr fpsr, xzr\n\tldr q1,[%1]\n\tldr q2,[%2]\n\tldr q0,[%3]\n\t"
                             "fmla v0.2d, v1.2d, v2.2d\n\tmrs %0, fpsr\n\tstr q0,[%3]\n\tmsr fpcr, xzr"
                             : "=&r"(st) : "r"(&alt), "r"(&in2), "r"(&res), "r"(fpcr) : "v0", "v1", "v2", "memory");
        break;
    default:
        __asm__ __volatile__("msr fpcr, %4\n\tmsr fpsr, xzr\n\tldr q1,[%1]\n\tldr q2,[%2]\n\tldr q0,[%3]\n\t"
                             ".inst 0x4e420c20\n\tmrs %0, fpsr\n\tstr q0,[%3]\n\tmsr fpcr, xzr" // fmla v0.8h,v1.8h,v2.8h
                             : "=&r"(st) : "r"(&alt), "r"(&in2), "r"(&res), "r"(fpcr) : "v0", "v1", "v2", "memory");
        break;
    }
    (void)st;
    for (unsigned l = 0; l < check; l++)
        if (usable[l] && lane(got, width, l) != lane(&res, width, l)) return 0;
    return 1;
}

static int steps(void) {
    int ok = 1;
    STEP_FORM("frecps v0.4s, v1.4s, v2.4s", 0, SIN, NS, 4, FMT_S, 0, 0);
    STEP_FORM("frecps v0.2d, v1.2d, v2.2d", 1, DIN, ND, 8, FMT_D, 0, 0);
    STEP_FORM(".inst 0x4e423c20", 2, HIN, NH, 2, FMT_H, 0, 0); // frecps v0.8h, v1.8h, v2.8h
    STEP_FORM("frsqrts v0.4s, v1.4s, v2.4s", 0, SIN, NS, 4, FMT_S, 1, 0);
    STEP_FORM("frsqrts v0.2d, v1.2d, v2.2d", 1, DIN, ND, 8, FMT_D, 1, 0);
    STEP_FORM(".inst 0x4ec23c20", 2, HIN, NH, 2, FMT_H, 1, 0); // frsqrts v0.8h, v1.8h, v2.8h
    STEP_FORM("frecps s0, s1, s2", 0, SIN, NS, 4, FMT_S, 0, 1);
    STEP_FORM("frecps d0, d1, d2", 1, DIN, ND, 8, FMT_D, 0, 1);
    STEP_FORM(".inst 0x5e423c20", 2, HIN, NH, 2, FMT_H, 0, 1); // frecps h0, h1, h2
    STEP_FORM("frsqrts s0, s1, s2", 0, SIN, NS, 4, FMT_S, 1, 1);
    STEP_FORM("frsqrts d0, d1, d2", 1, DIN, ND, 8, FMT_D, 1, 1);
    STEP_FORM(".inst 0x5ec23c20", 2, HIN, NH, 2, FMT_H, 1, 1); // frsqrts h0, h1, h2
    return ok;
}

// ---- FCVTXN / FCVTXN2 ---------------------------------------------------------------------------
// FPRounding_ODD: round toward zero, then force the low bit whenever anything was lost, so a value that
// is later narrowed again cannot be rounded the wrong way by the intermediate. Overflow therefore lands
// on the largest finite, never on infinity. Expressed here as the pseudocode spells it -- FCVT under
// RZ plus the forced bit -- so a FCVTXN that used round-to-nearest, or dropped the bit, disagrees.
static int cvtxn(void) {
    int ok = 1;
    for (unsigned c = 0; c < NC; c++)
        for (unsigned i = 0; i < ND; i++) {
            fpcr = FPCRS[c];
            fill(DIN, ND, i, 8, &in);
            escape(&in);
            out.lo = out.hi = 0;
            RUN1(".inst 0x2e616820"); // fcvtxn v0.2s, v1.2d
            q_t got = out;
            uint64_t got_status = status;

            // Reference: the same narrowing under RZ, with the low bit forced when Inexact was reported.
            uint64_t ref_fpcr = (fpcr & ~(UINT64_C(3) << 22)) | ((uint64_t)RZ << 22), want_lo = 0, want_st = 0;
            for (unsigned l = 0; l < 2; l++) {
                q_t one;
                memset(&one, 0, sizeof one);
                one.lo = lane(&in, 8, l);
                escape(&one);
                uint64_t st, r = 0;
                __asm__ __volatile__("msr fpcr, %3\n\tmsr fpsr, xzr\n\tldr d1, [%2]\n\tfcvt s0, d1\n\t"
                                     "mrs %0, fpsr\n\tfmov %w1, s0\n\tmsr fpcr, xzr"
                                     : "=&r"(st), "=&r"(r)
                                     : "r"(&one), "r"(ref_fpcr)
                                     : "v0", "v1", "memory");
                uint32_t v = (uint32_t)r;
                if (st & IXC) v |= 1u;
                want_lo |= (uint64_t)v << (32u * l);
                want_st |= st;
            }
            int good = got.lo == want_lo && got.hi == 0 && (got_status & FPSR_MASK) == (want_st & FPSR_MASK);
            if (!good) ok = 0;
            note(good, got.lo, got_status);

            // FCVTXN2 keeps the destination's low half and writes the high one.
            out.lo = UINT64_C(0x0123456789ABCDEF);
            out.hi = 0;
            RUN1(".inst 0x6e616820"); // fcvtxn2 v0.4s, v1.2d
            good = out.lo == UINT64_C(0x0123456789ABCDEF) && out.hi == want_lo;
            if (!good) ok = 0;
            note(good, out.hi, status);

            // The scalar spelling takes lane 0 alone and zeroes the rest.
            out.lo = UINT64_C(0xA5A5A5A5A5A5A5A5);
            out.hi = UINT64_C(0x5A5A5A5A5A5A5A5A);
            RUN1(".inst 0x7e616820"); // fcvtxn s0, d1
            good = out.lo == (want_lo & 0xFFFFFFFFu) && out.hi == 0;
            if (!good) ok = 0;
            note(good, out.lo, status);
        }
    return ok;
}

int main(void) {
    int t = tables();
    int e = estimates();
    int u = uint_estimates();
    int s = steps();
    int x = cvtxn();
    printf("neon-recip tables=%d est=%d uint=%d step=%d cvtxn=%d\n", t, e, u, s, x);
    printf("neon-recip checks=%lu fails=%lu digest=%016llx\n", checks, fails, (unsigned long long)digest);
    return 0;
}
