static int interp_simd_three_same_shift(struct cpu *cpu, uint32_t insn, unsigned q, unsigned u) {
    unsigned opcode = (insn >> 11) & 0x1fu;
    if (opcode < 0x09u || opcode > 0x0bu) return INTERP_SIMD_UNHANDLED;
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned size = (insn >> 22) & 3u, lanes = interp_vec_lanes(size, q);
    uint64_t mask = interp_element_mask(size);
    interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
    memset(result.byte, 0, sizeof result.byte);
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
                int overflow = shift != 0 && (shift >= esize ? saturated != 0 : (saturated >> (esize - shift)) != 0);
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
                uint64_t round = rounding && shift > 0 ? (uint64_t)((x >> (shift >= 64u ? 63u : shift - 1u)) & 1) : 0u;
                int64_t shifted = shift >= esize ? (x >> (esize - 1u)) : (x >> shift);
                value = (uint64_t)shifted + round;
            }
            value &= mask;
        }
        interp_vec_set_element(&result, size, lane, value);
    }
    interp_vec_write(cpu, rd, result, q);
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}
