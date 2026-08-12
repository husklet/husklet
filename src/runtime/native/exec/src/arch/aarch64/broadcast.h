#ifndef HL_NATIVE_AARCH64_BROADCAST_H
#define HL_NATIVE_AARCH64_BROADCAST_H

#include "assembler.h"

#define HL_A64_BROADCAST_MAX_BYTES 560u

int hl_a64_broadcast_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_broadcast_body(hl_a64_assembler *, uint32_t);

#endif
