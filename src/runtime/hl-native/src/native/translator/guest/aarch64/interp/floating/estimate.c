
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
