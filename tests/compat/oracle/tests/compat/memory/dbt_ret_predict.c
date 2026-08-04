// Return-address prediction / return-stack stress. Mutually recursive functions with many distinct
// call sites and varied return points build deep call chains whose returns the engine must route
// correctly (return is an indirect branch; DBTs keep a return-target cache). A misrouted return
// corrupts the accumulator. Deterministic bounded recursion, checksum only.
#include <stdint.h>
#include <stdio.h>

static uint64_t odd(uint64_t, unsigned);
static uint64_t even(uint64_t, unsigned);

static uint64_t even(uint64_t a, unsigned d) {
    a = a * 0x100000001b3ULL + 0xe;
    if (d == 0) return a;
    uint64_t r = odd(a ^ (a >> 13), d - 1); // call site A
    r = odd(r + d, d - 1);                  // call site B (distinct return point)
    return r ^ (a << 1);
}
static uint64_t odd(uint64_t a, unsigned d) {
    a = a * 0x2545f4914f6cdd1dULL + 0xd;
    if (d == 0) return a;
    uint64_t r = even(a ^ (a >> 7), d - 1); // call site C
    return (r + a) ^ (r >> 3);
}

int main(void) {
    uint64_t acc = 0;
    for (unsigned s = 0; s < 4000; s++) acc = acc * 31 + even(0x9e3779b9ULL + s, 22);
    printf("ret-predict acc=%llu\n", (unsigned long long)acc);
    return 0;
}
