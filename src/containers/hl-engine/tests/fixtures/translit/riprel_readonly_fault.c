#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

__attribute__((aligned(4096))) static const volatile unsigned char target_page[4096] = {[0] = 0xa5};
static volatile sig_atomic_t handled;
extern char fault_entry[], fault_instruction[], resume_instruction[];

static void fault_handler(int signal, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    greg_t *registers = context->uc_mcontext.gregs;
    uint64_t flags = (uint64_t)registers[REG_EFL];
    if (signal != SIGSEGV || info->si_addr != (void *)target_page ||
        (uint64_t)registers[REG_RIP] != (uint64_t)(uintptr_t)fault_instruction ||
        (uint64_t)registers[REG_R11] != UINT64_C(0x1122334455667788) ||
        (flags & UINT64_C(0xcd5)) != (UINT64_C(0x402) & UINT64_C(0xcd5)))
        _exit(91);
    handled = 1;
    registers[REG_RIP] = (greg_t)(uintptr_t)resume_instruction;
}

__attribute__((noinline, used)) static uint64_t fault_once(void) {
    uint64_t output;
    __asm__ volatile(
        ".globl fault_entry\n"
        "fault_entry: movabs $0x1122334455667788, %%r11\n"
        ".globl fault_instruction\n"
        "fault_instruction: cmpb $0xa5, target_page(%%rip)\n"
        ".globl resume_instruction\n"
        "resume_instruction: movq %%r11, %0\n"
        : "=r"(output)
        :
        : "r11", "cc", "memory");
    return output;
}

__attribute__((noinline)) static uint64_t arm_flags_and_fault_once(void) {
    uint64_t output;
    __asm__ volatile("pushq $0x402\n popfq\n call fault_once" : "=a"(output) : : "r11", "cc", "memory");
    return output;
}

int main(void) {
    struct sigaction action = {0};
    action.sa_flags = SA_SIGINFO;
    action.sa_sigaction = fault_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    uint64_t restored = arm_flags_and_fault_once();
    if (handled || restored != UINT64_C(0x1122334455667788)) return 6;
    if (mprotect((void *)target_page, 4096, PROT_NONE) != 0) return 2;
    restored = arm_flags_and_fault_once();
    if (!handled || restored != UINT64_C(0x1122334455667788)) return 3;
    if (mprotect((void *)target_page, 4096, PROT_READ) != 0) return 4;
    handled = 0;
    restored = arm_flags_and_fault_once();
    if (handled || restored != UINT64_C(0x1122334455667788)) return 5;
    printf("riprel fault recovery ok entry=%llx\n", (unsigned long long)(uintptr_t)fault_entry);
    return 0;
}
