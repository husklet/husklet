#ifndef HL_NATIVE_AARCH64_TERMINATOR_H
#define HL_NATIVE_AARCH64_TERMINATOR_H

#include "assembler.h"

#define HL_A64_TERMINATOR_MAX_BYTES 500u

int hl_a64_terminator_emit(hl_a64_assembler *, uint32_t, uint64_t);

#endif
