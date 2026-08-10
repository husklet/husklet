#include "asm.h"

static uint32_t field(int value) {
    return (uint32_t)value;
}

void hl_a64_emit32(hl_emit_state *state, uint32_t instruction) {
    *(uint32_t *)state->cursor = instruction;
    state->cursor += 4;
}

void hl_a64_str(hl_emit_state *state, int rt, int rn, int offset) {
    hl_a64_emit32(state, 0xF9000000u | (((uint32_t)offset / 8) << 10) | (field(rn) << 5) | field(rt));
}

void hl_a64_ldr(hl_emit_state *state, int rt, int rn, int offset) {
    hl_a64_emit32(state, 0xF9400000u | (((uint32_t)offset / 8) << 10) | (field(rn) << 5) | field(rt));
}

void hl_a64_mov_sp_from(hl_emit_state *state, int rn) {
    hl_a64_emit32(state, 0x9100001Fu | (field(rn) << 5));
}

void hl_a64_mov_from_sp(hl_emit_state *state, int rd) {
    hl_a64_emit32(state, 0x910003E0u | field(rd));
}

void hl_a64_movz(hl_emit_state *state, int rd, uint32_t immediate, int shift) {
    hl_a64_emit32(state, 0xD2800000u | (field(shift) << 21) | (immediate << 5) | field(rd));
}

void hl_a64_movk(hl_emit_state *state, int rd, uint32_t immediate, int shift) {
    hl_a64_emit32(state, 0xF2800000u | (field(shift) << 21) | (immediate << 5) | field(rd));
}

void hl_a64_br(hl_emit_state *state, int rn) {
    hl_a64_emit32(state, 0xD61F0000u | (field(rn) << 5));
}

void hl_a64_movconst(hl_emit_state *state, int rd, uint64_t value) {
    hl_a64_movz(state, rd, (uint32_t)(value & 0xffffu), 0);
    if ((value >> 16) & 0xffffu) hl_a64_movk(state, rd, (uint32_t)((value >> 16) & 0xffffu), 1);
    if ((value >> 32) & 0xffffu) hl_a64_movk(state, rd, (uint32_t)((value >> 32) & 0xffffu), 2);
    if ((value >> 48) & 0xffffu) hl_a64_movk(state, rd, (uint32_t)((value >> 48) & 0xffffu), 3);
}

void hl_a64_stp(hl_emit_state *state, int rt, int rt2, int rn, int offset) {
    hl_a64_emit32(state, 0xA9000000u | (((uint32_t)(offset / 8) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) |
                             field(rt));
}

void hl_a64_ldp(hl_emit_state *state, int rt, int rt2, int rn, int offset) {
    hl_a64_emit32(state, 0xA9400000u | (((uint32_t)(offset / 8) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) |
                             field(rt));
}

void hl_a64_addi(hl_emit_state *state, int rd, int rn, unsigned immediate) {
    hl_a64_emit32(state, 0x91000000u | ((immediate & 0xFFF) << 10) | (field(rn) << 5) | field(rd));
}

void hl_a64_addlsl4(hl_emit_state *state, int rd, int rn, int rm) {
    hl_a64_emit32(state, 0x8B000000u | (field(rm) << 16) | (4u << 10) | (field(rn) << 5) | field(rd));
}

void hl_a64_addlsl3(hl_emit_state *state, int rd, int rn, int rm) {
    hl_a64_emit32(state, 0x8B000000u | (field(rm) << 16) | (3u << 10) | (field(rn) << 5) | field(rd));
}

void hl_a64_movr(hl_emit_state *state, int rd, int rm) {
    hl_a64_emit32(state, 0xAA0003E0u | (field(rm) << 16) | field(rd));
}

void hl_a64_subi(hl_emit_state *state, int rd, int rn, unsigned immediate) {
    hl_a64_emit32(state, 0xD1000000u | ((immediate & 0xFFF) << 10) | (field(rn) << 5) | field(rd));
}

void hl_a64_ret(hl_emit_state *state) {
    hl_a64_emit32(state, 0xD65F03C0u);
}

void hl_a64_adrp_add(hl_emit_state *state, int rd, uint64_t target) {
    uint64_t target_page = target & ~UINT64_C(0xFFF);
    uint64_t pc_page = (uint64_t)(uintptr_t)hl_emit_rx(state, state->cursor) & ~UINT64_C(0xFFF);
    int64_t offset;
    if ((target_page >= pc_page && target_page - pc_page > ((UINT64_C(1) << 20) - 1) * 4096) ||
        (target_page < pc_page && pc_page - target_page > (UINT64_C(1) << 20) * 4096)) {
        hl_a64_movconst(state, rd, target);
        return;
    }
    offset =
        target_page >= pc_page ? (int64_t)((target_page - pc_page) >> 12) : -(int64_t)((pc_page - target_page) >> 12);
    hl_a64_emit32(state, 0x90000000u | (((uint32_t)offset & 3) << 29) | (((uint32_t)(offset >> 2) & 0x7FFFF) << 5) |
                             field(rd));
    hl_a64_emit32(state, 0x91000000u | (((uint32_t)(target & 0xFFF)) << 10) | (field(rd) << 5) | field(rd));
}

void hl_a64_load_cpu(hl_emit_state *state, int reg, uintptr_t tls_key) {
    hl_a64_emit32(state, 0xD53BD060u | field(reg));
    hl_a64_emit32(state, 0x9243F000u | (field(reg) << 5) | field(reg));
    hl_a64_ldr(state, reg, reg, (int)(tls_key * 8));
}

void hl_a64_stur(hl_emit_state *state, int rt, int rn, int immediate) {
    hl_a64_emit32(state, 0xF8000000u | (((uint32_t)immediate & 0x1FF) << 12) | (field(rn) << 5) | field(rt));
}

void hl_a64_ldur(hl_emit_state *state, int rt, int rn, int immediate) {
    hl_a64_emit32(state, 0xF8400000u | (((uint32_t)immediate & 0x1FF) << 12) | (field(rn) << 5) | field(rt));
}

void hl_a64_stp_q(hl_emit_state *state, int rt, int rt2, int rn, int offset) {
    hl_a64_emit32(state, 0xAD000000u | (((uint32_t)(offset / 16) & 0x7F) << 15) | (field(rt2) << 10) |
                             (field(rn) << 5) | field(rt));
}

void hl_a64_ldp_q(hl_emit_state *state, int rt, int rt2, int rn, int offset) {
    hl_a64_emit32(state, 0xAD400000u | (((uint32_t)(offset / 16) & 0x7F) << 15) | (field(rt2) << 10) |
                             (field(rn) << 5) | field(rt));
}
