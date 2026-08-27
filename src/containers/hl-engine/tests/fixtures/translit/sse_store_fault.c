#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <ucontext.h>

extern void fault_sequence(const uint8_t source[16], uint8_t *target);
extern char fault_store, after_store;

static volatile sig_atomic_t faults;

static void fault(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    if (signal != SIGSEGV || (uintptr_t)context->uc_mcontext.gregs[REG_RIP] != (uintptr_t)&fault_store)
        _Exit(90);
    faults++;
    context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)&after_store;
    (void)info;
}

__asm__(".text\n"
        ".type fault_sequence,@function\n"
        "fault_sequence:\n"
        "movdqu (%rdi), %xmm9\n"
        ".globl fault_store\n"
        "fault_store:\n"
        "movaps %xmm9, 1(%rsi)\n"
        ".globl after_store\n"
        "after_store:\n"
        "ret\n"
        ".size fault_sequence, .-fault_sequence\n");

int main(void) {
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 91;
    _Alignas(16) uint8_t source[16] = {1, 2, 3, 4};
    _Alignas(16) uint8_t target[32] = {0};
    fault_sequence(source, target);
    printf("faults=%d unchanged=%d\n", (int)faults, target[1] == 0);
    return faults == 1 && target[1] == 0 ? 0 : 92;
}
