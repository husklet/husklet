// 0F D6's prefix picks the REGISTER FILE on each side, and ignoring it wrote the right eight bytes to the
// wrong place with no diagnostic: F3 0F D6 is MOVQ2DQ (xmm <- mm), F2 0F D6 is MOVDQ2Q (mm <- xmm), and
// only 66 0F D6 is the MOVQ xmm/m64 store. NP 0F D6 has no encoding, and the two MMX forms name a REGISTER
// operand (Nq/Uq), so a memory ModRM is #UD as well -- all four checked here against native hardware, which
// reports SIGILL/ILL_ILLOPN. Registers are chosen so no mmN aliases an xmmN the test also uses.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

static sigjmp_buf jb;
static volatile int got_sig, got_code;

static void handler(int sig, siginfo_t *si, void *ctx) {
    (void)ctx;
    got_sig = sig;
    got_code = si->si_code;
    siglongjmp(jb, 1);
}

static unsigned char page[8192] __attribute__((aligned(4096)));

static void try_bytes(const char *tag, const unsigned char *code, unsigned n) {
    memcpy(page, code, n);
    page[n] = 0xc3; // ret
    if (mprotect(page, 8192, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) return;
    got_sig = got_code = 0;
    if (sigsetjmp(jb, 1) == 0) {
        ((void (*)(void))(void *)page)();
        printf("%s: ran\n", tag);
    } else {
        printf("%s: sig=%d code=%d\n", tag, got_sig, got_code);
    }
}

static void dump(const char *tag, const unsigned char *p, int n) {
    printf("%s:", tag);
    for (int i = 0; i < n; i++)
        printf(" %02x", p[i]);
    printf("\n");
}

int main(void) {
    unsigned long long src = 0x0123456789abcdefULL;
    unsigned char sent[16], wide[16], out[16];
    memset(sent, 0xa5, sizeof sent);
    for (int i = 0; i < 16; i++)
        wide[i] = (unsigned char)(0x10 + i);

    // MOVQ2DQ: xmm7 := mm3, upper 64 bits ZEROED over the 0xa5 seed.
    memset(out, 0, sizeof out);
    __asm__ volatile("movdqu %2, %%xmm7\n\tmovq %1, %%mm3\n\tmovq2dq %%mm3, %%xmm7\n\tmovdqu %%xmm7, %0\n\temms"
                     : "=m"(out)
                     : "r"(src), "m"(sent)
                     : "mm3", "xmm7", "memory");
    dump("movq2dq", out, 16);

    // MOVDQ2Q: mm2 := xmm5's LOW 64 bits.
    unsigned char got[8];
    memset(got, 0x5a, sizeof got);
    __asm__ volatile("movdqu %1, %%xmm5\n\tmovdq2q %%xmm5, %%mm2\n\tmovq %%mm2, %0\n\temms"
                     : "=m"(got)
                     : "m"(wide)
                     : "mm2", "xmm5", "memory");
    dump("movdq2q", got, 8);

    // 66 0F D6 must still zero the upper half of a register destination, and store only 8 bytes to memory.
    memset(out, 0, sizeof out);
    __asm__ volatile("movdqu %1, %%xmm6\n\tmovdqu %2, %%xmm7\n\tmovq %%xmm6, %%xmm7\n\tmovdqu %%xmm7, %0"
                     : "=m"(out)
                     : "m"(wide), "m"(sent)
                     : "xmm6", "xmm7", "memory");
    dump("movq-reg", out, 16);
    memset(out, 0xcc, sizeof out);
    __asm__ volatile("movdqu %1, %%xmm6\n\tmovq %%xmm6, %0" : "=m"(out) : "m"(wide) : "xmm6", "memory");
    dump("movq-mem", out, 16);

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGILL, &sa, NULL);
    sigaction(SIGSEGV, &sa, NULL);

    // The memory forms address the red zone, so a claim that "it ran" cannot corrupt the return address.
    static const unsigned char np_reg[] = {0x0f, 0xd6, 0xc1};
    static const unsigned char np_mem[] = {0x0f, 0xd6, 0x44, 0x24, 0xc0};
    static const unsigned char f3_mem[] = {0xf3, 0x0f, 0xd6, 0x44, 0x24, 0xc0};
    static const unsigned char f2_mem[] = {0xf2, 0x0f, 0xd6, 0x44, 0x24, 0xc0};
    static const unsigned char f3_reg[] = {0xf3, 0x0f, 0xd6, 0xc1, 0x0f, 0x77};
    static const unsigned char f2_reg[] = {0xf2, 0x0f, 0xd6, 0xc1, 0x0f, 0x77};
    try_bytes("np-reg", np_reg, sizeof np_reg);
    try_bytes("np-mem", np_mem, sizeof np_mem);
    try_bytes("f3-mem", f3_mem, sizeof f3_mem);
    try_bytes("f2-mem", f2_mem, sizeof f2_mem);
    try_bytes("f3-reg", f3_reg, sizeof f3_reg);
    try_bytes("f2-reg", f2_reg, sizeof f2_reg);
    return 0;
}
