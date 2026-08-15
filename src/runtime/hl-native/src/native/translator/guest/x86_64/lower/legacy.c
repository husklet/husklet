#include "legacy.h"
#include "arithmetic.h"
#include "alu.h"
#include "mov.h"
#include "repstr.h"
#include "shift.h"
#include "trace.h"
#include "x87.h"
#include "execution_control.h"
#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"

int lower_primary_fast(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                              const hl_x86_trace_state *trace_state) {
    hl_x86_move_image image;
    hl_x86_legacy_image(&image.low, &image.high, &image.bias);
    int result = hl_x86_lower_mov(instruction, next, &image);
    if (result != TX_FALL) return result;
    result = hl_x86_lower_alu(instruction, next);
    if (result != TX_FALL) return result;

    const hl_x86_shift_state shift_state = {
        .parity_aux_dead = hl_x86_legacy_pfaf_dead(),
        .output_flags_dead = trace_state->flag_elision &&
                             !(hl_x86_trace_flags_livein(trace_state, next, guest_pc) &
                               (HL_X86_FLAG_ALL & ~HL_X86_FLAG_AF)),
        .direct_registers = 1,
    };
    return hl_x86_lower_shift(instruction, next, &shift_state);
}

int lower_primary_string(struct insn *instruction, uint64_t next, hl_x86_crypto_state *crypto_state) {
    hl_x86_repstr_state state = {.direction = hl_x86_legacy_direction(), .optimize = 1};
    int result = hl_x86_lower_repstr(instruction, next, &state);
    hl_x86_legacy_direction_set(state.direction);
    if (result == TX_NEXT) {
        // The ERMS funnel can clobber v16..v31, including hoisted constants.
        crypto_state->zero_ready = crypto_state->mask_ready = 0;
    }
    return result;
}

int lower_group3_unary(struct insn *instruction, uint64_t next) {
    if (instruction->op != 0xF6 && instruction->op != 0xF7) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = instruction->op == 0xF6 ? 1 : instruction->opsize;
    int memory;
    if (operation == 0) {
        int value = rm_load(instruction, next, width, &memory);
        e_movconst(19, (uint64_t)instruction->imm);
        do_alu(4, -1, value, 19, width);
        return TX_NEXT;
    }
    if (operation == 2) {
        int value = rm_load(instruction, next, width, &memory);
        emit32(0xAA2003E0u | ((uint32_t)value << 16) | 16u);
        rm_store(instruction, width, 16);
        return TX_NEXT;
    }
    if (operation != 3) return TX_FALL;

    int value = rm_load(instruction, next, width, &memory);
    if (width < 4) {
        do_alu(5, 16, 31, value, width);
    } else {
        e_rrr(A_SUBS, 16, 31, value, width == 8, 0);
        e_nzcv_save();
        e_pf_save(16);
        e_af_addsub(31, value, 16, 19);
    }
    rm_store(instruction, width, 16);
    return TX_NEXT;
}

int lower_group3_narrow_muldiv(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xF6 && instruction->op != 0xF7) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = instruction->op == 0xF6 ? 1 : instruction->opsize;
    if (operation < 4 || width > 2) return TX_FALL;

    int memory;
    int value = rm_load(instruction, next, width, &memory);
    if (width == 1) {
        if (operation == 4 || operation == 5) {
            if (operation == 4) {
                e_uxt(19, RAX, 1);
                e_uxt(20, value, 1);
            } else {
                e_sxt(19, RAX, 1);
                e_sxt(20, value, 1);
            }
            e_mul(21, 19, 20, 0);
            e_mul_oc_narrow(21, operation, 1);
            e_bfi(RAX, 21, 0, 16, 1);
        } else {
            if (operation == 6) {
                e_uxt(19, RAX, 2);
                e_uxt(20, value, 1);
                emit_div_zero_check(20, guest_pc, 0);
                e_udiv(21, 19, 20, 0);
            } else {
                e_sxt(19, RAX, 2);
                e_sxt(20, value, 1);
                emit_div_zero_check(20, guest_pc, 1);
                e_sdiv(21, 19, 20, 0);
            }
            e_msub(22, 21, 20, 19, 0);
            emit_div_ovf_check(21, 23, 1, operation == 7, guest_pc, operation == 7);
            e_bfi(RAX, 21, 0, 8, 1);
            e_bfi(RAX, 22, 8, 8, 1);
        }
        return TX_NEXT;
    }

    if (operation == 4 || operation == 5) {
        if (operation == 4) {
            e_uxt(19, RAX, 2);
            e_uxt(20, value, 2);
        } else {
            e_sxt(19, RAX, 2);
            e_sxt(20, value, 2);
        }
        e_mul(21, 19, 20, 0);
        e_mul_oc_narrow(21, operation, 2);
        e_bfi(RAX, 21, 0, 16, 1);
        e_lsr_i(21, 21, 16, 0);
        e_bfi(RDX, 21, 0, 16, 1);
    } else {
        e_uxt(19, RAX, 2);
        e_bfi(19, RDX, 16, 16, 0);
        if (operation == 6) {
            e_uxt(20, value, 2);
            emit_div_zero_check(20, guest_pc, 0);
            e_udiv(21, 19, 20, 0);
        } else {
            e_sxt(20, value, 2);
            emit_div_zero_check(20, guest_pc, 1);
            e_sdiv(21, 19, 20, 0);
        }
        e_msub(22, 21, 20, 19, 0);
        emit_div_ovf_check(21, 23, 2, operation == 7, guest_pc, operation == 7);
        e_bfi(RAX, 21, 0, 16, 1);
        e_bfi(RDX, 22, 0, 16, 1);
    }
    return TX_NEXT;
}

