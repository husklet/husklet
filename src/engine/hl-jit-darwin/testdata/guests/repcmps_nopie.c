// rep cmps / rep scas whose STRING operand is a .rodata constant at a LOW absolute link address.
// In a non-PIE ET_EXEC the loader biases the image HIGH (macOS __PAGEZERO), so a guest pointer that
// still carries a low link address (here &S1/&S2, materialized as `mov edi,imm32` — exactly node:20's
// non-PIE argv/flag parser: `mov edi,<flagstr>; rep cmpsb`) must be rebased to the high mapping before
// the rep-string helper dereferences it. rep movs/stos already did this; cmps/scas did not -> SIGSEGV
// (node:20-slim `node --version`, #424). This is the regression guard. Diffed byte-for-byte vs qemu-x86_64.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// LOW-addressed constants in a non-PIE image (their &-address is an absolute low imm32).
static const char S1[13] = "hello world!";
static const char S2[13] = "hello WORLD!";

// REPE cmpsb: compare [rsi..] vs [rdi..], rcx elems. Returns remaining rcx + ZF (equal-run terminator).
static void repe_cmpsb(const void *si, const void *di, unsigned long n, unsigned long *rcx_out, int *zf_out) {
    unsigned long rcx;
    unsigned char zf;
    __asm__ volatile("cld\n\t repe cmpsb\n\t setz %b1\n\t"
                     : "=c"(rcx), "=&r"(zf), "+S"(si), "+D"(di)
                     : "0"(n)
                     : "cc", "memory");
    *rcx_out = rcx;
    *zf_out = zf;
}
// REPNE scasb: scan [rdi..] for AL, rcx elems. Returns remaining rcx + ZF (found).
static void repne_scasb(const void *di, int al, unsigned long n, unsigned long *rcx_out, int *zf_out) {
    unsigned long rcx;
    unsigned char zf;
    __asm__ volatile("cld\n\t repne scasb\n\t setz %b1\n\t"
                     : "=c"(rcx), "=&r"(zf), "+D"(di)
                     : "0"(n), "a"(al)
                     : "cc", "memory");
    *rcx_out = rcx;
    *zf_out = zf;
}

int main(void) {
    char stack_copy[13];
    memcpy(stack_copy, S1, 13); // a HIGH (stack) buffer identical to the LOW .rodata S1
    unsigned long rcx;
    int zf;

    // 1) rsi=stack (high), rdi=&S1 (.rodata, LOW -> must be rebased). Full match.
    repe_cmpsb(stack_copy, S1, 13, &rcx, &zf);
    printf("cmps stack-vs-S1: rcx=%lu zf=%d\n", rcx, zf);

    // 2) BOTH operands LOW .rodata: rsi=&S1, rdi=&S2 -> differ at index 6 ('w' vs 'W').
    repe_cmpsb(S1, S2, 13, &rcx, &zf);
    printf("cmps S1-vs-S2: rcx=%lu zf=%d\n", rcx, zf);

    // 3) scas over a LOW .rodata string: find 'w' (0x77) in S1.
    repne_scasb(S1, 'w', 13, &rcx, &zf);
    printf("scas S1 find-w: rcx=%lu zf=%d\n", rcx, zf);

    // 4) scas over LOW .rodata: byte NOT present -> scan exhausts, zf=0, rcx=0.
    repne_scasb(S1, 'Z', 13, &rcx, &zf);
    printf("scas S1 find-Z: rcx=%lu zf=%d\n", rcx, zf);
    return 0;
}
