/* Bit-scan / population builtins across a fixed table: clz/ctz/ffs/parity/popcount for 32- and
   64-bit lanes. Codegen lowers these to RBIT+CLZ (aarch64) or LZCNT/TZCNT/POPCNT-or-sequences
   (x86_64); the derived counts are arch-neutral. Deterministic. */
#include <stdio.h>
#include <stdint.h>

int main(void) {
    static const uint64_t v[] = {
        0x1u, 0x80000000u, 0x8000000000000000ull, 0xFFFFFFFFFFFFFFFFull,
        0x0000000100000000ull, 0xF0F0F0F0F0F0F0F0ull, 0x123456789ABCDEF0ull, 0x00000000000FF000ull,
    };
    unsigned long acc = 1469598103u;
    for (unsigned i = 0; i < sizeof(v) / sizeof(v[0]); i++) {
        uint64_t x = v[i];
        uint32_t lo = (uint32_t)x;
        int clz32 = lo ? __builtin_clz(lo) : 32;
        int ctz32 = lo ? __builtin_ctz(lo) : 32;
        int clz64 = x ? __builtin_clzll(x) : 64;
        int ctz64 = x ? __builtin_ctzll(x) : 64;
        int pop = __builtin_popcountll(x);
        int par = __builtin_parityll(x);
        int ffs = __builtin_ffsll((long long)x);
        printf("i=%u clz32=%d ctz32=%d clz64=%d ctz64=%d pop=%d par=%d ffs=%d\n",
               i, clz32, ctz32, clz64, ctz64, pop, par, ffs);
        acc = (acc ^ (unsigned long)(clz64 + ctz64 * 7 + pop * 31 + par * 3 + ffs)) * 16777619u;
    }
    printf("acc=%08lx\n", acc & 0xFFFFFFFFul);
    return 0;
}