int lower_group3_wide_muldiv(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xF7 || (instruction->opsize != 4 && instruction->opsize != 8)) return TX_FALL;
    int operation = instruction->reg & 7;
    if (operation < 4) return TX_FALL;
    int width = instruction->opsize;
    int memory;
    int value = rm_load(instruction, next, width, &memory);
    if (operation == 4 || operation == 5) {
        if (width == 4) {
            if (operation == 4) {
                e_uxt(20, RAX, 4);
                e_uxt(21, value, 4);
            } else {
                e_sxt(20, RAX, 4);
                e_sxt(21, value, 4);
            }
            e_mul(19, 20, 21, 1);
            e_lsr_i(RDX, 19, 32, 1);
            e_mov_rr(RAX, 19, 0);
            if (operation == 4) {
                e_lsr_i(22, 19, 32, 1);
                e_subi_s(23, 22, 0, 1);
            } else {
                e_sxt(22, 19, 4);
                e_rrr(A_SUBS, 23, 19, 22, 1, 0);
            }
        } else {
            e_mul(19, RAX, value, 1);
            if (operation == 4)
                e_umulh(RDX, RAX, value);
            else
                e_smulh(RDX, RAX, value);
            e_mov_rr(RAX, 19, 1);
            if (operation == 4) {
                e_mov_rr(22, RDX, 1);
                e_subi_s(23, 22, 0, 1);
            } else {
                e_asr_i(22, 19, 63, 1);
                e_rrr(A_SUBS, 23, RDX, 22, 1, 0);
            }
        }
        e_cset(21, 1 /*NE*/, 1);
        e_mul_set_oc(21);
        return TX_NEXT;
    }
    if (operation != 6 && operation != 7) {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    if (width == 8) {
        emit_div64_fast(next, guest_pc, operation == 7, value);
        return TX_NEXT;
    }

    e_lsl_i(19, RDX, 32, 1);
    e_bfi(19, RAX, 0, 32, 1);
    if (operation == 6) {
        e_uxt(22, value, 4);
        emit_div_zero_check(22, guest_pc, 0);
        e_udiv(20, 19, 22, 1);
    } else {
        e_sxt(22, value, 4);
        emit_div_zero_check(22, guest_pc, 1);
        e_sdiv(20, 19, 22, 1);
    }
    e_msub(21, 20, 22, 19, 1);
    emit_div_ovf_check(20, 23, 4, operation == 7, guest_pc, operation == 7);
    e_mov_rr(RAX, 20, 0);
    e_mov_rr(RDX, 21, 0);
    return TX_NEXT;
}

