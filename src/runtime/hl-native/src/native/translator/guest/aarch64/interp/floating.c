// Floating point: FPCR/FPSR, and how a guest FP result is computed.
// Project FPCR onto the host FP environment (<fenv.h>; this file backs every non-AArch64 host) and let the
// host compute: for FADD/FSUB/FMUL/FDIV/FSQRT/FMADD and single <-> double both ISAs define the same
// correctly-rounded IEEE-754 result. guest/x86_64/x87state.c is this same map read right to left.
//
// Handled below instead, because the ISAs differ: NaN propagation (AArch64 prefers a SIGNALLING operand, in
// operand order; x86 the first), the default NaN (AArch64's sign bit is CLEAR, x86's SET), FPCR.DN and
// FPCR.FZ/FZ16, FMIN/FMAX (signs ANDed for max(+0,-0)), and the integer and half conversions, which x86-64
// has no baseline correctly-rounded instruction for. AArch64 also detects tininess BEFORE rounding and
// x86-64 after, so FPSR.UFC can differ for a value tiny before rounding that rounds up to normal.

// The three formats, ordered so (fmt + 1) is the AdvSIMD element-size code the vector accessors take.
#define INTERP_FP_H 0u
#define INTERP_FP_S 1u
#define INTERP_FP_D 2u

static unsigned interp_fp_width(unsigned fmt) {
    return fmt == INTERP_FP_H ? 16u : (fmt == INTERP_FP_S ? 32u : 64u);
}

static unsigned interp_fp_mant(unsigned fmt) {
    return fmt == INTERP_FP_H ? 10u : (fmt == INTERP_FP_S ? 23u : 52u);
}

static int interp_fp_bias(unsigned fmt) {
    return fmt == INTERP_FP_H ? 15 : (fmt == INTERP_FP_S ? 127 : 1023);
}

static unsigned interp_fp_inf_exp(unsigned fmt) {
    return fmt == INTERP_FP_H ? 0x1Fu : (fmt == INTERP_FP_S ? 0xFFu : 0x7FFu);
}

static uint64_t interp_fp_mant_mask(unsigned fmt) {
    return (UINT64_C(1) << interp_fp_mant(fmt)) - 1u;
}

static uint64_t interp_fp_sign_mask(unsigned fmt) {
    return UINT64_C(1) << (interp_fp_width(fmt) - 1u);
}

// ptype: 10 is unallocated as a format (only FMOV to/from Vn.D[1] spells it, and carries no format).
static int interp_fp_type_fmt(unsigned type, unsigned *fmt) {
    switch (type) {
    case 0: *fmt = INTERP_FP_S; return 1;
    case 1: *fmt = INTERP_FP_D; return 1;
    case 3: *fmt = INTERP_FP_H; return 1;
    default: return 0;
    }
}

// QNAN/SNAN last, so "is this a NaN" is one comparison.
#define INTERP_FPC_ZERO 0u
#define INTERP_FPC_DENORM 1u
#define INTERP_FPC_NORM 2u
#define INTERP_FPC_INF 3u
#define INTERP_FPC_QNAN 4u
#define INTERP_FPC_SNAN 5u

static unsigned interp_fp_class(uint64_t bits, unsigned fmt) {
    unsigned mant = interp_fp_mant(fmt);
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    if (biased == 0) return frac == 0 ? INTERP_FPC_ZERO : INTERP_FPC_DENORM;
    if (biased != interp_fp_inf_exp(fmt)) return INTERP_FPC_NORM;
    if (frac == 0) return INTERP_FPC_INF;
    return ((frac >> (mant - 1u)) & 1u) ? INTERP_FPC_QNAN : INTERP_FPC_SNAN;
}

// The first four are FPCR.RMode verbatim; RA (ties away) has no FPCR encoding -- FCVTA*/FRINTA name it.
#define INTERP_RM_RN 0u // to nearest, ties to even
#define INTERP_RM_RP 1u // toward +infinity
#define INTERP_RM_RM 2u // toward -infinity
#define INTERP_RM_RZ 3u // toward zero
#define INTERP_RM_RA 4u // to nearest, ties away from zero

static int interp_fp_host_round(unsigned rmode) {
    switch (rmode) {
    case INTERP_RM_RP: return FE_UPWARD;
    case INTERP_RM_RM: return FE_DOWNWARD;
    case INTERP_RM_RZ: return FE_TOWARDZERO;
    default: return FE_TONEAREST;
    }
}

static int interp_fp_round_away(unsigned rmode, unsigned sign, int round_bit, int sticky, unsigned lsb) {
    switch (rmode) {
    case INTERP_RM_RN: return round_bit && (sticky || lsb);
    case INTERP_RM_RA: return round_bit != 0;
    case INTERP_RM_RP: return !sign && (round_bit || sticky);
    case INTERP_RM_RM: return sign && (round_bit || sticky);
    default: return 0; // RZ truncates
    }
}

// Enter installs the guest rounding mode and clears the host's sticky flags; leave harvests them onto FPSR
// and RESTORES the host mode, so the engine's own C cannot inherit it. Barriers and volatiles stop GCC
// (FENV_ACCESS off) hoisting FP work out.
typedef struct {
    int host_round;
} interp_fpenv;

static void interp_fp_env_enter(interp_fpenv *env) {
    int want = interp_fp_host_round(INTERP_FPCR_RMODE(g_interp_fpcr));
    env->host_round = fegetround();
    if (env->host_round != want) (void)fesetround(want);
    (void)feclearexcept(FE_ALL_EXCEPT);
    __asm__ __volatile__("" ::: "memory");
}

static unsigned interp_fp_env_leave(interp_fpenv *env) {
    __asm__ __volatile__("" ::: "memory");
    int raised = fetestexcept(FE_ALL_EXCEPT);
    if (env->host_round != interp_fp_host_round(INTERP_FPCR_RMODE(g_interp_fpcr))) (void)fesetround(env->host_round);
    unsigned bits = 0;
    if (raised & FE_INVALID) bits |= INTERP_FPSR_IOC;
    if (raised & FE_DIVBYZERO) bits |= INTERP_FPSR_DZC;
    if (raised & FE_OVERFLOW) bits |= INTERP_FPSR_OFC;
    if (raised & FE_UNDERFLOW) bits |= INTERP_FPSR_UFC;
    if (raised & FE_INEXACT) bits |= INTERP_FPSR_IXC;
    return bits;
}

static double interp_fp_to_double(uint64_t bits) {
    double value;
    memcpy(&value, &bits, sizeof value);
    return value;
}

static uint64_t interp_fp_from_double(double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    return bits;
}

static float interp_fp_to_float(uint32_t bits) {
    float value;
    memcpy(&value, &bits, sizeof value);
    return value;
}

static uint64_t interp_fp_from_float(float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    return (uint64_t)bits;
}

// Exact re-encoding, so it raises nothing. A half denormal becomes a double NORMAL.
static double interp_fp_half_to_double(uint64_t bits) {
    uint64_t sign = (bits & 0x8000u) ? (UINT64_C(1) << 63) : 0;
    unsigned biased = (unsigned)((bits >> 10) & 0x1Fu);
    uint64_t frac = bits & 0x3FFu;
    if (biased == 0x1Fu) return interp_fp_to_double(sign | (UINT64_C(0x7FF) << 52) | (frac << 42));
    if (biased == 0) {
        if (frac == 0) return interp_fp_to_double(sign);
        int exponent = 1 - 15;
        while (!(frac & 0x400u)) {
            frac <<= 1;
            exponent--;
        }
        frac &= 0x3FFu;
        return interp_fp_to_double(sign | ((uint64_t)(exponent + 1023) << 52) | (frac << 42));
    }
    return interp_fp_to_double(sign | ((uint64_t)((int)biased - 15 + 1023) << 52) | (frac << 42));
}

// Widening to double is EXACT here, so comparisons and half arithmetic add no rounding. Screen NaNs first.
static double interp_fp_widen(uint64_t bits, unsigned fmt) {
    if (fmt == INTERP_FP_D) return interp_fp_to_double(bits);
    if (fmt == INTERP_FP_S) return (double)interp_fp_to_float((uint32_t)bits);
    return interp_fp_half_to_double(bits);
}

static uint64_t interp_fp_default_nan(unsigned fmt) {
    unsigned mant = interp_fp_mant(fmt);
    return ((uint64_t)interp_fp_inf_exp(fmt) << mant) | (UINT64_C(1) << (mant - 1u));
}

static uint64_t interp_fp_process_nan(uint64_t bits, unsigned fmt) {
    if (interp_fp_class(bits, fmt) == INTERP_FPC_SNAN) interp_fpsr_raise(INTERP_FPSR_IOC);
    if (INTERP_FPCR_DN(g_interp_fpcr)) return interp_fp_default_nan(fmt);
    return bits | (UINT64_C(1) << (interp_fp_mant(fmt) - 1u));
}

static int interp_fp_process_nans(unsigned fmt, unsigned count, const uint64_t *operands, uint64_t *result) {
    for (unsigned pass = 0; pass < 2; pass++)
        for (unsigned index = 0; index < count; index++) {
            unsigned cls = interp_fp_class(operands[index], fmt);
            if (cls == (pass == 0 ? INTERP_FPC_SNAN : INTERP_FPC_QNAN)) {
                *result = interp_fp_process_nan(operands[index], fmt);
                return 1;
            }
        }
    return 0;
}

