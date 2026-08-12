static int interp_simd_shift(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    // AdvSIMD shift by immediate
    if ((decode & 0x9F800400u) == 0x0F000400u) {
        unsigned immh = (insn >> 19) & 0xFu, immb = (insn >> 16) & 7u, opcode = (insn >> 11) & 0x1Fu;
        unsigned size;
        if (immh & 8u)
            size = 3;
        else if (immh & 4u)
            size = 2;
        else if (immh & 2u)
            size = 1;
        else
            size = 0;
        unsigned esize = 8u << size;
        unsigned combined = (immh << 3) | immb;
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        // The scalar spelling has one lane, at the vector group's reserved 1D width.
        unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
        uint64_t mask = interp_element_mask(size);

        // fixed-point conversions: fbits = 2*esize - (immh:immb), a SCALE not a shift
        if (opcode == 0x1C || opcode == 0x1F) {
            if (size < 1) return interp_undefined(cpu, insn, "AdvSIMD shift -- fixed-point conversion needs immh != 0");
            unsigned fmt = size == 3 ? INTERP_FP_D : (size == 2 ? INTERP_FP_S : INTERP_FP_H);
            unsigned fbits = 2u * esize - combined;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                uint64_t value;
                if (opcode == 0x1C) // SCVTF / UCVTF
                    value = interp_fp_from_int(fmt, element, esize, !u, INTERP_FPCR_RMODE(g_interp_fpcr), fbits);
                else // FCVTZS / FCVTZU
                    value = interp_fp_to_int(fmt, element, esize, !u, INTERP_RM_RZ, fbits);
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            interp_vec_write(cpu, rd, result, scalar ? 0u : q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (opcode == 0x10 || opcode == 0x11 || opcode == 0x12 || opcode == 0x13) {
            // NARROWING right shifts: sources are TWICE the destination width, the 64-bit result goes in the
            // half Q selects. SHRN/RSHRN truncate; SQSHRUN saturates signed -> unsigned, SQSHRN/UQSHRN
            // signed/unsigned. Odd opcodes round.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD shift -- narrowing shift with a 64-bit result");
            unsigned shift = 2u * esize - combined;
            unsigned narrow_lanes = 64u / esize;
            uint64_t wide_mask = interp_element_mask(size + 1u);
            int saturating = opcode >= 0x12 || u;
            int source_signed = opcode >= 0x12 ? !u : 1;
            int dest_signed = opcode >= 0x12 ? !u : 0;
            int rounding = (opcode & 1u) != 0;
            interp_vec packed;
            memset(packed.byte, 0, sizeof packed.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                uint64_t element = interp_vec_element(&source, size + 1u, lane) & wide_mask;
                uint64_t shifted;
                // The rounding constant can carry out of the wide element (0xFFFF..FF + half at a 64-bit
                // source), so add in 128 bits and clamp: a carry-out is always a saturation, and letting it
                // wrap gave 0 with QC CLEAR where the answer is the maximum with QC SET.
                if (saturating && source_signed) {
                    // Shift the SIGN-EXTENDED value so saturation sees the true magnitude.
                    __int128 wide = (__int128)(int64_t)interp_element_sext(element, size + 1u);
                    if (rounding && shift > 0) wide += (__int128)1 << (shift - 1u);
                    __int128 value = wide >> shift;
                    __int128 wide_max = (__int128)(wide_mask >> 1);
                    shifted = (uint64_t)(value > wide_max ? wide_max : value) & wide_mask;
                } else {
                    unsigned __int128 wide = element;
                    if (rounding && shift > 0) wide += (unsigned __int128)1 << (shift - 1u);
                    unsigned __int128 value = wide >> shift;
                    shifted = value > (unsigned __int128)wide_mask ? wide_mask : (uint64_t)value;
                }
                interp_vec_set_element(&packed, size, lane,
                                       saturating ? interp_sat_narrow(shifted, size, source_signed, dest_signed)
                                                  : (shifted & mask));
            }
            if (!q || scalar) {
                // Unsuffixed: write the low 64 bits, ZERO the upper half.
                interp_vec_write(cpu, rd, packed, 0);
            } else {
                // "2": write the UPPER 64 bits, leave the lower half untouched.
                interp_vec destination = interp_vec_read(cpu, rd);
                memcpy(destination.byte + 8, packed.byte, 8);
                interp_vec_write(cpu, rd, destination, 1);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (opcode == 0x14) {
            // SSHLL / USHLL (SXTL/UXTL at zero shift): widen; Q picks the source half.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD shift -- SSHLL/USHLL with a 64-bit source");
            unsigned shift = combined - esize;
            unsigned wide_lanes = 64u / esize;
            uint64_t wide_mask = interp_element_mask(size + 1u);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, q ? lane + wide_lanes : lane);
                if (!u) element = interp_element_sext(element, size);
                interp_vec_set_element(&result, size + 1u, lane, (element << shift) & wide_mask);
            }
            // Full 128-bit destination either way.
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (size == 3 && !q && !scalar)
            return interp_undefined(cpu, insn, "AdvSIMD shift -- 64-bit element requires Q");
        if (opcode == 0x0A && !u) { // SHL
            unsigned shift = combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++)
                interp_vec_set_element(&result, size, lane, (interp_vec_element(&source, size, lane) << shift) & mask);
        } else if (opcode == 0x08 || (opcode == 0x0A && u)) {
            // SRI / SLI: the shifted-in bits come from the DESTINATION, not zeroes.
            interp_vec destination = interp_vec_read(cpu, rd);
            unsigned shift = opcode == 0x08 ? 2u * esize - combined : combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane) & mask;
                uint64_t base = interp_vec_element(&destination, size, lane) & mask;
                uint64_t moved, keep;
                if (opcode == 0x08) { // SRI: keep the destination's TOP bits
                    moved = shift >= esize ? 0 : (element >> shift);
                    keep = shift == 0 ? 0 : (mask << (esize - shift)) & mask;
                } else { // SLI: keep the destination's BOTTOM bits
                    moved = (element << shift) & mask;
                    keep = shift == 0 ? 0 : ((UINT64_C(1) << shift) - 1u);
                }
                interp_vec_set_element(&result, size, lane, (moved & ~keep) | (base & keep));
            }
        } else if (opcode == 0x0C || opcode == 0x0E) {
            // SQSHLU, SQSHL, UQSHL: repeated saturating doubling, matching SQADD's QC.
            if (opcode == 0x0C && !u) return interp_undefined(cpu, insn, "AdvSIMD shift -- unallocated SQSHLU");
            unsigned shift = combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                uint64_t value;
                if (opcode == 0x0E && u) { // UQSHL
                    value = element & mask;
                    for (unsigned step = 0; step < shift; step++)
                        value = interp_uqadd_element(value, value, size, 0);
                } else if (opcode == 0x0E) { // SQSHL
                    value = element & mask;
                    for (unsigned step = 0; step < shift; step++)
                        value = interp_sqadd_element(value, value, size, 0);
                } else { // SQSHLU: UNSIGNED saturation
                    int64_t signed_element = (int64_t)interp_element_sext(element, size);
                    if (signed_element < 0) {
                        interp_fpsr_raise(INTERP_FPSR_QC);
                        value = 0;
                    } else {
                        value = (uint64_t)signed_element & mask;
                        for (unsigned step = 0; step < shift; step++)
                            value = interp_uqadd_element(value, value, size, 0);
                    }
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
        } else if (opcode == 0x00 || opcode == 0x02 || opcode == 0x04 || opcode == 0x06) {
            // SSHR/USHR, SSRA/USRA, and the rounding SRSHR/URSHR, SRSRA/URSRA.
            unsigned shift = 2u * esize - combined;
            int rounding = opcode == 0x04 || opcode == 0x06;
            int accumulating = opcode == 0x02 || opcode == 0x06;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                // A full-element-width right shift is defined here but UB in C.
                uint64_t shifted;
                if (u) {
                    uint64_t value = element & mask;
                    uint64_t round = rounding && shift > 0 ? ((value >> (shift - 1u)) & 1u) : 0u;
                    shifted = (shift >= esize ? 0 : (value >> shift)) + round;
                } else {
                    int64_t signed_element = (int64_t)interp_element_sext(element, size);
                    uint64_t round = rounding && shift > 0
                                         ? (uint64_t)((signed_element >> (shift > esize ? esize - 1u : shift - 1u)) & 1)
                                         : 0u;
                    shifted = (uint64_t)(shift >= esize ? (signed_element >> (esize - 1)) : (signed_element >> shift));
                    shifted += round;
                }
                shifted &= mask;
                if (accumulating) shifted = (shifted + interp_vec_element(&accumulate, size, lane)) & mask;
                interp_vec_set_element(&result, size, lane, shifted);
            }
        } else {
            return interp_undefined(cpu, insn, "AdvSIMD shift by immediate -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, scalar ? 0u : q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    return INTERP_SIMD_UNHANDLED;
}
