/* x86-xflags cross-block dead-flag elimination — differential guest (oracle: qemu-x86_64).
   hl's x86 JIT now extends dead-flag elision ACROSS direct block boundaries: at a direct jmp/call/jcc
   edge it scans the SUCCESSOR's guest bytes and skips materializing flags the successor provably
   overwrites before reading (NZCV via FL_SUB/FL_ADD/FL_LOGIC deferral; PF/AF via the #346 substrate).
   This guest puts a flag producer LAST in a block and reads the flags FIRST in the successor through
   every consumer family, plus the flag-dead shapes where the elision fires — so both a wrong elision
   (flag lost across the edge) and a wrong keep (stale value) diverge from qemu byte-exactly.

   Shapes covered (each with values sweeping carry/zero/sign/overflow/parity corners):
     A. cmp/sub/add/and in block A -> unconditional jmp -> jcc/setcc/lahf/pushfq in block B
        (forward jmp = stitched edge on first translation; backward jmp = chained edge).
     B. cmp + jcc where the CONSUMER is in the taken/fall successor (jcc-of-jcc cascades).
     C. adc/sbb chains crossing an unconditional jmp (carry consumed across the edge).
     D. inc/dec in the successor (kills ZF/SF/OF/PF/AF but PRESERVES CF -> producer's CF must
        survive THROUGH the inc to a jc/adc after it).
     E. pushfq / lahf / setp as the FIRST insn of a successor (all-live: elision must not fire).
     F. flag-dead successors (mov;add first) where the elision DOES fire -> results must be identical.
     G. two-block loops whose back-edge crosses blocks with flags carried around the loop.
   All prints are deterministic; the case is oracle-diffed vs qemu-x86_64. */
#include <stdio.h>
#include <stdint.h>

#define FLMASK 0x8D5UL /* CF PF AF ZF SF OF */
static unsigned long acc;

static const uint64_t V[] = {0, 1, 2, 0x7f, 0x80, 0xff, 0x100, 0x7fffffffull, 0x80000000ull,
                             0xffffffffull, 0x7fffffffffffffffull, 0x8000000000000000ull,
                             0xffffffffffffffffull, 0x123456789abcdef0ull};
#define NV (sizeof V / sizeof V[0])

