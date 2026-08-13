#ifndef HL_AARCH64_INTERP_CRYPTO_PRIMITIVES_H
#define HL_AARCH64_INTERP_CRYPTO_PRIMITIVES_H

#include <stdint.h>

extern const uint8_t interp_aes_sbox[256];
extern const uint8_t interp_aes_inv_sbox[256];

void interp_aes_shift_rows(const uint8_t *input, uint8_t *output, int inverse);
void interp_aes_mix_columns(const uint8_t *input, uint8_t *output, int inverse);
uint32_t interp_ror32_bits(uint32_t value, unsigned amount);
uint32_t interp_rol32_bits(uint32_t value, unsigned amount);
uint32_t interp_sha_choose(uint32_t x, uint32_t y, uint32_t z);
uint32_t interp_sha_majority(uint32_t x, uint32_t y, uint32_t z);
uint32_t interp_sha_parity(uint32_t x, uint32_t y, uint32_t z);

#endif
