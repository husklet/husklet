#ifndef HL_NATIVE_AARCH64_SIMD_PAIRWISE_H
#define HL_NATIVE_AARCH64_SIMD_PAIRWISE_H

#include "assembler.h"

#define HL_A64_SIMD_PAIRWISE_MAX_BYTES 560u

int hl_a64_simd_pairwise_body(hl_a64_assembler *assembler, uint32_t word);
int hl_a64_simd_pairwise_emit(hl_a64_assembler *assembler, uint32_t word,
                              uint64_t pc);

#endif
