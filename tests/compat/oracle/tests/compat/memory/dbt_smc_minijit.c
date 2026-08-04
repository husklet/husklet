// A mini-JIT: compile a small bytecode program into native machine code in an RWX arena, run it, then
// compile a DIFFERENT program to the SAME arena and run again -- thousands of times. This mirrors a
// real method-JIT (V8/RyuJIT/PyPy) generating, discarding, and regenerating code at one address. Each
// generated function is cross-checked against a C reference interpreter, so any stale translation or
// missed SMC re-arm surfaces immediately. Per-arch code generation; deterministic checksum.
#include "dbt.h"

enum { OP_ADD, OP_SUB, OP_SHL1, OP_END };

// Compile `prog` (op,imm pairs) to native `uint64_t f(uint64_t)` at buf. Returns byte length.
static int jit_compile(unsigned char *buf, const unsigned char *ops, const uint32_t *imm, int n) {
    int off = 0;
#if defined(__x86_64__)
    buf[off++] = 0x48; buf[off++] = 0x89; buf[off++] = 0xF8; // mov rax, rdi
#endif
    for (int i = 0; i < n; i++) {
        uint32_t v = imm[i];
        switch (ops[i]) {
        case OP_ADD:
#if defined(__aarch64__)
            *(uint32_t *)(buf + off) = 0x91000000u | ((v & 0xfff) << 10); off += 4; // add x0,x0,#imm12
#elif defined(__x86_64__)
            buf[off++] = 0x48; buf[off++] = 0x05;
            buf[off++] = (unsigned char)(v); buf[off++] = (unsigned char)(v >> 8);
            buf[off++] = (unsigned char)(v >> 16); buf[off++] = (unsigned char)(v >> 24);
#endif
            break;
        case OP_SUB:
#if defined(__aarch64__)
            *(uint32_t *)(buf + off) = 0xD1000000u | ((v & 0xfff) << 10); off += 4; // sub x0,x0,#imm12
#elif defined(__x86_64__)
            buf[off++] = 0x48; buf[off++] = 0x2D;
            buf[off++] = (unsigned char)(v); buf[off++] = (unsigned char)(v >> 8);
            buf[off++] = (unsigned char)(v >> 16); buf[off++] = (unsigned char)(v >> 24);
#endif
            break;
        case OP_SHL1:
#if defined(__aarch64__)
            *(uint32_t *)(buf + off) = 0x8B000000u | (0u << 16) | (0u << 5) | 0u; // add x0,x0,x0
            off += 4;
#elif defined(__x86_64__)
            buf[off++] = 0x48; buf[off++] = 0xD1; buf[off++] = 0xE0; // shl rax,1
#endif
            break;
        }
    }
#if defined(__aarch64__)
    *(uint32_t *)(buf + off) = 0xD65F03C0u; off += 4; // ret
#elif defined(__x86_64__)
    buf[off++] = 0xC3; // ret
#endif
    return off;
}

static uint64_t ref_run(uint64_t x, const unsigned char *ops, const uint32_t *imm, int n) {
    for (int i = 0; i < n; i++) {
        switch (ops[i]) {
        case OP_ADD: x += (imm[i] & 0xfff); break;   // match aarch64 12-bit add width
        case OP_SUB: x -= (imm[i] & 0xfff); break;
        case OP_SHL1: x += x; break;
        }
    }
    return x;
}

int main(void) {
    size_t sz = 8192;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    uint64_t (*f)(uint64_t) = (uint64_t (*)(uint64_t))p;
    uint64_t acc = 0;
    uint32_t s = 0x2468ace0u;
    for (int round = 0; round < 12000; round++) {
        unsigned char ops[24];
        uint32_t imm[24];
        int n = 4 + (round % 16);
        for (int i = 0; i < n; i++) {
            s = s * 1103515245u + 12345u;
            ops[i] = (unsigned char)((s >> 20) % 3); // OP_ADD/SUB/SHL1
            imm[i] = (s >> 8) & 0xfff;
        }
        memset(p, 0xCC, sz);
        jit_compile(p, ops, imm, n);
        dbt_flush(p, sz);
        uint64_t x = 0x1000 + (uint64_t)round;
        uint64_t got = f(x);
        uint64_t want = ref_run(x, ops, imm, n);
        if (got != want) {
            printf("smc-minijit MISMATCH round=%d want=%llu got=%llu\n", round,
                   (unsigned long long)want, (unsigned long long)got);
            return 1;
        }
        acc = acc * 1000003ULL + got;
    }
    printf("smc-minijit acc=%llu\n", (unsigned long long)acc);
    return 0;
}
