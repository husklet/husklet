#ifndef HL_NATIVE_AARCH64_CRYPTO_H
#define HL_NATIVE_AARCH64_CRYPTO_H

#include "assembler.h"

#define HL_A64_CRYPTO_MAX_BYTES 560u

int hl_a64_crypto_body(hl_a64_assembler *, uint32_t);
int hl_a64_crypto_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_crypto_host_supports(uint32_t);

#endif