int lower_group45(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xFE && opcode != 0xFF) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = opcode == 0xFE ? 1 : instruction->opsize;
    int wide = width == 8;
    int memory;
    if (operation == 0 || operation == 1) {
        uint32_t access = X86_SOFT_READ | X86_SOFT_WRITE;
        int value = rm_load_access(instruction, next, width, &memory, access);
        if (instruction->lock && memory) {
            e_movconst(19, operation == 0 ? 1 : (uint64_t)-1);
            e_lse(LSE_LDADD, width, 19, 20, 17);
            if (width >= 4) {
                if (operation == 0)
                    e_addi_s(21, 20, 1, wide);
                else
                    e_subi_s(21, 20, 1, wide);
                e_af_addsub(20, 21, 31, 19);
                e_nzcv_save_keepC();
                e_pf_save(21);
            } else {
                int shift = 8 * (4 - width);
                e_mov_rr(26, 20, 0);
                e_lsl_i(21, 20, shift, 0);
                e_movconst(19, 1u << shift);
                if (operation == 0)
                    e_rrr(A_ADDS, 21, 21, 19, 0, 0);
                else
                    e_rrr(A_SUBS, 21, 21, 19, 0, 0);
                e_nzcv_save_keepC();
                e_lsr_i(21, 21, shift, 0);
                e_pf_save(21);
                e_af_addsub(26, 21, 31, 19);
            }
            if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
            return TX_NEXT;
        }
        int output = memory ? 16 : instruction->rm_reg;
        if (width >= 4) {
            e_mov_rr(26, value, wide);
            if (operation == 0)
                e_addi_s(output, value, 1, wide);
            else
                e_subi_s(output, value, 1, wide);
            e_nzcv_save_keepC();
            e_pf_save(output);
            e_af_addsub(26, output, 31, 19);
            rm_store_after_guard(instruction, width, output);
        } else {
            int shift = 8 * (4 - width);
            e_lsl_i(21, value, shift, 0);
            e_movconst(19, 1u << shift);
            if (operation == 0)
                e_rrr(A_ADDS, 21, 21, 19, 0, 0);
            else
                e_rrr(A_SUBS, 21, 21, 19, 0, 0);
            e_nzcv_save_keepC();
            e_lsr_i(21, 21, shift, 0);
            e_pf_save(21);
            e_af_addsub(value, 21, 31, 19);
            rm_store_after_guard(instruction, width, 21);
        }
        return TX_NEXT;
    }
    if (opcode == 0xFF && (operation == 4 || operation == 2)) {
        int target = rm_load(instruction, next, 8, &memory);
        if (target != 16) e_mov_rr(16, target, 1);
        e_movconst(19, guest_pc);
        e_str(19, 28, OFF_IBSRC);
        if (operation == 2) {
            e_subi(RSP, RSP, 8, 1);
            e_movconst(19, call_return_pc(next));
            e_store(8, 19, RSP);
        }
        emit_ibranch();
        return TX_BREAK;
    }
    if (opcode == 0xFF && operation == 6) {
        int value = rm_load(instruction, next, 8, &memory);
        if (value != 16) e_mov_rr(16, value, 1);
        e_subi(RSP, RSP, 8, 1);
        e_store(8, 16, RSP);
        return TX_NEXT;
    }
    return TX_FALL;
}

int lower_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0x86 && instruction->op != 0x87) return TX_FALL;
    int width = (instruction->op & 1) ? instruction->opsize : 1;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        int source = width == 1 ? byte_val(instruction, instruction->reg, 19) : instruction->reg;
        // A memory XCHG is implicitly atomic even without a LOCK prefix.
        e_lse(LSE_SWP, width, source, 16, 17);
        if (width >= 4)
            e_mov_rr(instruction->reg, 16, width == 8);
        else if (width == 1)
            byte_wb(instruction, instruction->reg, 16);
        else
            e_bfi(instruction->reg, 16, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width == 1) {
        // Materialize both byte lanes before either write; they may alias.
        int left = byte_val(instruction, instruction->reg, 16);
        int right = byte_val(instruction, instruction->rm_reg, 17);
        e_mov_rr(19, left, 0);
        e_mov_rr(23, right, 0);
        byte_wb(instruction, instruction->reg, 23);
        byte_wb(instruction, instruction->rm_reg, 19);
    } else if (width == 2) {
        e_mov_rr(19, instruction->rm_reg, 1);
        e_bfi(instruction->rm_reg, instruction->reg, 0, 16, 1);
        e_bfi(instruction->reg, 19, 0, 16, 1);
    } else {
        int wide = width == 8;
        e_mov_rr(19, instruction->rm_reg, wide);
        e_mov_rr(instruction->rm_reg, instruction->reg, wide);
        e_mov_rr(instruction->reg, 19, wide);
    }
    return TX_NEXT;
}

int lower_stack_control(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op == 0x68 || instruction->op == 0x6A) {
        e_subi(RSP, RSP, 8, 1);
        e_movconst(16, (uint64_t)instruction->imm);
        e_store(8, 16, RSP);
        return TX_NEXT;
    }
    if (instruction->op == 0x8F) {
        if (instruction->is_mem) {
            // The destination address observes RSP after the pop.
            e_load(8, 19, RSP);
            e_addi(RSP, RSP, 8, 1);
            emit_ea(instruction, next);
            e_store(8, 19, 17);
        } else {
            e_load(8, 16, RSP);
            e_addi(RSP, RSP, 8, 1);
            e_mov_rr(instruction->rm_reg, 16, 1);
        }
        return TX_NEXT;
    }
    if (instruction->op == 0xC3 || instruction->op == 0xC2) {
        if (emit_soft_memory_active() || emit_displaced_stack_active()) {
            e_mov_rr(17, RSP, 1);
            emit_displaced_stack_address(17);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
        }
        e_load(8, 16, (emit_soft_memory_active() || emit_displaced_stack_active()) ? 17 : RSP);
        e_addi(RSP, RSP, 8, 1);
        if (instruction->op == 0xC2) {
            e_movconst(19, (uint64_t)(uint16_t)instruction->imm);
            e_rrr(A_ADD, RSP, RSP, 19, 1, 0);
        }
        e_movconst(19, guest_pc);
        e_str(19, 28, OFF_IBSRC);
        emit_ibranch();
        return TX_BREAK;
    }
    if (instruction->op != 0xC9) return TX_FALL;
    if (emit_soft_memory_active()) {
        e_mov_rr(17, RBP, 1);
        emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
    }
    e_mov_rr(RSP, RBP, 1);
    e_load(8, RBP, emit_soft_memory_active() ? 17 : RSP);
    e_addi(RSP, RSP, 8, 1);
    return TX_NEXT;
}

