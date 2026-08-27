#define _GNU_SOURCE
#include <stddef.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

volatile sig_atomic_t delivered;
volatile uint64_t observed_frame[2], frame_update, handler_live_flags;
extern unsigned char pf_af_fault[], pf_af_resume[];
extern uint64_t signal_pf_af(void *, uint64_t);
extern void pf_af_handler(int, siginfo_t *, void *);

_Static_assert(offsetof(ucontext_t, uc_mcontext.gregs[REG_RIP]) == 168, "Linux x86-64 RIP layout");
_Static_assert(offsetof(ucontext_t, uc_mcontext.gregs[REG_EFL]) == 176, "Linux x86-64 EFLAGS layout");

__asm__(".text\n"
        ".global signal_pf_af,pf_af_fault,pf_af_resume,pf_af_handler\n"
        "signal_pf_af: push %rsi; popfq\n"
        "pf_af_fault: mov (%rdi),%rax\n"
        "pf_af_resume: pushfq; pop %rax; ret\n"
        "pf_af_handler:\n"
        " mov delivered(%rip),%ecx\n"
        " cmp $2,%ecx; jae pf_af_bad\n"
        " lea pf_af_fault(%rip),%r8\n"
        " cmp %r8,168(%rdx); jne pf_af_bad\n"
        " lea observed_frame(%rip),%r8\n"
        " mov 176(%rdx),%rax\n"
        " mov %rax,(%r8,%rcx,8)\n"
        " inc %ecx; mov %ecx,delivered(%rip)\n"
        " lea pf_af_resume(%rip),%rax\n"
        " mov %rax,168(%rdx)\n"
        " andq $-21,176(%rdx)\n"
        " mov frame_update(%rip),%rax\n"
        " or %rax,176(%rdx)\n"
        " pushq handler_live_flags(%rip); popfq; ret\n"
        "pf_af_bad: mov $60,%eax; mov $90,%edi; syscall; ud2\n");

static uint64_t run_case(void *guard, uint64_t interrupted, uint64_t frame, uint64_t handler_live) {
    frame_update = frame;
    handler_live_flags = handler_live;
    return signal_pf_af(guard, interrupted);
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *guard = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (guard == MAP_FAILED) return 2;
    struct sigaction action = {.sa_sigaction = pf_af_handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;

    // PF-only interrupted state; ask sigreturn for AF-only while the handler itself ends PF-only.
    uint64_t first = run_case(guard, UINT64_C(0x206), UINT64_C(0x10), UINT64_C(0x206));
    // AF-only interrupted state; ask sigreturn for PF-only while the handler itself ends AF-only.
    uint64_t second = run_case(guard, UINT64_C(0x212), UINT64_C(0x04), UINT64_C(0x212));
    uint64_t frame0 = observed_frame[0] & UINT64_C(0x14), frame1 = observed_frame[1] & UINT64_C(0x14);
    first &= UINT64_C(0x14);
    second &= UINT64_C(0x14);
    printf("pf-af frame=%02llx/%02llx resumed=%02llx/%02llx\n", (unsigned long long)frame0,
           (unsigned long long)frame1, (unsigned long long)first, (unsigned long long)second);
    return delivered == 2 && frame0 == UINT64_C(0x04) && frame1 == UINT64_C(0x10) &&
                   first == UINT64_C(0x10) && second == UINT64_C(0x04)
               ? 0
               : 3;
}
