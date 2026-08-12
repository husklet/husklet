static int interp_simd_extra(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
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
    return INTERP_SIMD_UNHANDLED;
}
