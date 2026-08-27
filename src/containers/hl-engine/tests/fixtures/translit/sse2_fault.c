#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>

extern uint32_t sse2_alignment(const unsigned char *);
extern unsigned char sse2_fault_pc[], sse2_fault_resume[];
extern uint32_t sse2_aligned_pand(const unsigned char *, const unsigned char *);
extern uint64_t sse2_movq(const unsigned char *);
extern uint32_t sse2_shift0(const unsigned char *), sse2_shift16(const unsigned char *),
    sse2_shift255(const unsigned char *);
extern uint64_t sse2_high_alias_flags(const unsigned char *);

__asm__(".text\n"
        ".global sse2_alignment,sse2_fault_pc,sse2_fault_resume\n"
        "sse2_alignment: movdqu (%rdi),%xmm0; jmp sse2_fault_pc\n"
        "sse2_fault_pc: movdqa 1(%rdi),%xmm0\n"
        "sse2_fault_resume: movd %xmm0,%eax; ret\n"
        ".global sse2_aligned_pand\n"
        "sse2_aligned_pand: movdqu (%rsi),%xmm0; jmp 1f\n"
        "1: pand (%rdi),%xmm0; movd %xmm0,%eax; ret\n"
        ".global sse2_movq\n"
        "sse2_movq: movdqu (%rdi),%xmm3; jmp 2f\n"
        "2: movq %xmm3,%rax; ret\n"
        ".global sse2_shift0,sse2_shift16,sse2_shift255\n"
        "sse2_shift0: movdqu (%rdi),%xmm4; jmp 3f; 3: psrldq $0,%xmm4; movd %xmm4,%eax; ret\n"
        "sse2_shift16: movdqu (%rdi),%xmm4; jmp 4f; 4: psrldq $16,%xmm4; movd %xmm4,%eax; ret\n"
        "sse2_shift255: movdqu (%rdi),%xmm4; jmp 5f; 5: psrldq $255,%xmm4; movd %xmm4,%eax; ret\n");
__asm__(".text\n"
        ".global sse2_high_alias_flags\n"
        "sse2_high_alias_flags: movdqu (%rdi),%xmm9; cmp %edi,%edi; stc; jmp 6f\n"
        "6: pand %xmm9,%xmm9; por %xmm9,%xmm9; pushfq; pop %rax; ret\n");

static volatile sig_atomic_t delivered, exact_rip;

static void alignment(int signal, siginfo_t *info, void *opaque) {
    (void)info;
    ucontext_t *context = opaque;
    if (signal == SIGSEGV) {
        if (delivered) _exit(90);
        delivered = 1;
        exact_rip = (uintptr_t)context->uc_mcontext.gregs[REG_RIP] == (uintptr_t)sse2_fault_pc;
        context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)sse2_fault_resume;
    }
}

int main(void) {
    static const unsigned char input[32] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff};
    struct sigaction action = {.sa_sigaction = alignment, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    uint32_t after_fault = sse2_alignment(input);
    int preserved = after_fault == UINT32_C(0x04030201);
    uint32_t pand = sse2_aligned_pand(input, input);
    uint64_t wide = sse2_movq(input);
    uint32_t shift0 = sse2_shift0(input), shift16 = sse2_shift16(input), shift255 = sse2_shift255(input);
    uint64_t flags = sse2_high_alias_flags(input);
    int flags_ok = (flags & (UINT64_C(1) | (UINT64_C(1) << 6))) == (UINT64_C(1) | (UINT64_C(1) << 6));
    printf("sse2-fault delivered=%d rip=%d preserved=%d pand=%08x wide=%016llx shifts=%08x/%08x/%08x"
           " high-alias-flags=%d\n",
           delivered, exact_rip, preserved, pand, (unsigned long long)wide, shift0, shift16, shift255, flags_ok);
    return delivered == 1 && exact_rip == 1 && preserved && pand == UINT32_C(0x04030201) &&
                   wide == UINT64_C(0x0807060504030201) && shift0 == UINT32_C(0x04030201) && shift16 == 0 &&
                   shift255 == 0 && flags_ok
               ? 0
               : 3;
}
