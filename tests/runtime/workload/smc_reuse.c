// Stale-translation after unmap/remap: an executable VA is translated, unmapped, then a DIFFERENT executable
// page is mapped at the SAME VA (once via plain munmap+MAP_FIXED, once via MAP_FIXED replacing in place). The
// dispatcher keys cached host code by guest PC, so without invalidation on unmap/MAP_FIXED it re-runs the OLD
// translation. The mapping lifecycle is ISA-neutral, so both guest ISAs run it; golden-checked.
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
    int flags = MAP_PRIVATE | MAP_ANONYMOUS;
    int prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    unsigned char *a = mmap(0, ps, prot, flags, -1, 0);
    emit_ret(a, 111);
    __builtin___clear_cache((char *)a, (char *)a + ps);
    int r1 = ((fn)a)();
    munmap(a, ps);
    unsigned char *b = mmap(a, ps, prot, flags | MAP_FIXED, -1, 0);
    emit_ret(b, 222);
    __builtin___clear_cache((char *)b, (char *)b + ps);
    int r2 = (b == a) ? ((fn)b)() : -1;
    // now MAP_FIXED straight over the SAME VA (no explicit munmap) with new code
    unsigned char *c = mmap(a, ps, prot, flags | MAP_FIXED, -1, 0);
    emit_ret(c, 333);
    __builtin___clear_cache((char *)c, (char *)c + ps);
    int r3 = (c == a) ? ((fn)c)() : -1;
    printf("smc remap r1=%d r2=%d r3=%d\n", r1, r2, r3);
    return 0;
}
