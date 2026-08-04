#ifndef HL_NATIVE_AARCH64_SIMD_LOGIC_H
#define HL_NATIVE_AARCH64_SIMD_LOGIC_H

#include "assembler.h"

#define HL_A64_SIMD_LOGIC_MAX_BYTES 560u

int hl_a64_simd_logic_body(hl_a64_assembler *, uint32_t);
int hl_a64_simd_logic_emit(hl_a64_assembler *, uint32_t, uint64_t);

#endif
