/* x86 accumulator-immediate SBB: opcode 0x1C (SBB AL, imm8) and 0x1D (SBB eAX/rAX, imm32-sext).
   These accumulator forms bypass the ModRM group-1 path, so they are easy to miss in a translator.
   The guest forces GAS to emit exactly the short accumulator encodings (dest = %al / %eax / %rax with
   an immediate) and prints result + the six arithmetic flags across borrow / no-borrow and every
   interesting operand corner. Run on LinuxX86_64, oracle-diffed vs qemu: a mistranslation (wrong
   value/flags) OR an UNIMPL (crash/diag) diverges from qemu and is caught. */
#include <stdio.h>
#include <stdint.h>

/* the six x86 arithmetic flags: CF(0) PF(2) AF(4) ZF(6) SF(7) OF(11) */
#define FLMASK 0x8D5UL

/* SBB AL, imm8  -> opcode 0x1C.  cf_in selects incoming carry via stc/clc. */
#define SBB_AL(al, imm, cf_in) do {                                                       \
    unsigned long _r = 0, _f = 0;                                                         \
    __asm__ volatile(                                                                     \
        "movb %[a], %%al\n\t"                                                             \
        cf_in "\n\t"                                                                       \
        "sbb %[i], %%al\n\t"                                                              \
        "movzbl %%al, %k[r]\n\t"                                                          \
        "pushfq\n\t pop %[f]\n\t"                                                          \
        : [r] "=&r"(_r), [f] "=r"(_f)                                                      \
        : [a] "r"((unsigned char)(al)), [i] "i"((unsigned char)(imm))                      \
        : "rax", "cc");                                                                    \
    printf("SBB_AL al=%02x imm=%02x cf=%s -> r=%02lx fl=%03lx\n",                          \
           (unsigned)(al), (unsigned)(unsigned char)(imm), cf_in[0] == 's' ? "1" : "0",    \
           _r & 0xff, _f & FLMASK);                                                        \
} while (0)

/* SBB eAX, imm32 (sign-extended)  -> opcode 0x1D. imm must not fit in imm8, else GAS would pick 0x83/4. */
#define SBB_EAX(eax, imm, cf_in) do {                                                     \
    unsigned long _r = 0, _f = 0;                                                         \
    __asm__ volatile(                                                                     \
        "movl %k[a], %%eax\n\t"                                                           \
        cf_in "\n\t"                                                                       \
        "sbb %[i], %%eax\n\t"                                                             \
        "movl %%eax, %k[r]\n\t"                                                           \
        "pushfq\n\t pop %[f]\n\t"                                                          \
        : [r] "=&r"(_r), [f] "=r"(_f)                                                      \
        : [a] "r"((unsigned)(eax)), [i] "i"((int)(imm))                                    \
        : "rax", "cc");                                                                    \
    printf("SBB_EAX eax=%08x imm=%08x cf=%s -> r=%08lx fl=%03lx\n",                        \
           (unsigned)(eax), (unsigned)(imm), cf_in[0] == 's' ? "1" : "0",                  \
           _r & 0xffffffff, _f & FLMASK);                                                  \
} while (0)

/* SBB rAX, imm32 (sign-extended to 64)  -> REX.W 0x1D. */
#define SBB_RAX(rax, imm, cf_in) do {                                                     \
    unsigned long _r = 0, _f = 0;                                                         \
    __asm__ volatile(                                                                     \
        "movq %[a], %%rax\n\t"                                                            \
        cf_in "\n\t"                                                                       \
        "sbb %[i], %%rax\n\t"                                                             \
        "movq %%rax, %[r]\n\t"                                                            \
        "pushfq\n\t pop %[f]\n\t"                                                          \
        : [r] "=&r"(_r), [f] "=r"(_f)                                                      \
        : [a] "r"((unsigned long)(rax)), [i] "i"((int)(imm))                               \
        : "rax", "cc");                                                                    \
    printf("SBB_RAX rax=%016lx imm=%08x cf=%s -> r=%016lx fl=%03lx\n",                     \
           (unsigned long)(rax), (unsigned)(imm), cf_in[0] == 's' ? "1" : "0",             \
           _r, _f & FLMASK);                                                               \
} while (0)

int main(void) {
    /* --- 0x1C : SBB AL, imm8 --- borrow / no-borrow, ZF, SF, OF(signed overflow), AF corners --- */
    SBB_AL(0x10, 0x05, "clc");   /* 0x10-5   = 0x0b, no borrow */
    SBB_AL(0x10, 0x05, "stc");   /* 0x10-5-1 = 0x0a */
    SBB_AL(0x05, 0x05, "clc");   /* -> 0, ZF */
    SBB_AL(0x05, 0x05, "stc");   /* -> 0xff, borrow (CF), SF */
    SBB_AL(0x00, 0x01, "clc");   /* -> 0xff, borrow */
    SBB_AL(0x00, 0x00, "stc");   /* -> 0xff, borrow (0-0-1) */
    SBB_AL(0x80, 0x01, "clc");   /* 0x80-1 = 0x7f, signed OF */
    SBB_AL(0x7f, 0xff, "clc");   /* 0x7f-(-1)=0x80, signed OF, SF */
    SBB_AL(0x10, 0x01, "clc");   /* AF-clear */
    SBB_AL(0x10, 0x01, "stc");   /* 0x10-1-1=0x0e, AF set (nibble borrow) */
    SBB_AL(0xff, 0xff, "stc");   /* -> 0xff, borrow */

    /* --- 0x1D : SBB eAX, imm32 --- immediates must NOT fit in a sign-ext imm8, else GAS picks 0x83/4 --- */
    SBB_EAX(0x00000200, 0x00000105, "clc"); /* no borrow */
    SBB_EAX(0x00000200, 0x00000105, "stc"); /* borrow-in */
    SBB_EAX(0x00000105, 0x00000105, "clc"); /* -> 0, ZF */
    SBB_EAX(0x00000105, 0x00000105, "stc"); /* -> 0xffffffff, borrow, SF */
    SBB_EAX(0x00000000, 0x00000105, "clc"); /* -> negative, borrow */
    SBB_EAX(0x12345678, 0x00abcdef, "clc");
    SBB_EAX(0x80000000, 0x00000105, "clc"); /* pos-of-neg wraps positive -> OF */
    SBB_EAX(0x7fffffff, 0xffff0000, "stc"); /* minuend pos - neg imm -> OF */

    /* --- REX.W 0x1D : SBB rAX, imm32-sext --- immediates chosen so GAS emits 48 1D (not 48 83 /4) --- */
    SBB_RAX(0x0000000100000200UL, 0x00000105, "clc");
    SBB_RAX(0x0000000000000105UL, 0x00000105, "clc"); /* -> 0, ZF */
    SBB_RAX(0x0000000000000000UL, 0x00000105, "clc"); /* -> sign-extended borrow */
    SBB_RAX(0x0000000000000000UL, 0xffff0000, "clc"); /* imm sext to -65536 -> +65536 */
    SBB_RAX(0x8000000000000000UL, 0x00000105, "clc"); /* signed OF */
    return 0;
}
