#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    uint64_t iterations = argc == 2 ? strtoull(argv[1], NULL, 10) : 100000000;
    float value = 1.0f;
    const float factor = 1.00000011920928955078125f;
    for (uint64_t index = 0; index < iterations; ++index) {
        __asm__ volatile("mulss %1,%0" : "+x"(value) : "x"(factor));
        if ((index & 1023u) == 1023u) value = 1.0f;
    }
    uint32_t bits;
    __builtin_memcpy(&bits, &value, sizeof bits);
    return printf("%08x\n", bits) < 0;
}
