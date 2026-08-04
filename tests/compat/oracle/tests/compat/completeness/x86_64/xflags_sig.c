/* x86-xflags signal-timing differential — flags carried across block back-edges must survive async
   signal delivery (the #292 irq-poll exit at block entry). The loops below keep CF live ACROSS the
   back-edge (adc consumes the PREVIOUS iteration's carry; dec preserves CF), while SIGALRM fires
   repeatedly via setitimer. hl delivers the signal at a block boundary where the deferred/elided
   flag state must be reconstructible (live NZCV is spilled by the exit; sigreturn restores it) —
   any CF loss at the boundary changes the final sums deterministically. The handler count is
   timing-dependent, so only `fired > 0` is printed; the sums are exact and oracle-diffed vs qemu. */
#include <stdio.h>
#include <stdint.h>
#include <signal.h>
#include <sys/time.h>

static volatile sig_atomic_t fired;
static void on_alrm(int s) { (void)s; fired++; }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = on_alrm;
    sigaction(SIGALRM, &sa, 0);
    struct itimerval it = {{0, 2000}, {0, 2000}}; /* 2ms interval, repeats */
    setitimer(ITIMER_REAL, &it, 0);

    /* 1: single-block self-loop (tier-2 fold + irq poll on the folded back-edge). */
    uint64_t s1 = 0;
    __asm__ volatile("movq $30000000, %%rcx\n\t"
                     "movq $0, %%rax\n\t"
                     "stc\n\t"
                     "1: adcq $1, %%rax\n\t" /* CF from previous iteration */
                     "decq %%rcx\n\t"        /* preserves CF, sets ZF */
                     "jnz 1b\n\t"
                     "movq %%rax, %[s]\n\t"
                     : [s] "=r"(s1)::"rax", "rcx", "cc");

    /* 2: TWO-block loop -- the back-edge is an unconditional jmp (cross-block edge each iteration),
       with a cmp+jcc pair inside whose successors are flag-dead (the elision fires in-loop). */
    uint64_t s2 = 0;
    __asm__ volatile("movq $30000000, %%rcx\n\t"
                     "movq $0, %%rax\n\t"
                     "movq $0, %%rdx\n\t"
                     "1: addq $7, %%rax\n\t"
                     "cmpq $1000000, %%rax\n\t" /* producer whose flags die in both jcc arms */
                     "jb 2f\n\t"
                     "subq $1000000, %%rax\n\t addq $1, %%rdx\n\t"
                     "2: decq %%rcx\n\t"
                     "jz 3f\n\t"
                     "jmp 1b\n\t" /* cross-block back-edge */
                     "3: addq %%rdx, %%rax\n\t movq %%rax, %[s]\n\t"
                     : [s] "=r"(s2)::"rax", "rcx", "rdx", "cc");

    /* 3: adc chain where the carry crosses an in-loop jmp edge every iteration. */
    uint64_t s3 = 0;
    __asm__ volatile("movq $20000000, %%rcx\n\t"
                     "movq $0, %%rax\n\t"
                     "movq $0xffffffffffffffff, %%rdx\n\t"
                     "1: addq %%rdx, %%rax\n\t" /* CF nearly always set */
                     "jmp 2f\n\t"               /* carry crosses this edge */
                     "2: adcq $2, %%rax\n\t"
                     "decq %%rcx\n\t"
                     "jnz 1b\n\t"
                     "movq %%rax, %[s]\n\t"
                     : [s] "=r"(s3)::"rax", "rcx", "rdx", "cc");

    it.it_value.tv_sec = it.it_value.tv_usec = 0;
    it.it_interval = it.it_value;
    setitimer(ITIMER_REAL, &it, 0); /* stop the timer before printing */
    printf("xflags-sig s1=%llu s2=%llu s3=%llu fired=%d\n", (unsigned long long)s1, (unsigned long long)s2,
           (unsigned long long)s3, fired > 0);
    return 0;
}
