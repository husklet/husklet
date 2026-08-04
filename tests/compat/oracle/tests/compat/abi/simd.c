// simd_syscall.c — regression gate for the "SIMD-clean syscall block-exit" optimization (perf Lever #3).
//
// The engine may skip saving the guest vector register file on a plain syscall block-exit WHEN the
// translated block wrote no vector register (the values are already current in cpu->V). This test proves
// vector state survives across syscalls in the two cases the optimization must get right:
//   clean : a vector register is LIVE across a long loop of syscalls whose blocks touch no vector reg
//           (each syscall takes the slim exit) — the value must be preserved via cpu->V.
//   dirty : a vector register is WRITTEN in the SAME basic block as the syscall — the block must take the
//           FULL spill so the just-written value is not lost across the service round-trip.
//   multi : all of v0..v7 / xmm0..xmm7 carry distinct patterns across the syscall loop at once.
// Output is architecture-independent booleans, so cross-engine stdout comparison catches corruption
// on either guest architecture. getpid is used (never an engine inline-fast syscall) so every
// iteration exercises a real block-exit.
#include <stdio.h>
#include <string.h>
#include <stdint.h>

#define NSYS 4096

static void fill(unsigned char *b, int n, unsigned seed) {
    for (int i = 0; i < n; i++) b[i] = (unsigned char)(seed * 131u + i * 17u + 0x5A);
}

#if defined(__aarch64__)
// v0..v7 loaded with `pat` (128 bytes), NSYS getpid syscalls with them live, then stored to `out`.
static void clean_multi(const unsigned char *pat, unsigned char *out, long n) {
    __asm__ volatile(
        "ldp q0, q1, [%[p], #0]   \n"
        "ldp q2, q3, [%[p], #32]  \n"
        "ldp q4, q5, [%[p], #64]  \n"
        "ldp q6, q7, [%[p], #96]  \n"
        "mov x9, %[n]             \n"
        "1:                       \n"
        "mov x8, #172             \n" // __NR_getpid
        "svc #0                   \n"
        "subs x9, x9, #1          \n"
        "b.ne 1b                  \n"
        "stp q0, q1, [%[o], #0]   \n"
        "stp q2, q3, [%[o], #32]  \n"
        "stp q4, q5, [%[o], #64]  \n"
        "stp q6, q7, [%[o], #96]  \n"
        : : [p] "r"(pat), [o] "r"(out), [n] "r"(n)
        : "x8", "x9", "x0", "v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7", "memory");
}
// v9 WRITTEN in the same block as the svc: the block must full-spill so v9 survives.
static void dirty_block(const unsigned char *pat, unsigned char *out) {
    __asm__ volatile(
        "ldr q9, [%[p]] \n"
        "mov x8, #172   \n"
        "svc #0         \n" // block ends here having written v9 -> must full spill
        "str q9, [%[o]] \n"
        : : [p] "r"(pat), [o] "r"(out)
        : "x8", "x0", "v9", "memory");
}
#elif defined(__x86_64__)
static void clean_multi(const unsigned char *pat, unsigned char *out, long n) {
    __asm__ volatile(
        "movdqu 0(%[p]), %%xmm0   \n"
        "movdqu 16(%[p]), %%xmm1  \n"
        "movdqu 32(%[p]), %%xmm2  \n"
        "movdqu 48(%[p]), %%xmm3  \n"
        "movdqu 64(%[p]), %%xmm4  \n"
        "movdqu 80(%[p]), %%xmm5  \n"
        "movdqu 96(%[p]), %%xmm6  \n"
        "movdqu 112(%[p]), %%xmm7 \n"
        "mov %[n], %%r12          \n"
        "1:                       \n"
        "mov $39, %%eax           \n" // __NR_getpid
        "syscall                  \n"
        "dec %%r12                \n"
        "jnz 1b                   \n"
        "movdqu %%xmm0, 0(%[o])   \n"
        "movdqu %%xmm1, 16(%[o])  \n"
        "movdqu %%xmm2, 32(%[o])  \n"
        "movdqu %%xmm3, 48(%[o])  \n"
        "movdqu %%xmm4, 64(%[o])  \n"
        "movdqu %%xmm5, 80(%[o])  \n"
        "movdqu %%xmm6, 96(%[o])  \n"
        "movdqu %%xmm7, 112(%[o]) \n"
        : : [p] "r"(pat), [o] "r"(out), [n] "r"(n)
        : "rax", "rcx", "r11", "r12", "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7", "memory");
}
static void dirty_block(const unsigned char *pat, unsigned char *out) {
    __asm__ volatile(
        "movdqu 0(%[p]), %%xmm3 \n"
        "mov $39, %%eax         \n"
        "syscall                \n" // block ends here having written xmm9 -> must full spill
        "movdqu %%xmm3, 0(%[o]) \n"
        : : [p] "r"(pat), [o] "r"(out)
        : "rax", "rcx", "r11", "xmm3", "memory");
}
#else
static void clean_multi(const unsigned char *pat, unsigned char *out, long n) { (void)n; memcpy(out, pat, 128); }
static void dirty_block(const unsigned char *pat, unsigned char *out) { memcpy(out, pat, 16); }
#endif

int main(void) {
    unsigned char pat[128], out[128];

    // multi/clean: v0..v7 (xmm0..7) carry distinct patterns across NSYS syscalls.
    fill(pat, 128, 3);
    memset(out, 0, sizeof out);
    clean_multi(pat, out, NSYS);
    int clean = (memcmp(pat, out, 128) == 0);

    // dirty: a vector reg written in the same block as the syscall.
    fill(pat, 16, 9);
    memset(out, 0, sizeof out);
    dirty_block(pat, out);
    int dirty = (memcmp(pat, out, 16) == 0);

    // interleave: re-run the clean loop with a different pattern to catch any cpu->V staleness.
    fill(pat, 128, 42);
    memset(out, 0, sizeof out);
    clean_multi(pat, out, NSYS);
    int multi = (memcmp(pat, out, 128) == 0);

    printf("simd-syscall clean=%d dirty=%d multi=%d ok\n", clean, dirty, multi);
    return 0;
}
