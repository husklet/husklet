// Group 2: C0/C1 by imm8, D0/D1 by 1, D2/D3 by CL. RCL/RCR reduce the count modulo width+1.
static int interp_one_byte_shift(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xC0 && op != 0xC1 && op != 0xD0 && op != 0xD1 && op != 0xD2 && op != 0xD3) return -1;
    int kind = insn->reg & 7;
    if (kind == 6) kind = 4;
    int width = (op & 1) ? insn->opsize : 1;
    int by_cl = op == 0xD2 || op == 0xD3;
    int by_one = op == 0xD0 || op == 0xD1;
    interp_operand operand = interp_rm(cpu, insn, next);
    if (kind == 2 || kind == 3) {
        uint64_t descriptor = (uint64_t)width | ((uint64_t)(kind == 3) << 8);
        if (operand.is_memory) {
            cpu->x87_ea = hl_x86_guest_pointer(operand.address);
            descriptor |= UINT64_C(1) << 9;
        } else {
            int high_byte = interp_hi8(insn, operand.number, width);
            descriptor |= ((uint64_t)(high_byte != 0) << 10) |
                          ((uint64_t)((high_byte ? operand.number - 4 : operand.number) & 0x1f) << 16);
        }
        if (!by_cl) {
            unsigned count = (unsigned)(by_one ? 1 : (insn->imm & (width == 8 ? 63 : 31)));
            unsigned effective = count % ((unsigned)(8 * width) + 1u);
            uint64_t mask = interp_mask(width);
            uint64_t value = interp_rm_read(cpu, insn, &operand, width);
            unsigned carry = interp_cf(cpu);
            for (unsigned iteration = 0; iteration < effective; iteration++) {
                if (kind == 2) {
                    unsigned msb = interp_msb(value, width);
                    value = ((value << 1) | carry) & mask;
                    carry = msb;
                } else {
                    unsigned lsb = (unsigned)(value & 1);
                    value = ((value >> 1) | ((uint64_t)carry << (8 * width - 1))) & mask;
                    carry = lsb;
                }
            }
            if (count != 0) {
                interp_set_cf(cpu, carry);
                if (count == 1) {
                    unsigned overflow = kind == 2
                                            ? interp_msb(value, width) ^ carry
                                            : interp_msb(value, width) ^ (unsigned)((value >> (8 * width - 2)) & 1);
                    cpu->nzcv = (cpu->nzcv & ~NZ_V) | ((uint64_t)overflow << 28);
                }
            }
            interp_rm_write(cpu, insn, &operand, width, value);
            cpu->rip = next;
            return STEP_NEXT;
        }
        cpu->divop = descriptor;
        return interp_exit(cpu, next, R_RCL);
    }
    unsigned count = (unsigned)(by_cl ? cpu->r[RCX] & 0xff : (by_one ? 1u : (unsigned)(insn->imm & 0xff)));
    uint64_t value = interp_rm_read(cpu, insn, &operand, width);
    interp_rm_write(cpu, insn, &operand, width, interp_shift(cpu, kind, value, count, width));
    cpu->rip = next;
    return STEP_NEXT;
}
