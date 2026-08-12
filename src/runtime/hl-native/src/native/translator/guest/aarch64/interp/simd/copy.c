static int interp_simd_copy(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD copy: DUP, INS, SMOV, UMOV
    // bits[23:21] must ALL be 000: leaving 23:22 unconstrained swallowed the whole three-same-FP16 box
    // below, which shares bit21 == 0 and bit10 == 1, and ran it as INS/DUP with imm5 = Rm.
    if ((decode & 0x9FE08400u) == 0x0E000400u) {
        unsigned op = (insn >> 29) & 1, imm4 = (insn >> 11) & 0xFu, imm5 = (insn >> 16) & 0x1Fu;
        unsigned size, index;
        if (op) { // INS (element)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            unsigned source_index = imm4 >> size; // imm4 is the source lane, scaled by the element size
            interp_vec source = interp_vec_read(cpu, rn), destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, size, index, interp_vec_element(&source, size, source_index));
            // Single-lane write: must NOT zero the upper half.
            interp_vec_write(cpu, rd, destination, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        switch (imm4) {
        case 0: { // DUP (element)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            // The SCALAR spelling is DUP Dd, Vn.D[index]: do not reject 1D.
            if (size == 3 && !q && !scalar) return interp_undefined(cpu, insn, "AdvSIMD copy -- DUP 1D is reserved");
            interp_vec source = interp_vec_read(cpu, rn), result;
            uint64_t element = interp_vec_element(&source, size, index);
            memset(result.byte, 0, sizeof result.byte);
            // The scalar spelling (MOV Bd/Hd/Sd/Dd, Vn.T[index]) writes ONE element and zeroes the rest;
            // only the D form coincides with filling the 64-bit half.
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++)
                interp_vec_set_element(&result, size, lane, element);
            interp_vec_write(cpu, rd, result, q);
            break;
        }
        case 1: { // DUP (general)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            if (size == 3 && !q) return interp_undefined(cpu, insn, "AdvSIMD copy -- DUP 1D is reserved");
            uint64_t element = interp_gpr(cpu, rn) & interp_element_mask(size);
            interp_vec result;
            memset(result.byte, 0, sizeof result.byte);
            for (unsigned lane = 0; lane < interp_vec_lanes(size, q); lane++)
                interp_vec_set_element(&result, size, lane, element);
            interp_vec_write(cpu, rd, result, q);
            break;
        }
        case 3: { // INS (general)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            interp_vec destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, size, index, interp_gpr(cpu, rn) & interp_element_mask(size));
            interp_vec_write(cpu, rd, destination, 1);
            break;
        }
        case 5:   // SMOV
        case 7: { // UMOV
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            interp_vec source = interp_vec_read(cpu, rn);
            uint64_t element = interp_vec_element(&source, size, index);
            if (imm4 == 5) element = interp_element_sext(element, size);
            // Here Q selects the destination GPR width, not the vector length.
            if (q)
                interp_set_gpr(cpu, rd, element);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)element);
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD copy -- unallocated imm4");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    return INTERP_SIMD_UNHANDLED;
}
