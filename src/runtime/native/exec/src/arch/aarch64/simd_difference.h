#ifndef HL_NATIVE_AARCH64_SIMD_DIFFERENCE_H
#define HL_NATIVE_AARCH64_SIMD_DIFFERENCE_H
#include "assembler.h"
#define HL_A64_SIMD_DIFFERENCE_MAX_BYTES 560u
int hl_a64_simd_difference_body(hl_a64_assembler *, uint32_t);
int hl_a64_simd_difference_emit(hl_a64_assembler *, uint32_t, uint64_t);
#endif