int lower_immediate_multiply(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                                    const hl_x86_trace_state *trace_state) {
    if (instruction->op != 0x69 && instruction->op != 0x6B) return TX_FALL;
    int memory;
    int source = rm_load(instruction, next, instruction->opsize, &memory);
    e_movconst(19, (uint64_t)instruction->imm);
    int overflow_live = !trace_state->flag_elision ||
                        (hl_x86_trace_flags_livein(trace_state, next, guest_pc) & HL_X86_FLAG_NZCV);
    e_imul2(instruction->reg, source, 19, instruction->opsize, overflow_live);
    return TX_NEXT;
}

int lower_direct_call_loop(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                           hl_x86_trace_state *trace_state) {
    uint8_t opcode = instruction->op;
    uint64_t taken = next + (uint64_t)instruction->imm;
    if (opcode == 0xE8) {
        if (emit_soft_memory_active() || emit_displaced_stack_active()) {
            e_subi(17, RSP, 8, 1);
            emit_displaced_stack_address(17);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
        }
        e_subi(RSP, RSP, 8, 1);
        e_movconst(16, call_return_pc(next));
        e_store(8, 16, (emit_soft_memory_active() || emit_displaced_stack_active()) ? 17 : RSP);
        hl_x86_trace_flags_edge(trace_state, taken, guest_pc);
        emit_chain_exit(taken);
        return TX_BREAK;
    }
    if (opcode == 0xE3) {
        uint32_t cbz = instruction->addr32 ? 0x34000000u : 0xB4000000u;
        uint32_t *patch = hl_x86_emit_cursor();
        emit32(0);
        emit_chain_exit(next);
        int64_t distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)patch) / 4;
        *patch = cbz | (((uint32_t)distance & 0x7FFFF) << 5) | RCX;
        emit_chain_exit(taken);
        return TX_BREAK;
    }
    if (opcode != 0xE0 && opcode != 0xE1 && opcode != 0xE2) return TX_FALL;

    int wide = instruction->addr32 ? 0 : 1;
    uint32_t cbz = instruction->addr32 ? 0x34000000u : 0xB4000000u;
    uint32_t cbnz = instruction->addr32 ? 0x35000000u : 0xB5000000u;
    e_subi(RCX, RCX, 1, wide);
    if (opcode == 0xE2) {
        uint32_t *patch = hl_x86_emit_cursor();
        emit32(0);
        emit_chain_exit(next);
        int64_t distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)patch) / 4;
        *patch = cbnz | (((uint32_t)distance & 0x7FFFF) << 5) | RCX;
        emit_chain_exit(taken);
        return TX_BREAK;
    }

    e_nzcv_load();
    int fail_condition = opcode == 0xE1 ? 1 : 0;
    uint32_t *counter_patch = hl_x86_emit_cursor();
    emit32(0);
    uint32_t *flag_patch = hl_x86_emit_cursor();
    emit32(0);
    emit_chain_exit(taken);
    int64_t counter_distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)counter_patch) / 4;
    *counter_patch = cbz | (((uint32_t)counter_distance & 0x7FFFF) << 5) | RCX;
    int64_t flag_distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)flag_patch) / 4;
    *flag_patch = 0x54000000u | (((uint32_t)flag_distance & 0x7FFFF) << 5) | (uint32_t)fail_condition;
    emit_chain_exit(next);
    return TX_BREAK;
}