// On an INPUT: FPCR.FZ governs single/double, FPCR.FZ16 half. FPSR.IDC means "denormal AND discarded" --
// but only at single and double: FPUnpack's half-precision branch flushes with no InputDenorm exception,
// so an FZ16 flush is SILENT. Measured across 15 half-precision forms this file already implemented:
// every FZ16 flush reported IDC here and none did under qemu-aarch64.
static uint64_t interp_fp_flush_input(uint64_t bits, unsigned fmt) {
    unsigned flush = fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr);
    if (!flush || interp_fp_class(bits, fmt) != INTERP_FPC_DENORM) return bits;
    if (fmt != INTERP_FP_H) interp_fpsr_raise(INTERP_FPSR_IDC);
    return bits & interp_fp_sign_mask(fmt);
}

static uint64_t interp_fp_postprocess(unsigned fmt, uint64_t bits, unsigned raised) {
    unsigned cls = interp_fp_class(bits, fmt);
    if (cls >= INTERP_FPC_QNAN) {
        // No operand was a NaN, so this is an invalid operation and the host's NaN is not FPDefaultNaN.
        bits = interp_fp_default_nan(fmt);
        // A result the host already underflowed all the way to zero is just as tiny as a denormal one, and
        // FZ reports it the same way; the host's Precision flag for it is not AArch64's.
    } else if (cls == INTERP_FPC_DENORM || (cls == INTERP_FPC_ZERO && (raised & INTERP_FPSR_UFC))) {
        unsigned flush = fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr);
        if (flush) {
            bits &= interp_fp_sign_mask(fmt);
            // Underflow ALONE: FPRoundBase returns the flushed zero before the Inexact test, so the host's
            // IXC for the denormal it actually computed must not survive.
            raised = (raised & ~(unsigned)INTERP_FPSR_IXC) | INTERP_FPSR_UFC;
        }
    }
    interp_fpsr_raise(raised);
    return bits;
}

// Round (-1)^sign * significand * 2^exponent into `fmt` under `rmode`; OFC/UFC/IXC are OR-ed into *raised.
// The only soft-float rounder here: unsigned-64 -> FP, conversion to half and the fixed-point forms have no
// correctly-rounded baseline x86-64 instruction. Clamping `lsb_exp` to the subnormal ulp is AArch64's
// BEFORE-rounding tininess rule.
static uint64_t interp_fp_pack(unsigned sign, uint64_t significand, int exponent, unsigned fmt, unsigned rmode,
                               unsigned *raised) {
    unsigned mant = interp_fp_mant(fmt);
    uint64_t sign_bit = sign ? interp_fp_sign_mask(fmt) : 0;
    if (significand == 0) return sign_bit; // exact zero keeps its sign

    int value_exp = exponent + (63 - __builtin_clzll(significand));
    int min_exp = 1 - interp_fp_bias(fmt) - (int)mant; // exponent of the smallest subnormal's last bit
    int lsb_exp = value_exp - (int)mant;
    int tiny = lsb_exp < min_exp;
    if (tiny) lsb_exp = min_exp;

    // A shift of 64 or more arises only in the clamped (tiny) case.
    int shift = lsb_exp - exponent;
    uint64_t quotient;
    int round_bit = 0, sticky = 0;
    if (shift <= 0) {
        // Cannot overflow: unclamped the leading one lands exactly at bit `mant`, clamped strictly below.
        quotient = significand << (unsigned)(-shift);
    } else if (shift >= 64) {
        quotient = 0;
        round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
        sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
    } else {
        quotient = significand >> (unsigned)shift;
        round_bit = (int)((significand >> (unsigned)(shift - 1)) & 1u);
        sticky = shift > 1 && (significand & ((UINT64_C(1) << (unsigned)(shift - 1)) - 1u)) != 0;
    }
    int inexact = round_bit | sticky;
    if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(quotient & 1u))) quotient++;
    if (inexact) *raised |= INTERP_FPSR_IXC;

    if ((quotient >> mant) == 0) {
        if (tiny && inexact) *raised |= INTERP_FPSR_UFC;
        return sign_bit | quotient;
    }
    // A round-up carries into the next binade; in the subnormal branch such a carry IS the smallest normal.
    if (quotient >> (mant + 1u)) {
        quotient >>= 1;
        lsb_exp++;
    }
    if (tiny && inexact) *raised |= INTERP_FPSR_UFC; // tiny before rounding, normal after: still Underflow
    int biased = lsb_exp + (int)mant + interp_fp_bias(fmt);
    if (biased >= (int)interp_fp_inf_exp(fmt)) {
        // IEEE-754: infinity if the mode rounds AWAY from zero at this sign, else the largest finite.
        *raised |= INTERP_FPSR_OFC | INTERP_FPSR_IXC;
        int away = rmode == INTERP_RM_RN || rmode == INTERP_RM_RA || (rmode == INTERP_RM_RP && !sign) ||
                   (rmode == INTERP_RM_RM && sign);
        if (away) return sign_bit | ((uint64_t)interp_fp_inf_exp(fmt) << mant);
        return sign_bit | ((uint64_t)(interp_fp_inf_exp(fmt) - 1u) << mant) | interp_fp_mant_mask(fmt);
    }
    return sign_bit | ((uint64_t)(unsigned)biased << mant) | (quotient & interp_fp_mant_mask(fmt));
}

static uint64_t interp_fp_half_from_double(double value, unsigned rmode, unsigned *raised) {
    uint64_t bits = interp_fp_from_double(value);
    unsigned sign = (unsigned)(bits >> 63);
    unsigned biased = (unsigned)((bits >> 52) & 0x7FFu);
    uint64_t frac = bits & ((UINT64_C(1) << 52) - 1u);
    if (biased == 0x7FFu)
        return frac == 0 ? (sign ? 0xFC00u : 0x7C00u) : 0x7E00u; // postprocess substitutes the default NaN
    if (biased == 0 && frac == 0) return sign ? 0x8000u : 0u;
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << 52));
    int exponent = (int)(biased == 0 ? 1u : biased) - 1023 - 52;
    return interp_fp_pack(sign, significand, exponent, INTERP_FP_H, rmode, raised);
}

#define INTERP_FPOP_ADD 0u
#define INTERP_FPOP_SUB 1u
#define INTERP_FPOP_MUL 2u
#define INTERP_FPOP_DIV 3u

// Half computes in the host's DOUBLE unit then rounds to half: exact, since 53 bits exceed the 24 that
// binary16 needs to survive a second rounding.
static uint64_t interp_fp_arith(unsigned fmt, unsigned op, uint64_t a, uint64_t b) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, r = 0;
        switch (op) {
        case INTERP_FPOP_ADD: r = x + y; break;
        case INTERP_FPOP_SUB: r = x - y; break;
        case INTERP_FPOP_MUL: r = x * y; break;
        default: r = x / y; break;
        }
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, r = 0;
        switch (op) {
        case INTERP_FPOP_ADD: r = x + y; break;
        case INTERP_FPOP_SUB: r = x - y; break;
        case INTERP_FPOP_MUL: r = x * y; break;
        default: r = x / y; break;
        }
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

static uint64_t interp_fp_sqrt(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    uint64_t nan;
    if (interp_fp_process_nans(fmt, 1, &a, &nan)) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float operand = interp_fp_to_float((uint32_t)a);
        interp_fp_env_enter(&env);
        volatile float x = operand, r;
        r = sqrtf(x);
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double operand = interp_fp_widen(a, fmt);
        interp_fp_env_enter(&env);
        volatile double x = operand, r;
        r = sqrt(x);
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// addend + a*b with a SINGLE rounding, which is what C's fma() is defined to be; `x*y + z` rounds twice.
static uint64_t interp_fp_muladd(unsigned fmt, uint64_t addend, uint64_t a, uint64_t b) {
    addend = interp_fp_flush_input(addend, fmt);
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    int zero_times_inf = (class_a == INTERP_FPC_INF && class_b == INTERP_FPC_ZERO) ||
                         (class_a == INTERP_FPC_ZERO && class_b == INTERP_FPC_INF);
    uint64_t operands[3] = {addend, a, b}, nan;
    int have_nan = interp_fp_process_nans(fmt, 3, operands, &nan);
    // A QUIET NaN addend does NOT win over an invalid multiply (inf*0 gives the default NaN); a SIGNALLING
    // one does.
    if (zero_times_inf && interp_fp_class(addend, fmt) == INTERP_FPC_QNAN) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_default_nan(fmt);
    }
    if (have_nan) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b),
              extra = interp_fp_to_float((uint32_t)addend);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, z = extra, r;
        r = fmaf(x, y, z);
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt), extra = interp_fp_widen(addend, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, z = extra, r;
        r = fma(x, y, z);
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// FMULX is FMUL except at 0 * inf, which is the ONLY reason the instruction exists: it yields +-2.0 with no
// Invalid, so a reciprocal-estimate refinement step stays finite at the extremes instead of turning into a NaN.
static uint64_t interp_fp_mulx(unsigned fmt, uint64_t a, uint64_t b) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    if ((class_a == INTERP_FPC_INF && class_b == INTERP_FPC_ZERO) ||
        (class_a == INTERP_FPC_ZERO && class_b == INTERP_FPC_INF))
        return ((a ^ b) & interp_fp_sign_mask(fmt)) | ((uint64_t)(interp_fp_bias(fmt) + 1) << interp_fp_mant(fmt));
    return interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
}

