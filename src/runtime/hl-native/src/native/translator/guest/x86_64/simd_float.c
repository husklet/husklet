#include "avx_internal.h"
#include "../../../host/cpu.h"

#include <fenv.h>
#include <math.h>
#include <string.h>
#if defined(HL_HOST_CPU_X86_64)
#include <xmmintrin.h>
#endif

static int nan32(uint32_t u) {
    return (u & 0x7f800000u) == 0x7f800000u && (u & 0x007fffffu) != 0;
}

static int nan64(uint64_t u) {
    return (u & 0x7ff0000000000000ull) == 0x7ff0000000000000ull && (u & 0x000fffffffffffffull) != 0;
}

// x86 add/sub/mul/div result NaN handling. Two distinct rules, both of which the host NEON FADD/FMUL/FSUB/
// FDIV that computed `r` gets WRONG, so we recompute from the operands here:
//   (1) INPUT NaN: plain SRC1 PRIORITY, quieted -- src1 if it is a NaN, else src2, with the mantissa MSB
//       set (payload + sign preserved). Not commutative, so HADD/HSUB's pairing order below is observable.
//       Measured on Zen 4 across all 64 ordered pairs of {+-SNaN min/max payload, +-QNaN min/max payload},
//       single and double, for ADD/SUB/MUL/DIV in SSE, SSE scalar, VEX, and the SSE3 horizontal/addsub
//       family: src1 wins every non-degenerate pair, on every one of them.
//       This used to implement softfloat's `float_2nan_prop_x87` (QNaN beats SNaN, then larger significand,
//       then positive) because qemu-x86_64 was taken as the oracle where it disagreed with the SDM. The
//       SDM was right and the oracle was modelling the x87 rule for SSE; that cost 107 golden lines.
//       ARM's rule is SNaN-first-else-src1, which agrees with x86 on 12 of 16 NaN pairs and diverges on
//       exactly (src1 QNaN, src2 SNaN) -- so the JIT's NaN-input gate into this code is still load-bearing.
//   (2) GENERATED NaN (result NaN, NO input NaN: 0/0, inf/inf, 0*inf, inf-inf): x86 yields the QNaN
//       floating-point INDEFINITE with the sign bit SET (0xFFC00000 / 0xFFF8000000000000); ARM yields the
//       positive default NaN. Same payload, opposite sign.
// MIN/MAX are NOT covered here: they return src2 VERBATIM (not even quieted) on any NaN, which is what the
// `x < y ? x : y` ternaries in avx_fp_arith_* already produce. Confirmed on the same sweep.
float avx_dnan_f32(float r, float x, float y) {
    uint32_t xb, yb;
    memcpy(&xb, &x, 4);
    memcpy(&yb, &y, 4);
    int xn = nan32(xb), yn = nan32(yb);
    if (xn || yn) {
        uint32_t w = (xn ? xb : yb) | 0x00400000u; // src1 priority, quieted
        memcpy(&r, &w, 4);
        return r;
    }
    if (r != r) {
        uint32_t u = 0xFFC00000u;
        memcpy(&r, &u, 4);
    }
    return r;
}

double avx_dnan_f64(double r, double x, double y) {
    uint64_t xb, yb;
    memcpy(&xb, &x, 8);
    memcpy(&yb, &y, 8);
    int xn = nan64(xb), yn = nan64(yb);
    if (xn || yn) {
        uint64_t w = (xn ? xb : yb) | 0x0008000000000000ull; // src1 priority, quieted
        memcpy(&r, &w, 8);
        return r;
    }
    if (r != r) {
        uint64_t u = 0xFFF8000000000000ull;
        memcpy(&r, &u, 8);
    }
    return r;
}

static void fma_raise_ie(void) {
    volatile double z = 0.0, q = z / z; // 0/0 raises #I and nothing else, on either host
    (void)q;
}

