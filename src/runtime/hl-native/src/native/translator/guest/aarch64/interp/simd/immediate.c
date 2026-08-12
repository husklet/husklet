static int interp_simd_immediate(struct cpu *cpu, uint32_t insn, uint32_t decode, unsigned scalar, unsigned q,
                                 unsigned u) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
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
    return INTERP_SIMD_UNHANDLED;
}
