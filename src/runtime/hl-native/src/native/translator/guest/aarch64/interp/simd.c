static int interp_exec_simd(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned q = (insn >> 30) & 1, u = (insn >> 29) & 1;

    // crypto: AES and two-register SHA
    // Before the scalar normalisation, which would turn bits[31:30] == 01 into another encoding.
    if ((insn & 0xFF3E0C00u) == 0x4E280800u || (insn & 0xFF3E0C00u) == 0x5E280800u) {
        unsigned opcode = (insn >> 12) & 0x1Fu, size = (insn >> 22) & 3u;
        interp_vec source = interp_vec_read(cpu, rn), destination = interp_vec_read(cpu, rd), result;
        if ((insn & 0xFF000000u) == 0x4E000000u) { // ---- AES ----
            if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD AES -- size must be 00");
            uint8_t stage[16], mixed[16];
            switch (opcode) {
            case 0x04:   // AESE
            case 0x05: { // AESD
                int inverse = opcode == 0x05;
                uint8_t combined[16];
                for (unsigned index = 0; index < 16u; index++)
                    combined[index] = (uint8_t)(destination.byte[index] ^ source.byte[index]);
                interp_aes_shift_rows(combined, stage, inverse);
                for (unsigned index = 0; index < 16u; index++)
                    stage[index] = inverse ? interp_aes_inv_sbox[stage[index]] : interp_aes_sbox[stage[index]];
                memcpy(result.byte, stage, 16);
                break;
            }
            case 0x06: // AESMC
            case 0x07: // AESIMC
                interp_aes_mix_columns(source.byte, mixed, opcode == 0x07);
                memcpy(result.byte, mixed, 16);
                break;
            default: return interp_undefined(cpu, insn, "AdvSIMD AES -- unallocated opcode");
            }
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // two-register SHA
        if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD SHA -- size must be 00");
        uint32_t d[4], n[4];
        for (unsigned index = 0; index < 4u; index++) {
            d[index] = (uint32_t)interp_vec_element(&destination, 2, index);
            n[index] = (uint32_t)interp_vec_element(&source, 2, index);
        }
        uint32_t out[4] = {0, 0, 0, 0};
        switch (opcode) {
        case 0x00: // SHA1H
            out[0] = interp_rol32_bits(n[0], 30);
            break;
        case 0x01: { // SHA1SU1
            uint32_t t[4];
            // T = Vd EOR (Vn >> 32), zero fill.
            for (unsigned index = 0; index < 4u; index++)
                t[index] = d[index] ^ (index < 3u ? n[index + 1u] : 0u);
            for (unsigned index = 0; index < 4u; index++)
                out[index] = interp_rol32_bits(t[index], 1);
            out[3] ^= interp_rol32_bits(t[0], 2);
            break;
        }
        case 0x02: { // SHA256SU0
            uint32_t t[4];
            for (unsigned index = 0; index < 4u; index++)
                t[index] = index < 3u ? d[index + 1u] : n[0];
            for (unsigned index = 0; index < 4u; index++) {
                uint32_t element = t[index];
                element = interp_ror32_bits(element, 7) ^ interp_ror32_bits(element, 18) ^ (element >> 3);
                out[index] = element + d[index];
            }
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD SHA -- unallocated two-register opcode");
        }
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < 4u; index++)
            interp_vec_set_element(&result, 2, index, out[index]);
        interp_vec_write(cpu, rd, result, 1);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // three-register SHA
    // Fixed bits are 31:24, 21, 15 and 11:10 ONLY: masking 15:10 as one field pins the opcode to zero.
    if ((insn & 0xFF208C00u) == 0x5E000000u) {
        unsigned opcode = (insn >> 12) & 7u;
        interp_vec vd = interp_vec_read(cpu, rd), vn = interp_vec_read(cpu, rn), vm = interp_vec_read(cpu, rm);
        uint32_t x[4], y[4], w[4], result_words[4];
        for (unsigned index = 0; index < 4u; index++) {
            x[index] = (uint32_t)interp_vec_element(&vd, 2, index);
            y[index] = (uint32_t)interp_vec_element(&vn, 2, index);
            w[index] = (uint32_t)interp_vec_element(&vm, 2, index);
        }
        if (opcode <= 2u) {
            // SHA1C/P/M: FOUR SHA-1 rounds; K is folded into Vm by the caller.
            uint32_t e = y[0];
            for (unsigned round = 0; round < 4u; round++) {
                uint32_t t = opcode == 0   ? interp_sha_choose(x[1], x[2], x[3])
                             : opcode == 1 ? interp_sha_parity(x[1], x[2], x[3])
                                           : interp_sha_majority(x[1], x[2], x[3]);
                uint32_t next = e + interp_rol32_bits(x[0], 5) + t + w[round];
                x[1] = interp_rol32_bits(x[1], 30);
                e = x[3];
                x[3] = x[2];
                x[2] = x[1];
                x[1] = x[0];
                x[0] = next;
            }
            memcpy(result_words, x, sizeof result_words);
        } else if (opcode == 3u) {
            // SHA1SU0
            uint32_t t[4] = {x[2], x[3], y[0], y[1]};
            for (unsigned index = 0; index < 4u; index++)
                result_words[index] = t[index] ^ x[index] ^ w[index];
        } else if (opcode == 4u || opcode == 5u) {
            // SHA256H -> x, SHA256H2 -> y, halves swapped.
            int part1 = opcode == 4u;
            uint32_t a[4], b[4];
            if (part1) {
                memcpy(a, x, sizeof a);
                memcpy(b, y, sizeof b);
            } else {
                memcpy(a, y, sizeof a);
                memcpy(b, x, sizeof b);
            }
            for (unsigned round = 0; round < 4u; round++) {
                uint32_t chs = interp_sha_choose(b[0], b[1], b[2]);
                uint32_t maj = interp_sha_majority(a[0], a[1], a[2]);
                uint32_t sigma1 =
                    interp_ror32_bits(b[0], 6) ^ interp_ror32_bits(b[0], 11) ^ interp_ror32_bits(b[0], 25);
                uint32_t sigma0 =
                    interp_ror32_bits(a[0], 2) ^ interp_ror32_bits(a[0], 13) ^ interp_ror32_bits(a[0], 22);
                uint32_t t = b[3] + sigma1 + chs + w[round];
                uint32_t new_a3 = t + a[3];
                uint32_t new_b3 = t + sigma0 + maj;
                // <y, x> = ROL(y : x, 32).
                uint32_t carry = new_a3;
                a[3] = a[2];
                a[2] = a[1];
                a[1] = a[0];
                a[0] = new_b3;
                b[3] = b[2];
                b[2] = b[1];
                b[1] = b[0];
                b[0] = carry;
            }
            memcpy(result_words, part1 ? a : b, sizeof result_words);
        } else if (opcode == 6u) {
            // SHA256SU1
            uint32_t t0[4] = {y[1], y[2], y[3], w[0]};
            uint32_t t1[2] = {w[2], w[3]};
            for (unsigned index = 0; index < 2u; index++) {
                uint32_t element = t1[index];
                element = interp_ror32_bits(element, 17) ^ interp_ror32_bits(element, 19) ^ (element >> 10);
                result_words[index] = element + x[index] + t0[index];
            }
            for (unsigned index = 2; index < 4u; index++) {
                uint32_t element = result_words[index - 2u];
                element = interp_ror32_bits(element, 17) ^ interp_ror32_bits(element, 19) ^ (element >> 10);
                result_words[index] = element + x[index] + t0[index];
            }
        } else {
            return interp_undefined(cpu, insn, "AdvSIMD SHA -- unallocated three-register opcode");
        }
        interp_vec result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < 4u; index++)
            interp_vec_set_element(&result, 2, index, result_words[index]);
        interp_vec_write(cpu, rd, result, 1);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // scalar FP (bit28 == 1, bit30 == 0)
    if ((insn & 0x7F000000u) == 0x1E000000u || (insn & 0x7F000000u) == 0x1F000000u)
        return interp_exec_fp_scalar(cpu, insn);

    // AdvSIMD SCALAR forms, normalised into their vector spelling
    // Clearing bits 30 and 28 gives the vector encoding at Q == 0, but a scalar form has ONE lane and zeroes
    // [127:esize]: `scalar` overrides interp_vec_lanes and the "1D is reserved" checks. Diagnostics use `insn`.
    unsigned scalar = 0;
    uint32_t decode = insn;
    if ((insn & 0xDE000000u) == 0x5E000000u) {
        scalar = 1;
        decode &= ~UINT32_C(0x50000000);
        q = 0;
    }

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

    // AdvSIMD modified immediate
    // Must precede shift-by-immediate: same box, separated only by immh (22:19).
    if ((decode & 0x9FF80400u) == 0x0F000400u) {
        unsigned op = (insn >> 29) & 1, cmode = (insn >> 12) & 0xFu, o2 = (insn >> 11) & 1;
        uint64_t imm8 = (uint64_t)(((insn >> 16) & 7u) << 5) | ((insn >> 5) & 0x1Fu);
        uint64_t pattern;
        if (!interp_advsimd_expand_imm(op, cmode, o2, q, imm8, &pattern))
            return interp_undefined(cpu, insn, "AdvSIMD modified immediate -- reserved cmode");
        // ORR/BIC is cmode 0xx1 and 10x1 only: cmode<0> == 1 with cmode<3:2> != 11. Testing cmode<3:1>
        // instead let 1101 -- MOVI/MVNI with MSL #16 -- read-modify the destination instead of replacing it.
        int read_modify = (cmode & 1u) && ((cmode >> 2) & 3u) != 3u;
        interp_vec result = interp_vec_read(cpu, rd);
        uint64_t low, high;
        memcpy(&low, result.byte, 8);
        memcpy(&high, result.byte + 8, 8);
        if (read_modify) {
            if (op) { // BIC
                low &= ~pattern;
                high &= ~pattern;
            } else { // ORR
                low |= pattern;
                high |= pattern;
            }
        } else if (op && ((cmode >> 1) & 7u) != 7u) { // MVNI
            low = high = ~pattern;
        } else { // MOVI
            low = high = pattern;
        }
        memcpy(result.byte, &low, 8);
        memcpy(result.byte + 8, &high, 8);
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

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

    // AdvSIMD two-register miscellaneous (FP16): the same operations at half precision, in a box of their
    // own at bits[23:17] == 1111100 that the mask below does not reach. Only the members this file
    // implements are decoded; the rest of the box (FABS/FNEG/FSQRT/FRINT*/FCVT*/FCMxx at .4H/.8H/H) has
    // never been decoded here and keeps reporting rather than being guessed at.
    if ((decode & 0x9FFE0C00u) == 0x0EF80800u) {
        unsigned opcode = (insn >> 12) & 0x1Fu;
        if (opcode != 0x1Du && !(opcode == 0x1Fu && !u && scalar))
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc (FP16) -- unimplemented opcode");
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned lane = 0; lane < (scalar ? 1u : (q ? 8u : 4u)); lane++) {
            uint64_t a = interp_vec_element(&source, INTERP_FP_H + 1u, lane), value;
            if (opcode == 0x1Fu)
                value = interp_fp_recpx(INTERP_FP_H, a); // FRECPX
            else
                value = u ? interp_fp_rsqrt_estimate(INTERP_FP_H, a) : interp_fp_recip_estimate(INTERP_FP_H, a);
            interp_vec_set_element(&result, INTERP_FP_H + 1u, lane, value);
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD two-register misc
    if ((decode & 0x9F3E0C00u) == 0x0E200800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned bytes = q ? 16u : 8u;

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
                        uint64_t element_bits =
                            interp_vec_element(&source, narrow + 1u, q ? lane + narrow_lanes : lane);
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
                        value = interp_fp_round_integral(fmt, a,
                                                         u ? INTERP_RM_RA : (high ? INTERP_RM_RP : INTERP_RM_RN), 0);
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
                        value =
                            interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RP : INTERP_RM_RN, 0);
                        break;
                    case 0x1B: // FCVTMS/FCVTMU or FCVTZS/FCVTZU
                        value =
                            interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RZ : INTERP_RM_RM, 0);
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
                            value = interp_fp_from_int(fmt, a, interp_fp_width(fmt), !u,
                                                       INTERP_FPCR_RMODE(g_interp_fpcr), 0);
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

        // FP members (opcode >= 11000): bit23 the operation, bit22 `sz`
        if (opcode >= 0x18) {
            unsigned fmt = fp16_three_same ? INTERP_FP_H : ((size & 1u) ? INTERP_FP_D : INTERP_FP_S);
            unsigned high = (size >> 1) & 1u;
            if (fmt == INTERP_FP_D && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD three same -- 2D form requires Q");
            unsigned fp_lanes = scalar ? 1u : interp_vec_lanes(fmt + 1u, q);
            unsigned element = fmt + 1u;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            uint64_t sign = interp_fp_sign_mask(fmt);
            // interp_fp_compare writes NZCV as scalar FCMP must; a VECTOR compare must not.
            uint64_t saved_nzcv = cpu->nzcv;
            for (unsigned lane = 0; lane < fp_lanes; lane++) {
                // Pairwise forms take both operands from Vn:Vm, not from matching lanes.
                int pairwise = u && (opcode == 0x18 || opcode == 0x1A || opcode == 0x1E) && !(opcode == 0x1A && high);
                uint64_t a, b;
                if (pairwise) {
                    const interp_vec *source = lane < fp_lanes / 2u ? &left : &right;
                    unsigned base = (lane < fp_lanes / 2u ? lane : lane - fp_lanes / 2u) * 2u;
                    a = interp_vec_element(source, element, base);
                    b = interp_vec_element(source, element, base + 1u);
                } else {
                    a = interp_vec_element(&left, element, lane);
                    b = interp_vec_element(&right, element, lane);
                }
                uint64_t value;
                if (!u) {
                    switch (opcode) {
                    case 0x18: value = interp_fp_minmax(fmt, a, b, !high, 1); break; // FMAXNM / FMINNM
                    case 0x19: {                                                     // FMLA / FMLS
                        uint64_t addend = interp_vec_element(&accumulate, element, lane);
                        value = interp_fp_muladd(fmt, addend, high ? (a ^ sign) : a, b);
                        break;
                    }
                    case 0x1A:
                        value = interp_fp_arith(fmt, high ? INTERP_FPOP_SUB : INTERP_FPOP_ADD, a, b);
                        break; // FADD / FSUB
                    case 0x1B:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_mulx(fmt, a, b);
                        break;   // FMULX
                    case 0x1C: { // FCMEQ (register)
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        interp_fp_compare(cpu, fmt, a, b, 0);
                        value = interp_flag_z(cpu) ? interp_element_mask(element) : UINT64_C(0);
                        break;
                    }
                    case 0x1E: value = interp_fp_minmax(fmt, a, b, !high, 0); break; // FMAX / FMIN
                    case 0x1F: value = interp_fp_recip_step(fmt, a, b, high); break; // FRECPS / FRSQRTS
                    default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                    }
                } else {
                    switch (opcode) {
                    case 0x18: value = interp_fp_minmax(fmt, a, b, !high, 1); break; // FMAXNMP / FMINNMP
                    case 0x1A:
                        // FADDP at bit23 clear, FABD at set; FABD is NOT pairwise, hence the exclusion.
                        value = high ? (interp_fp_arith(fmt, INTERP_FPOP_SUB, a, b) & ~sign)
                                     : interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b);
                        break;
                    case 0x1B:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
                        break;   // FMUL
                    case 0x1C:   // FCMGE / FCMGT
                    case 0x1D: { // FACGE / FACGT (absolute)
                        uint64_t x = a, y = b;
                        if (opcode == 0x1D) {
                            x &= ~sign;
                            y &= ~sign;
                        }
                        // FPCompareGE/GT raise Invalid for a QUIET NaN too, unlike FPCompareEQ above.
                        interp_fp_compare(cpu, fmt, x, y, 1);
                        // "ge" is C set minus unordered; "gt" adds Z clear.
                        int ordered = !(interp_flag_c(cpu) && interp_flag_v(cpu));
                        int holds = ordered && interp_flag_c(cpu) && (!high || !interp_flag_z(cpu));
                        value = holds ? interp_element_mask(element) : UINT64_C(0);
                        break;
                    }
                    case 0x1E: value = interp_fp_minmax(fmt, a, b, !high, 0); break; // FMAXP / FMINP
                    case 0x1F:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_arith(fmt, INTERP_FPOP_DIV, a, b);
                        break; // FDIV
                    default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                    }
                }
                interp_vec_set_element(&result, element, lane, value);
            }
            if (opcode == 0x1C || opcode == 0x1D) cpu->nzcv = saved_nzcv;
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

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
        case 0x09:   // SQSHL / UQSHL: variable shift, right when Rm's lane is negative
        case 0x0A:   // SRSHL / URSHL
        case 0x0B: { // SQRSHL / UQRSHL
            unsigned esize = 8u << size;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane);
                int8_t amount = (int8_t)(interp_vec_element(&right, size, lane) & 0xFFu);
                uint64_t value;
                if (amount >= 0) {
                    unsigned shift = (unsigned)amount;
                    if (opcode == 0x0A) { // SRSHL/URSHL left: exact, like SSHL
                        value = shift >= esize ? 0 : (a << shift) & mask;
                    } else if (u) {
                        uint64_t saturated = (a & mask);
                        // shift == 0 must be spelled out: at esize 64 the else arm shifts by 64, which the
                        // host masks to 0 and so saturates every nonzero input on a no-op shift.
                        int overflow =
                            shift != 0 && (shift >= esize ? saturated != 0 : (saturated >> (esize - shift)) != 0);
                        if (overflow) {
                            interp_fpsr_raise(INTERP_FPSR_QC);
                            value = mask;
                        } else {
                            value = (saturated << shift) & mask;
                        }
                    } else {
                        int64_t x = (int64_t)interp_element_sext(a, size);
                        int64_t max = esize == 64 ? INT64_MAX : (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u);
                        int64_t min = esize == 64 ? INT64_MIN : -max - 1;
                        int64_t shifted = x;
                        int overflowed = 0;
                        for (unsigned step = 0; step < shift && !overflowed; step++) {
                            if (shifted > (max >> 1) || shifted < (min >> 1) || (shifted << 1) >> 1 != shifted)
                                overflowed = 1;
                            else
                                shifted <<= 1;
                        }
                        if (overflowed || shifted > max || shifted < min) {
                            interp_fpsr_raise(INTERP_FPSR_QC);
                            shifted = x < 0 ? min : max;
                        }
                        value = (uint64_t)shifted & mask;
                    }
                } else {
                    // A negative amount is a right shift, never saturates; rounding adds half the field.
                    unsigned shift = (unsigned)(-amount);
                    int rounding = opcode == 0x0A || opcode == 0x0B;
                    if (u) {
                        uint64_t x = a & mask;
                        uint64_t round = rounding && shift <= 64u && shift > 0 ? (x >> (shift - 1u)) & 1u : 0u;
                        value = shift >= esize ? round : ((x >> shift) + round);
                    } else {
                        int64_t x = (int64_t)interp_element_sext(a, size);
                        uint64_t round =
                            rounding && shift > 0 ? (uint64_t)((x >> (shift >= 64u ? 63u : shift - 1u)) & 1) : 0u;
                        int64_t shifted = shift >= esize ? (x >> (esize - 1u)) : (x >> shift);
                        value = (uint64_t)shifted + round;
                    }
                    value &= mask;
                }
                interp_vec_set_element(&result, size, lane, value);
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
                interp_vec_set_element(
                    &result, size, lane,
                    (interp_vec_element(&left, size, lane) * interp_vec_element(&right, size, lane)) & mask);
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
        default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD three-same-EXTRA: FEAT_DotProd / FEAT_I8MM / FEAT_RDM / FCMLA-FCADD
    // Same mask as the copy group, separated only by bit15.
    if ((decode & 0x9F208400u) == 0x0E008400u) {
        unsigned opcode = (decode >> 11) & 0xFu, size = (decode >> 22) & 3u;
        int n_signed, m_signed;
        // !scalar: the normalisation above folds the scalar boxes into this spelling, and no dot/MMLA form has
        // a scalar variant, so a scalar encoding here is some other instruction.
        if (!scalar && size == 2u && interp_dot_signedness(opcode, u, &n_signed, &m_signed)) {
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
            interp_vec result = interp_vec_read(cpu, rd);
            if (opcode <= 3u) { // SDOT / UDOT / USDOT (vector): one 4-byte dot product per 32-bit lane
                for (unsigned lane = 0; lane < (q ? 4u : 2u); lane++)
                    interp_vec_set_element(&result, 2, lane,
                                           (uint32_t)interp_vec_element(&result, 2, lane) +
                                               interp_dot4(&left, &right, 4u * lane, 4u * lane, n_signed, m_signed));
            } else { // SMMLA / UMMLA / USMMLA: 2x8 by 8x2, one eight-element dot product per lane
                if (!q) return interp_undefined(cpu, insn, "AdvSIMD three-same-extra -- MMLA requires Q=1");
                for (unsigned i = 0; i < 2u; i++)
                    for (unsigned j = 0; j < 2u; j++) {
                        uint32_t sum = (uint32_t)interp_vec_element(&result, 2, 2u * i + j);
                        sum += interp_dot4(&left, &right, 8u * i, 8u * j, n_signed, m_signed);
                        sum += interp_dot4(&left, &right, 8u * i + 4u, 8u * j + 4u, n_signed, m_signed);
                        interp_vec_set_element(&result, 2, 2u * i + j, sum);
                    }
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // SQRDMLAH / SQRDMLSH (FEAT_RDM): opcode 0000/0001 at U=1, 16- or 32-bit elements, scalar forms too.
        if (u && opcode <= 1u && (size == 1u || size == 2u)) {
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
            interp_vec accumulate = interp_vec_read(cpu, rd), result;
            memset(result.byte, 0, sizeof result.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++)
                interp_vec_set_element(&result, size, lane,
                                       interp_sqrdmlah_element(interp_vec_element(&accumulate, size, lane),
                                                               interp_vec_element(&left, size, lane),
                                                               interp_vec_element(&right, size, lane), size, opcode));
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // FCMLA/FCADD (FEAT_FCMA) and the BF16/FP8 forms share this box and stay an honest gap report.
        return interp_undefined(cpu, insn, "AdvSIMD three-same-extra (FCMLA/FCADD, BFDOT/BFMMLA, FP8)");
    }

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

    return interp_undefined(cpu, insn, "scalar floating-point and Advanced SIMD");
}

// One guest instruction; cpu->pc ends on the NEXT instruction or the branch target. Returns INTERP_NEXT,
// or INTERP_END with cpu->reason set. The switch is the ARM ARM's op0 = insn[28:25] table, in order.