// x86 FMA, three-operand analogue of avx_dnan_*. Computes the SDM's a*b+c with `nmul`/`nadd`
// selecting the fmsub/fnmadd/fnmsub sign variants; a and b are the multiplicands, c the addend.
// Measured on Zen 4 over all 1728 triples of {+-SNaN,+-QNaN}x{min,max payload} + {1.0, +0, +-inf}
// for every form (132/213/231), every sign variant, both widths, scalar and packed:
//   (1) OPERAND NaN: the FIRST NaN in a*b+c ORDER wins -- a, then b, then c -- propagated VERBATIM
//       (sign and payload) with only the quiet bit forced. The addend never beats a multiplicand
//       and b never beats a, so the product/addend negation never touches the propagated sign. The
//       only flag raised is #I, and only for an SNaN operand: a NaN operand suppresses even the #D
//       of a denormal operand, and the #I that 0*inf would otherwise raise. So the host FMA must
//       NOT run here. aarch64 FMADD answers this case SNaN-first-then-ADDEND-first, and for 0*inf
//       with a QNaN addend the ARM ARM mandates DefaultNaN + IOC (measured on bare NEON under
//       qemu, so it is the ISA and not the emulator). An x86-64 host is no better: which
//       multiplicand lands in the encoding's first slot is the compiler's choice, and it chose the
//       wrong one for the float path -- 13008 of 43680 NaN triples were wrong on THIS host.
//   (2) GENERATED NaN (no NaN operand: 0*inf, or an inf-inf in the fused add): x86 yields the QNaN
//       indefinite with the sign SET, ARM the positive default NaN -- same payload, opposite sign.
// The negations are FNEG, not a multiply by -1: exact, and it cannot raise a flag of its own.
float fma_x86_f32(float a, float b, float c, int nmul, int nadd) {
    uint32_t ab, bb, cb;
    memcpy(&ab, &a, 4);
    memcpy(&bb, &b, 4);
    memcpy(&cb, &c, 4);
    if (nan32(ab) || nan32(bb) || nan32(cb)) {
        // #I follows ANY SNaN operand, not the winner: a QNaN a beside an SNaN b still raises it.
        if ((nan32(ab) && !(ab & 0x00400000u)) || (nan32(bb) && !(bb & 0x00400000u)) ||
            (nan32(cb) && !(cb & 0x00400000u)))
            fma_raise_ie();
        uint32_t w = (nan32(ab) ? ab : nan32(bb) ? bb : cb) | 0x00400000u;
        memcpy(&a, &w, 4);
        return a;
    }
    float r = __builtin_fmaf(nmul ? -a : a, b, nadd ? -c : c);
    if (r != r) {
        uint32_t u = 0xFFC00000u;
        memcpy(&r, &u, 4);
    }
    return r;
}

double fma_x86_f64(double a, double b, double c, int nmul, int nadd) {
    uint64_t ab, bb, cb;
    memcpy(&ab, &a, 8);
    memcpy(&bb, &b, 8);
    memcpy(&cb, &c, 8);
    if (nan64(ab) || nan64(bb) || nan64(cb)) {
        // #I follows ANY SNaN operand, not the winner: a QNaN a beside an SNaN b still raises it.
        if ((nan64(ab) && !(ab & 0x0008000000000000ull)) || (nan64(bb) && !(bb & 0x0008000000000000ull)) ||
            (nan64(cb) && !(cb & 0x0008000000000000ull)))
            fma_raise_ie();
        uint64_t w = (nan64(ab) ? ab : nan64(bb) ? bb : cb) | 0x0008000000000000ull;
        memcpy(&a, &w, 8);
        return a;
    }
    double r = __builtin_fma(nmul ? -a : a, b, nadd ? -c : c);
    if (r != r) {
        uint64_t u = 0xFFF8000000000000ull;
        memcpy(&r, &u, 8);
    }
    return r;
}

// Live guest rounding mode in the MXCSR.RC encoding {0=nearest,1=down,2=up,3=truncate}. The guest MXCSR IS
// the host control register on both hosts: MXCSR itself here, FPCR.RMode on aarch64 -- where ldmxcsr swaps
// the two directed modes on the way in, so this swaps them back.
static int cvt_host_rc(void) {
#if defined(HL_HOST_CPU_X86_64)
    return (int)((_mm_getcsr() >> 13) & 3u);
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long fpcr;
    __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr));
    unsigned m = (unsigned)((fpcr >> 22) & 3u);
    return m == 1 ? 2 : m == 2 ? 1 : (int)m;
#else
    switch (fegetround()) {
    case FE_DOWNWARD: return 1;
    case FE_UPWARD: return 2;
    case FE_TOWARDZERO: return 3;
    default: return 0;
    }
#endif
}

// Host sticky FP exception state, parked across the rounding step below. Volatile asm, not the _mm_*csr
// intrinsics or fenv: the intrinsics are ordinary function-like and can be reordered around the FP they
// are meant to bracket, and glibc's fesetexceptflag also writes the x87 status word, which is live guest
// state here.
unsigned cvt_fp_flags(void) {
#if defined(HL_HOST_CPU_X86_64)
    unsigned v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    return v & 0x3fu;
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpsr" : "=r"(f));
    return (unsigned)(f & 0x9ful); // IOC/DZC/OFC/UFC/IXC + IDC(7)
#else
    fexcept_t f = 0;
    fegetexceptflag(&f, FE_ALL_EXCEPT);
    return (unsigned)f;
#endif
}

