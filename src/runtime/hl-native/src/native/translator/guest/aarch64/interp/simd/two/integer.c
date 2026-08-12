static int interp_simd_two_register(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                                    unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD two-register misc
    if ((decode & 0x9F3E0C00u) == 0x0E200800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned bytes = q ? 16u : 8u;

        int floating = interp_simd_two_register_float(cpu, insn, scalar, q, u);
        if (floating != INTERP_SIMD_UNHANDLED) return floating;

        switch (opcode) {
        case 0x02:   // SADDLP / UADDLP (DOUBLE width)
        case 0x06: { // SADALP / UADALP: accumulating
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated ADDLP size");
            unsigned wide = size + 1u, wide_lanes = scalar ? 1u : interp_vec_lanes(wide, q);
            uint64_t wide_mask = interp_element_mask(wide);
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane * 2u);
                uint64_t b = interp_vec_element(&source, size, lane * 2u + 1u);
                if (!u) {
                    a = interp_element_sext(a, size);
                    b = interp_element_sext(b, size);
                }
                uint64_t total = (a + b) & wide_mask;
                if (opcode == 0x06) total = (total + interp_vec_element(&accumulate, wide, lane)) & wide_mask;
                interp_vec_set_element(&result, wide, lane, total);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x03: {
            // SUQADD / USQADD: the accumulator in Vd and the operand in Vn have OPPOSITE signedness and the
            // saturation follows the accumulator's, so neither SQADD nor UQADD applies. 128-bit intermediate
            // because a 64-bit element's sum does not fit either operand type.
            interp_vec accumulate = interp_vec_read(cpu, rd);
            unsigned esize = 8u << size, misc_lanes = scalar ? 1u : interp_vec_lanes(size, q);
            uint64_t emask = interp_element_mask(size);
            for (unsigned lane = 0; lane < misc_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane) & emask;
                uint64_t d = interp_vec_element(&accumulate, size, lane) & emask;
                __int128 total, low, high;
                if (!u) { // SUQADD: signed accumulator + unsigned operand
                    total = (__int128)(int64_t)interp_element_sext(d, size) + (__int128)a;
                    high = ((__int128)1 << (esize - 1u)) - 1;
                    low = -((__int128)1 << (esize - 1u));
                } else { // USQADD: unsigned accumulator + signed operand
                    total = (__int128)d + (__int128)(int64_t)interp_element_sext(a, size);
                    high = ((__int128)1 << esize) - 1;
                    low = 0;
                }
                if (total > high || total < low) {
                    interp_fpsr_raise(INTERP_FPSR_QC);
                    total = total > high ? high : low;
                }
                interp_vec_set_element(&result, size, lane, (uint64_t)total & emask);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x04: { // CLS (U=0) / CLZ (U=1)
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated CLS/CLZ size");
            unsigned esize = 8u << size, lanes = scalar ? 1u : interp_vec_lanes(size, q);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane) & interp_element_mask(size);
                // CLS counts leading bits MATCHING the sign, excluding it: 0..esize-1.
                uint64_t folded = ((a >> 1) ^ a) & (interp_element_mask(size) >> 1);
                unsigned count;
                if (!u)
                    count =
                        folded == 0 ? esize - 1u : (unsigned)(esize - 2u - (unsigned)(63 - __builtin_clzll(folded)));
                else
                    count = a == 0 ? esize : (unsigned)(esize - 1u - (unsigned)(63 - __builtin_clzll(a)));
                interp_vec_set_element(&result, size, lane, count);
            }
            // Must return here, not `break`: this switch's break falls into the NEXT switch, whose default
            // reports -- CLS/CLZ and SQABS/SQNEG were computed and then thrown away as unimplemented.
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x07: { // SQABS (U=0) / SQNEG (U=1)
            unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane);
                // As 0 - a, so the one overflowing input saturates through the group's helper.
                int64_t x = (int64_t)interp_element_sext(a, size);
                uint64_t value;
                if (!u && x >= 0)
                    value = a & interp_element_mask(size);
                else
                    value = interp_sqadd_element(0, a, size, 1);
                interp_vec_set_element(&result, size, lane, value);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x12:   // XTN (U=0) / SQXTUN (U=1)
        case 0x14: { // SQXTN (U=0) / UQXTN (U=1)
            // Narrowing: `size` names the RESULT element and sources are twice as wide; Q picks the half.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated XTN size");
            unsigned narrow_lanes = 64u / (8u << size);
            interp_vec packed;
            memset(packed.byte, 0, sizeof packed.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                uint64_t wide_element = interp_vec_element(&source, size + 1u, lane);
                uint64_t value;
                if (opcode == 0x12 && !u)
                    value = wide_element & interp_element_mask(size); // XTN
                else if (opcode == 0x12)
                    value = interp_sat_narrow(wide_element, size, 1, 0); // SQXTUN
                else
                    value = interp_sat_narrow(wide_element, size, u ? 0 : 1, u ? 0 : 1); // UQXTN / SQXTN
                interp_vec_set_element(&packed, size, lane, value);
            }
            if (!q || scalar) {
                interp_vec_write(cpu, rd, packed, 0);
            } else {
                interp_vec destination = interp_vec_read(cpu, rd);
                memcpy(destination.byte + 8, packed.byte, 8);
                interp_vec_write(cpu, rd, destination, 1);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x13: { // SHLL / SHLL2 (U=1): shift by the FULL width
            if (!u || size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated SHLL");
            unsigned wide = size + 1u, wide_lanes = 64u / (8u << size);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, q ? lane + wide_lanes : lane);
                interp_vec_set_element(&result, wide, lane, (a << (8u << size)) & interp_element_mask(wide));
            }
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        default: break;
        }

        return interp_simd_two_register_unary(cpu, insn, scalar, q, u);
    }
    return INTERP_SIMD_UNHANDLED;
}
