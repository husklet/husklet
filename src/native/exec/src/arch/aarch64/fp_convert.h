#ifndef HL_NATIVE_AARCH64_FP_CONVERT_H
#define HL_NATIVE_AARCH64_FP_CONVERT_H

#include "assembler.h"

#define HL_A64_FP_CONVERT_MAX_BYTES 560u

int hl_a64_fp_convert_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_fp_convert_body(hl_a64_assembler *, uint32_t);

#endif