void cvt_fp_flags_set(unsigned keep) {
#if defined(HL_HOST_CPU_X86_64)
    unsigned v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    v = (v & ~0x3fu) | keep;
    __asm__ volatile("ldmxcsr %0" : : "m"(v));
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpsr" : "=r"(f));
    f = (f & ~0x9ful) | keep;
    __asm__ volatile("msr fpsr, %0" : : "r"(f));
#else
    fexcept_t f = (fexcept_t)keep;
    fesetexceptflag(&f, FE_ALL_EXCEPT);
#endif
}

// Round to integral in the live mode, contributing NO exception of its own -- the caller raises #I and #P
// itself, and must be able to raise NEITHER. No rounding primitive here is trustworthy enough to do that
// unaided: __builtin_rint on x86-64 with no ROUNDSD assumable becomes |x| + 2^52 - 2^52 with the sign
// reapplied, which rounds the MAGNITUDE (RC=down returned -1 for -1.5); on aarch64 it is FRINTX, which
// reports #P; and glibc's own trunc/floor resolve to a ROUNDSD that reports #P for an inexact source and
// #D for a denormal one, neither of which an x86 convert raises. So park the sticky flags across it.
#define CVT_ROUND(name, ty, sfx)                                                                                       \
    static ty name(ty x, int trunc) {                                                                                  \
        unsigned parked = cvt_fp_flags();                                                                              \
        ty r;                                                                                                          \
        switch (trunc ? 3 : cvt_host_rc()) {                                                                           \
        case 1: r = __builtin_floor##sfx(x); break;                                                                    \
        case 2: r = __builtin_ceil##sfx(x); break;                                                                     \
        case 3: r = __builtin_trunc##sfx(x); break;                                                                    \
        default: r = __builtin_roundeven##sfx(x); break;                                                               \
        }                                                                                                              \
        cvt_fp_flags_set(parked);                                                                                      \
        return r;                                                                                                      \
    }
CVT_ROUND(cvt_round_d, double, )
CVT_ROUND(cvt_round_f, float, f)
_Static_assert(1, "conversion helpers declared");

static void cvt_raise_pe(void) {
    volatile double a = 1.0, b = 3.0, q = a / b; // 1/3 is inexact in every mode and raises only #P
    (void)q;
}

// OR an exact set of exceptions, named by MXCSR bit (SSE_XI..SSE_XP), into the host sticky state. Setting
// the bits beats synthesising each one with an arithmetic op (cvt_raise_pe's 1/3 above): no operation
// raises exactly one exception in every case, and on aarch64 no operation raises #D at all.
static void sse_raise(unsigned mxcsr_bits) {
    if (!mxcsr_bits) return;
#if defined(HL_HOST_CPU_AARCH64)
    static const unsigned to_fpsr[6] = {0, 7, 1, 2, 3, 4}; // IE<-IOC DE<-IDC ZE<-DZC OE<-OFC UE<-UFC PE<-IXC
    unsigned host = 0;
    for (unsigned i = 0; i < 6; i++)
        if (mxcsr_bits & (1u << i)) host |= 1u << to_fpsr[i];
    cvt_fp_flags_set(cvt_fp_flags() | host);
#else
    cvt_fp_flags_set(cvt_fp_flags() | mxcsr_bits);
#endif
}

void hl_x86_sse_raise(unsigned mxcsr_bits) {
    sse_raise(mxcsr_bits);
}

// Guest denormals-are-zero. On an x86-64 host the guest MXCSR IS the host MXCSR, so read DAZ(6) directly;
// on aarch64 the guest's FTZ|DAZ is carried by FPCR.FZ(24), which ldmxcsr set (see translate.c).
int sse_daz_active(void) {
#if defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpcr" : "=r"(f));
    return (f >> 24) & 1u;
#elif defined(HL_HOST_CPU_X86_64)
    return (_mm_getcsr() >> 6) & 1u;
#else
    return 0;
#endif
}

