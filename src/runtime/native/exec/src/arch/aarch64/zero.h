#ifndef HL_NATIVE_AARCH64_ZERO_H
#define HL_NATIVE_AARCH64_ZERO_H

#include "assembler.h"
#include "guard.h"
#include "memory.h"

int hl_a64_zero_body(hl_a64_assembler *, uint32_t, uint64_t, hl_a64_guard *, hl_a64_memory_sites *);

#endif
