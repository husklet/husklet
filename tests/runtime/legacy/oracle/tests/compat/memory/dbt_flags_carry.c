// Loop-carried condition flags across chained blocks. Multi-word add/subtract with carry/borrow and
// compare-driven branches force the engine to materialize and thread the guest condition flags (NZCV
// / RFLAGS) correctly across block boundaries -- a classic DBT lazy-flag-eval hazard. A 256-bit
// accumulator is rolled with add-with-carry and conditional mixing for many iterations. Deterministic.
#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t w0 = 1, w1 = 0, w2 = 0, w3 = 0;
    uint64_t x = 0x9e3779b97f4a7c15ULL;
    for (uint64_t i = 0; i < 50000000ULL; i++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        // 256-bit add-with-carry of a rotated x into {w3,w2,w1,w0}.
        unsigned __int128 s = (unsigned __int128)w0 + x;
        w0 = (uint64_t)s;
        unsigned __int128 c = s >> 64;
        s = (unsigned __int128)w1 + (x >> 1) + c;
        w1 = (uint64_t)s;
        c = s >> 64;
        s = (unsigned __int128)w2 + (x << 1) + c;
        w2 = (uint64_t)s;
        c = s >> 64;
        w3 += (x >> 2) + (uint64_t)c;
        // Flag-dependent branch that must observe the carry chain result.
        if ((w0 ^ w2) > (w1 ^ w3))
            w3 ^= (w0 - w1); // borrow path
        else
            w1 += (w2 - w0); // another borrow path
    }
    printf("flags-carry %llu %llu %llu %llu\n", (unsigned long long)w0, (unsigned long long)w1,
           (unsigned long long)w2, (unsigned long long)w3);
    return 0;
}