// The NM forms swap a QUIET NaN for the losing infinity; a SIGNALLING NaN still propagates with Invalid.
static uint64_t interp_fp_minmax(unsigned fmt, uint64_t a, uint64_t b, int want_max, int numeric) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    if (numeric) {
        uint64_t losing = ((uint64_t)interp_fp_inf_exp(fmt) << interp_fp_mant(fmt)) |
                          (want_max ? interp_fp_sign_mask(fmt) : UINT64_C(0));
        int a_quiet = interp_fp_class(a, fmt) == INTERP_FPC_QNAN, b_quiet = interp_fp_class(b, fmt) == INTERP_FPC_QNAN;
        if (a_quiet && !b_quiet)
            a = losing;
        else if (b_quiet && !a_quiet)
            b = losing;
    }
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    double x = interp_fp_widen(a, fmt), y = interp_fp_widen(b, fmt);
    uint64_t chosen = (want_max ? (x > y) : (x < y)) ? a : b;
    if (interp_fp_class(chosen, fmt) == INTERP_FPC_ZERO) {
        // +0 and -0 compare EQUAL: FPMax ANDs the operand signs (+0 wins), FPMin ORs them (-0 wins).
        unsigned sign_a = (a & interp_fp_sign_mask(fmt)) != 0, sign_b = (b & interp_fp_sign_mask(fmt)) != 0;
        unsigned sign = want_max ? (sign_a & sign_b) : (sign_a | sign_b);
        return sign ? interp_fp_sign_mask(fmt) : UINT64_C(0);
    }
    return chosen;
}

// `quiet_signals` selects FCMPE/FCCMPE, which raise Invalid for a quiet NaN too.
static void interp_fp_compare(struct cpu *cpu, unsigned fmt, uint64_t a, uint64_t b, int quiet_signals) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    if (class_a >= INTERP_FPC_QNAN || class_b >= INTERP_FPC_QNAN) {
        if (quiet_signals || class_a == INTERP_FPC_SNAN || class_b == INTERP_FPC_SNAN)
            interp_fpsr_raise(INTERP_FPSR_IOC);
        interp_set_flags(cpu, 0, 0, 1, 1); // unordered
        return;
    }
    double x = interp_fp_widen(a, fmt), y = interp_fp_widen(b, fmt);
    if (x == y)
        interp_set_flags(cpu, 0, 1, 1, 0);
    else if (x < y)
        interp_set_flags(cpu, 1, 0, 0, 0);
    else
        interp_set_flags(cpu, 0, 0, 1, 0);
}

// `exact` selects FRINTX, which raises Inexact when the value moves. Same format in and out.
static uint64_t interp_fp_round_integral(unsigned fmt, uint64_t bits, unsigned rmode, int exact) {
    bits = interp_fp_flush_input(bits, fmt);
    uint64_t nan;
    if (interp_fp_process_nans(fmt, 1, &bits, &nan)) return nan;
    unsigned cls = interp_fp_class(bits, fmt);
    if (cls == INTERP_FPC_INF || cls == INTERP_FPC_ZERO) return bits;

    unsigned mant = interp_fp_mant(fmt);
    unsigned sign = (bits & interp_fp_sign_mask(fmt)) != 0;
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << mant));
    int exponent = (int)(biased == 0 ? 1u : biased) - interp_fp_bias(fmt) - (int)mant;
    if (exponent >= 0) return bits; // already integral

    unsigned shift = (unsigned)(-exponent);
    uint64_t magnitude;
    int round_bit, sticky;
    if (shift >= 64) {
        magnitude = 0;
        round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
        sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
    } else {
        magnitude = significand >> shift;
        round_bit = (int)((significand >> (shift - 1u)) & 1u);
        sticky = shift > 1 && (significand & ((UINT64_C(1) << (shift - 1u)) - 1u)) != 0;
    }
    if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(magnitude & 1u))) magnitude++;
    if (exact && (round_bit || sticky)) interp_fpsr_raise(INTERP_FPSR_IXC);
    // A zero result keeps the operand's sign: FRINTM of -0.4 is -1.0, FRINTZ of -0.4 is -0.0.
    unsigned raised = 0;
    return interp_fp_pack(sign, magnitude, 0, fmt, INTERP_RM_RZ, &raised);
}

static uint64_t interp_fp_convert_nan(uint64_t bits, unsigned from, unsigned to) {
    if (interp_fp_class(bits, from) == INTERP_FPC_SNAN) interp_fpsr_raise(INTERP_FPSR_IOC);
    if (INTERP_FPCR_DN(g_interp_fpcr)) return interp_fp_default_nan(to);
    unsigned from_mant = interp_fp_mant(from), to_mant = interp_fp_mant(to);
    uint64_t payload = bits & interp_fp_mant_mask(from);
    payload = to_mant >= from_mant ? payload << (to_mant - from_mant) : payload >> (from_mant - to_mant);
    uint64_t sign = (bits & interp_fp_sign_mask(from)) ? interp_fp_sign_mask(to) : 0;
    return sign | ((uint64_t)interp_fp_inf_exp(to) << to_mant) | payload | (UINT64_C(1) << (to_mant - 1u));
}

