#include "assembler.h"

#include <string.h>

static uint32_t field(int value) { return (uint32_t)value; }

int hl_a64_assembler_begin(hl_a64_assembler *state, void *writable, void *executable, size_t capacity) {
    if (state == NULL || writable == NULL || executable == NULL || capacity > PTRDIFF_MAX) return 0;
    state->writable = writable;
    state->executable = executable;
    state->cursor = writable;
    state->limit = state->cursor + capacity;
    state->failed = 0;
    return 1;
}

size_t hl_a64_assembler_size(const hl_a64_assembler *state) {
    return state == NULL ? 0 : (size_t)(state->cursor - state->writable);
}

size_t hl_a64_assembler_remaining(const hl_a64_assembler *state) {
    return state == NULL || state->failed ? 0 : (size_t)(state->limit - state->cursor);
}

int hl_a64_assembler_ok(const hl_a64_assembler *state) { return state != NULL && state->failed == 0; }

void *hl_a64_assembler_rx(const hl_a64_assembler *state, const void *pointer) {
    if (state == NULL || pointer == NULL) return NULL;
    return state->executable + ((const uint8_t *)pointer - state->writable);
}

void hl_a64_emit32(hl_a64_assembler *state, uint32_t instruction) {
    if (state == NULL || state->failed || (size_t)(state->limit - state->cursor) < sizeof(instruction)) {
        if (state != NULL) state->failed = 1;
        return;
    }
    memcpy(state->cursor, &instruction, sizeof(instruction));
    state->cursor += sizeof(instruction);
}

void hl_a64_str(hl_a64_assembler *s, int rt, int rn, int o) { hl_a64_emit32(s, 0xF9000000u | (((uint32_t)o / 8) << 10) | (field(rn) << 5) | field(rt)); }
void hl_a64_ldr(hl_a64_assembler *s, int rt, int rn, int o) { hl_a64_emit32(s, 0xF9400000u | (((uint32_t)o / 8) << 10) | (field(rn) << 5) | field(rt)); }
void hl_a64_mov_sp_from(hl_a64_assembler *s, int rn) { hl_a64_emit32(s, 0x9100001Fu | (field(rn) << 5)); }
void hl_a64_mov_from_sp(hl_a64_assembler *s, int rd) { hl_a64_emit32(s, 0x910003E0u | field(rd)); }
void hl_a64_movz(hl_a64_assembler *s, int rd, uint32_t i, int sh) { hl_a64_emit32(s, 0xD2800000u | (field(sh) << 21) | (i << 5) | field(rd)); }
void hl_a64_movk(hl_a64_assembler *s, int rd, uint32_t i, int sh) { hl_a64_emit32(s, 0xF2800000u | (field(sh) << 21) | (i << 5) | field(rd)); }
void hl_a64_br(hl_a64_assembler *s, int rn) { hl_a64_emit32(s, 0xD61F0000u | (field(rn) << 5)); }

void hl_a64_movconst(hl_a64_assembler *s, int rd, uint64_t value) {
    hl_a64_movz(s, rd, (uint32_t)(value & 0xffffu), 0);
    if ((value >> 16) & 0xffffu) hl_a64_movk(s, rd, (uint32_t)((value >> 16) & 0xffffu), 1);
    if ((value >> 32) & 0xffffu) hl_a64_movk(s, rd, (uint32_t)((value >> 32) & 0xffffu), 2);
    if ((value >> 48) & 0xffffu) hl_a64_movk(s, rd, (uint32_t)((value >> 48) & 0xffffu), 3);
}

void hl_a64_stp(hl_a64_assembler *s, int rt, int rt2, int rn, int o) { hl_a64_emit32(s, 0xA9000000u | (((uint32_t)(o / 8) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) | field(rt)); }
void hl_a64_ldp(hl_a64_assembler *s, int rt, int rt2, int rn, int o) { hl_a64_emit32(s, 0xA9400000u | (((uint32_t)(o / 8) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) | field(rt)); }
void hl_a64_addi(hl_a64_assembler *s, int rd, int rn, unsigned i) { hl_a64_emit32(s, 0x91000000u | ((i & 0xFFF) << 10) | (field(rn) << 5) | field(rd)); }
void hl_a64_addlsl4(hl_a64_assembler *s, int rd, int rn, int rm) { hl_a64_emit32(s, 0x8B000000u | (field(rm) << 16) | (4u << 10) | (field(rn) << 5) | field(rd)); }
void hl_a64_addlsl3(hl_a64_assembler *s, int rd, int rn, int rm) { hl_a64_emit32(s, 0x8B000000u | (field(rm) << 16) | (3u << 10) | (field(rn) << 5) | field(rd)); }
void hl_a64_movr(hl_a64_assembler *s, int rd, int rm) { hl_a64_emit32(s, 0xAA0003E0u | (field(rm) << 16) | field(rd)); }
void hl_a64_subi(hl_a64_assembler *s, int rd, int rn, unsigned i) { hl_a64_emit32(s, 0xD1000000u | ((i & 0xFFF) << 10) | (field(rn) << 5) | field(rd)); }
void hl_a64_ret(hl_a64_assembler *s) { hl_a64_emit32(s, 0xD65F03C0u); }

void hl_a64_adrp_add(hl_a64_assembler *s, int rd, uint64_t target) {
    uint64_t target_page = target & ~UINT64_C(0xFFF);
    uint64_t pc_page = (uint64_t)(uintptr_t)hl_a64_assembler_rx(s, s->cursor) & ~UINT64_C(0xFFF);
    int64_t offset;
    if ((target_page >= pc_page && target_page - pc_page > ((UINT64_C(1) << 20) - 1) * 4096) ||
        (target_page < pc_page && pc_page - target_page > (UINT64_C(1) << 20) * 4096)) {
        hl_a64_movconst(s, rd, target);
        return;
    }
    offset = target_page >= pc_page ? (int64_t)((target_page - pc_page) >> 12)
                                    : -(int64_t)((pc_page - target_page) >> 12);
    hl_a64_emit32(s, 0x90000000u | (((uint32_t)offset & 3) << 29) |
                     (((uint32_t)(offset >> 2) & 0x7FFFF) << 5) | field(rd));
    hl_a64_emit32(s, 0x91000000u | (((uint32_t)(target & 0xFFF)) << 10) | (field(rd) << 5) | field(rd));
}

void hl_a64_stur(hl_a64_assembler *s, int rt, int rn, int i) { hl_a64_emit32(s, 0xF8000000u | (((uint32_t)i & 0x1FF) << 12) | (field(rn) << 5) | field(rt)); }
void hl_a64_ldur(hl_a64_assembler *s, int rt, int rn, int i) { hl_a64_emit32(s, 0xF8400000u | (((uint32_t)i & 0x1FF) << 12) | (field(rn) << 5) | field(rt)); }
void hl_a64_stp_q(hl_a64_assembler *s, int rt, int rt2, int rn, int o) { hl_a64_emit32(s, 0xAD000000u | (((uint32_t)(o / 16) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) | field(rt)); }
void hl_a64_ldp_q(hl_a64_assembler *s, int rt, int rt2, int rn, int o) { hl_a64_emit32(s, 0xAD400000u | (((uint32_t)(o / 16) & 0x7F) << 15) | (field(rt2) << 10) | (field(rn) << 5) | field(rt)); }
