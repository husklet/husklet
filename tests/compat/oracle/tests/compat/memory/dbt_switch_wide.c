// Wide sparse switch/jump-table dispatch: a 512-case switch (compiled to a jump table) driven in a
// state-dependent order. Jump tables become an indirect branch through a table base; a stale IBTC
// or wrong table-bounds handling under the DBT would route to the wrong arm. Deterministic checksum.
#include <stdint.h>
#include <stdio.h>

static uint64_t arm(unsigned k, uint64_t a) {
    switch (k & 0x1ff) {
#define A(n) case n: a = (a ^ (0x9e3779b97f4a7c15ULL * ((n) + 1))) + (a << (((n) & 7) + 1)); break;
#define A8(b) A(b + 0) A(b + 1) A(b + 2) A(b + 3) A(b + 4) A(b + 5) A(b + 6) A(b + 7)
#define A64(b) A8(b) A8(b + 8) A8(b + 16) A8(b + 24) A8(b + 32) A8(b + 40) A8(b + 48) A8(b + 56)
        A64(0) A64(64) A64(128) A64(192) A64(256) A64(320) A64(384) A64(448)
    default:
        a += 1;
        break;
    }
    return a;
}

int main(void) {
    uint64_t a = 0x12345678ULL;
    for (uint64_t i = 0; i < 30000000ULL; i++)
        a = arm((unsigned)(a ^ (a >> 17) ^ i), a);
    printf("switch-wide acc=%llu\n", (unsigned long long)a);
    return 0;
}