// A denormal SOURCE, decided on the BIT PATTERN. Any FP comparison would do it in fewer lines and is not
// available: COMISD/UCOMISD themselves report #D for a denormal operand, so the test would raise the flag
// it is measuring. Exponent field zero with a nonzero significand; +-0 is not denormal.
int sse_is_denorm_f32(uint32_t b) {
    return (b & 0x7f800000u) == 0 && (b & 0x007fffffu) != 0;
}

int sse_is_denorm_f64(uint64_t b) {
    return (b & UINT64_C(0x7ff0000000000000)) == 0 && (b & UINT64_C(0x000fffffffffffff)) != 0;
}

// x86 CVT[T]xx2{SI,DQ,PI}, the whole rule, for every convert the C emulator owns. Measured on Zen 4 over
// the four RC modes and a kit whose out-of-range values include NON-INTEGERS -- the only ones that can
// tell these three apart, and they exist only for an f64 source with a 32-bit destination:
//   * out of range AFTER rounding, or NaN -> the integer indefinite and #I ALONE. Not #P, even from an
//     inexact source; not #D, even from a denormal one. So no host FP op may run on that path.
//   * in range -> #P iff the rounding changed the value, and nothing else.
// The rounding itself is exception-free, so both flags are raised explicitly. Everything here used to be
// absent: the VEX scalar forms raised no MXCSR bit at all on either host.
// "Is it NaN" and "did the rounding change it" are decided on the BIT PATTERN, not with == : UCOMISD/
// COMISD report #D for a denormal operand (SDM, and measured), and a convert must not. Bitwise inequality
// is exactly inexactness here, because round-to-integral keeps the sign of a zero and NaN is already out.
#define CVT_TO_INT(name, ty, uty, isnan, rnd)                                                                          \
    int64_t name(ty x, int trunc, int w64) {                                                                           \
        ty lim = w64 ? (ty)9223372036854775808.0 : (ty)2147483648.0;                                                   \
        int64_t indef = w64 ? (int64_t)0x8000000000000000ull : (int64_t)(int32_t)0x80000000u;                          \
        uty xb, rb;                                                                                                    \
        memcpy(&xb, &x, sizeof xb);                                                                                    \
        if (isnan(xb)) {                                                                                               \
            fma_raise_ie();                                                                                            \
            return indef;                                                                                              \
        }                                                                                                              \
        ty r = rnd(x, trunc);                                                                                          \
        if (r >= lim || r < -lim) { /* r is integral, never denormal, so these cannot report #D */                     \
            fma_raise_ie();                                                                                            \
            return indef;                                                                                              \
        }                                                                                                              \
        memcpy(&rb, &r, sizeof rb);                                                                                    \
        if (rb != xb) cvt_raise_pe();                                                                                  \
        return (int64_t)r;                                                                                             \
    }
CVT_TO_INT(cvt_x86_d2i, double, uint64_t, nan64, cvt_round_d)
CVT_TO_INT(cvt_x86_f2i, float, uint32_t, nan32, cvt_round_f)
_Static_assert(1, "integer conversion helpers declared");

float avx_fp_arith_f32(int op, float x, float y) {
    switch (op) {
    case 0x58: return avx_dnan_f32(x + y, x, y);
    case 0x59: return avx_dnan_f32(x * y, x, y);
    case 0x5C: return avx_dnan_f32(x - y, x, y);
    case 0x5E: return avx_dnan_f32(x / y, x, y);
    case 0x5D: return x < y ? x : y; // min: NaN/equal/+-0 -> src2 (x86-exact)
    default: return x > y ? x : y;   // 0x5F max: NaN/equal/+-0 -> src2 (x86-exact)
    }
}

double avx_fp_arith_f64(int op, double x, double y) {
    switch (op) {
    case 0x58: return avx_dnan_f64(x + y, x, y);
    case 0x59: return avx_dnan_f64(x * y, x, y);
    case 0x5C: return avx_dnan_f64(x - y, x, y);
    case 0x5E: return avx_dnan_f64(x / y, x, y);
    case 0x5D: return x < y ? x : y; // min: NaN/equal/+-0 -> src2 (x86-exact)
    default: return x > y ? x : y;   // 0x5F max: NaN/equal/+-0 -> src2 (x86-exact)
    }
}

