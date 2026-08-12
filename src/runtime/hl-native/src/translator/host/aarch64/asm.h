#ifndef HL_TRANSLATOR_HOST_AARCH64_ASM_H
#define HL_TRANSLATOR_HOST_AARCH64_ASM_H

#include <stdint.h>

#include "../../emit.h"

void hl_a64_emit32(hl_emit_state *state, uint32_t instruction);
void hl_a64_str(hl_emit_state *state, int rt, int rn, int offset);
void hl_a64_ldr(hl_emit_state *state, int rt, int rn, int offset);
void hl_a64_mov_sp_from(hl_emit_state *state, int rn);
void hl_a64_mov_from_sp(hl_emit_state *state, int rd);
void hl_a64_movz(hl_emit_state *state, int rd, uint32_t immediate, int shift);
void hl_a64_movk(hl_emit_state *state, int rd, uint32_t immediate, int shift);
void hl_a64_br(hl_emit_state *state, int rn);
void hl_a64_movconst(hl_emit_state *state, int rd, uint64_t value);
void hl_a64_stp(hl_emit_state *state, int rt, int rt2, int rn, int offset);
void hl_a64_ldp(hl_emit_state *state, int rt, int rt2, int rn, int offset);
void hl_a64_addi(hl_emit_state *state, int rd, int rn, unsigned immediate);
void hl_a64_addlsl4(hl_emit_state *state, int rd, int rn, int rm);
void hl_a64_addlsl3(hl_emit_state *state, int rd, int rn, int rm);
void hl_a64_movr(hl_emit_state *state, int rd, int rm);
void hl_a64_subi(hl_emit_state *state, int rd, int rn, unsigned immediate);
void hl_a64_ret(hl_emit_state *state);
void hl_a64_adrp_add(hl_emit_state *state, int rd, uint64_t target);
void hl_a64_load_cpu(hl_emit_state *state, int reg, uintptr_t tls_key);
void hl_a64_stur(hl_emit_state *state, int rt, int rn, int immediate);
void hl_a64_ldur(hl_emit_state *state, int rt, int rn, int immediate);
void hl_a64_stp_q(hl_emit_state *state, int rt, int rt2, int rn, int offset);
void hl_a64_ldp_q(hl_emit_state *state, int rt, int rt2, int rn, int offset);

#endif
