#ifndef HL_NATIVE_AARCH64_MOVE_H
#define HL_NATIVE_AARCH64_MOVE_H

#include "assembler.h"

#define HL_A64_MOVE_MAX_BYTES 520u

int hl_a64_move_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_move_body(hl_a64_assembler *, uint32_t);

#endif
