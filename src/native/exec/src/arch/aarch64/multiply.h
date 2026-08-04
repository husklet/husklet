#ifndef HL_NATIVE_AARCH64_MULTIPLY_H
#define HL_NATIVE_AARCH64_MULTIPLY_H

#include "assembler.h"

#define HL_A64_MULTIPLY_MAX_BYTES 600u

int hl_a64_multiply_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_multiply_body(hl_a64_assembler *, uint32_t);

#endif
