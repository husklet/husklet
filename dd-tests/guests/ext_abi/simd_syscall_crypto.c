// simd_syscall_crypto.c — a vector-register write on the INLINE-LOWERED shuffle path must survive a
// syscall block-exit (perf Lever #3 slim SIMD-clean spill).
//
// Regression proof for a v0.9.19 latent bug on x86: the inline 0F38/0F3A crypto/shuffle glue
// (pshufb->TBL etc.) wrote guest xmm WITHOUT marking cpu->vdirty, so a following slim R_SYSCALL exit
// skipped the xmm save and the block-entry prologue reload restored STALE cpu->V.
//
// The trap sequence (block boundaries at each syscall):
//   block1: vector LOAD (SSE/SIMD region -> marks vdirty), getpid (FULL spill: V current, vdirty=0)
//   block2: pshufb/tbl ONLY (the inline shuffle path), getpid
//           -> if that path fails to mark vdirty, the slim exit skips the save and the shuffle is LOST.
//   block3: store the register, print.
// Correct output on both arches: the byte-reversed pattern (oracle-checked vs native/qemu).
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const uint8_t in[16] = { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15 };
static const uint8_t rev[16] = { 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0 };

int main(void) {
    uint8_t out[16];
#if defined(__x86_64__)
    __asm__ volatile("movdqu %[in], %%xmm7\n\t"
                     "movdqu %[rev], %%xmm6\n\t"
                     "mov $39, %%eax\n\t" // getpid
                     "syscall\n\t"
                     "pshufb %%xmm6, %%xmm7\n\t" // inline crypto-glue path (0F 38 00)
                     "mov $39, %%eax\n\t"
                     "syscall\n\t"
                     "movdqu %%xmm7, %[out]\n\t"
                     : [out] "=m"(out)
                     : [in] "m"(in), [rev] "m"(rev)
                     : "rax", "rcx", "r11", "xmm6", "xmm7", "memory");
#elif defined(__aarch64__)
    __asm__ volatile("ldr q7, %[in]\n\t"
                     "ldr q6, %[rev]\n\t"
                     "mov x8, #172\n\t" // getpid
                     "svc #0\n\t"
                     "tbl v7.16b, {v7.16b}, v6.16b\n\t" // the vector write between the two syscalls
                     "mov x8, #172\n\t"
                     "svc #0\n\t"
                     "str q7, %[out]\n\t"
                     : [out] "=m"(out)
                     : [in] "m"(in), [rev] "m"(rev)
                     : "x0", "x8", "v6", "v7", "memory");
#else
#error unsupported arch
#endif
    for (int i = 0; i < 16; i++) printf("%02x", out[i]);
    printf("\n");
    return memcmp(out, rev, 16) == 0 ? 0 : 1; // exit 0 = correct (reversed), 1 = stale/corrupt
}
