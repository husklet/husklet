#ifndef HL_NATIVE_AARCH64_INDIRECT_H
#define HL_NATIVE_AARCH64_INDIRECT_H

#include "assembler.h"

#define HL_A64_INDIRECT_MAX_BYTES 520u

int hl_a64_indirect_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_indirect_body(hl_a64_assembler *, uint32_t, uint64_t, void *);

#endif
