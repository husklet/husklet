#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
static volatile sig_atomic_t rsp_preserved;
static volatile uintptr_t resume_pc;
static volatile uintptr_t fault_pc;
static volatile uintptr_t expected_rsp;
static volatile uintptr_t stack_pointer;
static volatile uintptr_t saved_rsp;
static volatile sig_atomic_t low_half_committed;
static volatile sig_atomic_t expect_low_half;

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
extern char stack_fault_pc;
extern char stack_resume_pc;
extern char stack_after_call;

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

__attribute__((naked, noinline, used, visibility("hidden"))) void faulting_stack_call(void) {
    __asm__ volatile("mov stack_pointer(%rip),%rsp\n\t"
                     "movabs $0x8877665544332211,%r11\n\t"
                     "jmp stack_call_entry\n\t"
                     ".global stack_fault_pc\n"
                     "stack_call_entry:\n"
                     "stack_fault_pc: call target\n\t"
                     ".global stack_after_call\n"
                     "stack_after_call:\n"
                     "ret");
}

__attribute__((naked, noinline)) static void drive_stack_fault(void) {
    __asm__ volatile("mov %rsp,saved_rsp(%rip)\n\t"
                     "call faulting_stack_call\n\t"
                     ".global stack_resume_pc\n"
                     "stack_resume_pc: ret");
}

static void fault(int signal, siginfo_t *info, void *context) {
    (void)info;
    ucontext_t *state = context;
    if (signal == SIGSEGV) {
        faults++;
        r11_preserved += (uint64_t)state->uc_mcontext.gregs[REG_R11] == UINT64_C(0x8877665544332211);
        rip_preserved += (uintptr_t)state->uc_mcontext.gregs[REG_RIP] == fault_pc;
        rsp_preserved += expected_rsp == 0 || (uintptr_t)state->uc_mcontext.gregs[REG_RSP] == expected_rsp;
        if (expected_rsp != 0 && expect_low_half) {
            uint32_t low = 0;
            memcpy(&low, (const void *)(expected_rsp - 8), sizeof low);
            low_half_committed += low == (uint32_t)(uintptr_t)&stack_after_call;
        }
        if (expected_rsp != 0) state->uc_mcontext.gregs[REG_RSP] = (greg_t)saved_rsp;
        state->uc_mcontext.gregs[REG_RIP] = (greg_t)resume_pc;
    }
}

int main(int argc, char **argv) {
    (void)argv;
    stack_t alternate = {.ss_sp = malloc(SIGSTKSZ), .ss_size = SIGSTKSZ};
    if (alternate.ss_sp == NULL || sigaltstack(&alternate, NULL) != 0) return 2;
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO | SA_ONSTACK};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;

    uint64_t call = valid_call(11);
    uint64_t jump = valid_jump(13);
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t slot = (uintptr_t)&boundary_slot;
    if (page <= 0 || (slot & ((uintptr_t)page - 1)) != (uintptr_t)page - 4) return 3;
    uintptr_t second = (slot & ~((uintptr_t)page - 1)) + (uintptr_t)page;
    if (argc == 1) {
        fault_pc = (uintptr_t)&faulting_call_pc;
        resume_pc = (uintptr_t)&faulting_resume_pc;
        if (mprotect((void *)second, (size_t)page, PROT_NONE) != 0) return 4;
        faulting_call();
        if (mprotect((void *)second, (size_t)page, PROT_READ | PROT_WRITE) != 0) return 5;
    }

    if (argc > 1) {
        void *stack = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stack == MAP_FAILED) return 6;
        int split = strcmp(argv[1], "split") == 0;
        expect_low_half = split;
        void *protected_page = split ? (uint8_t *)stack + page : stack;
        if (mprotect(protected_page, (size_t)page, PROT_NONE) != 0) return 6;
        // At page+2 the low 32-bit return-word store succeeds wholly in the
        // first page and the high store crosses into the protected second page.
        stack_pointer = (uintptr_t)stack + (uintptr_t)page + (split ? 2u : 0u);
        expected_rsp = stack_pointer;
        fault_pc = (uintptr_t)&stack_fault_pc;
        resume_pc = (uintptr_t)&stack_resume_pc;
        drive_stack_fault();
        if (munmap(stack, (size_t)page * 2) != 0) return 7;
    }

    int expected = 1;
    printf("rip-indirect call=%llu jump=%llu faults=%d r11=%d rip=%d rsp=%d low=%d\n",
           (unsigned long long)call, (unsigned long long)jump, faults, r11_preserved, rip_preserved, rsp_preserved,
           low_half_committed);
    return call == 40 && jump == 46 && faults == expected && r11_preserved == expected && rip_preserved == expected &&
                   rsp_preserved == expected
               ? 0 : 8;
}
