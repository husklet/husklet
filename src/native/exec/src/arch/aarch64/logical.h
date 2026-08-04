#ifndef HL_NATIVE_AARCH64_LOGICAL_H
#define HL_NATIVE_AARCH64_LOGICAL_H

#include "assembler.h"

#define HL_A64_LOGICAL_MAX_BYTES 540u

int hl_a64_logical_mask(uint32_t, uint64_t *);
int hl_a64_logical_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_logical_body(hl_a64_assembler *, uint32_t);

#endif
