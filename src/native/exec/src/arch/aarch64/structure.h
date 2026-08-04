#ifndef HL_NATIVE_AARCH64_STRUCTURE_H
#define HL_NATIVE_AARCH64_STRUCTURE_H

#include "assembler.h"
#include "guard.h"
#include "memory.h"

#define HL_A64_STRUCTURE_MAX_BYTES 900u

int hl_a64_structure_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_structure_body(hl_a64_assembler *, uint32_t, uint64_t, hl_a64_guard *, hl_a64_memory_sites *);

#endif
