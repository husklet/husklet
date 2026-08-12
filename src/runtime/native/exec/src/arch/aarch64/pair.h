#ifndef HL_NATIVE_AARCH64_PAIR_H
#define HL_NATIVE_AARCH64_PAIR_H

#include "assembler.h"
#include "guard.h"
#include "memory.h"

#define HL_A64_PAIR_MAX_BYTES HL_A64_GUARDED_MAX_BYTES

int hl_a64_pair_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_pair_body(hl_a64_assembler *, uint32_t, uint64_t, hl_a64_guard *,
                     hl_a64_memory_sites *);

#endif
