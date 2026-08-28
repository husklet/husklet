#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

extern uint64_t mixed_state(const uint8_t *, uint8_t *, uint64_t *, uint64_t *);
extern uint32_t mixed_fault_sse_load(const uint8_t *, const uint8_t *);
extern uint32_t mixed_fault_normal(const uint8_t *, const uint8_t *);
extern uint32_t mixed_fault_sse_store(const uint8_t *, uint8_t *);
extern unsigned char mixed_sse_load_fault[], mixed_sse_load_resume[];
extern unsigned char mixed_normal_fault[], mixed_normal_resume[];
extern unsigned char mixed_sse_store_fault[], mixed_sse_store_resume[];

__asm__(".text\n"
        ".global mixed_state\n"
        ".type mixed_state,@function\n"
        "mixed_state: jmp .Lmixed_state\n"
        ".Lmixed_state:\n"
        "movabs $0x1122334455667788,%r10\n"
        "cmp %r10,%r10\n"
        "movdqu (%rdi),%xmm9\n"
        "lea 0x17(%r10),%r10\n"
        "pxor %xmm10,%xmm10\n"
        "lea 7(%r10),%r11\n"
        "movdqa %xmm9,%xmm10\n"
        "mov %r10,(%rdx)\n"
        "mov %r11,(%rcx)\n"
        "movdqu %xmm10,(%rsi)\n"
        "setz %al\n"
        "movzbq %al,%rax\n"
        "ret\n"
        ".size mixed_state,.-mixed_state\n");

__asm__(".text\n"
        ".global mixed_fault_sse_load,mixed_sse_load_fault,mixed_sse_load_resume\n"
        ".type mixed_fault_sse_load,@function\n"
        "mixed_fault_sse_load: jmp .Lmixed_fault_sse_load\n"
        ".Lmixed_fault_sse_load:\n"
        "movdqu (%rdi),%xmm9\n"
        "movabs $0x1020304050607080,%r10\n"
        "cmp %r10,%r10\n"
        "stc\n"
        "mixed_sse_load_fault: movdqu (%rsi),%xmm9\n"
        "mixed_sse_load_resume: movd %xmm9,%eax\n"
        "ret\n"
        ".size mixed_fault_sse_load,.-mixed_fault_sse_load\n"
        ".global mixed_fault_normal,mixed_normal_fault,mixed_normal_resume\n"
        ".type mixed_fault_normal,@function\n"
        "mixed_fault_normal: jmp .Lmixed_fault_normal\n"
        ".Lmixed_fault_normal:\n"
        "movdqu (%rdi),%xmm9\n"
        "movabs $0x8877665544332211,%r10\n"
        "pxor %xmm10,%xmm10\n"
        "mixed_normal_fault: mov (%rsi),%r10\n"
        "mixed_normal_resume: movd %xmm9,%eax\n"
        "ret\n"
        ".size mixed_fault_normal,.-mixed_fault_normal\n"
        ".global mixed_fault_sse_store,mixed_sse_store_fault,mixed_sse_store_resume\n"
        ".type mixed_fault_sse_store,@function\n"
        "mixed_fault_sse_store: jmp .Lmixed_fault_sse_store\n"
        ".Lmixed_fault_sse_store:\n"
        "movdqu (%rdi),%xmm9\n"
        "lea 7(%rdi),%r11\n"
        "mixed_sse_store_fault: movdqu %xmm9,(%rsi)\n"
        "mixed_sse_store_resume: movd %xmm9,%eax\n"
        "ret\n"
        ".size mixed_fault_sse_store,.-mixed_fault_sse_store\n");

static volatile sig_atomic_t fault_mask, register_checks;

static void fault(int signal, siginfo_t *info, void *opaque) {
    (void)info;
    ucontext_t *context = opaque;
    uintptr_t rip = (uintptr_t)context->uc_mcontext.gregs[REG_RIP];
    if (signal != SIGSEGV) _Exit(90);
    if (rip == (uintptr_t)mixed_sse_load_fault) {
        fault_mask |= 1;
        if ((uint64_t)context->uc_mcontext.gregs[REG_R10] == UINT64_C(0x1020304050607080) &&
            (context->uc_mcontext.gregs[REG_EFL] & 1) != 0)
            register_checks |= 1;
        context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)mixed_sse_load_resume;
    } else if (rip == (uintptr_t)mixed_normal_fault) {
        fault_mask |= 2;
        if ((uint64_t)context->uc_mcontext.gregs[REG_R10] == UINT64_C(0x8877665544332211))
            register_checks |= 2;
        context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)mixed_normal_resume;
    } else if (rip == (uintptr_t)mixed_sse_store_fault) {
        fault_mask |= 4;
        if ((uint64_t)context->uc_mcontext.gregs[REG_R11] == (uint64_t)context->uc_mcontext.gregs[REG_RDI] + 7)
            register_checks |= 4;
        context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)mixed_sse_store_resume;
    } else {
        _Exit(91);
    }
}

static int state_once(const uint8_t input[16]) {
    uint8_t output[16] = {0};
    uint64_t r10 = 0, r11 = 0;
    uint64_t zf = mixed_state(input, output, &r10, &r11);
    return zf == 1 && r10 == UINT64_C(0x112233445566779f) &&
           r11 == UINT64_C(0x11223344556677a6) &&
           __builtin_memcmp(input, output, 16) == 0;
}

int main(void) {
    static const uint8_t input[16] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    const uint8_t *bad = (const uint8_t *)(uintptr_t)1;
    uint32_t load = mixed_fault_sse_load(input, bad);
    uint32_t normal = mixed_fault_normal(input, bad);
    uint32_t store = mixed_fault_sse_store(input, (uint8_t *)bad);
    int state = state_once(input);
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) _Exit(state_once(input) ? 0 : 4);
    int status = 0;
    int fork_ok = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("mixed state=%d faults=%d registers=%d vectors=%08x/%08x/%08x fork=%d\n",
           state, (int)fault_mask, (int)register_checks, load, normal, store, fork_ok);
    return state && fault_mask == 7 && register_checks == 7 && load == UINT32_C(0x04030201) &&
                   normal == UINT32_C(0x04030201) && store == UINT32_C(0x04030201) && fork_ok
               ? 0
               : 5;
}
