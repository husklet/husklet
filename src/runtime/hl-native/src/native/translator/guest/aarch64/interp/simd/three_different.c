static int interp_simd_three_different(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                                       unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD three different (widening/narrowing)
    // bits[11:10] == 00 separates this from three-same and across-lanes. Source and destination widths differ
    // and `size` always names the NARROWER; Q selects WHICH HALF the "2" mnemonics read or write.
    if ((decode & 0x9F200C00u) == 0x0E200000u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0xFu;
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
        int narrowing = opcode == 0x4 || opcode == 0x6; // ADDHN/RADDHN and SUBHN/RSUBHN
        // PMULL 64x64 -> 128: a 128-bit result element, no element accessor fits.
        if (opcode == 0xE && size == 3) {
            uint64_t a, b, low, high;
            memcpy(&a, left.byte + (q ? 8 : 0), 8);
            memcpy(&b, right.byte + (q ? 8 : 0), 8);
            interp_poly_mul(a, b, 64, &low, &high);
            interp_vec result;
            memcpy(result.byte, &low, 8);
            memcpy(result.byte + 8, &high, 8);
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (size == 3)
            return interp_undefined(cpu, insn, "AdvSIMD three different -- 64-bit narrow element is reserved");
        unsigned wide = size + 1u, lanes = scalar ? 1u : 64u / (8u << size);
        uint64_t narrow_mask = interp_element_mask(size), wide_mask = interp_element_mask(wide);
        interp_vec result;
        memset(result.byte, 0, sizeof result.byte);
        interp_vec destination = interp_vec_read(cpu, rd);

        for (unsigned lane = 0; lane < lanes; lane++) {
            // Widening forms take narrow operands from the upper half when Q is set.
            unsigned narrow_lane = q && !narrowing ? lane + lanes : lane;
            uint64_t a, b;
            if (narrowing) {
                a = interp_vec_element(&left, wide, lane);
                b = interp_vec_element(&right, wide, lane);
                uint64_t sum = (opcode == 0x4 ? a + b : a - b) & wide_mask;
                // RADDHN/RSUBHN round: add half the discarded field first.
                if (u) sum = (sum + (UINT64_C(1) << ((8u << size) - 1u))) & wide_mask;
                interp_vec_set_element(&result, size, lane, (sum >> (8u << size)) & narrow_mask);
                continue;
            }
            a = interp_vec_element(&left, opcode == 0x1 || opcode == 0x3 ? wide : size,
                                   opcode == 0x1 || opcode == 0x3 ? lane : narrow_lane);
            b = interp_vec_element(&right, size, narrow_lane);
            // Widening forms sign-extend at U == 0, zero-extend at U == 1; PMULL is polynomial.
            uint64_t extended_a =
                opcode == 0x1 || opcode == 0x3 ? a : (u ? a & narrow_mask : (interp_element_sext(a, size) & wide_mask));
            uint64_t extended_b = u ? (b & narrow_mask) : (interp_element_sext(b, size) & wide_mask);
            uint64_t value;
            switch (opcode) {
            case 0x0: value = extended_a + extended_b; break; // SADDL / UADDL
            case 0x1: value = extended_a + extended_b; break; // SADDW / UADDW (Rn wide)
            case 0x2: value = extended_a - extended_b; break; // SSUBL / USUBL
            case 0x3: value = extended_a - extended_b; break; // SSUBW / USUBW
            case 0x5:                                         // SABAL / UABAL
            case 0x7: {                                       // SABDL / UABDL
                uint64_t difference;
                if (u) {
                    uint64_t x = a & narrow_mask, y = b & narrow_mask;
                    difference = x > y ? x - y : y - x;
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    difference = (uint64_t)(x > y ? x - y : y - x);
                }
                value = difference;
                if (opcode == 0x5) value += interp_vec_element(&destination, wide, lane);
                break;
            }
            case 0x8:   // SMLAL / UMLAL
            case 0xA:   // SMLSL / UMLSL
            case 0xC: { // SMULL / UMULL
                uint64_t product;
                if (u)
                    product = (a & narrow_mask) * (b & narrow_mask);
                else
                    product = (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                uint64_t base = interp_vec_element(&destination, wide, lane);
                value = opcode == 0x8 ? base + product : (opcode == 0xA ? base - product : product);
                break;
            }
            case 0x9:   // SQDMLAL / SQDMLAL2
            case 0xB:   // SQDMLSL / SQDMLSL2
            case 0xD: { // SQDMULL / SQDMULL2
                // Signed only; U=1 is unallocated. Two saturations for the accumulating forms: the doubled
                // product first, then the accumulate -- either can set QC.
                if (u || size == 0)
                    return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated doubling form");
                uint64_t product = interp_sqdmull_element(a, b, size);
                value = opcode == 0xD ? product
                                      : interp_sqadd_element(interp_vec_element(&destination, wide, lane), product,
                                                             wide, opcode == 0xB);
                break;
            }
            case 0xE: { // PMULL 8x8 -> 16
                if (u || size != 0)
                    return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated PMULL form");
                uint64_t low, high;
                interp_poly_mul(a & narrow_mask, b & narrow_mask, 8, &low, &high);
                value = low;
                break;
            }
            default: return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated opcode");
            }
            interp_vec_set_element(&result, wide, lane, value & wide_mask);
        }
        if (!narrowing) {
            interp_vec_write(cpu, rd, result, 1); // widening: full 128-bit result
        } else if (!q) {
            interp_vec_write(cpu, rd, result, 0); // ADDHN: low 64 bits, ZERO the upper half
        } else {
            memcpy(destination.byte + 8, result.byte, 8); // ADDHN2: upper half, preserve the lower
            interp_vec_write(cpu, rd, destination, 1);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return INTERP_SIMD_UNHANDLED;
}
