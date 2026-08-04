// Concurrent map-and-execute vs. reader: one thread repeatedly mmaps a fresh RWX region, JITs a leaf
// into it, executes it, and munmaps it; a second thread repeatedly reads (checksums) an overlapping-
// lifetime data region. This stresses the engine's translation of freshly-mapped executable memory
// while another thread churns the address space -- a race between code-cache install and unmap can
// crash or serve a translation for a reused address. The JIT thread verifies each call; deterministic.
#include "dbt.h"
#include <pthread.h>

#define ITERS 20000

static uint64_t jit_sum;
static uint64_t read_sum;
static int jit_bad = 0;

static void *jit_thread(void *arg) {
    (void)arg;
    uint64_t s = 0;
    uint32_t seed = 0x1357u;
    for (int i = 0; i < ITERS; i++) {
        unsigned char *p = dbt_alloc(4096, PROT_READ | PROT_WRITE | PROT_EXEC);
        seed = seed * 1103515245u + 12345u;
        int imm = (int)((seed >> 16) & 0x7fff);
        dbt_emit_ret_imm(p, imm);
        dbt_flush(p, 4096);
        int (*f)(void) = (int (*)(void))p;
        int got = f();
        if (got != imm) __atomic_store_n(&jit_bad, 1, __ATOMIC_RELAXED);
        s = s * 1000003ULL + (uint64_t)got;
        munmap(p, 4096); // unmap while the other thread churns memory
    }
    jit_sum = s;
    return NULL;
}

static void *read_thread(void *arg) {
    (void)arg;
    size_t len = 1 << 20;
    unsigned char *m = dbt_alloc(len, PROT_READ | PROT_WRITE);
    for (size_t i = 0; i < len; i += 4096) m[i] = (unsigned char)(i >> 12);
    uint64_t s = 0;
    for (int r = 0; r < ITERS; r++)
        for (size_t i = 0; i < len; i += 4096) s = s * 31 + m[i];
    munmap(m, len);
    read_sum = s;
    return NULL;
}

int main(void) {
    pthread_t a, b;
    pthread_create(&a, NULL, jit_thread, NULL);
    pthread_create(&b, NULL, read_thread, NULL);
    pthread_join(a, NULL);
    pthread_join(b, NULL);
    printf("conc-mmap-exec jit=%llu read=%llu bad=%d\n", (unsigned long long)jit_sum,
           (unsigned long long)read_sum, jit_bad);
    return 0;
}