// Widening is exact; double -> single is the one narrowing the host does identically.
static uint64_t interp_fp_convert(unsigned from, unsigned to, uint64_t bits) {
    if (interp_fp_class(bits, from) >= INTERP_FPC_QNAN) return interp_fp_convert_nan(bits, from, to);
    bits = interp_fp_flush_input(bits, from);
    if (interp_fp_width(to) > interp_fp_width(from)) {
        double wide = interp_fp_widen(bits, from);
        if (to == INTERP_FP_D) return interp_fp_postprocess(to, interp_fp_from_double(wide), 0);
        return interp_fp_postprocess(to, interp_fp_from_float((float)wide), 0);
    }
    unsigned raised = 0;
    uint64_t out;
    if (to == INTERP_FP_H) {
        out = interp_fp_half_from_double(interp_fp_widen(bits, from), INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    } else {
        double wide = interp_fp_to_double(bits); // from == INTERP_FP_D, to == INTERP_FP_S
        interp_fpenv env;
        interp_fp_env_enter(&env);
        volatile double x = wide;
        volatile float r = (float)x;
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    }
    return interp_fp_postprocess(to, out, raised);
}

// FCVTXN's FPRounding_ODD: truncate, then force the low bit whenever anything was lost, so a narrowing
// never lands on a value a second narrowing could round the wrong way. Overflow gives the largest finite.
// ODD replaces FPCR.RMode rather than composing with it, hence the (thread-local) override around the one
// host conversion; DN and FZ still come from the guest's, so it is restored before postprocess.
static uint64_t interp_fp_convert_odd(uint64_t bits) {
    if (interp_fp_class(bits, INTERP_FP_D) >= INTERP_FPC_QNAN)
        return interp_fp_convert_nan(bits, INTERP_FP_D, INTERP_FP_S);
    bits = interp_fp_flush_input(bits, INTERP_FP_D);
    double wide = interp_fp_to_double(bits);
    uint64_t saved_fpcr = g_interp_fpcr;
    g_interp_fpcr = (saved_fpcr & ~(UINT64_C(3) << 22)) | ((uint64_t)INTERP_RM_RZ << 22);
    interp_fpenv env;
    interp_fp_env_enter(&env);
    volatile double x = wide;
    volatile float r = (float)x;
    unsigned raised = interp_fp_env_leave(&env);
    g_interp_fpcr = saved_fpcr;
    uint64_t out = interp_fp_from_float(r);
    if (raised & INTERP_FPSR_IXC) out |= 1u;
    return interp_fp_postprocess(INTERP_FP_S, out, raised);
}

// x86's RCPPS is an implementation-defined approximation, so this engine answers it exactly. AArch64's
// estimates are the opposite: the ARM ARM specifies an 8-bit-mantissa table and the exact extraction, so
// two conforming implementations agree bit for bit and an approximation -- however close -- is a wrong
// answer. These two are the DDI 0487 shared pseudocode RecipEstimate/RecipSqrtEstimate transcribed; every
// entry of the 256- and 384-entry tables they generate was read back off qemu-aarch64 through six
// instruction paths (FRECPE.s/.d, URECPE, FRSQRTE.s/.d, URSQRTE), which agreed with each other and with
// these, and tests/compat/completeness/aarch64/neon_recip.c re-checks all 640 on whatever CPU runs it.
static unsigned interp_recip_estimate(unsigned a) {
    a = a * 2u + 1u; // to nearest, in units of 1/512
    unsigned b = (1u << 19) / a;
    return (b + 1u) / 2u; // to nearest
}

static unsigned interp_recip_sqrt_estimate(unsigned a) {
    a = a < 256u ? a * 2u + 1u : (((a >> 1) << 1) + 1u) * 2u; // 0.25..0.5 keeps its low bit, 0.5..1.0 drops it
    uint64_t b = 512;
    while ((uint64_t)a * (b + 1u) * (b + 1u) < (UINT64_C(1) << 28))
        b++;
    return (unsigned)((b + 1u) / 2u);
}

#define INTERP_FRAC52 ((UINT64_C(1) << 52) - 1u)

// Both estimates renormalise the operand into a 52-bit fraction, take 8 bits of it through the table and
// rebuild; the pseudocode spells the exponent arithmetic per format, and every constant is a bias multiple.
static uint64_t interp_fp_recip_estimate(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    unsigned mant = interp_fp_mant(fmt), cls = interp_fp_class(a, fmt), inf_exp = interp_fp_inf_exp(fmt);
    int bias = interp_fp_bias(fmt);
    uint64_t sign = a & interp_fp_sign_mask(fmt);
    if (cls >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    if (cls == INTERP_FPC_INF) return sign;
    if (cls == INTERP_FPC_ZERO) {
        interp_fpsr_raise(INTERP_FPSR_DZC);
        return sign | ((uint64_t)inf_exp << mant);
    }
    uint64_t frac = a & interp_fp_mant_mask(fmt);
    int exp = (int)((a >> mant) & inf_exp);
    // |x| < 2^-(bias+1) -- a denormal with its top two fraction bits clear -- reciprocates to an overflow.
    if (exp == 0 && (frac >> (mant - 2u)) == 0) {
        unsigned rmode = INTERP_FPCR_RMODE(g_interp_fpcr);
        int to_inf = rmode == INTERP_RM_RN || (rmode == INTERP_RM_RP && !sign) || (rmode == INTERP_RM_RM && sign);
        interp_fpsr_raise(INTERP_FPSR_OFC | INTERP_FPSR_IXC);
        return sign |
               (to_inf ? ((uint64_t)inf_exp << mant) : (((uint64_t)(inf_exp - 1u) << mant) | interp_fp_mant_mask(fmt)));
    }
    // The mirror case: FZ turns a reciprocal that would be denormal into zero, Underflow and no Inexact.
    if ((fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr)) && exp >= 2 * bias - 1) {
        interp_fpsr_raise(INTERP_FPSR_UFC);
        return sign;
    }
    uint64_t fraction = frac << (52u - mant);
    if (exp == 0) { // at most two shifts: a third would have been the overflow case above
        if ((fraction >> 51) == 0) {
            exp = -1;
            fraction = (fraction << 2) & INTERP_FRAC52;
        } else {
            fraction = (fraction << 1) & INTERP_FRAC52;
        }
    }
    unsigned estimate = interp_recip_estimate(256u + (unsigned)((fraction >> 44) & 0xFFu));
    int result_exp = 2 * bias - 1 - exp;
    fraction = (uint64_t)(estimate & 0xFFu) << 44;
    // A result_exp the estimate pushed below the normal range comes back as an explicit leading one.
    if (result_exp == 0) {
        fraction = (UINT64_C(1) << 51) | (fraction >> 1);
    } else if (result_exp == -1) {
        fraction = (UINT64_C(1) << 50) | (fraction >> 2);
        result_exp = 0;
    }
    return sign | ((uint64_t)(unsigned)result_exp << mant) | (fraction >> (52u - mant));
}

static uint64_t interp_fp_rsqrt_estimate(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    unsigned mant = interp_fp_mant(fmt), cls = interp_fp_class(a, fmt), inf_exp = interp_fp_inf_exp(fmt);
    uint64_t sign = a & interp_fp_sign_mask(fmt);
    if (cls >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    if (cls == INTERP_FPC_ZERO) {
        interp_fpsr_raise(INTERP_FPSR_DZC);
        return sign | ((uint64_t)inf_exp << mant);
    }
    if (sign) { // -0 took the branch above; every other negative is Invalid, not a signed result
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_default_nan(fmt);
    }
    if (cls == INTERP_FPC_INF) return 0;
    uint64_t fraction = (a & interp_fp_mant_mask(fmt)) << (52u - mant);
    int exp = (int)((a >> mant) & inf_exp);
    if (exp == 0) { // no bound needed: a denormal has a set bit to normalise onto
        while ((fraction >> 51) == 0) {
            fraction = (fraction << 1) & INTERP_FRAC52;
            exp--;
        }
        fraction = (fraction << 1) & INTERP_FRAC52;
    }
    // The exponent's PARITY survives the scaling, because a square root halves it: an odd one scales into
    // 0.25..0.5 and indexes the table's lower half.
    unsigned scaled = ((unsigned)exp & 1u) ? 128u + (unsigned)((fraction >> 45) & 0x7Fu)
                                           : 256u + (unsigned)((fraction >> 44) & 0xFFu);
    int result_exp = (3 * interp_fp_bias(fmt) - 1 - exp) / 2;
    unsigned estimate = interp_recip_sqrt_estimate(scaled);
    return ((uint64_t)(unsigned)result_exp << mant) | ((uint64_t)(estimate & 0xFFu) << (mant - 8u));
}

// FRECPX is NOT FRECPE: no table, no estimate. It reflects the exponent exactly and zeroes the mantissa,
// which is what a range-reduction step needs. FZ never changes the answer -- the exponent comes from the
// raw operand -- but flushing a denormal input still reports it at single and double.
static uint64_t interp_fp_recpx(unsigned fmt, uint64_t a) {
    unsigned mant = interp_fp_mant(fmt), inf_exp = interp_fp_inf_exp(fmt);
    if (interp_fp_class(a, fmt) >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    (void)interp_fp_flush_input(a, fmt);
    unsigned exp = (unsigned)((a >> mant) & inf_exp);
    return (a & interp_fp_sign_mask(fmt)) | ((uint64_t)(exp == 0 ? inf_exp - 1u : (~exp & inf_exp)) << mant);
}

// URECPE / URSQRTE: the same two tables read as unsigned Q0.32 fixed point. No FPCR, no FPSR.
static uint64_t interp_uint_recip_estimate(uint64_t a, int sqrt_form) {
    uint32_t value = (uint32_t)a;
    if (sqrt_form ? (value >> 30) == 0 : (value >> 31) == 0) return UINT64_C(0xFFFFFFFF);
    unsigned estimate = sqrt_form ? interp_recip_sqrt_estimate(value >> 23) : interp_recip_estimate(value >> 23);
    return (uint64_t)estimate << 23;
}

// FRECPS / FRSQRTS, the Newton-Raphson steps. Three things do not follow from the arithmetic: op1 is
// NEGATED before the unpack, so a NaN operand propagates with its sign FLIPPED; 0*inf yields 2.0 or 1.5
// rather than the Invalid a bare multiply would raise; and inf*finite yields a signed infinity directly.
static uint64_t interp_fp_recip_step(unsigned fmt, uint64_t a, uint64_t b, int sqrt_form) {
    unsigned mant = interp_fp_mant(fmt);
    a ^= interp_fp_sign_mask(fmt);
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    int inf_a = class_a == INTERP_FPC_INF, inf_b = class_b == INTERP_FPC_INF;
    if ((inf_a && class_b == INTERP_FPC_ZERO) || (class_a == INTERP_FPC_ZERO && inf_b))
        return sqrt_form ? (((uint64_t)interp_fp_bias(fmt) << mant) | (UINT64_C(1) << (mant - 1u)))
                         : ((uint64_t)(interp_fp_bias(fmt) + 1) << mant);
    if (inf_a || inf_b) return ((a ^ b) & interp_fp_sign_mask(fmt)) | ((uint64_t)interp_fp_inf_exp(fmt) << mant);

    // (3 - a*b)/2 is ONE rounding, so the halving must stay out of it: 1.5 + (a/2)*b is a single fma when
    // a/2 is exact. When it is not, |a| is below 2^(2-bias) and |a*b| below 8, so halving afterwards
    // neither overflows nor lands in the subnormals -- which halving 3 - a*b on its own can both do.
    int prehalve = sqrt_form && ((a >> mant) & interp_fp_inf_exp(fmt)) >= 2u;
    if (prehalve) a -= UINT64_C(1) << mant;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, r;
        r = fmaf(x, y, sqrt_form ? (prehalve ? 1.5f : 3.0f) : 2.0f);
        if (sqrt_form && !prehalve) r = r * 0.5f;
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, r;
        r = fma(x, y, sqrt_form ? (prehalve ? 1.5 : 3.0) : 2.0);
        if (sqrt_form && !prehalve) r = r * 0.5;
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// SatQ()'s out-of-range value; a 32-bit destination comes back sign-extended to 64.
static uint64_t interp_fp_int_saturate(unsigned sign, unsigned dest_bits, int is_signed) {
    if (!is_signed) return sign ? UINT64_C(0) : (dest_bits == 64 ? UINT64_MAX : ((UINT64_C(1) << dest_bits) - 1u));
    uint64_t limit = UINT64_C(1) << (dest_bits - 1u);
    return sign ? (UINT64_C(0) - limit) : (limit - 1u);
}

// FP to a `dest_bits`-wide integer, keeping `fbits` fractional bits (a scale is only an exponent addend).
// ORDER matters: round to an integer FIRST, then range-check -- FCVTZU of -0.5 is 0 with Inexact and no
// Invalid, and a NaN becomes 0 before the check. The two exceptions are exclusive (FPToFixed's if/elsif):
// -1.5 saturates with Invalid ALONE, Inexact suppressed.
static uint64_t interp_fp_to_int(unsigned fmt, uint64_t bits, unsigned dest_bits, int is_signed, unsigned rmode,
                                 unsigned fbits) {
    bits = interp_fp_flush_input(bits, fmt);
    unsigned cls = interp_fp_class(bits, fmt);
    unsigned sign = (bits & interp_fp_sign_mask(fmt)) != 0;
    if (cls >= INTERP_FPC_QNAN) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return 0;
    }
    if (cls == INTERP_FPC_INF) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(sign, dest_bits, is_signed);
    }
    if (cls == INTERP_FPC_ZERO) return 0;

    unsigned mant = interp_fp_mant(fmt);
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << mant));
    int exponent = (int)(biased == 0 ? 1u : biased) - interp_fp_bias(fmt) - (int)mant + (int)fbits;

    uint64_t magnitude = 0;
    int inexact = 0, too_large = 0;
    if (exponent >= 0) {
        // Already an integer, so nothing is inexact -- but it may not fit 64 bits, let alone the destination.
        // exponent == 0 must be spelled out: `significand >> 64` is masked to a no-op by the host and made
        // every scaled conversion whose result lands exactly at 2^0 saturate.
        too_large = exponent >= 64 || (exponent > 0 && (significand >> (64 - (unsigned)exponent)) != 0);
        if (!too_large) magnitude = significand << (unsigned)exponent;
    } else {
        unsigned shift = (unsigned)(-exponent);
        int round_bit, sticky;
        if (shift >= 64) {
            round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
            sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
        } else {
            magnitude = significand >> shift;
            round_bit = (int)((significand >> (shift - 1u)) & 1u);
            sticky = shift > 1 && (significand & ((UINT64_C(1) << (shift - 1u)) - 1u)) != 0;
        }
        inexact = round_bit | sticky;
        if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(magnitude & 1u))) magnitude++;
    }
    // FPToFixed's exceptions are an if/elsif: a conversion that saturates raises Invalid and NOT Inexact.
    if (too_large) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(sign, dest_bits, is_signed);
    }
    if (!is_signed) {
        if (sign && magnitude != 0) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return 0;
        }
        if (!sign && dest_bits < 64 && magnitude > ((UINT64_C(1) << dest_bits) - 1u)) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return interp_fp_int_saturate(0, dest_bits, 0);
        }
        if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
        return sign ? UINT64_C(0) : magnitude;
    }
    uint64_t limit = UINT64_C(1) << (dest_bits - 1u);
    if (sign) {
        if (magnitude > limit) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return interp_fp_int_saturate(1, dest_bits, 1);
        }
        if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
        return UINT64_C(0) - magnitude;
    }
    if (magnitude >= limit) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(0, dest_bits, 1);
    }
    if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
    return magnitude;
}

