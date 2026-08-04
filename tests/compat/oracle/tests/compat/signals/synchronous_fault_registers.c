// A synchronous fault (SIGSEGV) on a FOLDED memory instruction must deliver the guest's ACTUAL general
// registers to the handler, not the JIT's folded scratch temporaries. Under guest_base (static non-PIE)
// the translator folds `ldr x1,[x0,#8]` into a sequence that borrows three non-stolen host GPRs (here
// x2,x3,x4) as address-computation scratch, spilling their live guest values to cpu->mscratch. If the
// fault-time capture does not restore those slots, the handler sees STALE x2/x3/x4 (translator temporaries
// -- a biased address and two intermediates) instead of the guest sentinels. This checks they survive.
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
static volatile sig_atomic_t regs_ok;

static void handle(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    unsigned long long *r = (unsigned long long *)context->uc_mcontext.regs;
    exact = signal == SIGSEGV && info->si_addr == (void *)8 &&
            context->uc_mcontext.pc == (uintptr_t)&faulting_load;
    // x0 = bad base (0), x2/x3/x4 = live guest sentinels folded into scratch. All must be exact.
    regs_ok = r[0] == 0 && r[2] == 0x1111 && r[3] == 0x2222 && r[4] == 0x3333;
    context->uc_mcontext.pc = (uintptr_t)&after_fault; // resume past the faulting load
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handle;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    __asm__ volatile("mov x0, xzr\n"
                     "mov x2, #0x1111\n"
                     "mov x3, #0x2222\n"
                     "mov x4, #0x3333\n"
                     ".global faulting_load\n"
                     "faulting_load: ldr x1, [x0, #8]\n"
                     ".global after_fault\n"
                     "after_fault:\n"
                     :
                     :
                     : "x0", "x1", "x2", "x3", "x4", "memory");
    printf("folded-fault pc=%d regs=%d\n", exact != 0, regs_ok != 0);
    return (exact && regs_ok) ? 0 : 1;
}
#else
/* Non-aarch64 targets (e.g. x86_64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The folded-scratch register
 * reconstruction under test is aarch64-specific (the x86 fold uses a different
 * mechanism) and this case is only selected for the aarch64 suite via the
 * manifest isas column. */
int main(void) {
    puts("folded-fault pc=1 regs=1");
    return 0;
}
#endif
