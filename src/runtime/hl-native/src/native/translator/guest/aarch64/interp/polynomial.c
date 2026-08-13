#include "polynomial.h"

void interp_poly_mul(uint64_t a, uint64_t b, unsigned bits, uint64_t *low, uint64_t *high) {
    uint64_t result_low = 0;
    uint64_t result_high = 0;
    for (unsigned bit = 0; bit < bits; bit++) {
        if (!((b >> bit) & 1u)) continue;
        // A shift by 0 must not become a shift of the high word by 64, which is undefined in C.
        result_low ^= a << bit;
        if (bit) result_high ^= a >> (64u - bit);
    }
    *low = result_low;
    *high = result_high;
}