static uint64_t interp_fp_from_int(unsigned fmt, uint64_t value, unsigned source_bits, int is_signed, unsigned rmode,
                                   unsigned fbits) {
    unsigned sign = 0;
    uint64_t magnitude;
    // Any width, not just 32: a 16-bit source (SCVTF Vd.8H, Vn.8H, #fbits) arrives zero-extended, and
    // testing only for 32 made every negative half-width input convert as if it were unsigned.
    uint64_t width_mask = source_bits >= 64 ? UINT64_MAX : ((UINT64_C(1) << source_bits) - 1u);
    value &= width_mask;
    if (is_signed) {
        uint64_t sign_bit = UINT64_C(1) << (source_bits - 1u);
        int64_t signed_value = (int64_t)((value ^ sign_bit) - sign_bit);
        sign = signed_value < 0;
        magnitude = sign ? (UINT64_C(0) - (uint64_t)signed_value) : (uint64_t)signed_value;
    } else {
        magnitude = value;
    }
    // Two statements: as two arguments of one call, `raised` may be read before the pack that fills it.
    unsigned raised = 0;
    uint64_t packed = interp_fp_pack(sign, magnitude, -(int)fbits, fmt, rmode, &raised);
    return interp_fp_postprocess(fmt, packed, raised);
}

static uint64_t interp_fp_expand_imm(unsigned fmt, uint64_t imm8) {
    unsigned exp_bits = interp_fp_width(fmt) - interp_fp_mant(fmt) - 1u;
    unsigned mant = interp_fp_mant(fmt);
    uint64_t sign = (imm8 >> 7) & 1u;
    uint64_t b = (imm8 >> 6) & 1u;
    uint64_t exponent = (b ? 0u : 1u) << (exp_bits - 1u);
    if (b) exponent |= (((UINT64_C(1) << (exp_bits - 3u)) - 1u) << 2);
    exponent |= (imm8 >> 4) & 3u;
    return (sign << (interp_fp_width(fmt) - 1u)) | (exponent << mant) | ((imm8 & 0xFu) << (mant - 4u));
}

// AdvSIMD saturation. FPSR.QC is cumulative and sticky: any clamped result sets it, only MSR FPSR clears.

// The 64-bit element form is real, so the wide case detects overflow and clamps by the WRAPPED sign rather
// than computing in a wider type.
static uint64_t interp_sqadd_element(uint64_t a, uint64_t b, unsigned size, int subtract) {
    unsigned esize = 8u << size;
    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
    if (esize < 64) {
        int64_t result = subtract ? x - y : x + y;
        int64_t max = (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u), min = -max - 1;
        if (result > max) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            result = max;
        } else if (result < min) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            result = min;
        }
        return (uint64_t)result & interp_element_mask(size);
    }
    int64_t result;
    int overflow = subtract ? __builtin_sub_overflow(x, y, &result) : __builtin_add_overflow(x, y, &result);
    if (overflow) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        result = result < 0 ? INT64_MAX : INT64_MIN;
    }
    return (uint64_t)result;
}

static uint64_t interp_uqadd_element(uint64_t a, uint64_t b, unsigned size, int subtract) {
    uint64_t mask = interp_element_mask(size);
    a &= mask;
    b &= mask;
    if (subtract) {
        if (a < b) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return 0;
        }
        return a - b;
    }
    uint64_t sum;
    // A 64-bit element can carry out of the type itself, so the carry is tested as well as the mask.
    if (__builtin_add_overflow(a, b, &sum) || sum > mask) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        return mask;
    }
    return sum;
}

// Saturating NARROWING. `size` is the DESTINATION width and the source element is twice it. SQXTN is
// signed->signed, UQXTN unsigned->unsigned, SQXTUN signed->UNSIGNED.
static uint64_t interp_sat_narrow(uint64_t element, unsigned size, int source_signed, int dest_signed) {
    unsigned esize = 8u << size;
    uint64_t mask = interp_element_mask(size);
    if (source_signed) {
        int64_t value = (int64_t)interp_element_sext(element, size + 1u);
        if (dest_signed) {
            int64_t max = (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u), min = -max - 1;
            if (value > max) {
                interp_fpsr_raise(INTERP_FPSR_QC);
                value = max;
            } else if (value < min) {
                interp_fpsr_raise(INTERP_FPSR_QC);
                value = min;
            }
            return (uint64_t)value & mask;
        }
        if (value < 0) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return 0;
        }
        if ((uint64_t)value > mask) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return mask;
        }
        return (uint64_t)value;
    }
    uint64_t value = element & interp_element_mask(size + 1u);
    if (value > mask) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        return mask;
    }
    return value;
}

// Saturating DOUBLING multiply. All three forms compute 2*a*b and differ only in what they keep, so they
// share one 128-bit product: at esize 32 the doubling itself overflows int64 for INT32_MIN * INT32_MIN
// (2 * 2^62 == 2^63), which is exactly the input that must saturate rather than wrap.
static __int128 interp_sqdmul_wide(uint64_t a, uint64_t b, unsigned size) {
    return (__int128)2 * (int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size);
}

static uint64_t interp_signed_sat(__int128 value, unsigned size) {
    __int128 max = ((__int128)1 << ((8u << size) - 1u)) - 1, min = -max - 1;
    if (value > max) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        value = max;
    } else if (value < min) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        value = min;
    }
    return (uint64_t)value & interp_element_mask(size);
}

// SQDMULL: the doubled product kept at DOUBLE width. SQDMLAL/SQDMLSL saturate this, then saturate the
// accumulate separately -- two independent chances to set QC.
static uint64_t interp_sqdmull_element(uint64_t a, uint64_t b, unsigned size) {
    return interp_signed_sat(interp_sqdmul_wide(a, b, size), size + 1u);
}

// SQDMULH keeps the HIGH half; SQRDMULH adds half an element first, so its tie rounds away from zero.
static uint64_t interp_sqdmulh_element(uint64_t a, uint64_t b, unsigned size, int rounding) {
    unsigned esize = 8u << size;
    __int128 product = interp_sqdmul_wide(a, b, size);
    if (rounding) product += (__int128)1 << (esize - 1u);
    return interp_signed_sat(product >> esize, size);
}

// SQRDMLAH/SQRDMLSH (FEAT_RDM): the accumulator joins the doubled product SHIFTED UP by esize, so the
// rounding constant and the single saturation see the whole expression -- not a saturated product plus a
// saturated add, which is what SQDMLAL does and what gives the two forms different results.
static uint64_t interp_sqrdmlah_element(uint64_t accumulator, uint64_t a, uint64_t b, unsigned size, int subtract) {
    unsigned esize = 8u << size;
    __int128 product = interp_sqdmul_wide(a, b, size);
    __int128 total = ((__int128)(int64_t)interp_element_sext(accumulator, size) << esize) +
                     (subtract ? -product : product) + ((__int128)1 << (esize - 1u));
    return interp_signed_sat(total >> esize, size);
}

