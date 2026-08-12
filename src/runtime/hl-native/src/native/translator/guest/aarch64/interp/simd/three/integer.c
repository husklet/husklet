static int interp_simd_three_same(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                                  unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD three same, and the separate three-same-FP16 box (bit22 set, bit21 clear, bits[15:14] 00)
    // that spells the same FP operations at half precision with a 3-bit opcode under an implied 11.
    unsigned fp16_three_same = (decode & 0x9F60C400u) == 0x0E400400u;
    if (fp16_three_same || (decode & 0x9F200400u) == 0x0E200400u) {
        unsigned size = (insn >> 22) & 3u;
        unsigned opcode = fp16_three_same ? (0x18u | ((insn >> 11) & 7u)) : ((insn >> 11) & 0x1Fu);
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned bytes = q ? 16u : 8u;
        unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
        uint64_t mask = interp_element_mask(size);

        int floating = interp_simd_three_same_float(cpu, insn, scalar, q, u, fp16_three_same);
        if (floating != INTERP_SIMD_UNHANDLED) return floating;

        if (opcode == 0x03) { // bitwise group: size is a sub-opcode, not an element width
            interp_vec destination = interp_vec_read(cpu, rd);
            for (unsigned index = 0; index < bytes; index++) {
                uint8_t a = left.byte[index], b = right.byte[index], d = destination.byte[index];
                uint8_t value;
                if (!u) {
                    switch (size) {
                    case 0: value = (uint8_t)(a & b); break;            // AND
                    case 1: value = (uint8_t)(a & ~b); break;           // BIC
                    case 2: value = (uint8_t)(a | b); break;            // ORR (MOV when Rn == Rm)
                    default: value = (uint8_t)(a | (uint8_t)~b); break; // ORN
                    }
                } else {
                    // Which register is the mask differs; backwards is invisible until a `?:` inverts.
                    //   BSL  mask is Vd:            Vd = Vd ? Vn : Vm
                    //   BIT  mask Vm, insert true:  Vd = Vm ? Vn : Vd
                    //   BIF  mask Vm, insert false: Vd = Vm ? Vd : Vn
                    switch (size) {
                    case 0: value = (uint8_t)(a ^ b); break;
                    case 1: value = (uint8_t)((a & d) | (b & (uint8_t)~d)); break;
                    case 2: value = (uint8_t)(d ^ ((d ^ a) & b)); break;
                    default: value = (uint8_t)(d ^ ((d ^ a) & (uint8_t)~b)); break;
                    }
                }
                result.byte[index] = value;
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // The vector group reserves 64-bit elements at Q == 0; the SCALAR spelling is the D form.
        if (size == 3 && !q && !scalar && opcode != 0x10)
            return interp_undefined(cpu, insn, "AdvSIMD three same -- reserved 1D form");

        int shifted = interp_simd_three_same_shift(cpu, insn, q, u);
        if (shifted != INTERP_SIMD_UNHANDLED) return shifted;

        int pairwise = interp_simd_three_same_pairwise(cpu, insn, q, u);
        if (pairwise != INTERP_SIMD_UNHANDLED) return pairwise;

        switch (opcode) {
        case 0x00:   // SHADD / UHADD
        case 0x02:   // SRHADD / URHADD
        case 0x04: { // SHSUB / UHSUB
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t value;
                if (u) {
                    a &= mask;
                    b &= mask;
                    if (opcode == 0x04)
                        value = (a - b) >> 1;
                    else
                        // (a + b) can carry out of a 64-bit element; (a & b) + ((a ^ b) >> 1) does not.
                        value = (a & b) + (((a ^ b) >> 1) & (mask >> 1)) + (opcode == 0x02 ? ((a ^ b) & 1u) : 0u);
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    if (opcode == 0x04)
                        value = (uint64_t)((x - y) >> 1);
                    else
                        value = (uint64_t)((x & y) + ((x ^ y) >> 1) + (opcode == 0x02 ? ((x ^ y) & 1) : 0));
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            break;
        }
        case 0x01:   // SQADD / UQADD
        case 0x05: { // SQSUB / UQSUB
            int subtract = opcode == 0x05;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                interp_vec_set_element(&result, size, lane,
                                       u ? interp_uqadd_element(a, b, size, subtract)
                                         : interp_sqadd_element(a, b, size, subtract));
            }
            break;
        }
        case 0x0E:   // SABD / UABD
        case 0x0F: { // SABA / UABA
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t difference;
                if (u) {
                    a &= mask;
                    b &= mask;
                    difference = a > b ? a - b : b - a;
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    difference = (uint64_t)(x > y ? x - y : y - x);
                }
                if (opcode == 0x0F) difference += interp_vec_element(&accumulate, size, lane);
                interp_vec_set_element(&result, size, lane, difference & mask);
            }
            break;
        }
        case 0x16: { // SQDMULH / SQRDMULH
            if (size == 0 || size == 3)
                return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated SQDMULH element size");
            for (unsigned lane = 0; lane < lanes; lane++)
                interp_vec_set_element(&result, size, lane,
                                       interp_sqdmulh_element(interp_vec_element(&left, size, lane),
                                                              interp_vec_element(&right, size, lane), size, u));
            break;
        }
        case 0x06:   // CMGT (U=0) / CMHI (U=1)
        case 0x07:   // CMGE (U=0) / CMHS (U=1)
        case 0x11: { // CMTST (U=0) / CMEQ (U=1)
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                int holds;
                if (opcode == 0x11)
                    holds = u ? a == b : (a & b) != 0;
                else if (u)
                    holds = opcode == 0x06 ? a > b : a >= b;
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    holds = opcode == 0x06 ? x > y : x >= y;
                }
                interp_vec_set_element(&result, size, lane, holds ? mask : UINT64_C(0));
            }
            break;
        }
        case 0x08: { // SSHL / USHL: shift by Rm's LOW BYTE
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane);
                int8_t amount = (int8_t)(interp_vec_element(&right, size, lane) & 0xFFu);
                unsigned esize = 8u << size;
                uint64_t value;
                if (amount >= 0) {
                    value = (unsigned)amount >= esize ? 0 : (a << amount);
                } else {
                    unsigned shift = (unsigned)(-amount);
                    if (u)
                        value = shift >= esize ? 0 : (a >> shift);
                    else {
                        int64_t signed_a = (int64_t)interp_element_sext(a, size);
                        value = (uint64_t)(shift >= esize ? (signed_a >> (esize - 1)) : (signed_a >> shift));
                    }
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            break;
        }
        case 0x0C:   // SMAX / UMAX
        case 0x0D: { // SMIN / UMIN
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t chosen;
                if (u)
                    chosen = opcode == 0x0C ? (a > b ? a : b) : (a < b ? a : b);
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    chosen = (opcode == 0x0C ? (x > y) : (x < y)) ? a : b;
                }
                interp_vec_set_element(&result, size, lane, chosen);
            }
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    return INTERP_SIMD_UNHANDLED;
}
