#ifndef HL_NATIVE_AARCH64_SIMD_IMMEDIATE_H
#define HL_NATIVE_AARCH64_SIMD_IMMEDIATE_H

#include "assembler.h"

#define HL_A64_SIMD_IMMEDIATE_MAX_BYTES 560u

int hl_a64_simd_immediate_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_simd_immediate_body(hl_a64_assembler *, uint32_t);

#endif
