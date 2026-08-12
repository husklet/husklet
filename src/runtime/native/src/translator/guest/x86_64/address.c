#include "address.h"

enum { HL_X86_ADDRESS_SCRATCH = 16, HL_X86_ADDRESS_RESULT = 17, HL_X86_CPU_REGISTER = 28 };

static int guestfold_on(const hl_x86_address_state *state) {
    return state->nonpie_lo != 0;
}

static void add_displacement(const hl_x86_address_state *state, int64_t displacement) {
    const hl_x86_address_emitter *emit = state->emitter;
    if (displacement == 0) return;
    int subtract = displacement < 0;
    uint64_t magnitude = subtract ? UINT64_C(0) - (uint64_t)displacement : (uint64_t)displacement;
    if (magnitude <= UINT64_C(0xFFF)) {
        if (subtract)
            emit->sub_immediate(state->context, 17, 17, (unsigned)magnitude, 1, 0);
        else
            emit->add_immediate(state->context, 17, 17, (unsigned)magnitude, 1, 0);
    } else if (magnitude <= UINT64_C(0xFFFFFF)) {
        unsigned low = (unsigned)(magnitude & UINT64_C(0xFFF));
        unsigned high = (unsigned)((magnitude >> 12) & UINT64_C(0xFFF));
        if (subtract) {
            emit->sub_immediate(state->context, 17, 17, high, 1, 1);
            if (low) emit->sub_immediate(state->context, 17, 17, low, 1, 0);
        } else {
            emit->add_immediate(state->context, 17, 17, high, 1, 1);
            if (low) emit->add_immediate(state->context, 17, 17, low, 1, 0);
        }
    } else {
        emit->move_constant(state->context, 16, (uint64_t)displacement);
        emit->add_register(state->context, 17, 17, 16, 1, 0);
    }
}

static void emit_bias(const hl_x86_address_state *state, int rip_relative) {
    const hl_x86_address_emitter *emit = state->emitter;
    if (!guestfold_on(state)) return;
    emit->record_guest_address(state->context, 17, rip_relative);
    emit->logical_shift_right(state->context, 16, 17, 32, 1);
    uintptr_t branch = emit->branch_placeholder(state->context);
    emit->move_constant(state->context, 16, state->nonpie_bias);
    emit->add_register(state->context, 17, 17, 16, 1, 0);
    emit->patch_cbnz(state->context, branch, 16);
}

void hl_x86_address_emit(const hl_x86_address_state *state, const hl_x86_insn *insn, uint64_t next_rip,
                         int apply_bias) {
    const hl_x86_address_emitter *emit = state->emitter;
    if (!state->optimize) {
        if (insn->rip_rel) {
            emit->move_constant(state->context, 17, next_rip + (uint64_t)insn->disp);
        } else {
            if (insn->m_hasbase)
                emit->move_register(state->context, 17, insn->m_base, 1);
            else
                emit->move_zero(state->context, 17, 0, 0);
            if (insn->m_hasindex) emit->add_register(state->context, 17, 17, insn->m_index, 1, insn->m_scale);
            if (insn->disp) {
                emit->move_constant(state->context, 16, (uint64_t)insn->disp);
                emit->add_register(state->context, 17, 17, 16, 1, 0);
            }
        }
    } else if (insn->rip_rel) {
        emit->move_constant(state->context, 17, next_rip + (uint64_t)insn->disp);
    } else if (insn->m_hasbase) {
        if (insn->m_hasindex) {
            emit->add_register(state->context, 17, insn->m_base, insn->m_index, 1, insn->m_scale);
            add_displacement(state, insn->disp);
        } else if (insn->disp >= 0 && insn->disp <= 0xFFF) {
            emit->add_immediate(state->context, 17, insn->m_base, (unsigned)insn->disp, 1, 0);
        } else if (insn->disp < 0 && UINT64_C(0) - (uint64_t)insn->disp <= UINT64_C(0xFFF)) {
            emit->sub_immediate(state->context, 17, insn->m_base, (unsigned)(UINT64_C(0) - (uint64_t)insn->disp), 1, 0);
        } else {
            emit->move_register(state->context, 17, insn->m_base, 1);
            add_displacement(state, insn->disp);
        }
    } else if (insn->m_hasindex) {
        emit->add_register(state->context, 17, 31, insn->m_index, 1, insn->m_scale);
        add_displacement(state, insn->disp);
    } else {
        emit->move_constant(state->context, 17, (uint64_t)insn->disp);
    }
    if (insn->addr32) emit->zero_extend(state->context, 17, 17, 4);
    if (insn->seg) {
        emit->load_cpu(state->context, 16, insn->seg == 1 ? state->fs_offset : state->gs_offset);
        emit->add_register(state->context, 17, 17, 16, 1, 0);
    }
    int absolute = !insn->rip_rel && !insn->m_hasbase && !insn->m_hasindex && !insn->seg;
    uint64_t absolute_address = (uint64_t)insn->disp;
    if (apply_bias && !(absolute && guestfold_on(state) &&
                        (absolute_address < state->nonpie_lo || absolute_address >= state->nonpie_hi)))
        emit_bias(state, insn->rip_rel);
}

