// A signal must preserve the interrupted machine state, including the YMM UPPER lanes (bits[128:256)). The
// engine's signal frame saved only the low 128 bits (xmm), so a handler that touched ymm/zmm left the
// interrupted upper lanes corrupted on sigreturn -- here, zeroed. This sets ymm3 to all-ones, spins until a
// SIGALRM handler (which runs vzeroall, clobbering every ymm) has fired, then reports whether ymm3's upper
// 128 bits survived. Oracle-diffed vs qemu (all four upper dwords must stay 0xffffffff).
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/time.h>

static volatile sig_atomic_t ran = 0;
__attribute__((target("avx"))) static void on_alrm(int s) {
    (void)s;
    __asm__ volatile("vzeroall" ::: "memory"); // clobber all ymm in the handler context
    ran = 1;
}

__attribute__((target("avx"))) int main(void) {
    signal(SIGALRM, on_alrm);
    struct itimerval it = {{0, 0}, {0, 50000}}; // 50 ms one-shot
    setitimer(ITIMER_REAL, &it, NULL);
    uint32_t out[8];
    __asm__ volatile("vpcmpeqd %%ymm3, %%ymm3, %%ymm3\n\t" // ymm3 = all-ones (upper 128 = ffffffff...)
                     "1:\n\t"
                     "movl %1, %%eax\n\t"
                     "testl %%eax, %%eax\n\t"
                     "jz 1b\n\t" // spin until the handler ran (interrupts here with ymm3 live)
                     "vmovdqu %%ymm3, %0\n\t"
                     : "=m"(out)
                     : "m"(ran)
                     : "eax", "memory");
    printf("ymm3_upper %08x %08x %08x %08x\n", out[4], out[5], out[6], out[7]);
    return 0;
}
