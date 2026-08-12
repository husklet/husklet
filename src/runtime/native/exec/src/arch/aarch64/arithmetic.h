#ifndef HL_NATIVE_AARCH64_ARITHMETIC_H
#define HL_NATIVE_AARCH64_ARITHMETIC_H

#include "assembler.h"

#define HL_A64_ARITHMETIC_MAX_BYTES 560u

int hl_a64_arithmetic_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_arithmetic_body(hl_a64_assembler *, uint32_t);

#endif