int main(void) {
    /* A: producer -> jmp -> consumer-in-next-block (forward = stitch, backward = chain). */
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            unsigned long f = 0x99, ah = 0x99, st = 0x99, br = 0x99;
            __asm__ volatile( /* cmp ; jmp ; pushfq  (successor reads EVERYTHING -> no elide) */
                "cmpq %[b], %[a]\n\t jmp 1f\n\t 1: pushfq\n\t pop %[f]\n\t"
                : [f] "=r"(f) : [a] "r"(a), [b] "r"(b) : "cc");
            __asm__ volatile( /* add ; jmp ; lahf (FL_ADD across the edge) */
                "movq %[a], %%rax\n\t addq %[b], %%rax\n\t jmp 1f\n\t 1: lahf\n\t"
                "movzbl %%ah, %%ecx\n\t movl %%ecx, %k[r]\n\t"
                : [r] "=r"(ah) : [a] "r"(a), [b] "r"(b) : "rax", "rcx", "cc");
            __asm__ volatile( /* and ; jmp ; setcc family (FL_LOGIC across the edge) */
                "movq %[a], %%rax\n\t andq %[b], %%rax\n\t jmp 1f\n\t"
                "1: seta %%cl\n\t setl %%dl\n\t setp %%al\n\t"
                "movzbl %%cl, %k[r]\n\t movzbl %%dl, %%ecx\n\t leal (%k[r],%%ecx,4), %k[r]\n\t"
                "movzbl %%al, %%ecx\n\t shll $4, %%ecx\n\t addl %%ecx, %k[r]\n\t"
                : [r] "=&r"(st) : [a] "r"(a), [b] "r"(b) : "rax", "rcx", "rdx", "cc");
            __asm__ volatile( /* backward jmp: target translated first -> CHAINED edge */
                "jmp 2f\n\t"
                "1: jbe 3f\n\t movl $7, %k[r]\n\t jmp 4f\n\t"
                "3: movl $9, %k[r]\n\t jmp 4f\n\t"
                "2: cmpq %[b], %[a]\n\t jmp 1b\n\t" /* producer block, edge jumps BACK */
                "4:\n\t"
                : [r] "=r"(br) : [a] "r"(a), [b] "r"(b) : "cc");
            acc = acc * 1099511628211UL ^ (f & FLMASK) ^ (ah << 12) ^ (st << 20) ^ (br << 28);
            if ((i * NV + j) % 13 == 0)
                printf("A a=%016lx b=%016lx f=%03lx ah=%02lx set=%02lx br=%lu\n",
                       (unsigned long)a, (unsigned long)b, f & FLMASK, ah, st, br);
        }

    /* B: jcc cascades — consumer is the first insn of the taken AND of the fall successor. */
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            unsigned long r1 = 0, r2 = 0;
            __asm__ volatile( /* cmp; ja X: both arms then read CF/ZF/SF/OF again */
                "cmpq %[b], %[a]\n\t"
                "ja 1f\n\t"
                "jc 2f\n\t movl $10, %k[r]\n\t jmp 5f\n\t" /* fall arm reads CF */
                "2: movl $11, %k[r]\n\t jmp 5f\n\t"
                "1: jg 3f\n\t movl $12, %k[r]\n\t jmp 5f\n\t" /* taken arm reads SF/OF/ZF */
                "3: movl $13, %k[r]\n\t"
                "5:\n\t"
                : [r] "=r"(r1) : [a] "r"(a), [b] "r"(b) : "cc");
            __asm__ volatile( /* sub; jne X: parity read on one arm (PF must cross the edge) */
                "movq %[a], %%rax\n\t subq %[b], %%rax\n\t"
                "jne 1f\n\t"
                "setp %%cl\n\t movzbl %%cl, %k[r]\n\t jmp 2f\n\t"
                "1: setp %%cl\n\t movzbl %%cl, %k[r]\n\t addl $2, %k[r]\n\t"
                "2:\n\t"
                : [r] "=&r"(r2) : [a] "r"(a), [b] "r"(b) : "rax", "rcx", "cc");
            acc = acc * 1099511628211UL ^ (r1 << 4) ^ r2;
            if ((i + j) % 7 == 0)
                printf("B a=%016lx b=%016lx r1=%lu r2=%lu\n", (unsigned long)a, (unsigned long)b, r1, r2);
        }

    /* C: adc/sbb consuming a carry produced in the PREVIOUS block (crosses jmp / jcc edges). */
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            uint64_t s1 = 0, s2 = 0;
            unsigned long f1 = 0, f2 = 0;
            __asm__ volatile( /* add sets CF ; jmp ; adc consumes it in the next block */
                "movq %[a], %%rax\n\t addq %[b], %%rax\n\t jmp 1f\n\t"
                "1: movq %[a], %%rdx\n\t adcq %[b], %%rdx\n\t movq %%rdx, %[s]\n\t"
                "pushfq\n\t pop %[f]\n\t"
                : [s] "=r"(s1), [f] "=r"(f1) : [a] "r"(a), [b] "r"(b) : "rax", "rdx", "cc");
            __asm__ volatile( /* cmp sets borrow ; jae/jb ; sbb consumes it on BOTH arms */
                "cmpq %[b], %[a]\n\t"
                "jb 1f\n\t"
                "movq %[a], %%rdx\n\t sbbq %[b], %%rdx\n\t jmp 2f\n\t"
                "1: movq %[b], %%rdx\n\t sbbq %[a], %%rdx\n\t"
                "2: movq %%rdx, %[s]\n\t pushfq\n\t pop %[f]\n\t"
                : [s] "=r"(s2), [f] "=r"(f2) : [a] "r"(a), [b] "r"(b) : "rdx", "cc");
            acc = acc * 1099511628211UL ^ s1 ^ (f1 & FLMASK) ^ s2 ^ (f2 & FLMASK);
            if ((i * 3 + j) % 11 == 0)
                printf("C a=%016lx b=%016lx s1=%016lx f1=%03lx s2=%016lx f2=%03lx\n", (unsigned long)a,
                       (unsigned long)b, (unsigned long)s1, f1 & FLMASK, (unsigned long)s2, f2 & FLMASK);
        }

    /* D: successor starts with inc/dec (preserves CF) -> the producer's CF must survive THROUGH it. */
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            unsigned long r = 0;
            uint64_t s = 0;
            __asm__ volatile(
                "movq $5, %%rcx\n\t"
                "cmpq %[b], %[a]\n\t jmp 1f\n\t" /* CF crosses the edge... */
                "1: incq %%rcx\n\t"              /* ...survives inc (kills Z/S/O/P/A, keeps CF)... */
                "jc 2f\n\t movl $40, %k[r]\n\t jmp 3f\n\t" /* ...and is read by jc */
                "2: movl $41, %k[r]\n\t"
                "3: movq %%rcx, %[s]\n\t"
                : [r] "=r"(r), [s] "=r"(s) : [a] "r"(a), [b] "r"(b) : "rcx", "cc");
            unsigned long r2 = 0;
            __asm__ volatile( /* adc after inc after edge: CF(cmp) through inc into adc */
                "movq %[a], %%rdx\n\t cmpq %[b], %[a]\n\t jmp 1f\n\t"
                "1: incq %%rdx\n\t adcq $0, %%rdx\n\t movq %%rdx, %[r]\n\t"
                : [r] "=r"(r2) : [a] "r"(a), [b] "r"(b) : "rdx", "cc");
            acc = acc * 1099511628211UL ^ (r << 8) ^ s ^ r2;
            if ((i ^ j) % 9 == 0)
                printf("D a=%016lx b=%016lx r=%lu s=%lu r2=%016lx\n", (unsigned long)a, (unsigned long)b, r,
                       (unsigned long)s, (unsigned long)r2);
        }

    /* F: flag-DEAD successors — the elision fires; values (not flags) must still match qemu,
       and the KILLER's own flags (read after) must be the killer's, not the stale producer's. */
    for (unsigned i = 0; i < NV; i++)
        for (unsigned j = 0; j < NV; j++) {
            uint64_t a = V[i], b = V[j];
            unsigned long f = 0;
            uint64_t s = 0;
            __asm__ volatile( /* cmp (dead) ; jmp ; mov+add kills ; pushfq reads the ADD's flags */
                "cmpq %[b], %[a]\n\t jmp 1f\n\t"
                "1: movq %[a], %%rax\n\t addq %[b], %%rax\n\t movq %%rax, %[s]\n\t"
                "pushfq\n\t pop %[f]\n\t"
                : [f] "=r"(f), [s] "=r"(s) : [a] "r"(a), [b] "r"(b) : "rax", "cc");
            unsigned long f2 = 0;
            __asm__ volatile( /* add (FL_ADD dead) ; jcc both arms kill-then-produce */
                "movq %[a], %%rax\n\t addq %[b], %%rax\n\t"
                "jns 1f\n\t"
                "movq %[b], %%rax\n\t testq %%rax, %%rax\n\t jmp 2f\n\t"
                "1: movq %[a], %%rax\n\t testq %%rax, %%rax\n\t"
                "2: pushfq\n\t pop %[f]\n\t"
                : [f] "=r"(f2) : [a] "r"(a), [b] "r"(b) : "rax", "cc");
            acc = acc * 1099511628211UL ^ f ^ (f2 << 16) ^ s;
            if ((5 * i + j) % 13 == 0)
                printf("F a=%016lx b=%016lx s=%016lx f=%03lx f2=%03lx\n", (unsigned long)a, (unsigned long)b,
                       (unsigned long)s, f & FLMASK, f2 & FLMASK);
        }

    /* G: two-block loop, back-edge crosses blocks, CF carried around the whole loop via inc/dec. */
    {
        uint64_t sum = 0, n = 64;
        unsigned long fl = 0;
        __asm__ volatile(
            "movq %[n], %%rcx\n\t"
            "movq $0, %%rax\n\t"
            "stc\n\t" /* seed CF=1 */
            "1: adcq $3, %%rax\n\t" /* consumes CF from the previous iteration (crosses back-edge) */
            "decq %%rcx\n\t"        /* preserves CF; sets ZF for the jne */
            "jz 2f\n\t"
            "jmp 1b\n\t" /* back-edge = unconditional jmp -> separate block */
            "2: movq %%rax, %[s]\n\t pushfq\n\t pop %[f]\n\t"
            : [s] "=r"(sum), [f] "=r"(fl) : [n] "r"(n) : "rax", "rcx", "cc");
        printf("G sum=%lu f=%03lx\n", (unsigned long)sum, fl & FLMASK);
        acc = acc * 1099511628211UL ^ sum ^ fl;
    }

    printf("xflags acc=%016lx\n", acc);
    return 0;
}
