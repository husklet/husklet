static int interp_simd_fp16(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q, unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31); // two-register: one source, no Rm
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
    return INTERP_SIMD_UNHANDLED;
}
