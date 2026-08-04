// Deeply nested loops with loop-carried state and conditional early-continue/break. Nested tight
// loops are the densest form of direct + conditional block chaining; a mislinked chain or a dropped
// loop-carried flag surfaces as a checksum divergence. Five nested levels, deterministic checksum.
#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t acc = 0x12345678ULL;
    for (int a = 0; a < 40; a++) {
        for (int b = 0; b < 40; b++) {
            for (int c = 0; c < 40; c++) {
                if (((a + b + c) & 7) == 3) continue; // conditional back-edge into inner chain
                for (int d = 0; d < 24; d++) {
                    uint64_t v = acc + (uint64_t)(a * 131 + b * 17 + c * 7 + d);
                    for (int e = 0; e < 8; e++) {
                        v = (v << 1) ^ (v >> 63) ^ (uint64_t)(e * a + d);
                        if ((v & 0x3f) == 0) break; // early break out of innermost
                        acc += v;
                    }
                    acc ^= v * 0x100000001b3ULL;
                }
            }
        }
    }
    printf("nested-loops acc=%llu\n", (unsigned long long)acc);
    return 0;
}
