#include "floating/arithmetic.c"
#include "floating/estimate.c"
#include "floating/conversion.c"

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
