// microbench.c — self-timed DBT microbenchmark kernels (task #326).
//
// Each kernel times ONLY its own compute loop with clock_gettime(CLOCK_MONOTONIC),
// so process/VM/engine startup, exit, and the `mac` bridge spawn are ALL excluded
// from the reported number — the whole point of this suite. The guest prints exactly
// one line per run:
//
//     KERNEL <name> <nanoseconds>
//
// Usage: microbench <kernel>            (run one kernel)
//        microbench all                 (run every kernel, one line each)
//
// Built BOTH aarch64-static and x86_64-static (see dd-tests bench harness). Volatile
// sinks keep -O2 from deleting the work. Iteration counts are sized so native arm64 is
// ~0.5–1s per kernel → any residual spawn tax is <1% of the measured kernel time.
//
// Portable C only (no arch intrinsics): the SIMD kernel is a plain auto-vectorizable
// float loop, so gcc emits NEON on aarch64 and SSE/AVX on x86_64 at -O2 — a fair,
// same-source SIMD comparison across both guest ISAs.

#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/syscall.h>

static inline uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

// ── volatile sinks: consumed after each kernel so the optimizer can't elide the work ──
static volatile uint64_t sink_u64;
static volatile double   sink_f64;

// ─────────────────────────────────── kernels ───────────────────────────────────

// int-ALU: a 64-bit LCG interleaved with shift/xor mixing (data-dependent chain).
static uint64_t k_alu(void) {
    const uint64_t N = 350000000ull;
    uint64_t x = 0x9e3779b97f4a7c15ull, acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        x = x * 6364136223846793005ull + 1442695040888963407ull; // LCG
        x ^= x >> 29;
        x += x << 13;
        x ^= x >> 17;
        acc += x;
    }
    sink_u64 = acc;
    return N;
}

// int mul/div: a dependent chain of 64-bit multiply + divide + modulo (div is the slow op).
static uint64_t k_muldiv(void) {
    const uint64_t N = 750000000ull;
    uint64_t x = 0x1234567deadbeefull, acc = 1;
    for (uint64_t i = 0; i < N; i++) {
        x = x * 2654435761ull + 12345ull;
        uint64_t d = (x | 1);           // never 0
        acc += x / d + x % (d | 3);
        acc ^= (acc << 7);
    }
    sink_u64 = acc;
    return N;
}

// branch-heavy: data-dependent branches over a random byte array (defeats the predictor).
static uint64_t k_branch(void) {
    const size_t SZ = 1u << 22;         // 4 MiB of random bytes
    const uint64_t PASSES = 42;
    unsigned char *b = malloc(SZ);
    uint64_t x = 88172645463325252ull;
    for (size_t i = 0; i < SZ; i++) { x ^= x << 13; x ^= x >> 7; x ^= x << 17; b[i] = (unsigned char)x; }
    uint64_t acc = 0;
    for (uint64_t p = 0; p < PASSES; p++) {
        for (size_t i = 0; i < SZ; i++) {
            unsigned char v = b[i];
            if (v & 1)        acc += v;
            else if (v & 2)   acc -= v;
            else if (v & 4)   acc ^= v;
            else if (v & 8)   acc += v * 3;
            else if (v & 16)  acc -= v >> 1;
            else              acc += 7;
        }
    }
    sink_u64 = acc;
    free(b);
    return SZ * PASSES;
}

// MEMORY BANDWIDTH: stride the whole of a >256 MiB buffer so it is NEVER cache-resident.
// Touch one word per cache line, many passes; report bytes streamed (buffer size * passes).
static uint64_t k_mem(void) {
    const size_t SZ = 320ull << 20;     // 320 MiB — far bigger than any LLC
    const uint64_t PASSES = 250;
    const size_t STRIDE = 64 / sizeof(uint64_t); // one word per 64B cache line
    uint64_t *p = malloc(SZ);
    size_t n = SZ / sizeof(uint64_t);
    for (size_t i = 0; i < n; i++) p[i] = i;
    uint64_t acc = 0;
    for (uint64_t k = 0; k < PASSES; k++)
        for (size_t i = 0; i < n; i += STRIDE) acc += p[i];
    sink_u64 = acc;
    free(p);
    return SZ * PASSES;
}

// FP: dependent double FMA chain (a = a*b + c), the classic floating-point throughput probe.
static uint64_t k_fp(void) {
    const uint64_t N = 900000000ull;
    double a = 1.0000001, b = 0.9999999, c = 1e-9;
    for (uint64_t i = 0; i < N; i++) {
        a = a * b + c;
        if (a > 1e6 || a < 1e-6) a = 1.0000001; // keep it finite, cheap branch
    }
    sink_f64 = a;
    return N;
}

