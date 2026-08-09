// Stale-translation after mremap(MREMAP_FIXED): a translated executable VA is relocated (its mapping + code
// moved) and then the freed source VA is re-mapped with DIFFERENT code. Without dropping the source VA's
// cached translation on mremap, the dispatcher re-runs the OLD code. ISA-neutral; both guest ISAs run it.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdint.h>
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
typedef int (*fn)(void);
int main(void) {
    size_t ps = 4096;
    int prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    int flags = MAP_PRIVATE | MAP_ANONYMOUS;
    unsigned char *a = mmap(0, ps, prot, flags, -1, 0);
    unsigned char *b = mmap(0, ps, prot, flags, -1, 0);
    emit_ret(a, 11);
    __builtin___clear_cache((char *)a, (char *)a + ps);
    int first = ((fn)a)();      // translate A (=11)
    munmap(b, ps);              // free B as the relocation target
    void *r = mremap(a, ps, ps, MREMAP_MAYMOVE | MREMAP_FIXED, b); // move A -> B; A's VA freed
    int moved = (r == b);
    unsigned char *a2 = mmap(a, ps, prot, flags | MAP_FIXED, -1, 0); // reuse A's freed VA with new code
    emit_ret(a2, 22);
    __builtin___clear_cache((char *)a2, (char *)a2 + ps);
    int second = (a2 == a) ? ((fn)a)() : -1;
    printf("smc mremap first=%d moved=%d second=%d\n", first, moved, second);
    return 0;
}
