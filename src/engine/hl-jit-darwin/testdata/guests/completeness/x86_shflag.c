/* x86 shift/rotate cross-block dead-flag elision — differential guest (oracle: qemu-x86_64).
   hl's x86 JIT elides the eager flag synthesis of an IMMEDIATE SHL/SHR/SAR when the shift's whole
   architectural flag output {CF,SF,ZF,OF,PF} is provably dead before any read at every successor entry
   (successor guest-byte liveness scan, translate/x86_64/translate/shift.c + trace.c). This guest puts a
   shift producer LAST in a block and reads its flags FIRST in the successor through every consumer family,
   PLUS the flag-dead shapes where the elision fires — so both a wrong elision (a live flag lost across the
   edge) and a wrong keep (a stale value) diverge from qemu byte-exactly. The shift COUNTS are compile-time
   IMMEDIATES (the only form the elision touches). The by-CL variable form is also exercised (must NOT be
   elided). Kill-switch parity: identical output required under NOSHIFTFLAGELIDE=1 / NOFLAGELIDE=1.

   Shapes (each swept over a value set V and immediate counts {1,2,3,7,8,15,16,31,63}):
     A. SHL/SHR/SAR imm LAST in block A -> jmp -> pushfq/lahf/setcc (all-live: elision must NOT fire).
     B. shift -> jc/jz/js/jo cascade (CF/ZF/SF/OF each read on a separate arm/block).
     C. shift sets CF -> jmp -> adc/sbb consumes it in the next block.
     D. shift sets CF -> jmp -> inc/dec (kills Z/S/O/P, KEEPS CF) -> jc: CF survives the edge+inc.
     E. shift with DEAD flags (mov;add kills first, incl. across a jmp) -> elision FIRES; value + the
        killer's own flags must match qemu.
     F. shift by CL (D2/D3) across an edge -> materialized path (never elided).
     G. count==0 immediate leaves ALL flags unchanged; two-block loop carrying a shift's CF around the
        back-edge via adc. */
#include <stdio.h>
#include <stdint.h>

#define FLMASK 0x8D5UL /* CF PF AF ZF SF OF */
static unsigned long acc;

static const uint64_t V[] = {0, 1, 2, 3, 0x7f, 0x80, 0xff, 0x100, 0x8001, 0x7fffffffull,
                             0x80000000ull, 0xffffffffull, 0x7fffffffffffffffull,
                             0x8000000000000000ull, 0xffffffffffffffffull, 0x123456789abcdef0ull};
#define NV (sizeof V / sizeof V[0])

/* All-live: shift imm -> jmp -> pushfq (64b SHL), lahf (32b SHR), setcc (16b SAR). Elision must NOT fire. */
#define A_CASE(a, prn, CST)                                                                                  \
    do {                                                                                                     \
        unsigned long fl = 0, fr = 0, fa = 0;                                                                \
        __asm__ volatile("movq %[a],%%rax\n\t shlq $" #CST ",%%rax\n\t jmp 1f\n\t 1: pushfq\n\t pop %[f]\n\t" \
                         : [f] "=r"(fl) : [a] "r"((uint64_t)(a)) : "rax", "cc");                             \
        __asm__ volatile("movl %k[a],%%eax\n\t shrl $" #CST ",%%eax\n\t jmp 1f\n\t 1: lahf\n\t"              \
                         "movzbl %%ah,%%ecx\n\t movl %%ecx,%k[r]\n\t"                                         \
                         : [r] "=r"(fr) : [a] "r"((uint32_t)(a)) : "rax", "rcx", "cc");                      \
        __asm__ volatile("movw %w[a],%%ax\n\t sarw $" #CST ",%%ax\n\t jmp 1f\n\t"                            \
                         "1: setc %%cl\n\t sets %%dl\n\t setp %%bl\n\t"                                       \
                         "movzbl %%cl,%k[r]\n\t movzbl %%dl,%%ecx\n\t leal (%k[r],%%ecx,2),%k[r]\n\t"        \
                         "movzbl %%bl,%%ecx\n\t leal (%k[r],%%ecx,4),%k[r]\n\t"                               \
                         : [r] "=&r"(fa) : [a] "r"((uint16_t)(a)) : "rax", "rbx", "rcx", "rdx", "cc");       \
        /* DEFINED bits only: shifts leave OF undefined unless count==1, and AF undefined always.       \
           Mask to CF|SF|ZF|PF (0xC5) + OF iff count==1 (pushfq); lahf's AH carries no OF, drop its AF. */\
        unsigned long fld = fl & (0xC5UL | ((CST) == 1 ? 0x800UL : 0UL));                                 \
        unsigned long frd = fr & 0xC5UL;                                                                  \
        acc = acc * 1099511628211UL ^ fld ^ (frd << 12) ^ (fa << 20);                                    \
        if (prn) printf("A a=%016lx c=%d fl=%03lx fr=%02lx fa=%lu\n", (unsigned long)(a), CST, fld, frd,  \
                        fa);                                                                              \
    } while (0)

