#include "branch.h"

#include "primitives.h"
#include "../encoding.h"

static int x86cc_to_arm(int condition) {
    static const int mapping[16] = {6, 7, 3, 2, 0, 1, 9, 8, 4, 5, 6, 7, 11, 10, 13, 12};
    return mapping[condition & 0xF];
}

static int emit_parity_condition(int low) {
    if (hl_x86_legacy_flags_pending()) flags_materialize();
    e_pf_compute(19);
    e_rrr(A_SUBS, 31, 19, 31, 0, 0);
    return low == 0xA ? 1 : 0;
}

static void emit_edge_spill(int kind) { hl_x86_legacy_jcc_spill(kind); }

static int lower_conditional_branch(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                                    hl_x86_trace_state *trace, hl_x86_branch_region *region) {
    int low = instruction->op & 0xF;
    int parity = low == 0xA || low == 0xB;
    int condition = parity ? emit_parity_condition(low) : x86cc_to_arm(low);
    uint64_t taken = next + (uint64_t)instruction->imm;
    uint32_t *cursor = hl_x86_emit_cursor();
    if (!parity && taken == region->start && !region->tier_disabled() &&
        !hl_x86_trace_loop_hazard((uint64_t)region->body, (uint64_t)cursor)) {
        int slot = region->tier_two ? 0 : region->tier_slot(region->start);
        if (region->tier_two || slot >= 0) {
            hl_x86_trace_self_loop(trace, condition, region->start, next, region->body, slot);
            return TX_BREAK;
        }
    }
    uint64_t fall = next;
    int stitch_fall = region->stitch_allowed && fall != region->start &&
                      !hl_x86_trace_seen(region->seen, *region->seen_count, fall) && !region->body_mapped(fall) &&
                      !hl_x86_trace_trap_head(fall);
    int save_taken = 0;
    int save_fall = 0;
    if (!parity)
        hl_x86_trace_jcc_flags(trace, taken, fall, *guest_pc, stitch_fall, condition, &save_taken, &save_fall);
    if (stitch_fall) {
        int inverse = (condition ^ 1) & 0xF;
        uint32_t *patch = hl_x86_emit_cursor();
        emit32(0);
        if (parity) e_nzcv_load();
        emit_edge_spill(save_taken);
        emit_chain_exit(taken);
        int64_t distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)patch) / 4;
        *patch = 0x54000000u | (((uint32_t)distance & 0x7FFFF) << 5) | (uint32_t)inverse;
        if (parity) e_nzcv_load();
        region->seen[(*region->seen_count)++] = fall;
        (*region->trace_blocks)++;
        (*region->conditional_stitches)++;
        *guest_pc = fall;
        return TX_NEXT;
    }
    uint32_t *patch = hl_x86_emit_cursor();
    emit32(0);
    if (parity) e_nzcv_load();
    emit_edge_spill(save_fall);
    emit_chain_exit(next);
    int64_t distance = ((uint8_t *)hl_x86_emit_cursor() - (uint8_t *)patch) / 4;
    *patch = 0x54000000u | (((uint32_t)distance & 0x7FFFF) << 5) | (condition & 0xF);
    if (parity) e_nzcv_load();
    emit_edge_spill(save_taken);
    emit_chain_exit(taken);
    return TX_BREAK;
}

int hl_x86_lower_near_branch(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                             hl_x86_trace_state *trace, hl_x86_branch_region *region) {
    if ((instruction->op & 0xF0) != 0x80) return TX_FALL;
    return lower_conditional_branch(instruction, guest_pc, next, trace, region);
}

int hl_x86_lower_short_branch(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                              hl_x86_trace_state *trace, hl_x86_branch_region *region) {
    if (instruction->op < 0x70 || instruction->op > 0x7F) return TX_FALL;
    return lower_conditional_branch(instruction, guest_pc, next, trace, region);
}

int hl_x86_lower_direct_jump(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                             hl_x86_trace_state *trace, hl_x86_branch_region *region) {
    if (instruction->op != 0xE9 && instruction->op != 0xEB) return TX_FALL;
    uint64_t target = next + (uint64_t)instruction->imm;
    if (region->stitch_allowed && target != region->start &&
        !hl_x86_trace_seen(region->seen, *region->seen_count, target) && !region->body_mapped(target) &&
        !hl_x86_trace_trap_head(target)) {
        region->seen[(*region->seen_count)++] = target;
        (*region->trace_blocks)++;
        *guest_pc = target;
        return TX_NEXT;
    }
    hl_x86_trace_flags_edge(trace, target, *guest_pc);
    emit_chain_exit(target);
    return TX_BREAK;
}

int hl_x86_lower_conditional_move(struct insn *instruction, uint64_t guest_pc, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    if ((opcode & 0xF0) == 0x90) {
        int low = opcode & 0xF;
        if (low == 0xA || low == 0xB) {
            if (instruction->is_mem) emit_ea(instruction, next);
            e_pf_compute(19);
            if (low == 0xB) {
                e_movconst(16, 1);
                e_rrr(A_EOR, 19, 19, 16, 0, 0);
            }
            if (instruction->is_mem) e_store(1, 19, 17);
            else byte_wb(instruction, instruction->rm_reg, 19);
            return TX_NEXT;
        }
        int condition = x86cc_to_arm(low);
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            e_nzcv_load();
            e_cset(16, condition, 0);
            e_store(1, 16, 17);
        } else {
            e_nzcv_load();
            e_cset(16, condition, 0);
            byte_wb(instruction, instruction->rm_reg, 16);
        }
        return TX_NEXT;
    }
    if ((opcode & 0xF0) != 0x40) return TX_FALL;
    int low = opcode & 0xF;
    if (low == 0xA || low == 0xB) {
        e_pf_compute(19);
        int memory;
        int value = rm_load(instruction, next, instruction->opsize, &memory);
        e_rrr(A_SUBS, 31, 19, 31, 0, 0);
        if (instruction->opsize == 2) {
            e_csel(21, value, instruction->reg, low == 0xA ? 1 : 0, 0);
            e_bfi(instruction->reg, 21, 0, 16, 1);
        } else {
            e_csel(instruction->reg, value, instruction->reg, low == 0xA ? 1 : 0, sf);
        }
        e_nzcv_load();
        return TX_NEXT;
    }
    int condition = x86cc_to_arm(low);
    int memory;
    int value = rm_load(instruction, next, instruction->opsize, &memory);
    e_nzcv_load();
    if (instruction->opsize == 2) {
        e_csel(21, value, instruction->reg, condition, 0);
        e_bfi(instruction->reg, 21, 0, 16, 1);
    } else {
        e_csel(instruction->reg, value, instruction->reg, condition, sf);
    }
    (void)guest_pc;
    return TX_NEXT;
}
