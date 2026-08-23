// translit/operands -- operand and terminator coverage for the TL_COPY whitelist.
//
// RIP-relative loads and stores (translit_fix_riprel re-aims the displacement at the guest address, and
// out-of-int32 reach ends the block), indirect calls and jumps through a table (TL_CALL_REG/TL_JMP_REG),
// string operations, bswap/bt/shld/shrd/xadd/cmpxchg/xchg, and deep call/ret recursion.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
// RIP-relative operands, string ops, deep call/ret, indirect jmp/call, cmpxchg/xadd,
// shld/shrd, bt group, bswap, movs/stos/scas/cmps, xchg, leave, push imm.
static uint64_t table[256];
static volatile uint64_t global_a = 0x0123456789abcdefull;
static const char text[] = "the quick brown fox jumps over the lazy dog 0123456789";

static uint64_t h64(const void *p, size_t n) {
    const unsigned char *b = p;
    uint64_t h = 1469598103934665603ull;
    for (size_t i = 0; i < n; i++) {
        h ^= b[i];
        h *= 1099511628211ull;
    }
    return h;
}

__attribute__((noinline)) static uint64_t f0(uint64_t x) {
    return x * 3 + 1;
}

__attribute__((noinline)) static uint64_t f1(uint64_t x) {
    return x ^ 0x5555555555555555ull;
}

__attribute__((noinline)) static uint64_t f2(uint64_t x) {
    return (x << 7) | (x >> 57);
}

__attribute__((noinline)) static uint64_t f3(uint64_t x) {
    return x - 0x9E3779B9ull;
}

typedef uint64_t (*fp)(uint64_t);
static fp fns[4] = {f0, f1, f2, f3};

__attribute__((noinline)) static uint64_t recur(uint64_t n, uint64_t a) {
    if (n == 0) return a;
    return recur(n - 1, a * 31 + n) + 1;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0); // unbuffered: the ordering of a forked child\'s output is part of the comparison
    uint64_t h = 0;
    for (int i = 0; i < 256; i++)
        table[i] = (uint64_t)i * 0x9E3779B97F4A7C15ull;
    // rip-relative loads/stores against globals
    for (int r = 0; r < 4096; r++) {
        global_a = global_a * 6364136223846793005ull + 1442695040888963407ull;
        table[r & 255] ^= global_a;
        h = h * 31 + table[(r * 7) & 255];
    }
    // indirect calls through a table (TL_CALL_REG) and indirect jumps
    for (int r = 0; r < 4096; r++)
        h = fns[r & 3](h) ^ (h >> 3);
    // string ops
    char buf[512], buf2[512];
    memset(buf, 0xa5, sizeof buf);
    for (int r = 0; r < 512; r++) {
        memcpy(buf2, text, sizeof text);
        memmove(buf + (r & 63), buf2, 32);
        h = h * 31 + (uint64_t)(strlen(buf2) + (size_t)memcmp(buf2, text, sizeof text));
        h ^= h64(buf, sizeof buf);
    }
    // bswap / bt / shld / xadd / cmpxchg / xchg
    for (int r = 0; r < 4096; r++) {
        uint64_t x = h + (uint64_t)r, y = h ^ (uint64_t)r;
        __asm__ volatile("bswap %0" : "+r"(x));
        __asm__ volatile("shld $13,%1,%0" : "+r"(x) : "r"(y) : "cc");
        __asm__ volatile("shrd $7,%1,%0" : "+r"(x) : "r"(y) : "cc");
        unsigned char c;
        __asm__ volatile("bt %2,%1\n\tsetc %0" : "=r"(c) : "r"(x), "r"((uint64_t)(r & 63)) : "cc");
        uint64_t m = x;
        __asm__ volatile("xadd %0,%1" : "+r"(y), "+r"(m)::"cc");
        __asm__ volatile("xchg %0,%1" : "+r"(x), "+r"(y));
        h = h * 31 + x + y + m + c;
    }
    // deep call/ret with a large frame
    for (int r = 0; r < 64; r++)
        h = h * 31 + recur(200, (uint64_t)r);
    // ret imm16 / stdarg-ish
    printf("operands h=%016llx global=%016llx tbl=%016llx\n", (unsigned long long)h, (unsigned long long)global_a,
           (unsigned long long)h64(table, sizeof table));
    return 0;
}