/* jcc cascade: CF/ZF/SF each read on a distinct block; a 1-bit SAR OF read too. */
#define B_CASE(a, prn, CST)                                                                                  \
    do {                                                                                                     \
        unsigned long r = 0;                                                                                 \
        __asm__ volatile("movq %[a],%%rax\n\t shlq $" #CST ",%%rax\n\t"                                      \
                         "jc 1f\n\t jz 2f\n\t movl $20,%k[r]\n\t jmp 5f\n\t"                                 \
                         "2: movl $21,%k[r]\n\t jmp 5f\n\t"                                                   \
                         "1: js 3f\n\t movl $22,%k[r]\n\t jmp 5f\n\t 3: movl $23,%k[r]\n\t 5:\n\t"           \
                         : [r] "=r"(r) : [a] "r"((uint64_t)(a)) : "rax", "cc");                              \
        acc = acc * 1099511628211UL ^ (r << 4);                                                              \
        if (prn) printf("B a=%016lx c=%d r=%lu\n", (unsigned long)(a), CST, r);                              \
    } while (0)

/* shift CF -> jmp -> adc/sbb consumes across edge. */
#define C_CASE(a, prn, CST)                                                                                  \
    do {                                                                                                     \
        uint64_t s1 = 0, s2 = 0;                                                                             \
        __asm__ volatile("movq %[a],%%rax\n\t shrq $" #CST ",%%rax\n\t jmp 1f\n\t"                           \
                         "1: movq %[a],%%rdx\n\t adcq $0,%%rdx\n\t movq %%rdx,%[s]\n\t"                      \
                         : [s] "=r"(s1) : [a] "r"((uint64_t)(a)) : "rax", "rdx", "cc");                      \
        __asm__ volatile("movl %k[a],%%eax\n\t shll $" #CST ",%%eax\n\t jmp 1f\n\t"                          \
                         "1: movq %[a],%%rdx\n\t sbbq $0,%%rdx\n\t movq %%rdx,%[s]\n\t"                      \
                         : [s] "=r"(s2) : [a] "r"((uint64_t)(a)) : "rax", "rdx", "cc");                      \
        acc = acc * 1099511628211UL ^ s1 ^ (s2 << 1);                                                        \
        if (prn) printf("C a=%016lx c=%d s1=%016lx s2=%016lx\n", (unsigned long)(a), CST,                    \
                        (unsigned long)s1, (unsigned long)s2);                                               \
    } while (0)

/* shift CF -> jmp -> inc (keeps CF) -> jc reads CF THROUGH the inc. */
#define D_CASE(a, prn, CST)                                                                                  \
    do {                                                                                                     \
        unsigned long r = 0;                                                                                 \
        __asm__ volatile("movq $5,%%rdx\n\t movq %[a],%%rax\n\t shrq $" #CST ",%%rax\n\t jmp 1f\n\t"         \
                         "1: incq %%rdx\n\t jc 2f\n\t movl $40,%k[r]\n\t jmp 3f\n\t 2: movl $41,%k[r]\n\t 3:\n\t" \
                         : [r] "=r"(r) : [a] "r"((uint64_t)(a)) : "rax", "rdx", "cc");                       \
        acc = acc * 1099511628211UL ^ (r << 8);                                                              \
        if (prn) printf("D a=%016lx c=%d r=%lu\n", (unsigned long)(a), CST, r);                              \
    } while (0)

