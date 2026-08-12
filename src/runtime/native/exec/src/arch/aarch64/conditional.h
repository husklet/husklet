#ifndef HL_NATIVE_AARCH64_CONDITIONAL_H
#define HL_NATIVE_AARCH64_CONDITIONAL_H

#include "assembler.h"

#define HL_A64_CONDITIONAL_MAX_BYTES 1028u

int hl_a64_conditional_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_conditional_body(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_conditional_chain(hl_a64_assembler *, uint32_t, uint64_t,
                             uint32_t **, uint64_t *, uint32_t **, uint64_t *);
int hl_a64_conditional_stitch(hl_a64_assembler *, uint32_t, uint64_t,
                              uint32_t **, uint64_t *, uint32_t **);

#endif
