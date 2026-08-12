#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_SHIFT_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_SHIFT_H

#include <stdint.h>

struct insn;

typedef struct hl_x86_shift_state {
    int parity_aux_dead;
    int output_flags_dead;
    int direct_registers;
} hl_x86_shift_state;

int hl_x86_lower_shift(struct insn *insn, uint64_t next, const hl_x86_shift_state *state);

#endif
