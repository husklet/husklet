#ifndef HL_NATIVE_AARCH64_BITWISE_H
#define HL_NATIVE_AARCH64_BITWISE_H

#include "assembler.h"

#define HL_A64_BITWISE_MAX_BYTES 560u

int hl_a64_bitwise_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_bitwise_body(hl_a64_assembler *, uint32_t);

#endif
