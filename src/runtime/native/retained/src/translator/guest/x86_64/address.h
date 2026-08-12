#ifndef HL_TRANSLATOR_GUEST_X86_64_ADDRESS_H
#define HL_TRANSLATOR_GUEST_X86_64_ADDRESS_H

#include <stdint.h>

#include "decoder.h"

typedef struct hl_x86_address_emitter {
    void (*add_immediate)(void *context, int rd, int rn, unsigned immediate, int sf, int shift);
    void (*sub_immediate)(void *context, int rd, int rn, unsigned immediate, int sf, int shift);
    void (*move_constant)(void *context, int rd, uint64_t value);
    void (*add_register)(void *context, int rd, int rn, int rm, int sf, int shift);
    void (*logical_shift_right)(void *context, int rd, int rn, int shift, int sf);
    void (*move_register)(void *context, int rd, int rn, int sf);
    void (*move_zero)(void *context, int rd, uint32_t immediate, int shift);
    void (*zero_extend)(void *context, int rd, int rn, int bytes);
    void (*load_cpu)(void *context, int rt, int offset);
    void (*load_scaled)(void *context, int width, int rt, int rn, unsigned offset);
    void (*load_unscaled)(void *context, int width, int rt, int rn, int offset);
    void (*load)(void *context, int width, int rt, int rn);
    void (*record_guest_address)(void *context, int address_register, int rip_relative);
    void (*bus_guard)(void *context, int address_register, uint64_t size, uint64_t guest_pc);
    uintptr_t (*branch_placeholder)(void *context);
    void (*patch_cbnz)(void *context, uintptr_t placeholder, int reg);
} hl_x86_address_emitter;

typedef struct hl_x86_address_state {
    void *context;
    const hl_x86_address_emitter *emitter;
    uint64_t nonpie_lo;
    uint64_t nonpie_hi;
    uint64_t nonpie_bias;
    int fs_offset;
    int gs_offset;
    int optimize;
    int bus_active;
} hl_x86_address_state;

void hl_x86_address_emit(const hl_x86_address_state *state, const hl_x86_insn *insn, uint64_t next_rip, int apply_bias);
int hl_x86_address_fold(const hl_x86_address_state *state, const hl_x86_insn *insn, int width, int *rn, int *offset);
int hl_x86_address_fold_reg(const hl_x86_address_state *state, const hl_x86_insn *insn, int width, int *rn, int *rm,
                            int *shift);
void hl_x86_address_load(const hl_x86_address_state *state, const hl_x86_insn *insn, uint64_t next_rip, int width,
                         int rt);

#endif
