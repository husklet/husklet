#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

typedef uint64_t (*function)(uint64_t);

__attribute__((naked, noinline, visibility("hidden"))) uint64_t target(uint64_t value) {
    (void)value;
    __asm__ volatile("lea 7(%rdi,%rdi,2),%rax\n\tret");
}

__attribute__((visibility("hidden"))) function call_slot = target;
__attribute__((visibility("hidden"))) function jump_slot = target;
static volatile sig_atomic_t faults;
static volatile sig_atomic_t r11_preserved;
static volatile sig_atomic_t rip_preserved;
static volatile uintptr_t resume_pc;
static volatile uintptr_t fault_pc;

__asm__(".pushsection .rip_indirect_boundary,\"aw\",@progbits\n"
        ".balign 4096\n"
        ".space 4092\n"
        ".global boundary_slot\n"
        "boundary_slot:\n"
        ".quad target\n"
        ".space 4092\n"
        ".popsection\n");
extern function boundary_slot;
extern char faulting_call_pc;
extern char faulting_resume_pc;

__attribute__((naked, noinline)) static uint64_t valid_call(uint64_t value) {
    (void)value;
    __asm__ volatile("call *call_slot(%rip)\n\tret");
}

__attribute__((naked, noinline)) static uint64_t valid_jump(uint64_t value) {
    (void)value;
    __asm__ volatile("jmp *jump_slot(%rip)");
}

__attribute__((naked, noinline)) static void faulting_call(void) {
    __asm__ volatile("movabs $0x8877665544332211,%r11\n\t"
                     ".global faulting_call_pc\n"
                     "faulting_call_pc: call *boundary_slot(%rip)\n\t"
                     ".global faulting_resume_pc\n"
                     "faulting_resume_pc: ret");
}

static void fault(int signal, siginfo_t *info, void *context) {
    (void)info;
    ucontext_t *state = context;
    if (signal == SIGSEGV) {
        faults++;
        r11_preserved += (uint64_t)state->uc_mcontext.gregs[REG_R11] == UINT64_C(0x8877665544332211);
        rip_preserved += (uintptr_t)state->uc_mcontext.gregs[REG_RIP] == fault_pc;
        state->uc_mcontext.gregs[REG_RIP] = (greg_t)resume_pc;
    }
}

int main(void) {
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;

    uint64_t call = valid_call(11);
    uint64_t jump = valid_jump(13);
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t slot = (uintptr_t)&boundary_slot;
    if (page <= 0 || (slot & ((uintptr_t)page - 1)) != (uintptr_t)page - 4) return 3;
    uintptr_t second = (slot & ~((uintptr_t)page - 1)) + (uintptr_t)page;
    fault_pc = (uintptr_t)&faulting_call_pc;
    resume_pc = (uintptr_t)&faulting_resume_pc;
    if (mprotect((void *)second, (size_t)page, PROT_NONE) != 0) return 4;
    faulting_call();
    if (mprotect((void *)second, (size_t)page, PROT_READ | PROT_WRITE) != 0) return 5;

    printf("rip-indirect call=%llu jump=%llu faults=%d r11=%d rip=%d\n",
           (unsigned long long)call, (unsigned long long)jump, faults, r11_preserved, rip_preserved);
    return call == 40 && jump == 46 && faults == 1 && r11_preserved == 1 && rip_preserved == 1 ? 0 : 6;
}
