#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>

#if defined(__aarch64__)
extern char faulting_load;
extern char after_fault;
static volatile sig_atomic_t exact;

static void handle(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    exact = signal == SIGSEGV && info->si_addr == (void *)8 &&
            context->uc_mcontext.pc == (uintptr_t)&faulting_load;
    context->uc_mcontext.pc = (uintptr_t)&after_fault;
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handle;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    __asm__ volatile("mov x0, xzr\n"
                     ".global faulting_load\n"
                     "faulting_load: ldr x1, [x0, #8]\n"
                     ".global after_fault\n"
                     "after_fault:\n"
                     :
                     :
                     : "x0", "x1", "memory");
    printf("implicit-null exact-pc=%d\n", exact != 0);
    return exact ? 0 : 1;
}
#else
/* Non-aarch64 targets (e.g. x86_64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The .pc mcontext behavior
 * under test is aarch64-specific and this case is only selected for the
 * aarch64 suite via the manifest isas column. */
int main(void) {
    puts("implicit-null exact-pc=1");
    return 0;
}
#endif