int lower_flag_register_transfer(struct insn *instruction) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x9E) {
        emit32(0x53083C00u | (RAX << 5) | 16); // AH
        emit32(0x53000000u | (16 << 5) | 17);  // CF
        e_movconst(20, 1);
        e_rrr(A_EOR, 17, 17, 20, 0, 0); // stored borrow-C = !CF
        e_lsl_i(17, 17, 29, 0);
        emit32(0x53061800u | (16 << 5) | 18); // ZF
        e_lsl_i(18, 18, 30, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        emit32(0x53071C00u | (16 << 5) | 18); // SF
        e_lsl_i(18, 18, 31, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_str(17, 28, OFF_NZCV);
        emit32(0xD51B4200u | 17);
        emit32(0x53020800u | (16 << 5) | 19); // PF
        e_rrr(A_EOR, 19, 19, 20, 0, 0);
        e_str(19, 28, OFF_PF);
        e_af_save(16);
        hl_x86_legacy_flags_pending_clear();
        return TX_NEXT;
    }
    if (opcode == 0x9F) {
        if (hl_x86_legacy_flags_pending()) flags_materialize();
        e_ldr(16, 28, OFF_NZCV);
        emit32(0x53000000u | (31 << 16) | (31 << 10) | (16 << 5) | 17);
        e_lsl_i(17, 17, 7, 0);
        emit32(0x53000000u | (30 << 16) | (30 << 10) | (16 << 5) | 18);
        e_lsl_i(18, 18, 6, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18);
        e_movconst(19, 1);
        e_rrr(A_EOR, 18, 18, 19, 0, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_movconst(18, 2);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_pf_compute(18);
        e_rrr(A_ORR, 17, 17, 18, 0, 2);
        e_ldr(18, 28, OFF_AF);
        emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18);
        e_rrr(A_ORR, 17, 17, 18, 0, 4);
        e_bfi(RAX, 17, 8, 8, 1);
        return TX_NEXT;
    }
    if (opcode != 0xF8 && opcode != 0xF9 && opcode != 0xF5) return TX_FALL;
    e_nzcv_setcf_op(opcode == 0xF8 ? A_ORR : opcode == 0xF9 ? A_BIC : A_EOR);
    return TX_NEXT;
}

int lower_flag_stack_control(struct insn *instruction, uint64_t guest_pc) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x9C) {
        if (hl_x86_legacy_flags_pending()) flags_materialize();
        e_ldr(16, 28, OFF_NZCV);
        e_movconst(17, 0x202u);
        e_ldr(18, 28, OFF_DF);
        e_rrr(A_ORR, 17, 17, 18, 0, 10);
        emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18);
        e_movconst(19, 1);
        e_rrr(A_EOR, 18, 18, 19, 0, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_bit_move(17, 16, 30, 6, 18);
        e_bit_move(17, 16, 31, 7, 18);
        e_bit_move(17, 16, 28, 11, 18);
        e_pf_compute(18);
        e_rrr(A_ORR, 17, 17, 18, 0, 2);
        e_ldr(18, 28, OFF_AF);
        emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18);
        e_rrr(A_ORR, 17, 17, 18, 0, 4);
        e_ldr(18, 28, OFF_ID);
        e_rrr(A_ORR, 17, 17, 18, 0, 21);
        if (emit_soft_memory_active() || emit_displaced_stack_active()) {
            e_mov_rr(20, 17, 1);
            e_subi(17, RSP, 8, 1);
            emit_displaced_stack_address(17);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
            e_subi(RSP, RSP, 8, 1);
            e_store(8, 20, 17);
        } else {
            e_subi(RSP, RSP, 8, 1);
            e_store(8, 17, RSP);
        }
        return TX_NEXT;
    }
    if (opcode == 0x9D) {
        if (emit_soft_memory_active() || emit_displaced_stack_active()) {
            e_mov_rr(17, RSP, 1);
            emit_displaced_stack_address(17);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
        }
        e_load(8, 16, (emit_soft_memory_active() || emit_displaced_stack_active()) ? 17 : RSP);
        e_addi(RSP, RSP, 8, 1);
        emit_restore_rflags(16);
        return TX_NEXT;
    }
    if (opcode != 0xCF || !instruction->rexW) return TX_FALL;
    int frame = RSP;
    if (emit_soft_memory_active()) {
        e_mov_rr(17, RSP, 1);
        emit_memory_guard(17, 40, guest_pc, X86_SOFT_READ);
        frame = 17;
    }
    e_ldr(21, frame, 0);
    e_ldr(16, frame, 16);
    e_ldr(22, frame, 24);
    emit_restore_rflags(16);
    e_mov_rr(RSP, 22, 1);
    e_mov_rr(16, 21, 1);
    e_movconst(19, guest_pc);
    e_str(19, 28, OFF_IBSRC);
    emit_ibranch();
    return TX_BREAK;
}

int lower_accumulator_legacy(struct insn *instruction, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x90) {
        // 90 is XCHG eAX,rN and is only a NOP when N is rAX. REX.B selects r8.
        if (!instruction->rep && instruction->rexB) {
            int reg = instruction->rexB << 3;
            e_mov_rr(19, RAX, sf);
            e_mov_rr(RAX, reg, sf);
            e_mov_rr(reg, 19, sf);
        }
        return TX_NEXT;
    }
    if (opcode == 0x9B) return TX_NEXT; // fwait/wait: host FPU operations are synchronous
    if (opcode >= 0x91 && opcode <= 0x97) {
        int reg = (opcode - 0x90) | (instruction->rexB << 3);
        if (instruction->opsize == 2) {
            e_mov_rr(19, reg, 1);
            e_bfi(reg, RAX, 0, 16, 1);
            e_bfi(RAX, 19, 0, 16, 1);
        } else {
            e_mov_rr(19, RAX, sf);
            e_mov_rr(RAX, reg, sf);
            e_mov_rr(reg, 19, sf);
        }
        return TX_NEXT;
    }
    if (opcode == 0x98) {
        if (sf) {
            e_sxt(RAX, RAX, 4);
        } else if (instruction->p66) {
            emit32(0x13001C00u | (RAX << 5) | 16);
            e_bfi(RAX, 16, 0, 16, 1);
        } else {
            emit32(0x13003C00u | (RAX << 5) | RAX);
        }
        return TX_NEXT;
    }
    if (opcode != 0x99) return TX_FALL;
    if (sf) {
        e_asr_i(RDX, RAX, 63, 1);
    } else if (instruction->p66) {
        e_sxt(19, RAX, 2);
        e_asr_i(19, 19, 15, 0);
        e_bfi(RDX, 19, 0, 16, 1);
    } else {
        e_asr_i(RDX, RAX, 31, 0);
    }
    return TX_NEXT;
}