// VCMP{PS,PD,SS,SD} predicate (imm[4:0]). Float operands are promoted to double exactly, so a single
// comparator serves both widths; the 16..31 signaling variants yield the same boolean as 0..15.
int avx_cmp_pred(double x, double y, int pred) {
    switch (pred & 0xf) {
    case 0: return x == y;                             // EQ_OQ
    case 1: return x < y;                              // LT_OS
    case 2: return x <= y;                             // LE_OS
    case 3: return !(x == x) || !(y == y);             // UNORD_Q
    case 4: return !(x == y);                          // NEQ_UQ
    case 5: return !(x < y);                           // NLT_US
    case 6: return !(x <= y);                          // NLE_US
    case 7: return (x == x) && (y == y);               // ORD_Q
    case 8: return (x == y) || !(x == x) || !(y == y); // EQ_UQ
    case 9: return !(x >= y);                          // NGE_US
    case 10: return !(x > y);                          // NGT_US
    case 11: return 0;                                 // FALSE_OQ
    case 12: return (x < y) || (x > y);                // NEQ_OQ
    case 13: return x >= y;                            // GE_OS
    case 14: return x > y;                             // GT_OS
    default: return 1;                                 // TRUE_UQ
    }
}

// F16C uses the host's native fp16 so the half<->single conversion matches x86. `imm` is the vcvtps2ph
// rounding-control immediate: imm[2]=1 -> use MXCSR (host FPCR already tracks the guest rounding mode),
// else imm[1:0] selects 0=nearest-even, 1=down(-inf), 2=up(+inf), 3=truncate(toward-zero). x86 imm[1:0]
// maps onto ARM FPCR.RMode {0=nearest, 1=+inf, 2=-inf, 3=zero}. Do the single->half FCVT in inline asm
// under a locally-set FPCR so a directed mode is honored precisely (a plain _Float16 cast can be a
// round-to-nearest libcall or be reordered around a fesetround), then restore FPCR.
// _Float16 is a pre-C23 GNU/clang extension the half-precision (F16C/AVX-512-FP16) path genuinely needs;
// silence the -Wpedantic noise narrowly rather than dropping the type.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"

#if !defined(HL_HOST_CPU_AARCH64)
// Portable single->half for every host CPU that is not AArch64. NOT the F16C intrinsic (_cvtss_sh), which
// needs -mf16c that this build cannot assume of the host micro-architecture, and not a _Float16 cast,
// which is round-to-nearest-even whatever the FP environment says. `mode` is the x86 rounding control as
// ROUND* imm[1:0]: 0=nearest-even, 1=down, 2=up, 3=truncate. A NaN result is QUIET even from a signalling
// source; overflow gives Infinity only when the mode rounds away from zero.
// `flags`, when non-NULL, receives the MXCSR bits VCVTPS2PH raises -- measured against native for all six
// imm encodings over the overflow, underflow and denormal boundaries. Every clause falls out of a value
// this function already had: #P from round|sticky, #U from "tiny BEFORE rounding, and inexact" (which is
// exponent < -14, so 65519.996 -> 0x03ff under RM is #U even though it lands on a normal), #O from the
// same exponent16 >= 0x1f the return below tests (so +65520 under RM, which rounds DOWN to the largest
// finite, is correctly NOT an overflow), and #I from an SNaN. #D is the CALLER's: it is the one bit both
// host paths need alike, and the caller already inspects the source for DAZ.
static uint16_t avx_f32_to_f16_software(float f, unsigned mode, unsigned *flags) {
    uint32_t bits;
    memcpy(&bits, &f, 4);
    uint32_t sign = bits >> 31;
    uint32_t biased_exponent = (bits >> 23) & 0xffu;
    uint32_t mantissa = bits & 0x7fffffu;
    uint16_t sign16 = (uint16_t)(sign << 15);
    // Directed modes round by sign alone; the same predicate picks Infinity vs largest-finite on overflow.
    uint32_t away = ((mode == 1 && sign != 0) || (mode == 2 && sign == 0)) ? 1u : 0u;
    if (biased_exponent == 0xffu) {
        if (flags && mantissa != 0 && !(mantissa & 0x400000u)) *flags = SSE_XI; // SNaN
        return mantissa == 0 ? (uint16_t)(sign16 | 0x7c00u) : (uint16_t)(sign16 | 0x7e00u | (uint16_t)(mantissa >> 13));
    }
    if (biased_exponent == 0 && mantissa == 0) return sign16; // +-0 exact, sign preserved
    // significand * 2^(exponent-23), implicit bit explicit (a binary32 subnormal has none; exponent -126).
    uint32_t significand = biased_exponent == 0 ? mantissa : (mantissa | 0x800000u);
    int32_t exponent = (int32_t)(biased_exponent == 0 ? 1u : biased_exponent) - 127;
    // Bits to drop to reach the half's ulp: 13 normally, more when subnormal (ulp pinned at 2^-24). The
    // clamp at 25 only avoids UB; a wider shift gives the same round/sticky.
    int32_t shift = exponent >= -14 ? 13 : -1 - exponent;
    if (shift > 25) shift = 25;
    uint32_t half = significand >> shift;
    uint32_t round_bit = (significand >> (shift - 1)) & 1u;
    uint32_t sticky = (significand & ((1u << (shift - 1)) - 1u)) != 0 ? 1u : 0u;
    if (mode == 0)
        half += round_bit & (sticky | (half & 1u)); // nearest-even: up on >half, or half-to-even
    else
        half += away & (round_bit | sticky);
    if (flags && (round_bit | sticky)) *flags |= SSE_XP | (exponent < -14 ? SSE_XU : 0u);
    if (exponent < -14) return (uint16_t)(sign16 | half); // subnormal; a carry out lands on 0x0400
    int32_t exponent16 = exponent + 15;
    if (half >> 11) { // the increment carried into the next binade
        half >>= 1;
        exponent16++;
    }
    if (exponent16 >= 0x1f) {
        if (flags) *flags |= SSE_XO | SSE_XP;
        return (uint16_t)(sign16 | (away != 0 || mode == 0 ? 0x7c00u : 0x7bffu));
    }
    return (uint16_t)(sign16 | ((uint32_t)exponent16 << 10) | (half & 0x3ffu)); // implicit bit 0x400 drops out
}
#endif

