#ifndef HL_NATIVE_AARCH64_DIRECT_H
#define HL_NATIVE_AARCH64_DIRECT_H

#include "assembler.h"

#define HL_A64_DIRECT_MAX_BYTES 564u

int hl_a64_direct_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_direct_body(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_direct_chain(hl_a64_assembler *, uint32_t, uint64_t, uint32_t **,
                        uint64_t *);

#endif