int lower_bit_scan(struct insn *instruction, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xBC && opcode != 0xBD) return TX_FALL;
    int mem;
    int rmv = rm_load(instruction, next, instruction->opsize, &mem);
    int count_form = instruction->rep;
    int source = rmv;
    if (!mem && instruction->reg == rmv) {
        e_mov_rr(23, rmv, sf);
        source = 23;
    }
    int word_form = instruction->opsize == 2;
    if (word_form) {
        e_movconst(19, 0xffff);
        e_rrr(A_AND, 23, source, 19, 0, 0);
        source = 23;
    }
    int destination = word_form ? 21 : instruction->reg;
    if (word_form) e_mov_rr(21, instruction->reg, 0);
    int bit_destination = count_form ? destination : 22;
    if (opcode == 0xBC) {
        e_rbit(bit_destination, source, sf);
        e_clz(bit_destination, bit_destination, sf);
    } else if (count_form) {
        e_clz(destination, source, sf);
    } else {
        e_clz(20, source, sf);
        e_movconst(19, sf ? 63 : 31);
        e_rrr(A_SUB, 22, 19, 20, sf, 0);
    }
    if (count_form) {
        e_rrr(A_SUBS, 31, source, 31, sf, 0);
        e_cset(19, 0, sf);
        e_rrr(A_ANDS, 31, destination, destination, sf, 0);
        e_nzcv_save_setcf(19);
    } else {
        if (word_form) {
            e_lsl_i(19, source, 16, 0);
            e_tst(19, 0);
        } else {
            e_tst(source, sf);
        }
        e_csel(destination, destination, 22, 0, sf);
        e_nzcv_save_c1();
    }
    if (word_form) e_bfi(instruction->reg, destination, 0, 16, 1);
    return TX_NEXT;
}

int lower_population_count(struct insn *instruction, uint64_t next, int sf) {
    if (instruction->op != 0xB8 || !instruction->rep) return TX_FALL;
    int mem;
    int rmv = rm_load(instruction, next, instruction->opsize, &mem);
    int source = rmv;
    if (!mem && instruction->reg == rmv) {
        e_mov_rr(23, rmv, sf);
        source = 23;
    }
    int word_form = instruction->opsize == 2;
    if (word_form) {
        e_movconst(19, 0xffff);
        e_rrr(A_AND, 23, source, 19, 0, 0);
        source = 23;
    }
    if (sf)
        e_fmov_to_d(16, source);
    else
        e_fmov_to_s(16, source);
    emit32(0x0E205800u | (16 << 5) | 16);
    emit32(0x0E31B800u | (16 << 5) | 16);
    e_fmov_from_s(word_form ? 21 : instruction->reg, 16);
    if (word_form) e_bfi(instruction->reg, 21, 0, 16, 1);
    e_rrr(A_ANDS, 31, source, source, sf, 0);
    e_nzcv_save_popcnt();
    return TX_NEXT;
}

int lower_compare_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xB0 && opcode != 0xB1) return TX_FALL;
    int width = opcode == 0xB0 ? 1 : instruction->opsize;
    int sf = width == 8;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        e_mov_rr(19, RAX, sf);
        e_cas(width, 19, instruction->reg, 17);
        do_alu(7, -1, RAX, 19, width);
        if (width >= 4)
            e_mov_rr(RAX, 19, sf);
        else
            e_bfi(RAX, 19, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width >= 4) {
        e_mov_rr(19, instruction->rm_reg, sf);
        do_alu(7, -1, RAX, 19, width);
        e_csel(instruction->rm_reg, instruction->reg, 19, 0, sf);
        e_csel(RAX, RAX, 19, 0, sf);
        return TX_NEXT;
    }
    int old_value = width == 1 ? byte_val(instruction, instruction->rm_reg, 19) : instruction->rm_reg;
    int source_value = width == 1 ? byte_val(instruction, instruction->reg, 24) : instruction->reg;
    if (old_value != 19) e_mov_rr(19, old_value, 1);
    do_alu(7, -1, RAX, 19, width);
    e_csel(21, source_value, 19, 0, 0);
    e_csel(22, RAX, 19, 0, 0);
    if (width == 1) {
        byte_wb(instruction, instruction->rm_reg, 21);
        e_bfi(RAX, 22, 0, 8, 1);
    } else {
        e_bfi(instruction->rm_reg, 21, 0, 16, 1);
        e_bfi(RAX, 22, 0, 16, 1);
    }
    return TX_NEXT;
}

