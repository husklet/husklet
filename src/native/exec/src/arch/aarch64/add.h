#ifndef HL_NATIVE_AARCH64_ADD_H
#define HL_NATIVE_AARCH64_ADD_H

#include "assembler.h"

#define HL_A64_ADD_MAX_BYTES 540u

int hl_a64_add_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_add_body(hl_a64_assembler *, uint32_t);

#endif