// SIMD: a portable auto-vectorizable float loop (gcc -O2 → NEON on arm, SSE/AVX on x86).
static uint64_t k_simd(void) {
    const size_t SZ = 1u << 20;         // 1M floats (4 MiB) — streams from L2/L3
    const uint64_t PASSES = 4600;
    float *x = malloc(SZ * sizeof(float));
    float *y = malloc(SZ * sizeof(float));
    for (size_t i = 0; i < SZ; i++) { x[i] = (float)(i & 255) * 0.5f; y[i] = (float)((i * 7) & 255); }
    float s = 0.0f;
    for (uint64_t p = 0; p < PASSES; p++) {
        float local = 0.0f;
        for (size_t i = 0; i < SZ; i++) local += x[i] * y[i]; // SAXPY-ish dot product
        s += local;
    }
    sink_f64 = (double)s;
    free(x); free(y);
    return SZ * PASSES;
}

// memcpy/memset: large libc memcpy + memset (exercises the runtime's block-move handling).
static uint64_t k_memcpy(void) {
    const size_t SZ = 64ull << 20;      // 64 MiB
    const uint64_t PASSES = 360;
    char *src = malloc(SZ), *dst = malloc(SZ);
    memset(src, 0x5a, SZ);
    for (uint64_t p = 0; p < PASSES; p++) {
        memcpy(dst, src, SZ);
        memset(src, (int)(p & 0xff), SZ);
        sink_u64 += (uint64_t)dst[p % SZ];
    }
    free(src); free(dst);
    return SZ * PASSES * 2; // bytes moved (copy + set)
}

// function-call overhead: direct calls + INDIRECT calls through a fn-ptr array. The indirect
// arm exercises the JIT's indirect-branch target cache (IBTC) — a known DBT hot path.
static uint64_t add1(uint64_t a){ return a + 1; }
static uint64_t xor7(uint64_t a){ return a ^ 7; }
static uint64_t mul3(uint64_t a){ return a * 3; }
static uint64_t rol5(uint64_t a){ return (a << 5) | (a >> 59); }
typedef uint64_t (*fn_t)(uint64_t);
static uint64_t k_call(void) {
    const uint64_t N = 650000000ull;
    static fn_t tab[4] = { add1, xor7, mul3, rol5 };
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) {
        acc = add1(acc);                 // direct
        acc = tab[acc & 3](acc);         // indirect (data-dependent target → IBTC churn)
    }
    sink_u64 = acc;
    return N * 2;
}

// syscall overhead: a tight getpid() loop. Each call is a guest↔engine crossing under dd
// (raw syscall, not glibc-cached), so this isolates the world-switch cost.
static uint64_t k_syscall(void) {
    const uint64_t N = 6000000ull;
    uint64_t acc = 0;
    for (uint64_t i = 0; i < N; i++) acc += (uint64_t)syscall(SYS_getpid);
    sink_u64 = acc;
    return N;
}

// ─────────────────────────────────── driver ───────────────────────────────────

struct kern { const char *name; uint64_t (*fn)(void); };
static const struct kern KERNELS[] = {
    { "alu",     k_alu },
    { "muldiv",  k_muldiv },
    { "branch",  k_branch },
    { "mem",     k_mem },
    { "fp",      k_fp },
    { "simd",    k_simd },
    { "memcpy",  k_memcpy },
    { "call",    k_call },
    { "syscall", k_syscall },
};
static const int NKERN = (int)(sizeof(KERNELS) / sizeof(KERNELS[0]));

static void run_one(const struct kern *k) {
    uint64_t t0 = now_ns();
    uint64_t work = k->fn();
    uint64_t t1 = now_ns();
    (void)work;
    printf("KERNEL %s %llu\n", k->name, (unsigned long long)(t1 - t0));
    fflush(stdout);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <kernel|all>\nkernels:", argv[0]);
        for (int i = 0; i < NKERN; i++) fprintf(stderr, " %s", KERNELS[i].name);
        fprintf(stderr, "\n");
        return 2;
    }
    if (strcmp(argv[1], "all") == 0) {
        for (int i = 0; i < NKERN; i++) run_one(&KERNELS[i]);
        return 0;
    }
    for (int i = 0; i < NKERN; i++)
        if (strcmp(argv[1], KERNELS[i].name) == 0) { run_one(&KERNELS[i]); return 0; }
    fprintf(stderr, "unknown kernel: %s\n", argv[1]);
    return 2;
}
