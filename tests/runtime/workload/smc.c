// SOAK: self-modifying-code re-translation (the hardest DBT endurance path). We hold a tiny leaf function
// in an RWX page (`return imm`), and 200k times: patch its immediate, flush the icache
// (__builtin___clear_cache), and call it. Each patch produces a NEW code version at the SAME address,
// forcing the JIT to notice the change and re-translate -- unbounded distinct translations over the run,
// which churns the code cache (eviction/recycle) and the per-address translation invalidation. A DBT
// that ever serves a stale translation returns the wrong immediate and the checksum diverges. Both ISAs
// run it: aarch64 signals the change with `ic ivau`, x86-64 has a coherent i-cache so the guest store is
// the only signal. Diffed against a native run -> oracle.
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

// Emit a leaf function `int f(void){ return imm; }` at p.
static void emit_ret(unsigned char *p, uint32_t v) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)p;
    w[0] = 0x52800000u | ((v & 0xffffu) << 5); // movz w0, #imm
    w[1] = 0xd65f03c0u;                        // ret
#elif defined(__x86_64__)
    p[0] = 0xB8; // mov eax, imm32
    memcpy(p + 1, &v, 4);
    p[5] = 0xC3; // ret
#else
#error "needs an emitter for this ISA"
#endif
}

int main(void) {
    unsigned char *code = mmap(NULL, 4096, PROT_READ | PROT_WRITE | PROT_EXEC,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) { perror("mmap"); return 1; }
    uint64_t sum = 0;
    for (uint32_t i = 0; i < 200000; i++) {
        emit_ret(code, i & 0xffff);
        __builtin___clear_cache((char *)code, (char *)code + 8); // signal the I-cache/DBT: code changed
        uint32_t (*f)(void) = (uint32_t (*)(void))code;
        sum += f(); // must observe the just-written immediate, never a stale translation
    }
    munmap(code, 4096);
    printf("soak smc sum=%llu\n", (unsigned long long)sum); // sum of (i & 0xffff), i=0..199999
    return 0;
}