// AES, SHA1 and SHA256. hwcap 0x1fb advertises HWCAP_AES/PMULL/SHA1/SHA2, so guest ifunc resolvers pick
// these paths. The tables are GENERATED (GF(2^8) inverse then the FIPS-197 affine transform).
static const uint8_t interp_aes_sbox[256] = {
    0x63, 0x7C, 0x77, 0x7B, 0xF2, 0x6B, 0x6F, 0xC5, 0x30, 0x01, 0x67, 0x2B, 0xFE, 0xD7, 0xAB, 0x76, 0xCA, 0x82, 0xC9,
    0x7D, 0xFA, 0x59, 0x47, 0xF0, 0xAD, 0xD4, 0xA2, 0xAF, 0x9C, 0xA4, 0x72, 0xC0, 0xB7, 0xFD, 0x93, 0x26, 0x36, 0x3F,
    0xF7, 0xCC, 0x34, 0xA5, 0xE5, 0xF1, 0x71, 0xD8, 0x31, 0x15, 0x04, 0xC7, 0x23, 0xC3, 0x18, 0x96, 0x05, 0x9A, 0x07,
    0x12, 0x80, 0xE2, 0xEB, 0x27, 0xB2, 0x75, 0x09, 0x83, 0x2C, 0x1A, 0x1B, 0x6E, 0x5A, 0xA0, 0x52, 0x3B, 0xD6, 0xB3,
    0x29, 0xE3, 0x2F, 0x84, 0x53, 0xD1, 0x00, 0xED, 0x20, 0xFC, 0xB1, 0x5B, 0x6A, 0xCB, 0xBE, 0x39, 0x4A, 0x4C, 0x58,
    0xCF, 0xD0, 0xEF, 0xAA, 0xFB, 0x43, 0x4D, 0x33, 0x85, 0x45, 0xF9, 0x02, 0x7F, 0x50, 0x3C, 0x9F, 0xA8, 0x51, 0xA3,
    0x40, 0x8F, 0x92, 0x9D, 0x38, 0xF5, 0xBC, 0xB6, 0xDA, 0x21, 0x10, 0xFF, 0xF3, 0xD2, 0xCD, 0x0C, 0x13, 0xEC, 0x5F,
    0x97, 0x44, 0x17, 0xC4, 0xA7, 0x7E, 0x3D, 0x64, 0x5D, 0x19, 0x73, 0x60, 0x81, 0x4F, 0xDC, 0x22, 0x2A, 0x90, 0x88,
    0x46, 0xEE, 0xB8, 0x14, 0xDE, 0x5E, 0x0B, 0xDB, 0xE0, 0x32, 0x3A, 0x0A, 0x49, 0x06, 0x24, 0x5C, 0xC2, 0xD3, 0xAC,
    0x62, 0x91, 0x95, 0xE4, 0x79, 0xE7, 0xC8, 0x37, 0x6D, 0x8D, 0xD5, 0x4E, 0xA9, 0x6C, 0x56, 0xF4, 0xEA, 0x65, 0x7A,
    0xAE, 0x08, 0xBA, 0x78, 0x25, 0x2E, 0x1C, 0xA6, 0xB4, 0xC6, 0xE8, 0xDD, 0x74, 0x1F, 0x4B, 0xBD, 0x8B, 0x8A, 0x70,
    0x3E, 0xB5, 0x66, 0x48, 0x03, 0xF6, 0x0E, 0x61, 0x35, 0x57, 0xB9, 0x86, 0xC1, 0x1D, 0x9E, 0xE1, 0xF8, 0x98, 0x11,
    0x69, 0xD9, 0x8E, 0x94, 0x9B, 0x1E, 0x87, 0xE9, 0xCE, 0x55, 0x28, 0xDF, 0x8C, 0xA1, 0x89, 0x0D, 0xBF, 0xE6, 0x42,
    0x68, 0x41, 0x99, 0x2D, 0x0F, 0xB0, 0x54, 0xBB, 0x16,
};

static const uint8_t interp_aes_inv_sbox[256] = {
    0x52, 0x09, 0x6A, 0xD5, 0x30, 0x36, 0xA5, 0x38, 0xBF, 0x40, 0xA3, 0x9E, 0x81, 0xF3, 0xD7, 0xFB, 0x7C, 0xE3, 0x39,
    0x82, 0x9B, 0x2F, 0xFF, 0x87, 0x34, 0x8E, 0x43, 0x44, 0xC4, 0xDE, 0xE9, 0xCB, 0x54, 0x7B, 0x94, 0x32, 0xA6, 0xC2,
    0x23, 0x3D, 0xEE, 0x4C, 0x95, 0x0B, 0x42, 0xFA, 0xC3, 0x4E, 0x08, 0x2E, 0xA1, 0x66, 0x28, 0xD9, 0x24, 0xB2, 0x76,
    0x5B, 0xA2, 0x49, 0x6D, 0x8B, 0xD1, 0x25, 0x72, 0xF8, 0xF6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xD4, 0xA4, 0x5C, 0xCC,
    0x5D, 0x65, 0xB6, 0x92, 0x6C, 0x70, 0x48, 0x50, 0xFD, 0xED, 0xB9, 0xDA, 0x5E, 0x15, 0x46, 0x57, 0xA7, 0x8D, 0x9D,
    0x84, 0x90, 0xD8, 0xAB, 0x00, 0x8C, 0xBC, 0xD3, 0x0A, 0xF7, 0xE4, 0x58, 0x05, 0xB8, 0xB3, 0x45, 0x06, 0xD0, 0x2C,
    0x1E, 0x8F, 0xCA, 0x3F, 0x0F, 0x02, 0xC1, 0xAF, 0xBD, 0x03, 0x01, 0x13, 0x8A, 0x6B, 0x3A, 0x91, 0x11, 0x41, 0x4F,
    0x67, 0xDC, 0xEA, 0x97, 0xF2, 0xCF, 0xCE, 0xF0, 0xB4, 0xE6, 0x73, 0x96, 0xAC, 0x74, 0x22, 0xE7, 0xAD, 0x35, 0x85,
    0xE2, 0xF9, 0x37, 0xE8, 0x1C, 0x75, 0xDF, 0x6E, 0x47, 0xF1, 0x1A, 0x71, 0x1D, 0x29, 0xC5, 0x89, 0x6F, 0xB7, 0x62,
    0x0E, 0xAA, 0x18, 0xBE, 0x1B, 0xFC, 0x56, 0x3E, 0x4B, 0xC6, 0xD2, 0x79, 0x20, 0x9A, 0xDB, 0xC0, 0xFE, 0x78, 0xCD,
    0x5A, 0xF4, 0x1F, 0xDD, 0xA8, 0x33, 0x88, 0x07, 0xC7, 0x31, 0xB1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xEC, 0x5F, 0x60,
    0x51, 0x7F, 0xA9, 0x19, 0xB5, 0x4A, 0x0D, 0x2D, 0xE5, 0x7A, 0x9F, 0x93, 0xC9, 0x9C, 0xEF, 0xA0, 0xE0, 0x3B, 0x4D,
    0xAE, 0x2A, 0xF5, 0xB0, 0xC8, 0xEB, 0xBB, 0x3C, 0x83, 0x53, 0x99, 0x61, 0x17, 0x2B, 0x04, 0x7E, 0xBA, 0x77, 0xD6,
    0x26, 0xE1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0C, 0x7D,
};

// GF(2^8) multiply by x, modulo the AES polynomial 0x11B.
static uint8_t interp_aes_xtime(uint8_t a) {
    return (uint8_t)((a & 0x80u) ? (uint8_t)((a << 1) ^ 0x1Bu) : (uint8_t)(a << 1));
}

static uint8_t interp_aes_mul(uint8_t a, uint8_t b) {
    uint8_t result = 0;
    for (unsigned bit = 0; bit < 8u; bit++) {
        if (b & 1u) result ^= a;
        b = (uint8_t)(b >> 1);
        a = interp_aes_xtime(a);
    }
    return result;
}

// The state is column-major (byte 4*column + row), so (column, row) reads from (column + row) mod 4. A
// reversed direction still round-trips through AESE/AESD: only test vectors catch it.
static void interp_aes_shift_rows(const uint8_t *in, uint8_t *out, int inverse) {
    for (unsigned column = 0; column < 4u; column++)
        for (unsigned row = 0; row < 4u; row++) {
            unsigned source = 4u * ((column + row) & 3u) + row, destination = 4u * column + row;
            if (inverse)
                out[source] = in[destination];
            else
                out[destination] = in[source];
        }
}

static void interp_aes_mix_columns(const uint8_t *in, uint8_t *out, int inverse) {
    static const uint8_t forward[4] = {2, 3, 1, 1}, backward[4] = {14, 11, 13, 9};
    const uint8_t *coefficient = inverse ? backward : forward;
    for (unsigned column = 0; column < 4u; column++)
        for (unsigned row = 0; row < 4u; row++) {
            uint8_t value = 0;
            for (unsigned term = 0; term < 4u; term++)
                value ^= interp_aes_mul(in[4u * column + ((row + term) & 3u)], coefficient[term]);
            out[4u * column + row] = value;
        }
}

// ---- SHA helpers ----
static uint32_t interp_ror32_bits(uint32_t value, unsigned amount) {
    amount &= 31u;
    return amount ? ((value >> amount) | (value << (32u - amount))) : value;
}

static uint32_t interp_rol32_bits(uint32_t value, unsigned amount) {
    return interp_ror32_bits(value, (32u - (amount & 31u)) & 31u);
}

static uint32_t interp_sha_choose(uint32_t x, uint32_t y, uint32_t z) {
    return (((y ^ z) & x) ^ z);
}

static uint32_t interp_sha_majority(uint32_t x, uint32_t y, uint32_t z) {
    return ((x & y) | ((x | y) & z));
}

static uint32_t interp_sha_parity(uint32_t x, uint32_t y, uint32_t z) {
    return (x ^ y ^ z);
}

