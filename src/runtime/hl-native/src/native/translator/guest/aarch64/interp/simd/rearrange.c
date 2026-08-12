static int interp_simd_rearrange(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                                 unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD EXT
    if ((decode & 0xBFE08400u) == 0x2E000000u) {
        unsigned position = (insn >> 11) & 0xFu;
        unsigned bytes = q ? 16u : 8u;
        if (!q && (position & 8u)) return interp_undefined(cpu, insn, "AdvSIMD EXT -- imm4 out of range for 8B");
        interp_vec first = interp_vec_read(cpu, rn), second = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < bytes; index++) {
            unsigned source = position + index;
            result.byte[index] = source < bytes ? first.byte[source] : second.byte[source - bytes];
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD table lookup: TBL / TBX
    if ((decode & 0xBF208C00u) == 0x0E000000u) {
        unsigned length = (insn >> 13) & 3u, extend = (insn >> 12) & 1u;
        unsigned bytes = q ? 16u : 8u;
        interp_vec index_vector = interp_vec_read(cpu, rm), result = interp_vec_read(cpu, rd);
        interp_vec table[4];
        for (unsigned entry = 0; entry <= length; entry++)
            table[entry] = interp_vec_read(cpu, (rn + (int)entry) % 32);
        for (unsigned index = 0; index < bytes; index++) {
            unsigned selector = index_vector.byte[index];
            if (selector < (length + 1u) * 16u)
                result.byte[index] = table[selector / 16u].byte[selector % 16u];
            else if (!extend)
                result.byte[index] = 0; // TBL zeroes an out-of-range index; TBX keeps the destination byte
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD permute: ZIP / UZP / TRN
    // Same mask as TBL/TBX, separated by bits[11:10] (00 there, 10 here).
    if ((decode & 0xBF208C00u) == 0x0E000800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 7u;
        if (size == 3 && !q) return interp_undefined(cpu, insn, "AdvSIMD permute -- 1D form is reserved");
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned lanes = interp_vec_lanes(size, q), half = lanes / 2u;
        for (unsigned lane = 0; lane < lanes; lane++) {
            uint64_t element;
            switch (opcode) {
            case 1:   // UZP1 (even lanes of Vn:Vm)
            case 5: { // UZP2 (odd)
                unsigned offset = opcode == 5 ? 1u : 0u;
                const interp_vec *source = lane < half ? &left : &right;
                unsigned index = (lane < half ? lane : lane - half) * 2u + offset;
                element = interp_vec_element(source, size, index);
                break;
            }
            case 2:   // TRN1 (even)
            case 6: { // TRN2 (odd)
                unsigned offset = opcode == 6 ? 1u : 0u;
                const interp_vec *source = (lane & 1u) ? &right : &left;
                element = interp_vec_element(source, size, (lane & ~1u) + offset);
                break;
            }
            case 3:   // ZIP1 (lower halves)
            case 7: { // ZIP2 (upper)
                unsigned base = opcode == 7 ? half : 0u;
                const interp_vec *source = (lane & 1u) ? &right : &left;
                element = interp_vec_element(source, size, base + lane / 2u);
                break;
            }
            default: return interp_undefined(cpu, insn, "AdvSIMD permute -- unallocated opcode");
            }
            interp_vec_set_element(&result, size, lane, element);
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD across lanes
    // Before two-register-misc: bits[21:17] == 10000/11000, differ in bit 20.
    if ((decode & 0x9F3E0C00u) == 0x0E300800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
        // FP reductions (U == 1): bit23 selects max vs min, bit22 is sz
        // Folded left to right; the FPMax/FPMin NaN and zero-sign rules are symmetric.
        if (u && (opcode == 0x0Cu || opcode == 0x0Fu)) {
            unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
            unsigned element = fmt + 1u, lanes = interp_vec_lanes(element, q);
            if (fmt == INTERP_FP_D || !q)
                return interp_undefined(cpu, insn, "AdvSIMD across lanes -- unallocated FP reduction size");
            interp_vec source = interp_vec_read(cpu, rn), result;
            memset(result.byte, 0, sizeof result.byte);
            uint64_t accumulator = interp_vec_element(&source, element, 0);
            for (unsigned lane = 1; lane < lanes; lane++)
                accumulator = interp_fp_minmax(fmt, accumulator, interp_vec_element(&source, element, lane), !high,
                                               opcode == 0x0Cu);
            interp_vec_set_element(&result, element, 0, accumulator);
            interp_vec_write(cpu, rd, result, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // AdvSIMD SCALAR pairwise, sharing this box
        // These combine the TWO source lanes and bypass the vector size/Q reservations.
        if (scalar) {
            interp_vec source = interp_vec_read(cpu, rn), result;
            memset(result.byte, 0, sizeof result.byte);
            if (!u && opcode == 0x1Bu) { // ADDP (scalar): 2D only
                if (size != 3) return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- ADDP needs 2D");
                interp_vec_set_element(&result, 3, 0,
                                       interp_vec_element(&source, 3, 0) + interp_vec_element(&source, 3, 1));
            } else if (u && (opcode == 0x0Cu || opcode == 0x0Du || opcode == 0x0Fu)) {
                unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
                unsigned element = fmt + 1u;
                uint64_t a = interp_vec_element(&source, element, 0), b = interp_vec_element(&source, element, 1);
                uint64_t value;
                if (opcode == 0x0Du) { // FADDP (scalar)
                    if (high) return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- unallocated FADDP");
                    value = interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b);
                } else { // FMAXNMP/FMINNMP, FMAXP/FMINP
                    value = interp_fp_minmax(fmt, a, b, !high, opcode == 0x0Cu);
                }
                interp_vec_set_element(&result, element, 0, value);
            } else {
                return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- unimplemented opcode");
            }
            interp_vec_write(cpu, rd, result, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (size == 3 || (size == 2 && !q))
            return interp_undefined(cpu, insn, "AdvSIMD across lanes -- reserved size/Q combination");
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned lanes = interp_vec_lanes(size, q);
        uint64_t accumulator = interp_vec_element(&source, size, 0);
        switch (opcode) {
        case 0x03: { // SADDLV / UADDLV (DOUBLE width)
            uint64_t total = 0;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                total += u ? element : interp_element_sext(element, size);
            }
            interp_vec_set_element(&result, size + 1u, 0, total & interp_element_mask(size + 1u));
            break;
        }
        case 0x0A: // SMAXV / UMAXV
        case 0x1A: // SMINV / UMINV
        case 0x1B: // ADDV
            for (unsigned lane = 1; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                if (opcode == 0x1B) {
                    accumulator = (accumulator + element) & interp_element_mask(size);
                } else if (u) {
                    int greater = element > accumulator;
                    if (opcode == 0x0A ? greater : !greater && element != accumulator) accumulator = element;
                } else {
                    int64_t left = (int64_t)interp_element_sext(accumulator, size);
                    int64_t right = (int64_t)interp_element_sext(element, size);
                    if (opcode == 0x0A ? right > left : right < left) accumulator = element;
                }
            }
            interp_vec_set_element(&result, size, 0, accumulator);
            break;
        default: return interp_undefined(cpu, insn, "AdvSIMD across lanes -- unimplemented opcode");
        }
        // A reduction is a SCALAR: only the low element is defined.
        interp_vec_write(cpu, rd, result, 0);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    return INTERP_SIMD_UNHANDLED;
}
