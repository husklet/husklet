#include "flags.h"

uint64_t hl_x86_sub_nzcv(uint64_t left, uint64_t right, int width) {
    int bits = 8 * width;
    uint64_t mask = (width == 8) ? UINT64_MAX : ((UINT64_C(1) << bits) - 1);
    uint64_t a = left & mask;
    uint64_t b = right & mask;
    uint64_t result = (a - b) & mask;
    uint64_t negative = result >> (bits - 1);
    uint64_t zero = result == 0;
    uint64_t carry = a >= b;
    uint64_t overflow = (((a ^ b) & (a ^ result)) >> (bits - 1)) & 1;
    return (negative << 31) | (zero << 30) | (carry << 29) | (overflow << 28);
}
