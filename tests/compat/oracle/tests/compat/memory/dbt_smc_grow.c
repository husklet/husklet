// SMC where the rewritten body has a DIFFERENT length than the original at the same address: a short
// `return imm` (6-8 bytes) is replaced by a longer straight-line chain of `add`s, and vice versa,
// across rounds. A translation cache that keyed the invalidation range on the OLD block length would
// leave stale tail bytes translated. `x` is threaded through so each new body's full effect is
// observed. Deterministic checksum; per-arch code emission.
#include "dbt.h"

// Emit `uint64_t f(uint64_t x){ for k in 1..count: x += (k*step); return x; }` as `count` add-imm
// instructions followed by ret. Returns byte length. Uses 12-bit imm adds (aarch64) / lea+add.
static int emit_add_chain(unsigned char *buf, int count, uint32_t step) {
    int off = 0;
    uint64_t total = 0;
    for (int k = 1; k <= count; k++) total += (uint64_t)k * step;
    (void)total;
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)buf;
    int wi = 0;
    for (int k = 1; k <= count; k++) {
        uint32_t imm = ((uint32_t)k * step) & 0xfff; // 12-bit add immediate
        w[wi++] = 0x91000000u | (imm << 10);         // add x0, x0, #imm
    }
    w[wi++] = 0xD65F03C0u; // ret
    off = wi * 4;
#elif defined(__x86_64__)
    // rdi in, rax out.  mov rax, rdi ; then `add rax, imm32` * count ; ret
    buf[off++] = 0x48;
    buf[off++] = 0x89;
    buf[off++] = 0xF8; // mov rax, rdi
    for (int k = 1; k <= count; k++) {
        uint32_t imm = (uint32_t)k * step;
        buf[off++] = 0x48;
        buf[off++] = 0x05; // add rax, imm32
        buf[off++] = (unsigned char)(imm & 0xff);
        buf[off++] = (unsigned char)((imm >> 8) & 0xff);
        buf[off++] = (unsigned char)((imm >> 16) & 0xff);
        buf[off++] = (unsigned char)((imm >> 24) & 0xff);
    }
    buf[off++] = 0xC3; // ret
#else
    (void)buf; (void)count; (void)step;
#endif
    return off;
}

// Reference value of the emitted chain, computed in C for cross-checking.
static uint64_t chain_ref(uint64_t x, int count, uint32_t step) {
    for (int k = 1; k <= count; k++) x += (uint64_t)k * step;
    return x;
}

int main(void) {
    size_t sz = 8192;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    uint64_t (*f)(uint64_t) = (uint64_t (*)(uint64_t))p;
    uint64_t acc = 0, x = 0x1000;
    for (int round = 0; round < 8000; round++) {
        int count = 1 + (round % 40); // body length swings 1..40 instructions -> different sizes
        uint32_t step = 1u + (uint32_t)(round % 7);
        memset(p, 0xCC, sz); // poison: stale tail bytes would be int3/undefined if executed
        emit_add_chain(p, count, step);
        dbt_flush(p, sz);
        uint64_t got = f(x);
        uint64_t want = chain_ref(x, count, step);
        if (got != want) {
            printf("smc-grow MISMATCH round=%d want=%llu got=%llu\n", round,
                   (unsigned long long)want, (unsigned long long)got);
            return 1;
        }
        acc = acc * 1000003ULL + got;
        x = (got ^ (got >> 29)) & 0xffff;
    }
    printf("smc-grow acc=%llu\n", (unsigned long long)acc);
    return 0;
}