int hl_x86_address_fold(const hl_x86_address_state *state, const hl_x86_insn *insn, int width, int *rn, int *offset) {
    if (state->bus_active || !state->optimize || guestfold_on(state) || insn->addr32 ||
        !(insn->m_hasbase && !insn->m_hasindex && !insn->seg && !insn->rip_rel))
        return 0;
    int64_t displacement = insn->disp;
    *rn = insn->m_base;
    *offset = (int)displacement;
    if (displacement >= 0 && displacement % width == 0 && (uint64_t)(displacement / width) <= UINT64_C(0xFFF)) return 1;
    if (displacement >= -256 && displacement <= 255) return 2;
    return 0;
}

// Register-offset addressing fold: base+index with no displacement is exactly ARM's
// `LDR/STR <Vt>, [Xn, Xm{, LSL #amount}]`, so the separate `add x17, base, index` that
// hl_x86_address_emit would emit is pure overhead. The conditions are the same as the imm-offset
// fold above (hl_x86_address_fold) -- no bus guard (it needs the effective address in a register),
// no non-PIE guest-base bias fold, no 32-bit address wrap, no segment override, not rip-relative --
// plus: the displacement must be zero (ARM has no base+index+imm form) and the x86 SIB scale must be
// one ARM can express with this encoding, i.e. LSL #0 or LSL #log2(width).
// Fault behaviour is unchanged: the access still faults on the same effective address, and the
// fault-PC provenance map keys on the emitted host code range, not on x17.
int hl_x86_address_fold_reg(const hl_x86_address_state *state, const hl_x86_insn *insn, int width, int *rn, int *rm,
                            int *shift) {
    if (state->bus_active || !state->optimize || guestfold_on(state) || insn->addr32 || insn->seg || insn->rip_rel)
        return 0;
    if (!insn->m_hasbase || !insn->m_hasindex || insn->disp != 0) return 0;
    int log2w = 0;
    while ((1 << log2w) < width)
        log2w++;
    if (insn->m_scale != 0 && insn->m_scale != log2w) return 0;
    *rn = insn->m_base;
    *rm = insn->m_index;
    *shift = insn->m_scale;
    return 1;
}

void hl_x86_address_load(const hl_x86_address_state *state, const hl_x86_insn *insn, uint64_t next_rip, int width,
                         int rt) {
    int rn;
    int offset;
    int fold = hl_x86_address_fold(state, insn, width, &rn, &offset);
    if (fold == 1)
        state->emitter->load_scaled(state->context, width, rt, rn, (unsigned)offset);
    else if (fold == 2)
        state->emitter->load_unscaled(state->context, width, rt, rn, offset);
    else {
        hl_x86_address_emit(state, insn, next_rip, 1);
        state->emitter->bus_guard(state->context, HL_X86_ADDRESS_RESULT, (uint64_t)width,
                                  next_rip - (uint64_t)insn->len);
        state->emitter->load(state->context, width, rt, HL_X86_ADDRESS_RESULT);
    }
}
