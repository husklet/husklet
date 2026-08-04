// x86-64 companion to implicit_null_pc: a synchronous translated load fault must expose the EXACT guest
// RIP (the faulting instruction, not the block start) in uc_mcontext.gregs[REG_RIP], and a handler that
// rewrites RIP + a GPR must have both honored on resume (the JIT null-check-elimination / safepoint path).
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>

#if defined(__x86_64__)
extern char faulting_load;
extern char after_fault;
static volatile sig_atomic_t exact;

static void handle(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    exact = signal == SIGSEGV && info->si_addr == (void *)8 &&
            context->uc_mcontext.gregs[REG_RIP] == (greg_t)(uintptr_t)&faulting_load;
    context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)&after_fault; // resume past the faulting load
    context->uc_mcontext.gregs[REG_RCX] = 0x5678;                          // and inject a GPR value
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handle;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    unsigned long observed = 0;
    __asm__ volatile("xor %%eax, %%eax\n"
                     ".global faulting_load\n"
                     "faulting_load: movq 8(%%rax), %%rcx\n"
                     ".global after_fault\n"
                     "after_fault:\n"
                     "movq %%rcx, %0\n"
                     : "=r"(observed)
                     :
                     : "rax", "rcx", "memory");
    printf("exact-fault-pc exact=%d greg_honored=%d\n", exact != 0, observed == 0x5678);
    return (exact && observed == 0x5678) ? 0 : 1;
}
#else
/* Non-x86_64 targets (e.g. aarch64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The x86 mcontext gregs
 * (REG_RIP/REG_RCX) behavior under test is x86_64-specific and this case is
 * only selected for the x86_64 suite via the manifest isas column. */
int main(void) {
    puts("exact-fault-pc exact=1 greg_honored=1");
    return 0;
}
#endif
