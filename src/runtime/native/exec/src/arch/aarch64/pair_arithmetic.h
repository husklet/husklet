#ifndef HL_NATIVE_AARCH64_PAIR_ARITHMETIC_H
#define HL_NATIVE_AARCH64_PAIR_ARITHMETIC_H

#include "assembler.h"

#define HL_A64_PAIR_ARITHMETIC_MAX_BYTES 560u

int hl_a64_pair_arithmetic_body(hl_a64_assembler *assembler, uint32_t word);
int hl_a64_pair_arithmetic_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc);

#endif
