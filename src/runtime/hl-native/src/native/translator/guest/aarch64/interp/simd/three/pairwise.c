static int interp_simd_three_same_pairwise(struct cpu *cpu, uint32_t insn, unsigned q, unsigned u) {
    unsigned opcode = (insn >> 11) & 0x1fu;
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned size = (insn >> 22) & 3u, lanes = interp_vec_lanes(size, q);
    uint64_t mask = interp_element_mask(size);
    interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
    memset(result.byte, 0, sizeof result.byte);
    switch (opcode) {
    case 0x10: { // ADD (U=0) / SUB (U=1)
        for (unsigned lane = 0; lane < lanes; lane++) {
            uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
            interp_vec_set_element(&result, size, lane, (u ? a - b : a + b) & mask);
        }
        break;
    }
    case 0x17: { // ADDP: pairwise across Rn:Rm
        if (u) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated ADDP U bit");
        for (unsigned lane = 0; lane < lanes; lane++) {
            const interp_vec *source = lane < lanes / 2 ? &left : &right;
            unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
            uint64_t a = interp_vec_element(source, size, base);
            uint64_t b = interp_vec_element(source, size, base + 1u);
            interp_vec_set_element(&result, size, lane, (a + b) & mask);
        }
        break;
    }
    case 0x12: { // MLA (U=0) / MLS (U=1)
        if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD three same -- 64-bit element MLA/MLS");
        interp_vec accumulate = interp_vec_read(cpu, rd);
        for (unsigned lane = 0; lane < lanes; lane++) {
            uint64_t product = interp_vec_element(&left, size, lane) * interp_vec_element(&right, size, lane);
            uint64_t base = interp_vec_element(&accumulate, size, lane);
            interp_vec_set_element(&result, size, lane, (u ? base - product : base + product) & mask);
        }
        break;
    }
    case 0x13: { // MUL / PMUL (U=1, carry-less)
        if (u) {
            if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD three same -- PMUL requires 8B/16B");
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t low, high;
                interp_poly_mul(interp_vec_element(&left, 0, lane), interp_vec_element(&right, 0, lane), 8, &low,
                                &high);
                interp_vec_set_element(&result, 0, lane, low & 0xFFu);
            }
            break;
        }
        if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD three same -- 64-bit element MUL");
        for (unsigned lane = 0; lane < lanes; lane++)
            interp_vec_set_element(&result, size, lane,
                                   (interp_vec_element(&left, size, lane) * interp_vec_element(&right, size, lane)) &
                                       mask);
        break;
    }
    case 0x14: { // SMAXP / UMAXP
        for (unsigned lane = 0; lane < lanes; lane++) {
            const interp_vec *source = lane < lanes / 2 ? &left : &right;
            unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
            uint64_t a = interp_vec_element(source, size, base);
            uint64_t b = interp_vec_element(source, size, base + 1u);
            uint64_t chosen;
            if (u)
                chosen = a > b ? a : b;
            else {
                int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                chosen = x > y ? a : b;
            }
            interp_vec_set_element(&result, size, lane, chosen);
        }
        break;
    }
    case 0x15: { // SMINP / UMINP
        for (unsigned lane = 0; lane < lanes; lane++) {
            const interp_vec *source = lane < lanes / 2 ? &left : &right;
            unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
            uint64_t a = interp_vec_element(source, size, base);
            uint64_t b = interp_vec_element(source, size, base + 1u);
            uint64_t chosen;
            if (u)
                chosen = a < b ? a : b;
            else {
                int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                chosen = x < y ? a : b;
            }
            interp_vec_set_element(&result, size, lane, chosen);
        }
        break;
    }
    default: return INTERP_SIMD_UNHANDLED;
    }
    interp_vec_write(cpu, rd, result, q);
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}
