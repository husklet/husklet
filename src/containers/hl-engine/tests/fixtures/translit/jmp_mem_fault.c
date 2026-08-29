#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

extern unsigned char fault_pc[], fault_resume[], fault_landed[];
static volatile uintptr_t expected_ea, expected_si, expected_rip;
static volatile sig_atomic_t delivered, mismatch;

__asm__(".text\n"
        ".global fault_probe,fault_pc,fault_resume,fault_landed\n"
        "fault_probe: push %rdi; push %rbx; push %rbp; push %r12; push %r13; push %r14; push %r15\n"
        "mov $0x1111,%rbx; mov $0x2222,%rcx; mov $0x3333,%rdx\n"
        "mov $0x5555,%rbp; mov $0x6666,%rsi; mov $0x7777,%rdi\n"
        "mov $0x8888,%r8; mov $0x9999,%r9; mov $0xaaaa,%r10; mov $0xbbbb,%r11\n"
        "mov $0xcccc,%r12; mov $0xdddd,%r13; mov $0xeeee,%r14; mov $0xffff,%r15\n"
        "pushq $0x246; popfq\n"
        "mov 48(%rsp),%rdi; call fault_entry\n"
        "pop %r15; pop %r14; pop %r13; pop %r12; pop %rbp; pop %rbx; add $8,%rsp; ret\n"
        "fault_entry: mov %rdi,%rax\n"
        "fault_pc: jmp *(%rax)\n"
        "fault_resume: xor %eax,%eax; ret\n"
        "fault_landed: mov $1,%eax; ret\n");
extern int fault_probe(const uint64_t *);

static void handler(int sig, siginfo_t *info, void *opaque) {
    ucontext_t *uc = opaque;
    greg_t *g = uc->uc_mcontext.gregs;
    if (sig != SIGSEGV || (uintptr_t)info->si_addr != expected_si) mismatch |= 1;
    if ((uintptr_t)g[REG_RIP] != expected_rip) mismatch |= 2;
    if ((uintptr_t)g[REG_RAX] != expected_ea || (uintptr_t)g[REG_RDI] != expected_ea) mismatch |= 4;
    if (g[REG_RBX] != 0x1111 || g[REG_RCX] != 0x2222 || g[REG_RDX] != 0x3333 ||
        g[REG_RBP] != 0x5555 || g[REG_RSI] != 0x6666 || g[REG_R8] != 0x8888 ||
        g[REG_R9] != 0x9999 || g[REG_R10] != 0xaaaa || g[REG_R11] != 0xbbbb ||
        g[REG_R12] != 0xcccc || g[REG_R13] != 0xdddd || g[REG_R14] != 0xeeee || g[REG_R15] != 0xffff)
        mismatch |= 8;
    if ((g[REG_EFL] & 0x8d5) != (0x246 & 0x8d5)) mismatch |= 16;
    delivered++;
    g[REG_RIP] = (greg_t)(uintptr_t)fault_resume;
}

static int fault_one(const uint64_t *operand, uintptr_t si, uintptr_t rip) {
    expected_ea = (uintptr_t)operand;
    expected_si = si;
    expected_rip = rip;
    return fault_probe(operand) == 0;
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = handler;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;

    uint64_t good = (uint64_t)(uintptr_t)fault_landed;
    for (int i = 0; i < 64; i++) if (fault_probe(&good) != 1) return 3;
    int unmapped = fault_one((const uint64_t *)8, 8, (uintptr_t)fault_pc);

    unsigned char *pair = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pair == MAP_FAILED) return 4;
    if (mprotect(pair, (size_t)page, PROT_NONE) != 0) return 5;
    int protected = fault_one((const uint64_t *)pair, (uintptr_t)pair, (uintptr_t)fault_pc);
    if (mprotect(pair, (size_t)page, PROT_READ | PROT_WRITE) != 0 ||
        mprotect(pair + page, (size_t)page, PROT_NONE) != 0) return 6;
    int split = fault_one((const uint64_t *)(pair + page - 4), (uintptr_t)(pair + page),
                          (uintptr_t)fault_pc);
    uint64_t noncanonical_target = UINT64_MAX;
    /* The engine's authoritative guest signal ABI reports a noncanonical fetch as si_addr=NULL
       while exposing the attempted target in RIP. */
    int noncanonical = fault_one(&noncanonical_target, 0, UINT64_MAX);
    printf("faults=%d mismatch=%d unmapped=%d protected=%d split=%d noncanonical=%d\n",
           (int)delivered, (int)mismatch, unmapped, protected, split, noncanonical);
    return delivered == 4 && !mismatch && unmapped && protected && split && noncanonical ? 0 : 7;
}
