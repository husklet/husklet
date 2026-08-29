#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

static volatile uintptr_t fault_pc, resume_pc, stack_pointer, saved_rsp;
static volatile unsigned faults, rip_ok, rsp_ok, regs_ok, flags_ok;

extern char ret_c3_pc, ret_c2_pc, resume_c3, resume_c2;

__attribute__((naked, noinline)) static void faulting_ret_c3(void) {
    __asm__ volatile(".global ret_c3_pc\nret_c3_pc: ret");
}

__attribute__((naked, noinline)) static void faulting_ret_c2(void) {
    __asm__ volatile(".global ret_c2_pc\nret_c2_pc: ret $16");
}

__attribute__((naked, noinline)) static void drive_c3(void) {
    __asm__ volatile(
        "mov %rsp,saved_rsp(%rip)\n"
        "push $0x8d7\n popfq\n"
        "mov $0x101,%rax\n mov $0x102,%rbx\n mov $0x103,%rcx\n mov $0x104,%rdx\n"
        "mov $0x105,%rsi\n mov $0x106,%rdi\n mov $0x107,%rbp\n"
        "mov $0x108,%r8\n mov $0x109,%r9\n mov $0x10a,%r10\n mov $0x10b,%r11\n"
        "mov $0x10c,%r12\n mov $0x10d,%r13\n mov $0x10e,%r14\n mov $0x10f,%r15\n"
        "mov stack_pointer(%rip),%rsp\n jmp faulting_ret_c3\n"
        ".global resume_c3\nresume_c3: ret");
}

__attribute__((naked, noinline)) static void drive_c2(void) {
    __asm__ volatile(
        "mov %rsp,saved_rsp(%rip)\n"
        "push $0x8d7\n popfq\n"
        "mov $0x101,%rax\n mov $0x102,%rbx\n mov $0x103,%rcx\n mov $0x104,%rdx\n"
        "mov $0x105,%rsi\n mov $0x106,%rdi\n mov $0x107,%rbp\n"
        "mov $0x108,%r8\n mov $0x109,%r9\n mov $0x10a,%r10\n mov $0x10b,%r11\n"
        "mov $0x10c,%r12\n mov $0x10d,%r13\n mov $0x10e,%r14\n mov $0x10f,%r15\n"
        "mov stack_pointer(%rip),%rsp\n jmp faulting_ret_c2\n"
        ".global resume_c2\nresume_c2: ret");
}

static void fault(int signal, siginfo_t *info, void *opaque) {
    (void)info;
    ucontext_t *uc = opaque;
    greg_t *g = uc->uc_mcontext.gregs;
    if (signal != SIGSEGV) _exit(90);
    faults++;
    rip_ok += (uintptr_t)g[REG_RIP] == fault_pc;
    rsp_ok += (uintptr_t)g[REG_RSP] == stack_pointer;
    regs_ok += (uint64_t)g[REG_RAX] == 0x101 && (uint64_t)g[REG_RBX] == 0x102 &&
               (uint64_t)g[REG_RCX] == 0x103 && (uint64_t)g[REG_RDX] == 0x104 &&
               (uint64_t)g[REG_RSI] == 0x105 && (uint64_t)g[REG_RDI] == 0x106 &&
               (uint64_t)g[REG_RBP] == 0x107 && (uint64_t)g[REG_R8] == 0x108 &&
               (uint64_t)g[REG_R9] == 0x109 && (uint64_t)g[REG_R10] == 0x10a &&
               (uint64_t)g[REG_R11] == 0x10b && (uint64_t)g[REG_R12] == 0x10c &&
               (uint64_t)g[REG_R13] == 0x10d && (uint64_t)g[REG_R14] == 0x10e &&
               (uint64_t)g[REG_R15] == 0x10f;
    flags_ok += ((uint64_t)g[REG_EFL] & UINT64_C(0x8d7)) == UINT64_C(0x8d7);
    g[REG_RSP] = (greg_t)saved_rsp;
    g[REG_RIP] = (greg_t)resume_pc;
}

int main(int argc, char **argv) {
    (void)argv;
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 2;
    stack_t alternate = {.ss_sp = malloc(SIGSTKSZ), .ss_size = SIGSTKSZ};
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_SIGINFO | SA_ONSTACK};
    sigemptyset(&action.sa_mask);
    if (alternate.ss_sp == NULL || sigaltstack(&alternate, NULL) != 0 || sigaction(SIGSEGV, &action, NULL) != 0)
        return 3;
    int c2 = argc > 1 && argv[1][0] == '2';
    int unmapped = argc > 2 && argv[2][0] == 'u';
    void *page_address = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page_address == MAP_FAILED) return 4;
    if ((unmapped ? munmap(page_address, (size_t)page)
                  : mprotect(page_address, (size_t)page, PROT_NONE)) != 0)
        return 5;
    stack_pointer = (uintptr_t)page_address + 128;
    fault_pc = (uintptr_t)(c2 ? &ret_c2_pc : &ret_c3_pc);
    resume_pc = (uintptr_t)(c2 ? &resume_c2 : &resume_c3);
    if (c2) drive_c2(); else drive_c3();
    printf("ret-stack c2=%d unmapped=%d faults=%u rip=%u rsp=%u regs=%u flags=%u\n",
           c2, unmapped, faults, rip_ok, rsp_ok, regs_ok, flags_ok);
    return faults == 1 && rip_ok == 1 && rsp_ok == 1 && regs_ok == 1 && flags_ok == 1 ? 0 : 6;
}
