// Deep + wide recursion for translation-cache and return-address-prediction pressure. Ackermann(2..3)
// and a branching Fibonacci-style tree generate deep call/return nests and many distinct return sites
// that stress the engine's return handling and block cache. Bounded inputs, deterministic checksum.
#include <stdint.h>
#include <stdio.h>

static uint64_t ack(uint64_t m, uint64_t n) {
    if (m == 0) return n + 1;
    if (n == 0) return ack(m - 1, 1);
    return ack(m - 1, ack(m, n - 1));
}

// Branching tree recursion: two recursive calls per frame -> wide + deep, many return sites.
static uint64_t tree(uint64_t d, uint64_t seed) {
    if (d == 0) return seed * 0x9e3779b97f4a7c15ULL + 1;
    uint64_t a = tree(d - 1, seed ^ (seed >> 7));
    uint64_t b = tree(d - 1, seed + 0x1234567ULL);
    return (a ^ (b << 1)) + (a >> 3);
}

int main(void) {
    uint64_t acc = 0;
    for (uint64_t m = 0; m <= 3; m++)
        for (uint64_t n = 0; n <= 6; n++) acc = acc * 31 + ack(m, n);
    for (uint64_t s = 0; s < 64; s++) acc ^= tree(20, s);
    printf("deepwide acc=%llu\n", (unsigned long long)acc);
    return 0;
}
