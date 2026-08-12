static int interp_simd_three_same_float(struct cpu *cpu, uint32_t insn, unsigned scalar, unsigned q, unsigned u,
                                        unsigned fp16_three_same) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned size = (insn >> 22) & 3u;
    unsigned opcode = fp16_three_same ? (0x18u | ((insn >> 11) & 7u)) : ((insn >> 11) & 0x1Fu);
    interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
    memset(result.byte, 0, sizeof result.byte);
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
    return INTERP_SIMD_UNHANDLED;
}
