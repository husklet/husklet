#ifndef HL_NATIVE_AARCH64_PCREL_H
#define HL_NATIVE_AARCH64_PCREL_H

#include "assembler.h"

#define HL_A64_PCREL_MAX_BYTES 540u

int hl_a64_pcrel_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_pcrel_body(hl_a64_assembler *, uint32_t, uint64_t);

#endif
