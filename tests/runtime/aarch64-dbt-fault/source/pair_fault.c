#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

extern char faulting_pair_load, after_pair_load, faulting_pair_store, after_pair_store;
static volatile sig_atomic_t phase;
static volatile sig_atomic_t exact_load, exact_store, load_registers_unchanged;
static unsigned char *fault_address;

static void handle(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    unsigned long long *r = (unsigned long long *)context->uc_mcontext.regs;
    if (phase == 0) {
        exact_load = signal == SIGSEGV && info->si_addr == fault_address &&
                     context->uc_mcontext.pc == (uintptr_t)&faulting_pair_load;
        load_registers_unchanged = r[2] == UINT64_C(0x1111) && r[3] == UINT64_C(0x2222);
        context->uc_mcontext.pc = (uintptr_t)&after_pair_load;
        phase = 1;
    } else {
        exact_store = signal == SIGSEGV && info->si_addr == fault_address &&
                      context->uc_mcontext.pc == (uintptr_t)&faulting_pair_store;
        context->uc_mcontext.pc = (uintptr_t)&after_pair_store;
        phase = 2;
    }
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handle;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    long page = sysconf(_SC_PAGESIZE);
    unsigned char *area = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED || mprotect(area + page, (size_t)page, PROT_NONE) != 0) return 2;
    uint64_t *last = (uint64_t *)(area + page - 8);
    *last = UINT64_C(0xaaaaaaaaaaaaaaaa);
    fault_address = area + page;

    __asm__ volatile("mov x0,%0\n"
                     "mov x2,#0x1111\n"
                     "mov x3,#0x2222\n"
                     ".global faulting_pair_load\n"
                     "faulting_pair_load: ldp x2,x3,[x0]\n"
                     ".global after_pair_load\n"
                     "after_pair_load:\n"
                     : : "r"(last) : "x0", "x2", "x3", "memory");

    __asm__ volatile("mov x0,%0\n"
                     "mov x2,#0x3333\n"
                     "mov x3,#0x4444\n"
                     ".global faulting_pair_store\n"
                     "faulting_pair_store: stp x2,x3,[x0]\n"
                     ".global after_pair_store\n"
                     "after_pair_store:\n"
                     : : "r"(last) : "x0", "x2", "x3", "memory");

    int first_store_visible = *last == UINT64_C(0x3333);
    printf("pair-fault load=%d regs=%d store=%d partial=%d\n", exact_load != 0,
           load_registers_unchanged != 0, exact_store != 0, first_store_visible);
    return exact_load && load_registers_unchanged && exact_store && first_store_visible && phase == 2 ? 0 : 1;
}
