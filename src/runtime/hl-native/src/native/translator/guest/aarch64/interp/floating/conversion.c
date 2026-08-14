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
