#ifndef HL_NATIVE_AARCH64_SINGLE_H
#define HL_NATIVE_AARCH64_SINGLE_H

#include "assembler.h"
#include "guard.h"
#include "memory.h"

#define HL_A64_SINGLE_MAX_BYTES HL_A64_GUARDED_MAX_BYTES

int hl_a64_single_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_single_body(hl_a64_assembler *, uint32_t, uint64_t, hl_a64_guard *, hl_a64_memory_sites *);
int hl_a64_single_direct_body(hl_a64_assembler *, uint32_t, uint64_t,
                              const hl_native_direct_authority *, hl_a64_guard *,
                              hl_a64_memory_sites *);

#endif
