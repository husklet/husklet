// A SCALAR SSE instruction reads lane 0 and nothing else, so no exception may reach MXCSR from the upper
// lanes of its operands. That is easy to break in an emulator written with intrinsics: `scalar ? _mm_add_ss
// : _mm_add_ps` is if-converted by -O2 into BOTH instructions plus a select, and the packed one ORs its
// upper lanes' #I into the flags the guest is about to read. Every scalar row here parks a QNaN and an
// overflow pair in lanes 1..3, keeps lane 0 exact, and requires MXCSR to come back CLEAR; the packed twin
// of each row is the control that must raise #I.
//
// Also here because it is the other half of the same measurement: REX.R does NOT extend a group's /reg
// opcode extension, so `44 F6 /0 ib` is still TEST with an immediate. Decoding it as NOT (no immediate)
// makes the instruction one byte short and the immediate executes as the next instruction.
//
// Golden measured on Zen 4. Registers only, one volatile asm per row with its own ldmxcsr/stmxcsr: gcc
// hoists SSE arithmetic across LDMXCSR and constant-folds intrinsic sequences (see denorm_flags.c).
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint8_t A[16] __attribute__((aligned(16))); // xmm0 = destination / src1
static uint8_t B[16] __attribute__((aligned(16))); // xmm1 = src2
static uint8_t Out[16] __attribute__((aligned(16)));
static unsigned mxin, mxout;

#define ROW(label, insn)                                                                                               \
    do {                                                                                                               \
        mxin = 0x1f80;                                                                                                 \
        __asm__ volatile("movaps %2,%%xmm0\n\t"                                                                        \
                         "movaps %3,%%xmm1\n\t"                                                                        \
                         "ldmxcsr %4\n\t" insn "\n\t"                                                                  \
                         "stmxcsr %0\n\t"                                                                              \
                         "movaps %%xmm0,%1"                                                                            \
                         : "=m"(mxout), "=m"(Out)                                                                      \
                         : "m"(A), "m"(B), "m"(mxin)                                                                   \
                         : "xmm0", "xmm1", "memory");                                                                  \
        uint32_t low;                                                                                                  \
        uint64_t low64;                                                                                                \
        memcpy(&low, Out, 4);                                                                                          \
        memcpy(&low64, Out, 8);                                                                                        \
        printf("%-10s mx=%02x low32=%08x low64=%016llx\n", label, mxout & 0x3f, low, (unsigned long long)low64);        \
    } while (0)

// lane 0 exact, lanes 1..3 a QNaN and a pair that overflows if anyone touches them.
static void load_single(float lane0_a, float lane0_b) {
    uint32_t qnan = 0x7fc00000u, big = 0x7f7fffffu;
    memcpy(A + 0, &lane0_a, 4);
    memcpy(A + 4, &qnan, 4);
    memcpy(A + 8, &big, 4);
    memcpy(A + 12, &big, 4);
    memcpy(B + 0, &lane0_b, 4);
    memcpy(B + 4, &qnan, 4);
    memcpy(B + 8, &big, 4);
    memcpy(B + 12, &big, 4);
}

static void load_double(double lane0_a, double lane0_b) {
    uint64_t qnan = 0x7ff8000000000000ull;
    memcpy(A + 0, &lane0_a, 8);
    memcpy(A + 8, &qnan, 8);
    memcpy(B + 0, &lane0_b, 8);
    memcpy(B + 8, &qnan, 8);
}

int main(void) {
    load_single(1.0f, 2.0f);
    ROW("addss", "addss %%xmm1,%%xmm0");
    ROW("addps", "addps %%xmm1,%%xmm0");
    load_single(3.0f, 1.0f);
    ROW("subss", "subss %%xmm1,%%xmm0");
    ROW("subps", "subps %%xmm1,%%xmm0");
    load_single(2.0f, 3.0f);
    ROW("mulss", "mulss %%xmm1,%%xmm0");
    ROW("mulps", "mulps %%xmm1,%%xmm0");
    load_single(4.0f, 2.0f);
    ROW("divss", "divss %%xmm1,%%xmm0");
    ROW("divps", "divps %%xmm1,%%xmm0");
    load_single(4.0f, 4.0f);
    ROW("sqrtss", "sqrtss %%xmm1,%%xmm0");
    ROW("sqrtps", "sqrtps %%xmm1,%%xmm0");
    load_single(1.0f, 2.0f);
    ROW("cmpltss", "cmpltss %%xmm1,%%xmm0");
    ROW("cmpltps", "cmpltps %%xmm1,%%xmm0");

    load_double(1.0, 2.0);
    ROW("addsd", "addsd %%xmm1,%%xmm0");
    ROW("addpd", "addpd %%xmm1,%%xmm0");
    load_double(2.0, 3.0);
    ROW("mulsd", "mulsd %%xmm1,%%xmm0");
    ROW("mulpd", "mulpd %%xmm1,%%xmm0");
    load_double(4.0, 2.0);
    ROW("divsd", "divsd %%xmm1,%%xmm0");
    ROW("divpd", "divpd %%xmm1,%%xmm0");
    load_double(4.0, 4.0);
    ROW("sqrtsd", "sqrtsd %%xmm1,%%xmm0");
    ROW("sqrtpd", "sqrtpd %%xmm1,%%xmm0");
    load_double(1.0, 2.0);
    ROW("cmpltsd", "cmpltsd %%xmm1,%%xmm0");
    ROW("cmpltpd", "cmpltpd %%xmm1,%%xmm0");

    // 44 F6 C0 B1 = REX.R TEST al, 0xb1 -- REX.R alone, so the r/m operand is still AL. AL = 0x0f and
    // 0x0f & 0xb1 = 0x01, so ZF = 0; `marker` proves execution resumed at the byte AFTER the immediate
    // rather than inside it.
    uint64_t zf = 0, marker = 0;
    __asm__ volatile("mov $0x0f,%%eax\n\t"
                     ".byte 0x44,0xf6,0xc0,0xb1\n\t"
                     "mov $0x5a5a,%1\n\t"
                     "setz %b0"
                     : "=&r"(zf), "=r"(marker)::"rax", "cc");
    printf("test8      zf=%d marker=%llx\n", (int)(zf & 1), (unsigned long long)marker);
    // 66 44 F7 C3 CC B0 = 66 REX.R TEST bx, 0xb0cc, whose 16-bit immediate is the byte pair the buggy decode
    // dropped. 0x1234 & 0xb0cc = 0x0004, so ZF = 0.
    uint64_t zf16 = 0, marker16 = 0;
    __asm__ volatile("mov $0x1234,%%ebx\n\t"
                     ".byte 0x66,0x44,0xf7,0xc3,0xcc,0xb0\n\t"
                     "mov $0x5a5a,%1\n\t"
                     "setz %b0"
                     : "=&r"(zf16), "=r"(marker16)::"rbx", "cc");
    printf("test16     zf=%d marker=%llx\n", (int)(zf16 & 1), (unsigned long long)marker16);
    return 0;
}
