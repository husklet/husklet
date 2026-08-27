#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <setjmp.h>
#include <ucontext.h>

typedef void (*invalid_fn)(const void *);
static sigjmp_buf invalid_pad;
static volatile sig_atomic_t invalid_signal;
static volatile sig_atomic_t resumed_faults;
static volatile sig_atomic_t resume_bytes;

static void invalid_handler(int signal) {
    invalid_signal = signal;
    siglongjmp(invalid_pad, 1);
}

static int expect_ill(invalid_fn function, const void *operand) {
    invalid_signal = 0;
    if (sigsetjmp(invalid_pad, 1) == 0) function(operand);
    return invalid_signal == SIGILL;
}

static void resume_handler(int signal, siginfo_t *information, void *opaque) {
    (void)information;
    if (signal != SIGSEGV || resume_bytes == 0) _Exit(90);
    ucontext_t *context = opaque;
    context->uc_mcontext.gregs[REG_RIP] += resume_bytes;
    resumed_faults++;
}

#define PSR(name, immediate)                                                                                          \
    __asm__(".text\n.globl " #name "\n.type " #name ",@function\n" #name ":\n"                          \
            "movdqu (%rdi),%xmm0\n.byte 0x66,0x0f,0x73,0xd8," #immediate "\n"                             \
            "movdqu %xmm0,(%rsi)\nret\n.size " #name ",.-" #name "\n");
PSR(psr_0, 0x00)
PSR(psr_15, 0x0f)
PSR(psr_16, 0x10)
PSR(psr_17, 0x11)
PSR(psr_255, 0xff)

extern void psr_0(const void *, void *);
extern void psr_15(const void *, void *);
extern void psr_16(const void *, void *);
extern void psr_17(const void *, void *);
extern void psr_255(const void *, void *);

__asm__(".text\n"
        ".globl high_register_sequence\n.type high_register_sequence,@function\nhigh_register_sequence:\n"
        "movdqu (%rdi),%xmm8\nmovdqu (%rsi),%xmm9\n"
        ".byte 0x66,0x45,0x0f,0xdb,0xc1\n" /* pand xmm8,xmm9 */
        ".byte 0x66,0x45,0x0f,0x6f,0xc0\n" /* movdqa xmm8,xmm8 */
        ".byte 0x66,0x41,0x0f,0x73,0xd8,0x01\n" /* psrldq xmm8,1; REX.R ignored */
        ".byte 0x66,0x45,0x0f,0xeb,0xc1\n" /* por xmm8,xmm9 */
        "movdqu %xmm8,(%rdx)\nret\n.size high_register_sequence,.-high_register_sequence\n"
        ".globl pand_memory\n.type pand_memory,@function\npand_memory:\n"
        "movdqu (%rdi),%xmm0\n.byte 0x66,0x0f,0xdb,0x06\nmovdqu %xmm0,(%rdx)\nret\n"
        ".size pand_memory,.-pand_memory\n"
        ".globl por_memory\n.type por_memory,@function\npor_memory:\n"
        "movdqu (%rdi),%xmm0\n.byte 0x66,0x0f,0xeb,0x06\nmovdqu %xmm0,(%rdx)\nret\n"
        ".size por_memory,.-por_memory\n"
        ".globl movdqa_register\n.type movdqa_register,@function\nmovdqa_register:\n"
        "movdqu (%rdi),%xmm9\n.byte 0x66,0x45,0x0f,0x6f,0xc1\nmovdqu %xmm8,(%rsi)\nret\n"
        ".size movdqa_register,.-movdqa_register\n"
        ".globl faulting_movdqa\n.type faulting_movdqa,@function\nfaulting_movdqa:\n"
        "movdqu (%rdi),%xmm8\n.byte 0x66,0x44,0x0f,0x6f,0x06\nmovdqu %xmm8,(%rdx)\nret\n"
        ".size faulting_movdqa,.-faulting_movdqa\n"
        ".globl faulting_pand\n.type faulting_pand,@function\nfaulting_pand:\n"
        "movdqu (%rdi),%xmm8\n.byte 0x66,0x44,0x0f,0xdb,0x06\nmovdqu %xmm8,(%rdx)\nret\n"
        ".size faulting_pand,.-faulting_pand\n"
        ".globl faulting_por\n.type faulting_por,@function\nfaulting_por:\n"
        "movdqu (%rdi),%xmm8\n.byte 0x66,0x44,0x0f,0xeb,0x06\nmovdqu %xmm8,(%rdx)\nret\n"
        ".size faulting_por,.-faulting_por\n"
        ".globl movd32\n.type movd32,@function\nmovd32:\n"
        "movdqu (%rdi),%xmm8\nmov $-1,%r8\n.byte 0x66,0x45,0x0f,0x7e,0xc0\nmov %r8,(%rsi)\nret\n"
        ".size movd32,.-movd32\n"
        ".globl movd64\n.type movd64,@function\nmovd64:\n"
        "movdqu (%rdi),%xmm8\n.byte 0x66,0x4d,0x0f,0x7e,0xc0\nmov %r8,(%rsi)\nret\n"
        ".size movd64,.-movd64\n"
        ".globl movd_memory32\n.type movd_memory32,@function\nmovd_memory32:\n"
        "movdqu (%rdi),%xmm0\n.byte 0x66,0x0f,0x7e,0x06\nret\n.size movd_memory32,.-movd_memory32\n"
        ".globl flags_unchanged\n.type flags_unchanged,@function\nflags_unchanged:\n"
        "movdqu (%rdi),%xmm0\nmovdqu (%rsi),%xmm1\n"
        "push %rdx\npopfq\n"
        ".byte 0x66,0x0f,0xdb,0xc1\n.byte 0x66,0x0f,0x6f,0xc0\n"
        ".byte 0x66,0x0f,0x73,0xd8,0x01\n.byte 0x66,0x0f,0xeb,0xc1\n"
        ".byte 0x66,0x0f,0x7e,0xc0\npushfq\npop %rax\ncld\nret\n"
        ".size flags_unchanged,.-flags_unchanged\n"
        ".globl invalid_lock\n.type invalid_lock,@function\ninvalid_lock:\n"
        ".byte 0xf0,0x66,0x0f,0xdb,0xc0\nret\n.size invalid_lock,.-invalid_lock\n"
        ".globl invalid_f2\n.type invalid_f2,@function\ninvalid_f2:\n"
        ".byte 0x66,0xf2,0x0f,0xdb,0xc0\nret\n.size invalid_f2,.-invalid_f2\n"
        ".globl invalid_f3\n.type invalid_f3,@function\ninvalid_f3:\n"
        ".byte 0x66,0xf3,0x0f,0xdb,0xc0\nret\n.size invalid_f3,.-invalid_f3\n"
        ".globl invalid_psr_memory\n.type invalid_psr_memory,@function\ninvalid_psr_memory:\n"
        ".byte 0x66,0x0f,0x73,0x1f,0x01\nret\n.size invalid_psr_memory,.-invalid_psr_memory\n");

extern void high_register_sequence(const void *, const void *, void *);
extern void pand_memory(const void *, const void *, void *);
extern void por_memory(const void *, const void *, void *);
extern void movdqa_register(const void *, void *);
extern void faulting_movdqa(const void *, const void *, void *);
extern void faulting_pand(const void *, const void *, void *);
extern void faulting_por(const void *, const void *, void *);
extern void movd32(const void *, uint64_t *);
extern void movd64(const void *, uint64_t *);
extern void movd_memory32(const void *, void *);
extern uint64_t flags_unchanged(const void *, const void *, uint64_t);
extern void invalid_lock(const void *);
extern void invalid_f2(const void *);
extern void invalid_f3(const void *);
extern void invalid_psr_memory(const void *);

static int same(const uint8_t *a, const uint8_t *b) { return memcmp(a, b, 16) == 0; }

int main(void) {
    struct sigaction action = {0};
    action.sa_handler = invalid_handler;
    sigemptyset(&action.sa_mask);
    sigaction(SIGILL, &action, NULL);
    struct sigaction fault_action = {0};
    fault_action.sa_flags = SA_SIGINFO;
    fault_action.sa_sigaction = resume_handler;
    sigemptyset(&fault_action.sa_mask);
    sigaction(SIGSEGV, &fault_action, NULL);

    uint8_t a[32] __attribute__((aligned(16))) = {
        0xff, 0x10, 0x33, 0x55, 0x77, 0x99, 0xbb, 0xdd,
        0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01};
    uint8_t b[32] __attribute__((aligned(16))) = {
        0x0f, 0xf0, 0x3c, 0xaa, 0x07, 0x90, 0x4b, 0x22,
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe};
    uint8_t out[16], expected[16];

    high_register_sequence(a, b, out);
    for (int i = 0; i < 16; i++)
        expected[i] = (uint8_t)((i == 15 ? 0 : (a[i + 1] & b[i + 1])) | b[i]);
    if (!same(out, expected)) return 1;

    pand_memory(a, b, out);
    for (int i = 0; i < 16; i++) expected[i] = a[i] & b[i];
    if (!same(out, expected)) return 2;
    por_memory(a, b, out);
    for (int i = 0; i < 16; i++) expected[i] = a[i] | b[i];
    if (!same(out, expected)) return 3;
    movdqa_register(a, out);
    if (!same(out, a)) return 4;
    void (*faults[])(const void *, const void *, void *) = {faulting_movdqa, faulting_pand, faulting_por};
    for (unsigned i = 0; i < 3; i++) {
        memset(out, 0, sizeof out);
        resume_bytes = 5;
        sig_atomic_t before = resumed_faults;
        faults[i](a, b + 1, out);
        resume_bytes = 0;
        if (resumed_faults != before + 1 || !same(out, a)) return 40 + (int)i;
    }
    memset(out, 0, sizeof out);
    resume_bytes = 5;
    sig_atomic_t before = resumed_faults;
    faulting_movdqa(a, (const void *)(uintptr_t)1, out);
    resume_bytes = 0;
    if (resumed_faults != before + 1 || !same(out, a)) return 43;

    void (*shifts[])(const void *, void *) = {psr_0, psr_15, psr_16, psr_17, psr_255};
    const unsigned counts[] = {0, 15, 16, 17, 255};
    for (unsigned n = 0; n < 5; n++) {
        shifts[n](a, out);
        memset(expected, 0, 16);
        if (counts[n] < 16) memcpy(expected, a + counts[n], 16 - counts[n]);
        if (!same(out, expected)) return 5 + (int)n;
    }

    uint64_t value = 0;
    movd32(a, &value);
    if (value != UINT64_C(0x00000000553310ff)) return 10;
    movd64(a, &value);
    if (value != UINT64_C(0xddbb9977553310ff)) return 11;
    memset(out, 0xa5, sizeof out);
    movd_memory32(a, out + 1);
    if (memcmp(out + 1, a, 4) != 0 || out[0] != 0xa5 || out[5] != 0xa5) return 12;

    if ((flags_unchanged(a, b, UINT64_C(0xcd7)) & UINT64_C(0xcd5)) != UINT64_C(0xcd5)) return 13;
    if ((flags_unchanged(a, b, UINT64_C(0x002)) & UINT64_C(0xcd5)) != 0) return 13;
    if (!expect_ill(invalid_lock, a) || !expect_ill(invalid_f2, a) || !expect_ill(invalid_f3, a) ||
        !expect_ill(invalid_psr_memory, a))
        return 14;

    puts("sse2-exact-ok");
    return 0;
}
