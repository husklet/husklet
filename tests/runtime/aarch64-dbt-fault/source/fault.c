#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>

extern char faulting_load;
extern char after_fault;
static volatile sig_atomic_t exact;
static volatile sig_atomic_t registers_ok;

static void handle(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    unsigned long long *r = (unsigned long long *)context->uc_mcontext.regs;
    exact = signal == SIGSEGV && info->si_addr == (void *)8 && context->uc_mcontext.pc == (uintptr_t)&faulting_load;
    registers_ok = r[0] == 0 && r[2] == 0x1111 && r[3] == 0x2222;
    context->uc_mcontext.pc = (uintptr_t)&after_fault;
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handle;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    __asm__ volatile("mov x0,xzr\n"
                     "mov x2,#0x1111\n"
                     "mov x3,#0x2222\n"
                     "b faulting_load\n"
                     ".balign 16\n"
                     ".global faulting_load\n"
                     "faulting_load: ldr x1,[x0,#8]\n"
                     ".global after_fault\n"
                     "after_fault:\n"
                     : : : "x0", "x1", "x2", "x3", "memory");
    printf("memory-fault pc=%d regs=%d\n", exact != 0, registers_ok != 0);
    return exact && registers_ok ? 0 : 1;
}
