#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

static const uint64_t sentinel = UINT64_C(0x8877665544332211);
__attribute__((aligned(4096), section(".faultpage"))) static volatile unsigned char fault_page[4096];
static volatile sig_atomic_t delivered;
static volatile uint64_t observed;
static volatile uintptr_t resume_pc;

__attribute__((naked, noinline)) static uint64_t faulting(void) {
    __asm__ volatile("lea 1f(%rip),%r11\n\t"
                     "mov %r11,resume_pc(%rip)\n\t"
                     "movabs $0x8877665544332211,%r10\n\t"
                     "mov fault_page(%rip),%r10\n\t"
                     "1: ret");
}

static void fault(int signal, siginfo_t *info, void *context) {
    (void)info;
    ucontext_t *state = context;
    if (signal == SIGSEGV) {
        delivered = 1;
        observed = (uint64_t)state->uc_mcontext.gregs[REG_R10];
        state->uc_mcontext.gregs[REG_RIP] = (greg_t)resume_pc;
    }
}

int main(void) {
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t address = (uintptr_t)fault_page;
    if (page <= 0 || mprotect((void *)(address & ~((uintptr_t)page - 1)), (size_t)page, PROT_NONE) != 0) return 3;
    (void)faulting();
    printf("displaced-fault delivered=%d preserved=%d\n", delivered, observed == sentinel);
    return delivered == 1 && observed == sentinel ? 0 : 4;
}
