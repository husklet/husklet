#ifndef HL_NATIVE_AARCH64_REVERSE_H
#define HL_NATIVE_AARCH64_REVERSE_H

#include "assembler.h"

#define HL_A64_REVERSE_MAX_BYTES 560u

int hl_a64_reverse_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_reverse_body(hl_a64_assembler *, uint32_t);

#endif
