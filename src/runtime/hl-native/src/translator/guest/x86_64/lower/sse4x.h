#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_SSE4X_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_SSE4X_H

#include <stdint.h>

#include "../decoder.h"
#include "primitives.h"

typedef struct hl_x86_sse4x_state {
    int optimize;
} hl_x86_sse4x_state;

int hl_x86_lower_sse4x(struct insn *insn, uint64_t next, const hl_x86_sse4x_state *state);

#endif