// The low element as a scalar of `fmt`. A scalar FP write ZEROES bits [127:N] (interp_vec_write's q == 0).
static uint64_t interp_fp_read(const struct cpu *cpu, int reg, unsigned fmt) {
    interp_vec value = interp_vec_read(cpu, reg);
    return interp_vec_element(&value, fmt + 1u, 0);
}

static void interp_fp_write(struct cpu *cpu, int reg, unsigned fmt, uint64_t bits) {
    interp_vec result;
    memset(result.byte, 0, sizeof result.byte);
    interp_vec_set_element(&result, fmt + 1u, 0, bits);
    interp_vec_write(cpu, reg, result, 0);
}

// The scalar FP encoding space: bits[30:29] == 00, bits[28:24] == 11110 (11111 for the three-source
// multiply-adds). bit30 separates it from the AdvSIMD SCALAR boxes (bits[31:30] == 01); bit31 is `sf` in the
// conversion boxes and M, which must be 0, elsewhere.
static int interp_exec_fp_multiply_fixed(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned type = (insn >> 22) & 3u, sf = (insn >> 31) & 1u;
    unsigned fmt = INTERP_FP_S;

    // ---- 3-source: FMADD / FMSUB / FNMADD / FNMSUB ----
    if ((insn & 0x7F000000u) == 0x1F000000u) {
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 3-source with M set");
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 3-source unallocated ptype");
        unsigned o1 = (insn >> 21) & 1u, o0 = (insn >> 15) & 1u;
        int ra = (int)((insn >> 10) & 31);
        uint64_t addend = interp_fp_read(cpu, ra, fmt);
        uint64_t left = interp_fp_read(cpu, rn, fmt), right = interp_fp_read(cpu, rm, fmt);
        //   FMADD =  Ra + Rn*Rm    FMSUB  =  Ra - Rn*Rm    FNMADD = -Ra - Rn*Rm    FNMSUB = -Ra + Rn*Rm
        // One FPMulAdd; the flip is a literal sign-bit toggle (FPNeg), so a propagated NaN's sign flips.
        uint64_t sign = interp_fp_sign_mask(fmt);
        if (o1) addend ^= sign;
        if (o1 != o0) left ^= sign;
        interp_fp_write(cpu, rd, fmt, interp_fp_muladd(fmt, addend, left, right));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (!((insn >> 21) & 1u)) {
        // ---- FP <-> fixed-point ----
        // fbits = 64 - scale, and a 32-bit general register cannot name more than 32 fractional bits.
        unsigned rmode = (insn >> 19) & 3u, opcode = (insn >> 16) & 7u, scale = (insn >> 10) & 0x3Fu;
        unsigned fbits = 64u - scale;
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- fixed-point conversion unallocated ptype");
        if (!sf && scale < 32u)
            return interp_undefined(cpu, insn, "scalar FP -- 32-bit fixed-point conversion with scale < 32");
        if (rmode == 0 && (opcode == 2 || opcode == 3)) { // SCVTF / UCVTF (fixed-point)
            uint64_t value = interp_gpr(cpu, rn);
            interp_fp_write(
                cpu, rd, fmt,
                interp_fp_from_int(fmt, value, sf ? 64u : 32u, opcode == 2, INTERP_FPCR_RMODE(g_interp_fpcr), fbits));
        } else if (rmode == 3 && opcode <= 1) { // FCVTZS / FCVTZU (fixed-point)
            uint64_t out =
                interp_fp_to_int(fmt, interp_fp_read(cpu, rn, fmt), sf ? 64u : 32u, opcode == 0, INTERP_RM_RZ, fbits);
            if (sf)
                interp_set_gpr(cpu, rd, out);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)out);
        } else {
            return interp_undefined(cpu, insn, "scalar FP -- unallocated fixed-point conversion");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "scalar FP -- unallocated multiply/fixed encoding");
}

// Conditional compare, binary arithmetic, and conditional select.
static int interp_exec_fp_binary(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned type = (insn >> 22) & 3u, sf = (insn >> 31) & 1u;
    unsigned fmt = INTERP_FP_S;

    // bit21 == 1. The rest are selected by bits[11:10], the four 00 boxes most specific first.
    unsigned op_low = (insn >> 10) & 3u;

    if (op_low == 1) { // ---- FCCMP / FCCMPE ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- FCCMP with M set");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FCCMP ptype");
        unsigned cond = (insn >> 12) & 0xFu, quiet_signals = (insn >> 4) & 1u;
        if (interp_cond_holds(cpu, cond))
            interp_fp_compare(cpu, fmt, interp_fp_read(cpu, rn, fmt), interp_fp_read(cpu, rm, fmt), (int)quiet_signals);
        else
            // No comparison happens and no exception can be raised; NZCV comes from the insn's nzcv field.
            cpu->nzcv = ((uint64_t)(insn & 0xFu)) << 28;
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (op_low == 2) { // ---- 2-source ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 2-source with M set");
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 2-source unallocated ptype");
        unsigned opcode = (insn >> 12) & 0xFu;
        uint64_t a = interp_fp_read(cpu, rn, fmt), b = interp_fp_read(cpu, rm, fmt), out;
        switch (opcode) {
        case 0: out = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b); break; // FMUL
        case 1: out = interp_fp_arith(fmt, INTERP_FPOP_DIV, a, b); break; // FDIV
        case 2: out = interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b); break; // FADD
        case 3: out = interp_fp_arith(fmt, INTERP_FPOP_SUB, a, b); break; // FSUB
        case 4: out = interp_fp_minmax(fmt, a, b, 1, 0); break;           // FMAX
        case 5: out = interp_fp_minmax(fmt, a, b, 0, 0); break;           // FMIN
        case 6: out = interp_fp_minmax(fmt, a, b, 1, 1); break;           // FMAXNM
        case 7: out = interp_fp_minmax(fmt, a, b, 0, 1); break;           // FMINNM
        case 8:
            // FNMUL negates the PRODUCT, after rounding and NaN propagation, so a propagated NaN flips too.
            out = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b) ^ interp_fp_sign_mask(fmt);
            break;
        default: return interp_undefined(cpu, insn, "scalar FP -- unallocated 2-source opcode");
        }
        interp_fp_write(cpu, rd, fmt, out);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (op_low == 3) { // ---- FCSEL ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- FCSEL with M set");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FCSEL ptype");
        unsigned cond = (insn >> 12) & 0xFu;
        // A pure register copy: no flushing, no exceptions -- a signalling NaN passes through.
        interp_fp_write(cpu, rd, fmt,
                        interp_cond_holds(cpu, cond) ? interp_fp_read(cpu, rn, fmt) : interp_fp_read(cpu, rm, fmt));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "scalar FP -- unallocated binary encoding");
}

