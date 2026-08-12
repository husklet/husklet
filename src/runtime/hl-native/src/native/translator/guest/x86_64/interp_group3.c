static int interp_one_byte_group3(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xF6 && op != 0xF7) return -1;

    int width = (op & 1) ? insn->opsize : 1;
    int operation = insn->reg & 7;
    interp_operand operand = interp_rm(cpu, insn, next);
    switch (operation) {
    case 0:
    case 1: {
        uint64_t value = interp_rm_read(cpu, insn, &operand, width);
        interp_flags_logic(cpu, value & (uint64_t)insn->imm & interp_mask(width), width);
        break;
    }
    case 2:
        if (insn->lock && operand.is_memory) {
            (void)interp_locked_rmw(operand.address, width, RMW_NOT, 0, 0);
        } else {
            uint64_t value = interp_rm_read(cpu, insn, &operand, width);
            interp_rm_write(cpu, insn, &operand, width, (~value) & interp_mask(width));
        }
        break;
    case 3: {
        uint64_t old = insn->lock && operand.is_memory ? interp_locked_rmw(operand.address, width, RMW_NEG, 0, 0)
                                                       : interp_rm_read(cpu, insn, &operand, width);
        uint64_t result = interp_alu_sub(cpu, 0, old, 0, width);
        if (!(insn->lock && operand.is_memory)) interp_rm_write(cpu, insn, &operand, width, result);
        break;
    }
    case 4:
    case 5:
        interp_widening_multiply(cpu, insn, interp_rm_read(cpu, insn, &operand, width), width, operation == 5);
        break;
    case 6:
    case 7: return interp_divide(cpu, interp_rm_read(cpu, insn, &operand, width), width, operation == 7, pc, next);
    default: break;
    }
    cpu->rip = next;
    return STEP_NEXT;
}
