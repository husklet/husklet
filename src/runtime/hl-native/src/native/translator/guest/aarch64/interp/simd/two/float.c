static int interp_simd_two_register_float(struct cpu *cpu, uint32_t insn, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31);
    unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
    interp_vec source = interp_vec_read(cpu, rn), result;
    memset(result.byte, 0, sizeof result.byte);
    // the floating-point members (opcodes 01100..01111, >= 10110)
    // `size` is not an element width: bit23 is an operation selector, bit22 is `sz`.
    if ((opcode >= 0x0Cu && opcode <= 0x0Fu) || opcode >= 0x16u) {
        unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
        unsigned element = fmt + 1u;
        uint64_t saved_nzcv = cpu->nzcv; // see the note in the three-same FP block
        // FCVTL/FCVTN change the element width; sz names the NARROW format (0 half, 1 single).
        if (opcode == 0x16u || opcode == 0x17u) {
            // FCVTXN/FCVTXN2 is FCVTN with FPRounding_ODD, and exists only D -> S. U elsewhere in this
            // pair spells the FEAT_FP8 widenings (F1CVTL/F2CVTL/BF1CVTL) and bit23 the BF16 narrowings,
            // which were reaching FCVTL's code; there is no scalar FCVTL/FCVTN.
            unsigned odd = u && opcode == 0x16u && (size & 1u);
            if (high || (u && !odd) || (scalar && !odd))
                return interp_undefined(cpu, insn,
                                        "AdvSIMD two-reg misc -- BFCVTN/F1CVTL/F2CVTL/BF1CVTL or "
                                        "unallocated FCVTL/FCVTN/FCVTXN form");
            unsigned narrow = (size & 1u) ? INTERP_FP_S : INTERP_FP_H, wide = narrow + 1u;
            // The narrow side is 64 bits of elements; Q picks the half.
            unsigned narrow_lanes = narrow == INTERP_FP_S ? 2u : 4u;
            if (opcode == 0x17u) { // FCVTL / FCVTL2
                for (unsigned lane = 0; lane < narrow_lanes; lane++) {
                    uint64_t element_bits = interp_vec_element(&source, narrow + 1u, q ? lane + narrow_lanes : lane);
                    interp_vec_set_element(&result, wide + 1u, lane, interp_fp_convert(narrow, wide, element_bits));
                }
                interp_vec_write(cpu, rd, result, 1);
            } else { // FCVTN / FCVTN2 / FCVTXN / FCVTXN2
                interp_vec packed;
                memset(packed.byte, 0, sizeof packed.byte);
                for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                    uint64_t element_bits = interp_vec_element(&source, wide + 1u, lane);
                    interp_vec_set_element(&packed, narrow + 1u, lane,
                                           odd ? interp_fp_convert_odd(element_bits)
                                               : interp_fp_convert(wide, narrow, element_bits));
                }
                if (!q) {
                    interp_vec_write(cpu, rd, packed, 0);
                } else {
                    interp_vec destination = interp_vec_read(cpu, rd);
                    memcpy(destination.byte + 8, packed.byte, 8);
                    interp_vec_write(cpu, rd, destination, 1);
                }
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (fmt == INTERP_FP_D && !q && !scalar)
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 2D form requires Q");
        // Per width, not interp_vec_lanes(element, q): a derived `element` makes the optimiser warn.
        unsigned fp_lanes = scalar ? 1u : (element == 3u ? (q ? 2u : 1u) : (q ? 4u : 2u));
        for (unsigned lane = 0; lane < fp_lanes; lane++) {
            uint64_t a = interp_vec_element(&source, element, lane), value;
            uint64_t all_ones = interp_element_mask(element);
            if (opcode >= 0x0Cu && opcode <= 0x0Fu) {
                // Compare-against-zero; FABS/FNEG at 01111.
                if (opcode == 0x0Fu) {
                    value = u ? (a ^ interp_fp_sign_mask(fmt)) : (a & ~interp_fp_sign_mask(fmt));
                } else {
                    // Only FCMEQ is FPCompareEQ; FPCompareGE/GT/LE/LT raise Invalid for a QUIET NaN too.
                    interp_fp_compare(cpu, fmt, a, 0, !(opcode == 0x0Du && !u));
                    int ordered = !(interp_flag_c(cpu) && interp_flag_v(cpu));
                    int zero = interp_flag_z(cpu) != 0, negative = interp_flag_n(cpu) != 0;
                    int holds;
                    if (opcode == 0x0Cu)
                        holds = ordered && (u ? (!negative) : (!negative && !zero)); // FCMGE / FCMGT
                    else if (opcode == 0x0Du)
                        holds = ordered && (u ? (negative || zero) : zero); // FCMLE / FCMEQ
                    else
                        holds = ordered && negative && !u; // FCMLT
                    if (opcode == 0x0Eu && u)
                        return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated FP compare");
                    value = holds ? all_ones : UINT64_C(0);
                }
            } else {
                switch (opcode) {
                case 0x18: // FRINTN / FRINTP; FRINTA / FRINTX under U
                    value =
                        interp_fp_round_integral(fmt, a, u ? INTERP_RM_RA : (high ? INTERP_RM_RP : INTERP_RM_RN), 0);
                    if (u && high) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated FRINT");
                    break;
                case 0x19:
                    // FRINTM/FRINTZ (U == 0), FRINTX/FRINTI (U == 1). Only FRINTX reports Inexact.
                    if (u)
                        value = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), high ? 0 : 1);
                    else
                        value = interp_fp_round_integral(fmt, a, high ? INTERP_RM_RZ : INTERP_RM_RM, 0);
                    break;
                case 0x1A: // FCVTNS/FCVTNU or FCVTPS/FCVTPU
                    value = interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RP : INTERP_RM_RN, 0);
                    break;
                case 0x1B: // FCVTMS/FCVTMU or FCVTZS/FCVTZU
                    value = interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RZ : INTERP_RM_RM, 0);
                    break;
                case 0x1C: // FCVTAS/FCVTAU at bit23 clear, URECPE/URSQRTE at set (.2S/.4S only)
                    if (high) {
                        if (fmt != INTERP_FP_S || scalar)
                            return interp_undefined(cpu, insn,
                                                    "AdvSIMD two-reg misc -- unallocated URECPE/URSQRTE form");
                        value = interp_uint_recip_estimate(a, u);
                    } else {
                        value = interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, INTERP_RM_RA, 0);
                    }
                    break;
                case 0x1D: // SCVTF/UCVTF at bit23 clear, FRECPE/FRSQRTE at set
                    if (high)
                        value = u ? interp_fp_rsqrt_estimate(fmt, a) : interp_fp_recip_estimate(fmt, a);
                    else
                        value =
                            interp_fp_from_int(fmt, a, interp_fp_width(fmt), !u, INTERP_FPCR_RMODE(g_interp_fpcr), 0);
                    break;
                case 0x1F: // FSQRT is the VECTOR U == 1 form, FRECPX the SCALAR U == 0 one: allocated
                           // exactly when u != scalar. bit23 clear is FRINT32Z/FRINT64Z/FRINT32X/FRINT64X
                           // (FEAT_FRINTTS), which shares this opcode and was being executed as FSQRT.
                    if (!high || (u != 0) == (scalar != 0))
                        return interp_undefined(cpu, insn,
                                                "AdvSIMD two-reg misc -- FRINT32Z/FRINT64Z/FRINT32X/FRINT64X "
                                                "or unallocated opcode 11111");
                    value = u ? interp_fp_sqrt(fmt, a) : interp_fp_recpx(fmt, a);
                    break;
                default: return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unimplemented FP opcode");
                }
            }
            interp_vec_set_element(&result, element, lane, value);
        }
        cpu->nzcv = saved_nzcv;
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    return INTERP_SIMD_UNHANDLED;
}
