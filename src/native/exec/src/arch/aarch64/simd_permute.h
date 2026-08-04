#ifndef HL_NATIVE_AARCH64_SIMD_PERMUTE_H
#define HL_NATIVE_AARCH64_SIMD_PERMUTE_H

#include "assembler.h"

#define HL_A64_SIMD_PERMUTE_MAX_BYTES 560u

int hl_a64_simd_permute_body(hl_a64_assembler *, uint32_t);
int hl_a64_simd_permute_emit(hl_a64_assembler *, uint32_t, uint64_t);

#endif
