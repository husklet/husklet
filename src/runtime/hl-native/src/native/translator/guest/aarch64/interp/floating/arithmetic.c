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
