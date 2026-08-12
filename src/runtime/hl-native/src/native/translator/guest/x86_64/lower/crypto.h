#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_CRYPTO_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_CRYPTO_H

#include <stdint.h>

#include "../decoder.h"
#include "primitives.h"

typedef struct hl_x86_crypto_state {
    int zero_ready;
    int mask_ready;
    int optimize;
} hl_x86_crypto_state;

int hl_x86_lower_crypto(struct insn *insn, uint64_t next, hl_x86_crypto_state *state);

#endif
