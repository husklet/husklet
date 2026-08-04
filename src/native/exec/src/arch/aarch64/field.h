#ifndef HL_NATIVE_AARCH64_FIELD_H
#define HL_NATIVE_AARCH64_FIELD_H

#include "assembler.h"

#define HL_A64_FIELD_MAX_BYTES 580u

int hl_a64_field_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_field_body(hl_a64_assembler *, uint32_t);

#endif
