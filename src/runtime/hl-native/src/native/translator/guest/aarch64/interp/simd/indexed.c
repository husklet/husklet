static int interp_simd_indexed(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                               unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD vector x indexed element -- the box every compiled `*_lane` intrinsic lands in. `size` names the
    // integer element (01 = H, 10 = S) but the FP FORMAT for FMLA/FMLS/FMUL/FMULX (00 = H, 10 = S, 11 = D);
    // interp_elem_index() is keyed on the resulting element size, which is what the index split follows.
    // Still reported: FEAT_FHM (FMLAL/FMLSL, opcode 0000/0100/1000/1100 at size 10), FEAT_FCMA (FCMLA, U=1 odd
    // opcodes), FEAT_BF16 (BFDOT/BFMLAL at opcode 1111, size 01/11) and the FEAT_FP8 forms at size 11.
    if ((decode & 0x9F000400u) == 0x0F000000u) {
        unsigned opcode = (decode >> 12) & 0xFu, size = (decode >> 22) & 3u;
        // The by-element spelling shifts the vector opcodes one nibble up: 1110 is SDOT/UDOT, and 1111 is
        // USDOT at size 10 / SUDOT at size 00 -- the same pair the vector box spells 0010 and 0011.
        if (!scalar && ((opcode == 0xEu && size == 2u) || (opcode == 0xFu && !u && (size == 2u || size == 0u)))) {
            int n_signed = opcode == 0xEu ? !u : (size == 0u), m_signed = opcode == 0xEu ? !u : !(size == 0u);
            // Rm is M:Rm here, and H:L indexes the 32-bit group of Vm broadcast to every lane.
            interp_vec left = interp_vec_read(cpu, rn);
            interp_vec right = interp_vec_read(cpu, (int)(((decode >> 16) & 15u) | (((decode >> 20) & 1u) << 4)));
            interp_vec result = interp_vec_read(cpu, rd);
            unsigned index = (((decode >> 11) & 1u) << 1) | ((decode >> 21) & 1u);
            for (unsigned lane = 0; lane < (q ? 4u : 2u); lane++)
                interp_vec_set_element(&result, 2, lane,
                                       (uint32_t)interp_vec_element(&result, 2, lane) +
                                           interp_dot4(&left, &right, 4u * lane, 4u * index, n_signed, m_signed));
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // FMLA / FMLS / FMUL (U=0, opcode 0001/0101/1001) and FMULX (U=1, opcode 1001). size 01 is the FEAT_FP8
        // FDOT/FMLALL box, not these.
        if (size != 1u && ((!u && (opcode == 0x1u || opcode == 0x5u || opcode == 0x9u)) || (u && opcode == 0x9u))) {
            unsigned fmt = size == 0u ? INTERP_FP_H : (size == 2u ? INTERP_FP_S : INTERP_FP_D);
            unsigned element = fmt + 1u, index;
            int vm;
            if (!interp_elem_index(decode, element, &index, &vm))
                return interp_undefined(cpu, insn, "AdvSIMD by element -- a 64-bit index needs L == 0");
            if (fmt == INTERP_FP_D && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD by element -- 2D form requires Q");
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, vm), result;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            memset(result.byte, 0, sizeof result.byte);
            uint64_t b = interp_vec_element(&right, element, index);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(element, q)); lane++) {
                uint64_t a = interp_vec_element(&left, element, lane), value;
                if (opcode == 0x9u)
                    value = u ? interp_fp_mulx(fmt, a, b) : interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
                else
                    // FUSED: one rounding of Vd + (+-Vn[lane])*Vm[index]. Multiply-then-add is wrong in the
                    // last bit and a fixture that only checks a few digits will not notice.
                    value = interp_fp_muladd(fmt, interp_vec_element(&accumulate, element, lane),
                                             opcode == 0x5u ? (a ^ interp_fp_sign_mask(fmt)) : a, b);
                interp_vec_set_element(&result, element, lane, value);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // The integer forms, all of them 16- or 32-bit elements only.
        int mla = u && opcode == 0x0u, mls = u && opcode == 0x4u, mul = !u && opcode == 0x8u;
        int mulh = !u && (opcode == 0xCu || opcode == 0xDu);                       // SQDMULH / SQRDMULH
        int rdm = u && (opcode == 0xDu || opcode == 0xFu);                         // SQRDMLAH / SQRDMLSH (FEAT_RDM)
        int wide_acc = opcode == 0x2u || opcode == 0x6u;                           // S/UMLAL, S/UMLSL
        int wide_mul = opcode == 0xAu;                                             // S/UMULL
        int wide_sat = !u && (opcode == 0x3u || opcode == 0x7u || opcode == 0xBu); // SQDML{A,S}L, SQDMULL
        if ((size == 1u || size == 2u) && (mla || mls || mul || mulh || rdm || wide_acc || wide_mul || wide_sat)) {
            // Only the SATURATING forms have scalar spellings; a scalar MUL/MLA/MLAL encoding is unallocated
            // and must not fall through the scalar normalisation into the vector one.
            if (scalar && !(mulh || rdm || wide_sat))
                return interp_undefined(cpu, insn, "AdvSIMD by element -- no scalar form for this opcode");
            unsigned index;
            int vm;
            interp_elem_index(decode, size, &index, &vm);
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, vm), result;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            memset(result.byte, 0, sizeof result.byte);
            uint64_t b = interp_vec_element(&right, size, index), mask = interp_element_mask(size);
            int widening = wide_acc || wide_mul || wide_sat;
            unsigned wide = size + 1u,
                     lanes = scalar ? 1u : (widening ? 64u / (8u << size) : interp_vec_lanes(size, q));
            for (unsigned lane = 0; lane < lanes; lane++) {
                // The "2" mnemonics: Q picks WHICH half of Vn the narrow operands come from.
                uint64_t a = interp_vec_element(&left, size, widening && q ? lane + lanes : lane);
                if (!widening) {
                    uint64_t value;
                    if (mulh)
                        value = interp_sqdmulh_element(a, b, size, opcode == 0xDu);
                    else if (rdm)
                        value = interp_sqrdmlah_element(interp_vec_element(&accumulate, size, lane), a, b, size,
                                                        opcode == 0xFu);
                    else {
                        uint64_t product =
                            (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                        uint64_t base = interp_vec_element(&accumulate, size, lane);
                        value = mla ? base + product : (mls ? base - product : product);
                    }
                    interp_vec_set_element(&result, size, lane, value & mask);
                    continue;
                }
                uint64_t value;
                if (wide_sat) {
                    uint64_t product = interp_sqdmull_element(a, b, size);
                    value = opcode == 0xBu ? product
                                           : interp_sqadd_element(interp_vec_element(&accumulate, wide, lane), product,
                                                                  wide, opcode == 0x7u);
                } else {
                    uint64_t product =
                        u ? (a & mask) * (b & mask)
                          : (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                    uint64_t base = interp_vec_element(&accumulate, wide, lane);
                    value = opcode == 0x2u ? base + product : (opcode == 0x6u ? base - product : product);
                }
                interp_vec_set_element(&result, wide, lane, value & interp_element_mask(wide));
            }
            // A widening result is always 128-bit; the scalar spelling zeroes above its one element either way.
            interp_vec_write(cpu, rd, result, widening ? 1u : q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        return interp_undefined(cpu, insn,
                                "AdvSIMD vector x indexed element -- FMLAL/FMLSL, FCMLA, BFDOT/BFMLAL, FP8, "
                                "or unallocated");
    }
    return INTERP_SIMD_UNHANDLED;
}
