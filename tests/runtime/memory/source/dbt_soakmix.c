// Soak-lite: a 300M-iteration mixing loop with a golden checksum. Long steady-state hot loops are
// where slow IBTC/block-chaining drift shows up -- a single wrong-block event anywhere in 3e8
// iterations perturbs the final checksum. Bounded well under 60s at -O2. Deterministic.
#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t a = 0x243f6a8885a308d3ULL;
    uint64_t b = 0x13198a2e03707344ULL;
    for (uint64_t i = 0; i < 300000000ULL; i++) {
        a += (b ^ (a << 7)) + i;
        b ^= (a >> 11) + (b << 3);
        a = (a << 41) | (a >> 23); // rotate to keep the branch-free chain lively
        if ((a & 0xffff) == 0x1234) b += a; // rare data-dependent branch
    }
    printf("soak-mix a=%llu b=%llu\n", (unsigned long long)a, (unsigned long long)b);
    return 0;
}