int lower_exchange_add(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xC0 && opcode != 0xC1) return TX_FALL;
    int width = opcode == 0xC0 ? 1 : instruction->opsize;
    int sf = width == 8;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        e_lse(LSE_LDADD, width, instruction->reg, 19, 17);
        do_alu(0, -1, 19, instruction->reg, width);
        if (width >= 4)
            e_mov_rr(instruction->reg, 19, sf);
        else
            e_bfi(instruction->reg, 19, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width >= 4) {
        e_mov_rr(19, instruction->rm_reg, sf);
        e_rrr(A_ADDS, instruction->rm_reg, instruction->rm_reg, instruction->reg, sf, 0);
        e_nzcv_save_ci();
        e_mov_rr(instruction->reg, 19, sf);
        return TX_NEXT;
    }
    int old_value = width == 1 ? byte_val(instruction, instruction->rm_reg, 19) : instruction->rm_reg;
    int addend = width == 1 ? byte_val(instruction, instruction->reg, 24) : instruction->reg;
    if (old_value != 19) e_mov_rr(19, old_value, 1);
    if (addend != 24) e_mov_rr(24, addend, 1);
    do_alu(0, -1, 19, 24, width);
    e_rrr(A_ADD, 26, 19, 24, 0, 0);
    if (width == 1) {
        byte_wb(instruction, instruction->reg, 19);
        byte_wb(instruction, instruction->rm_reg, 26);
    } else {
        e_bfi(instruction->reg, 19, 0, 16, 1);
        e_bfi(instruction->rm_reg, 26, 0, 16, 1);
    }
    return TX_NEXT;
}

int lower_wide_compare_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xC7 || (instruction->reg & 7) != 1 || !instruction->is_mem) return TX_FALL;
    if (hl_x86_legacy_flags_pending()) flags_materialize();
    emit_ea(instruction, next);
    if (instruction->opsize == 8) {
        emit_memory_guard(17, 16, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        if (emit_soft_memory_active()) {
            emit_soft_store_commit(16);
            e_ldr(17, 28, OFF_BUS_EA);
        }
        e_str(17, 28, OFF_X87EA);
        emit_exit_const(next, R_CMPXCHG16);
        return TX_BREAK;
    }
    emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
    e_uxt(19, RAX, 4);
    e_bfi(19, RDX, 32, 32, 1);
    e_uxt(20, RBX, 4);
    e_bfi(20, RCX, 32, 32, 1);
    e_mov_rr(22, 19, 1);
    e_cas(8, 19, 20, 17);
    e_uxt(24, 19, 4);
    e_lsr_i(25, 19, 32, 1);
    e_rrr(A_SUBS, 31, 19, 22, 1, 0);
    e_csel(RAX, RAX, 24, 0, 1);
    e_csel(RDX, RDX, 25, 0, 1);
    e_ldr(21, 28, OFF_NZCV);
    e_movconst(23, 0x40000000u);
    e_rrr(A_BIC, 21, 21, 23, 1, 0);
    e_cset(23, 0, 1);
    e_lsl_i(23, 23, 30, 1);
    e_rrr(A_ORR, 21, 21, 23, 1, 0);
    e_str(21, 28, OFF_NZCV);
    emit32(0xD51B4200u | 21);
    if (emit_soft_memory_active()) emit_soft_store_commit(8);
    return TX_NEXT;
}

int lower_system_query(struct insn *instruction, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode == 0xA2) {
        emit_exit_const(next, R_CPUID);
        return TX_BREAK;
    }
    if (opcode == 0x31) {
        emit32(0xD53BE040u | 16);
        e_mov_rr(RAX, 16, 0);
        e_lsr_i(RDX, 16, 32, 1);
        return TX_NEXT;
    }
    if (opcode != 0x01 || !instruction->has_modrm) return TX_FALL;
    if (instruction->modrm == 0xF9) {
        emit32(0xD53BE040u | 16);
        e_mov_rr(RAX, 16, 0);
        e_lsr_i(RDX, 16, 32, 1);
        e_movz(RCX, 0, 0);
        return TX_NEXT;
    }
    if (instruction->modrm == 0xD0) {
        e_movz(RAX, 3, 0);
        e_movz(RDX, 0, 0);
        return TX_NEXT;
    }
    if (instruction->modrm == 0xD5) return TX_NEXT;
    return TX_FALL;
}