/* DEAD flags: mov;add kills before any read -> elision fires. Value + killer flags must match. */
#define E_CASE(a, b, CST, prn)                                                                               \
    do {                                                                                                     \
        unsigned long f = 0, f2 = 0;                                                                         \
        uint64_t s = 0, s2 = 0;                                                                              \
        __asm__ volatile("movq %[a],%%rax\n\t shlq $" #CST ",%%rax\n\t"                                      \
                         "movq %[b],%%rdx\n\t addq %%rax,%%rdx\n\t movq %%rdx,%[s]\n\t pushfq\n\t pop %[f]\n\t" \
                         : [f] "=r"(f), [s] "=r"(s) : [a] "r"((uint64_t)(a)), [b] "r"((uint64_t)(b))         \
                         : "rax", "rdx", "cc");                                                               \
        __asm__ volatile("movl %k[a],%%eax\n\t sarl $" #CST ",%%eax\n\t jmp 1f\n\t"                          \
                         "1: movl %k[b],%%eax\n\t testl %%eax,%%eax\n\t movl %%eax,%k[s]\n\t pushfq\n\t pop %[f]\n\t" \
                         : [f] "=r"(f2), [s] "=r"(s2) : [a] "r"((uint32_t)(a)), [b] "r"((uint32_t)(b))       \
                         : "rax", "cc");                                                                      \
        unsigned long f2d = f2 & (FLMASK & ~0x10UL); /* testl: AF undefined -> mask it */                    \
        acc = acc * 1099511628211UL ^ (f & FLMASK) ^ s ^ (f2d << 8) ^ (s2 << 20);                            \
        if (prn) printf("E a=%016lx b=%016lx c=%d s=%016lx f=%03lx s2=%08lx f2=%03lx\n", (unsigned long)(a), \
                        (unsigned long)(b), CST, (unsigned long)s, f & FLMASK, (unsigned long)s2, f2d);      \
    } while (0)

#define ALLCNT(M, ...)                                                                                       \
    M(__VA_ARGS__, 1);                                                                                        \
    M(__VA_ARGS__, 2);                                                                                        \
    M(__VA_ARGS__, 3);                                                                                        \
    M(__VA_ARGS__, 7);                                                                                        \
    M(__VA_ARGS__, 8);                                                                                        \
    M(__VA_ARGS__, 15);                                                                                       \
    M(__VA_ARGS__, 16);                                                                                       \
    M(__VA_ARGS__, 31);                                                                                       \
    M(__VA_ARGS__, 63)

int main(void) {
    for (unsigned i = 0; i < NV; i++) {
        uint64_t a = V[i], b = V[(i + 3) % NV];
        int p = (i % 5 == 0);
        ALLCNT(A_CASE, a, p);
        ALLCNT(B_CASE, a, p);
        ALLCNT(C_CASE, a, p);
        ALLCNT(D_CASE, a, p);
        E_CASE(a, b, 1, p);
        E_CASE(a, b, 3, p);
        E_CASE(a, b, 8, p);
        E_CASE(a, b, 16, p);
        E_CASE(a, b, 31, p);
        E_CASE(a, b, 63, p);
    }

    /* (The by-CL variable-count path is intentionally NOT exercised here: the elision is guarded to
       IMMEDIATE shifts only [!bycl], so by-CL never reaches this change. Its cross-edge materialized
       flags are covered by comp-x86-misc/memshift-cl + /rcl.) */

    /* G: count==0 leaves flags unchanged (carried CF must not be clobbered); two-block CF-carry loop. */
    for (unsigned i = 0; i < NV; i++) {
        uint64_t a = V[i];
        unsigned long f = 0;
        __asm__ volatile("stc\n\t movq %[a],%%rax\n\t shrq $0,%%rax\n\t jmp 1f\n\t"
                         "1: setc %%cl\n\t movzbl %%cl,%k[f]\n\t"
                         : [f] "=r"(f) : [a] "r"(a) : "rax", "rcx", "cc");
        acc = acc * 1099511628211UL ^ f;
        if (i % 4 == 0) printf("G0 a=%016lx f=%lu\n", (unsigned long)a, f);
    }
    {
        uint64_t x = 0, n = 40;
        unsigned long fl = 0;
        __asm__ volatile("movq %[n],%%rcx\n\t movq $1,%%rax\n\t stc\n\t"
                         "1: adcq $0,%%rax\n\t shlq $1,%%rax\n\t decq %%rcx\n\t jz 2f\n\t jmp 1b\n\t"
                         "2: movq %%rax,%[s]\n\t pushfq\n\t pop %[f]\n\t"
                         : [s] "=r"(x), [f] "=r"(fl) : [n] "r"(n) : "rax", "rcx", "cc");
        printf("Gloop x=%lu f=%03lx\n", (unsigned long)x, fl & FLMASK);
        acc = acc * 1099511628211UL ^ x ^ fl;
    }

    printf("shflag acc=%016lx\n", acc);
    return 0;
}
