#ifndef HL_NATIVE_AARCH64_SATURATING_NARROW_H
#define HL_NATIVE_AARCH64_SATURATING_NARROW_H
#include "assembler.h"
#define HL_A64_SATURATING_NARROW_MAX_BYTES 560u
int hl_a64_saturating_narrow_body(hl_a64_assembler *, uint32_t);
int hl_a64_saturating_narrow_emit(hl_a64_assembler *, uint32_t, uint64_t);
#endif