#if defined(HL_HOST_CPU_X86_64)
// Live MXCSR.RC in the ROUND*-immediate encoding; defined with sse_round_d.
int sse_host_rounding_control(void);
#endif

// The two host paths reach the same exception set from opposite directions: the aarch64 FCVT raises #I/#U/
// #O/#P itself and can only miss #D (ARM reports IDC solely when FPCR.FZ flushed an input), while the
// software converter raises nothing and reports the whole set through its out-param.
uint16_t avx_f32_to_f16(float f, int imm) {
    uint32_t fb;
    memcpy(&fb, &f, 4);
    if (sse_is_denorm_f32(fb)) { // DAZ zeroes the source first, making the conversion exact and flagless
        if (sse_daz_active()) {
            fb &= 0x80000000u;
            memcpy(&f, &fb, 4);
        } else
            sse_raise(SSE_XD);
    }
#if defined(HL_HOST_CPU_AARCH64)
    _Float16 h;
    uint16_t o;
    if (imm & 4) { // imm[2]=1: MXCSR-controlled (current host FPCR mirrors guest MXCSR rounding)
        h = (_Float16)f;
        memcpy(&o, &h, 2);
        return o;
    }
    static const unsigned rm[4] = {0, 2, 1, 3}; // x86 nearest/down/up/trunc -> ARM RMode nearest/-inf/+inf/zero
    unsigned long fpcr_orig, fpcr_new;
    __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr_orig));
    fpcr_new = (fpcr_orig & ~(3UL << 22)) | ((unsigned long)rm[imm & 3] << 22);
    __asm__ volatile("msr fpcr, %1\n\tisb\n\tfcvt %h0, %s2" : "=w"(h) : "r"(fpcr_new), "w"(f));
    __asm__ volatile("msr fpcr, %0\n\tisb" ::"r"(fpcr_orig));
    memcpy(&o, &h, 2);
    return o;
#else
    // imm[2]=1 asks for the MXCSR-controlled mode; with no host FPCR, use the host FP environment.
    unsigned mode = (unsigned)(imm & 3);
    if (imm & 4) {
#if defined(HL_HOST_CPU_X86_64)
        // NOT fegetround(): glibc's x86-64 fegetround reads the **x87** control word, not the guest's
        // MXCSR (written by LDMXCSR, not FLDCW). MXCSR.RC already uses this encoding.
        mode = (unsigned)sse_host_rounding_control();
#else
        switch (fegetround()) {
        case FE_DOWNWARD: mode = 1; break;
        case FE_UPWARD: mode = 2; break;
        case FE_TOWARDZERO: mode = 3; break;
        default: mode = 0; break;
        }
#endif
    }
    unsigned flags = 0;
    uint16_t o = avx_f32_to_f16_software(f, mode, &flags);
    sse_raise(flags);
    return o;
#endif
}

float avx_f16_to_f32(uint16_t bits) {
    _Float16 h;
    memcpy(&h, &bits, 2);
    return (float)h;
}

#pragma GCC diagnostic pop
