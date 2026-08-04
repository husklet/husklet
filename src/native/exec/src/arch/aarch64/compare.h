#ifndef HL_NATIVE_AARCH64_COMPARE_H
#define HL_NATIVE_AARCH64_COMPARE_H

#include "assembler.h"

#define HL_A64_COMPARE_MAX_BYTES 560u

int hl_a64_compare_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_compare_body(hl_a64_assembler *, uint32_t);

#endif
