static int interp_simd_two_register_unary(struct cpu *cpu, uint32_t insn, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31);
    unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
    unsigned bytes = q ? 16u : 8u;
    interp_vec source = interp_vec_read(cpu, rn), result;
    memset(result.byte, 0, sizeof result.byte);
    switch (opcode) {
    case 0x00:   // REV64 (U=0) / REV32 (U=1)
    case 0x01: { // REV16 (U=0)
        // Reverse bytes within each container (8, 4 or 2 by opcode); `size` is the element width.
        unsigned container = opcode == 0x01 ? 2u : (u ? 4u : 8u);
        unsigned element = 1u << size;
        if (element >= container)
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- REV element wider than container");
        for (unsigned base = 0; base < bytes; base += container)
            for (unsigned offset = 0; offset < container; offset += element)
                memcpy(result.byte + base + (container - element - offset), source.byte + base + offset, element);
        break;
    }
    case 0x05: {  // CNT / NOT (size=0) / RBIT (size=1)
        if (!u) { // CNT
            if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- CNT requires 8B/16B");
            for (unsigned index = 0; index < bytes; index++)
                result.byte[index] = (uint8_t)__builtin_popcount(source.byte[index]);
        } else if (size == 0) { // NOT / MVN
            for (unsigned index = 0; index < bytes; index++)
                result.byte[index] = (uint8_t)~source.byte[index];
        } else if (size == 1) { // RBIT
            for (unsigned index = 0; index < bytes; index++) {
                uint8_t value = source.byte[index];
                value = (uint8_t)(((value & 0x55u) << 1) | ((value >> 1) & 0x55u));
                value = (uint8_t)(((value & 0x33u) << 2) | ((value >> 2) & 0x33u));
                value = (uint8_t)(((value & 0x0Fu) << 4) | ((value >> 4) & 0x0Fu));
                result.byte[index] = value;
            }
        } else {
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated CNT/NOT/RBIT size");
        }
        break;
    }
    case 0x08:   // CMGT / CMGE (zero)
    case 0x09:   // CMEQ / CMLE (zero)
    case 0x0A: { // CMLT (zero)
        if (size == 3 && !q && !scalar)
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 1D compare is reserved");
        uint64_t mask = interp_element_mask(size);
        for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++) {
            int64_t element = (int64_t)interp_element_sext(interp_vec_element(&source, size, lane), size);
            int holds;
            if (opcode == 0x08)
                holds = u ? element >= 0 : element > 0;
            else if (opcode == 0x09)
                holds = u ? element <= 0 : element == 0;
            else {
                if (u) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated compare-zero");
                holds = element < 0;
            }
            interp_vec_set_element(&result, size, lane, holds ? mask : UINT64_C(0));
        }
        break;
    }
    case 0x0B: { // ABS (U=0) / NEG (U=1)
        if (size == 3 && !q && !scalar)
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 1D ABS/NEG reserved");
        uint64_t mask = interp_element_mask(size);
        for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++) {
            int64_t element = (int64_t)interp_element_sext(interp_vec_element(&source, size, lane), size);
            uint64_t value = u ? (uint64_t)(-element) : (uint64_t)(element < 0 ? -element : element);
            interp_vec_set_element(&result, size, lane, value & mask);
        }
        break;
    }
    default: return interp_undefined(cpu, insn, "AdvSIMD two-register misc -- unimplemented opcode");
    }
    interp_vec_write(cpu, rd, result, q);
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}
