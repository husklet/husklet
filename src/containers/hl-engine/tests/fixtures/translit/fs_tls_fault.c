#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

unsigned long original_fs;
static volatile sig_atomic_t delivered, exact_rip, old_rax;
extern unsigned char fs_fault_pc[], fs_fault_resume[];
extern uint64_t fs_fault_load(void *);
extern void fs_fault_handler(int, siginfo_t *, void *);

__asm__(".text\n"
        ".global fs_fault_load,fs_fault_pc,fs_fault_resume\n"
        "fs_fault_load: push %r12; mov %rdi,%r12\n"
        " mov $0x1003,%edi; lea original_fs(%rip),%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 9f\n"
        " mov $0x1002,%edi; mov %r12,%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 9f\n"
        " mov $0x8877665544332211,%rax; jmp fs_fault_pc\n"
        "fs_fault_pc: .byte 0x64,0x48,0x8b,0x04,0x25,0x20,0,0,0\n"
        "fs_fault_resume: pop %r12; ret\n"
        "9: mov $-1,%rax; pop %r12; ret\n"
        ".global fs_fault_handler\n"
        "fs_fault_handler: push %r12; push %r13; push %r14\n"
        " mov %rdi,%r12; mov %rsi,%r13; mov %rdx,%r14\n"
        " mov $0x1002,%edi; mov original_fs(%rip),%rsi; mov $158,%eax; syscall\n"
        " mov %r12,%rdi; mov %r13,%rsi; mov %r14,%rdx\n"
        " pop %r14; pop %r13; pop %r12; jmp fs_fault_handler_c\n");

void fs_fault_handler_c(int signal, siginfo_t *info, void *opaque) {
    (void)info;
    ucontext_t *context = opaque;
    delivered = signal == SIGSEGV;
    exact_rip = context->uc_mcontext.gregs[REG_RIP] == (greg_t)(uintptr_t)fs_fault_pc;
    old_rax = context->uc_mcontext.gregs[REG_RAX] == (greg_t)UINT64_C(0x8877665544332211);
    context->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)fs_fault_resume;
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *guard = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (guard == MAP_FAILED) return 2;
    struct sigaction action = {.sa_sigaction = fs_fault_handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;
    uint64_t result = fs_fault_load((unsigned char *)guard - 0x20);
    printf("fs-fault delivered=%d rip=%d old=%d result=%016llx\n", delivered, exact_rip, old_rax,
           (unsigned long long)result);
    return delivered && exact_rip && old_rax && result == UINT64_C(0x8877665544332211) ? 0 : 3;
}
