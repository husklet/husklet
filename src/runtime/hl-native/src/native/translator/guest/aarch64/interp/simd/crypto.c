#include "../crypto/primitives.h"

static int interp_simd_crypto(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                              unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
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
    return INTERP_SIMD_UNHANDLED;
}
