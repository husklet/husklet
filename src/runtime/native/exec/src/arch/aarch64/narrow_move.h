#ifndef HL_NATIVE_AARCH64_NARROW_MOVE_H
#define HL_NATIVE_AARCH64_NARROW_MOVE_H
#include "assembler.h"
#define HL_A64_NARROW_MOVE_MAX_BYTES 560u
int hl_a64_narrow_move_body(hl_a64_assembler *, uint32_t);
int hl_a64_narrow_move_emit(hl_a64_assembler *, uint32_t, uint64_t);
#endif
