#ifndef HL_AARCH64_INTERP_POLYNOMIAL_H
#define HL_AARCH64_INTERP_POLYNOMIAL_H

#include <stdint.h>

void interp_poly_mul(uint64_t a, uint64_t b, unsigned bits, uint64_t *low, uint64_t *high);

#endif