int lower_bit_test_modify(struct insn *instruction, uint64_t guest_pc, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xA3 && opcode != 0xAB && opcode != 0xB3 && opcode != 0xBB && opcode != 0xBA)
        return TX_FALL;
    int immediate = opcode == 0xBA;
    int operation = immediate ? (instruction->reg & 7)
                              : (opcode == 0xA3 ? 4 : opcode == 0xAB ? 5 : opcode == 0xB3 ? 6 : 7);
    if (operation < 4) {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    int width = instruction->opsize;
    int memory;
    int bits = width * 8;
    int log_bits = width == 8 ? 6 : width == 4 ? 5 : 4;
    int log_width = width == 8 ? 3 : width == 4 ? 2 : 1;
    int value;
    uint32_t access = operation == 4 ? X86_SOFT_READ : X86_SOFT_READ | X86_SOFT_WRITE;
    if (instruction->is_mem && !immediate) {
        emit_ea(instruction, next);
        if (width == 8)
            e_mov_rr(20, instruction->reg, 1);
        else
            e_sxt(20, instruction->reg, width);
        e_asr_i(20, 20, log_bits, 1);
        e_rrr(A_ADD, 17, 17, 20, 1, log_width);
        emit_memory_guard(17, (uint64_t)width, guest_pc, access);
        e_load(width, 16, 17);
        value = 16;
        memory = 1;
    } else {
        value = rm_load_access(instruction, next, width, &memory, access);
    }
    if (immediate) {
        e_movconst(19, (uint64_t)instruction->imm & (uint64_t)(bits - 1));
    } else {
        e_movconst(21, (uint64_t)(bits - 1));
        e_rrr(A_AND, 19, instruction->reg, 21, sf, 0);
    }
    e_shv(S_LSRV, 21, value, 19, sf);
    e_movconst(22, 1);
    e_rrr(A_AND, 21, 21, 22, sf, 0);
    e_rrr(A_SUBS, 31, 31, 21, 1, 0);
    e_nzcv_save();
    if (operation == 4) return TX_NEXT;
    e_movconst(22, 1);
    e_shv(S_LSLV, 22, 22, 19, sf);
    if (memory && instruction->lock) {
        uint32_t lse = operation == 5 ? LSE_LDSET : operation == 6 ? LSE_LDCLR : LSE_LDEOR;
        e_lse(lse, width, 22, 23, 17);
        e_shv(S_LSRV, 24, 23, 19, sf);
        e_movconst(25, 1);
        e_rrr(A_AND, 24, 24, 25, sf, 0);
        e_rrr(A_SUBS, 31, 31, 24, 1, 0);
        e_nzcv_save();
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    int output = memory || width < 4 ? 16 : instruction->rm_reg;
    if (operation == 5)
        e_rrr(A_ORR, output, value, 22, sf, 0);
    else if (operation == 6)
        e_rrr(A_BIC, output, value, 22, sf, 0);
    else
        e_rrr(A_EOR, output, value, 22, sf, 0);
    rm_store_after_guard(instruction, width, output);
    return TX_NEXT;
}

int lower_extended_state(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op == 0x77) return TX_NEXT;
    if (instruction->op != 0xAE) return TX_FALL;
    int operation = instruction->reg & 7;
    if (operation >= 5 && !instruction->is_mem) {
        emit32(0xD5033BBFu);
        return TX_NEXT;
    }
    if (operation == 2) {
        emit_ldmxcsr(instruction, next);
        return TX_NEXT;
    }
    if (operation == 3) {
        emit_stmxcsr(instruction, next);
        return TX_NEXT;
    }
    if (operation == 4 && instruction->is_mem) {
        if (instruction->p66 || instruction->rep || instruction->repne) {
            emit_guest_signal(guest_pc, 4, 2);
            return TX_BREAK;
        }
        emit_ea(instruction, next);
        emit_guest_address_store(17, OFF_X87EA);
        e_movconst(16, next);
        e_str(16, 28, OFF_DIVOP);
        if (hl_x86_legacy_flags_pending()) flags_materialize();
        if (hl_x86_x87_known()) hl_x86_x87_drop();
        emit_exit_const(guest_pc, R_XSAVE);
        return TX_BREAK;
    }
    if ((operation != 0 && operation != 1) || !instruction->is_mem) return TX_FALL;
    emit_ea(instruction, next);
    emit_memory_guard(17, 512, guest_pc, operation == 0 ? X86_SOFT_WRITE : X86_SOFT_READ);
    if (operation == 0 && emit_soft_memory_active()) {
        emit_soft_store_commit(512);
        e_ldr(17, 28, OFF_BUS_EA);
    }
    e_str(17, 28, OFF_X87EA);
    if (hl_x86_legacy_flags_pending()) flags_materialize();
    if (hl_x86_x87_known()) hl_x86_x87_drop();
    emit_exit_const(next, operation == 0 ? R_FXSAVE : R_FXRSTOR);
    return TX_BREAK;
}

int lower_multibyte_hint(const struct insn *instruction) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x1E && instruction->imm_bytes == 0) return TX_NEXT;
    if (opcode == 0x1F || opcode == 0x18 || opcode == 0x0D || (opcode >= 0x19 && opcode <= 0x1D))
        return TX_NEXT;
    return TX_FALL;
}