// Moves and conversions between scalar floating-point and integer registers.
static int interp_exec_fp_integer(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31);
    unsigned type = (insn >> 22) & 3u, sf = (insn >> 31) & 1u;
    unsigned fmt = INTERP_FP_S;

    if ((insn & 0x0000FC00u) == 0) { // ---- FP <-> integer ----
        unsigned rmode = (insn >> 19) & 3u, opcode = (insn >> 16) & 7u;
        if (rmode == 0 && opcode == 6) { // FMOV to a general register (Vn's low element -> Rd)
            interp_vec source = interp_vec_read(cpu, rn);
            if (type == 0 && !sf) // FMOV Wd, Sn
                interp_set_gpr32(cpu, rd, (uint32_t)interp_vec_element(&source, 2, 0));
            else if (type == 1 && sf) // FMOV Xd, Dn
                interp_set_gpr(cpu, rd, interp_vec_element(&source, 3, 0));
            else if (type == 3 && !sf) // FMOV Wd, Hn
                interp_set_gpr32(cpu, rd, (uint32_t)interp_vec_element(&source, 1, 0));
            else
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV to general register");
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 0 && opcode == 7) { // FMOV from a general register (Rn -> Vd's low element)
            if (type == 0 && !sf)        // FMOV Sd, Wn
                interp_fp_write(cpu, rd, INTERP_FP_S, interp_gpr(cpu, rn) & 0xFFFFFFFFu);
            else if (type == 1 && sf) // FMOV Dd, Xn
                interp_fp_write(cpu, rd, INTERP_FP_D, interp_gpr(cpu, rn));
            else if (type == 3 && !sf) // FMOV Hd, Wn
                interp_fp_write(cpu, rd, INTERP_FP_H, interp_gpr(cpu, rn) & 0xFFFFu);
            else
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV from general register");
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 1 && opcode == 6 && type == 2 && sf) { // FMOV Xd, Vn.D[1]
            interp_vec source = interp_vec_read(cpu, rn);
            interp_set_gpr(cpu, rd, interp_vec_element(&source, 3, 1));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 1 && opcode == 7 && type == 2 && sf) { // FMOV Vd.D[1], Xn
            interp_vec destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, 3, 1, interp_gpr(cpu, rn));
            interp_vec_write(cpu, rd, destination, 1); // single-lane write: keep the low half
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 3 && opcode == 6 && type == 1 && !sf) {
            // FJCVTZS: double -> int32 with JavaScript ToInt32 semantics. It WRAPS modulo 2^32 rather than
            // saturating, and Z reports exactness (1 only if nothing was lost).
            uint64_t bits = interp_fp_flush_input(interp_fp_read(cpu, rn, INTERP_FP_D), INTERP_FP_D);
            unsigned cls = interp_fp_class(bits, INTERP_FP_D);
            unsigned exact = 1;
            uint64_t result = 0;
            if (cls >= INTERP_FPC_INF) { // NaN or infinity: Invalid, result 0, not exact
                interp_fpsr_raise(INTERP_FPSR_IOC);
                exact = 0;
            } else if (cls != INTERP_FPC_ZERO) {
                // A 64-bit signed destination so nothing saturates; exactness then comes off the flags.
                uint64_t before = g_interp_fpsr;
                uint64_t wide = interp_fp_to_int(INTERP_FP_D, bits, 64u, 1, INTERP_RM_RZ, 0);
                unsigned raised = (unsigned)((g_interp_fpsr & ~before) & (INTERP_FPSR_IXC | INTERP_FPSR_IOC));
                if (raised) exact = 0;
                if ((int64_t)wide != (int64_t)(int32_t)(uint32_t)wide) {
                    interp_fpsr_raise(INTERP_FPSR_IOC);
                    exact = 0;
                }
                result = (uint64_t)(uint32_t)wide;
            } else if (bits & interp_fp_sign_mask(INTERP_FP_D)) {
                exact = 0; // -0.0 converts to +0, which ToInt32 does not consider exact
            }
            interp_set_gpr32(cpu, rd, (uint32_t)result);
            interp_set_flags(cpu, 0, exact, 0, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- integer conversion unallocated ptype");
        if (opcode == 2 || opcode == 3) { // SCVTF / UCVTF
            if (rmode != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated SCVTF/UCVTF rmode");
            interp_fp_write(cpu, rd, fmt,
                            interp_fp_from_int(fmt, interp_gpr(cpu, rn), sf ? 64u : 32u, opcode == 2,
                                               INTERP_FPCR_RMODE(g_interp_fpcr), 0));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opcode <= 1 || opcode == 4 || opcode == 5) {
            // rmode picks the rounding for opcode 0/1; opcode 4/5 is FCVTA, whose ties-away has no FPCR code.
            unsigned convert_mode;
            if (opcode >= 4) {
                if (rmode != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated FCVTA rmode");
                convert_mode = INTERP_RM_RA;
            } else {
                static const unsigned by_rmode[4] = {INTERP_RM_RN, INTERP_RM_RP, INTERP_RM_RM, INTERP_RM_RZ};
                convert_mode = by_rmode[rmode];
            }
            uint64_t out = interp_fp_to_int(fmt, interp_fp_read(cpu, rn, fmt), sf ? 64u : 32u, (opcode & 1u) == 0,
                                            convert_mode, 0);
            if (sf)
                interp_set_gpr(cpu, rd, out);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)out);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        return interp_undefined(cpu, insn, "scalar FP -- unallocated integer conversion");
    }

    return interp_undefined(cpu, insn, "scalar FP -- unallocated integer encoding");
}

// Unary arithmetic, compare, and immediate forms.
static int interp_exec_fp_unary(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned type = (insn >> 22) & 3u, sf = (insn >> 31) & 1u;
    unsigned fmt = INTERP_FP_S;

    if ((insn & 0x00007C00u) == 0x00004000u) { // ---- 1-source ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 1-source with M set");
        unsigned opcode = (insn >> 15) & 0x3Fu;
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 1-source unallocated ptype");
        // FCVT names its DESTINATION in the low two opcode bits and its source in ptype, so split it first.
        if ((opcode & 0x3Cu) == 0x04u) {
            // Opcode 000110 shares this box but is BFCVT (FEAT_BF16), single -> BFloat16, not an FCVT.
            if (opcode == 0x06u) {
                // (ftype 01, opcode 000110) IS the encoding; the operand is V[n,32] anyway, so `fmt` above
                // does not apply. ARM ARM FPConvertBF: bf16 is the TOP HALF of the binary32 encoding, so the
                // whole conversion is a rounding of the discarded low 16 bits -- exact for normals,
                // subnormals and the overflow-to-infinity carry alike. FPCR.RMode selects it; only the
                // default tie-to-even is implemented, so the other three report rather than guess.
                if (type != 1u) return interp_undefined(cpu, insn, "scalar FP -- BFCVT unallocated ptype");
                if (INTERP_FPCR_RMODE(g_interp_fpcr) != 0u)
                    return interp_undefined(cpu, insn, "scalar FP -- BFCVT with a non-default FPCR.RMode");
                uint32_t bits = (uint32_t)interp_fp_flush_input(interp_fp_read(cpu, rn, INTERP_FP_S), INTERP_FP_S);
                unsigned cls = interp_fp_class(bits, INTERP_FP_S);
                uint64_t out;
                if (cls >= INTERP_FPC_QNAN) {
                    out = UINT64_C(0x7FC0);
                } else if (cls == INTERP_FPC_INF || cls == INTERP_FPC_ZERO) {
                    out = bits >> 16;
                } else {
                    // Tie-to-even: add half an ulp, plus one more when the kept bit is already odd.
                    uint32_t rounded = bits + 0x7FFFu + ((bits >> 16) & 1u);
                    if (bits & 0xFFFFu) {
                        unsigned raised = INTERP_FPSR_IXC;
                        if ((rounded & 0x7F800000u) == 0x7F800000u) raised |= INTERP_FPSR_OFC;
                        // bf16 shares binary32's exponent field, so the result is tiny exactly when the source
                        // was -- tested BEFORE rounding, which is AArch64's tininess rule.
                        if ((bits & 0x7F800000u) == 0) raised |= INTERP_FPSR_UFC;
                        interp_fpsr_raise(raised);
                    }
                    out = rounded >> 16;
                }
                interp_fp_write(cpu, rd, INTERP_FP_H, out);
                cpu->pc = gpc + 4;
                return INTERP_NEXT;
            }
            unsigned to;
            if (!interp_fp_type_fmt(opcode & 3u, &to) || to == fmt)
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FCVT destination");
            if ((to == INTERP_FP_H || fmt == INTERP_FP_H) && INTERP_FPCR_AHP(g_interp_fpcr))
                return interp_undefined(cpu, insn, "scalar FP -- FCVT with FPCR.AHP (alternative half format)");
            interp_fp_write(cpu, rd, to, interp_fp_convert(fmt, to, interp_fp_read(cpu, rn, fmt)));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        uint64_t a = interp_fp_read(cpu, rn, fmt), out;
        switch (opcode) {
        case 0x00: out = a; break;                             // FMOV (register): a pure bit copy
        case 0x01: out = a & ~interp_fp_sign_mask(fmt); break; // FABS: clears the sign bit, nothing else
        case 0x02: out = a ^ interp_fp_sign_mask(fmt); break;  // FNEG: a sign-bit toggle
        case 0x03:
            out = interp_fp_sqrt(fmt, a);
            break; // FSQRT
        // The FRINT family differs only in the mode and in whether a change is Inexact; only FRINTX is.
        case 0x08: out = interp_fp_round_integral(fmt, a, INTERP_RM_RN, 0); break;                     // FRINTN
        case 0x09: out = interp_fp_round_integral(fmt, a, INTERP_RM_RP, 0); break;                     // FRINTP
        case 0x0A: out = interp_fp_round_integral(fmt, a, INTERP_RM_RM, 0); break;                     // FRINTM
        case 0x0B: out = interp_fp_round_integral(fmt, a, INTERP_RM_RZ, 0); break;                     // FRINTZ
        case 0x0C: out = interp_fp_round_integral(fmt, a, INTERP_RM_RA, 0); break;                     // FRINTA
        case 0x0E: out = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), 1); break; // FRINTX
        case 0x0F: out = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), 0); break; // FRINTI
        default: return interp_undefined(cpu, insn, "scalar FP -- unimplemented 1-source opcode");
        }
        interp_fp_write(cpu, rd, fmt, out);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x00003C00u) == 0x00002000u) { // ---- FCMP / FCMPE ----
        if (sf || ((insn >> 14) & 3u) != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated compare");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- compare ptype");
        unsigned opcode2 = insn & 0x1Fu;
        if (opcode2 & 7u) return interp_undefined(cpu, insn, "scalar FP -- unallocated compare opcode2");
        // opcode2<4> is E (Invalid for a quiet NaN too); opcode2<3> selects the compare-with-zero form.
        int quiet_signals = (opcode2 >> 4) & 1;
        uint64_t b = (opcode2 & 8u) ? UINT64_C(0) : interp_fp_read(cpu, rm, fmt);
        interp_fp_compare(cpu, fmt, interp_fp_read(cpu, rn, fmt), b, quiet_signals);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x00001C00u) == 0x00001000u) { // ---- FMOV (immediate) ----
        if (sf || ((insn >> 5) & 0x1Fu) != 0)
            return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV immediate");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FMOV immediate ptype");
        interp_fp_write(cpu, rd, fmt, interp_fp_expand_imm(fmt, (insn >> 13) & 0xFFu));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "scalar FP -- unallocated unary encoding");
}

static int interp_exec_fp_scalar(struct cpu *cpu, uint32_t insn) {
    if ((insn & 0x7F000000u) == 0x1F000000u || ((insn >> 21) & 1u) == 0)
        return interp_exec_fp_multiply_fixed(cpu, insn);
    unsigned op_low = (insn >> 10) & 3u;
    if (op_low != 0) return interp_exec_fp_binary(cpu, insn);
    if ((insn & 0x0000FC00u) == 0) return interp_exec_fp_integer(cpu, insn);
    return interp_exec_fp_unary(cpu, insn);
}

// Scalar floating-point and Advanced SIMD.
// The subset guests actually reach. Reported, not implemented: BFloat16, reciprocal estimates,
// saturating-doubling multiplies, by-element forms, SVE.
