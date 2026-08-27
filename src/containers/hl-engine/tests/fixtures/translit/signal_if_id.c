#define _GNU_SOURCE
#include <stddef.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

volatile sig_atomic_t delivered;
volatile uint64_t observed_frame[2], frame_id, handler_live_flags;
volatile sig_atomic_t apply_handler_live_flags;
extern unsigned char if_id_fault[], if_id_resume[];
extern uint64_t signal_if_id(void *, uint64_t);
extern void if_id_handler(int, siginfo_t *, void *);

_Static_assert(offsetof(ucontext_t, uc_mcontext.gregs[REG_RIP]) == 168, "Linux x86-64 RIP layout");
_Static_assert(offsetof(ucontext_t, uc_mcontext.gregs[REG_EFL]) == 176, "Linux x86-64 EFLAGS layout");

__asm__(".text\n"
        ".global signal_if_id,if_id_fault,if_id_resume,if_id_handler\n"
        "signal_if_id: push %rsi; popfq\n"
        "if_id_fault: mov (%rdi),%rax\n"
        "if_id_resume: pushfq; pop %rax; ret\n"
        "if_id_handler:\n"
        " mov delivered(%rip),%ecx\n"
        " cmp $2,%ecx; jae if_id_bad\n"
        " lea if_id_fault(%rip),%r8\n"
        " cmp %r8,168(%rdx); jne if_id_bad\n"
        " lea observed_frame(%rip),%r8\n"
        " mov 176(%rdx),%rax\n"
        " mov %rax,(%r8,%rcx,8)\n"
        " inc %ecx; mov %ecx,delivered(%rip)\n"
        " lea if_id_resume(%rip),%rax\n"
        " mov %rax,168(%rdx)\n"
        " andq $-2097665,176(%rdx)\n"
        " mov frame_id(%rip),%rax\n"
        " or %rax,176(%rdx)\n"
        " cmpl $0,apply_handler_live_flags(%rip); je if_id_return\n"
        " pushq handler_live_flags(%rip); popfq\n"
        "if_id_return: ret\n"
        "if_id_bad: mov $60,%eax; mov $90,%edi; syscall; ud2\n");

static uint64_t run_case(void *guard, uint64_t interrupted, uint64_t requested_id, int apply_live,
                         uint64_t handler_live) {
    frame_id = requested_id;
    apply_handler_live_flags = apply_live;
    handler_live_flags = handler_live;
    return signal_if_id(guard, interrupted);
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *guard = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (guard == MAP_FAILED) return 2;
    struct sigaction action = {.sa_sigaction = if_id_handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 2;

    // The frame asks to clear both IF and seeded ID. Linux ignores both edits on return.
    uint64_t first = run_case(guard, UINT64_C(0x200202), 0, 0, 0);
    // With ID initially clear, a live handler POPF sets it; the unchanged frame still asks for ID clear.
    uint64_t second = run_case(guard, UINT64_C(0x202), 0, 1, UINT64_C(0x200202));
    uint64_t frame0 = observed_frame[0] & UINT64_C(0x200200);
    uint64_t frame1 = observed_frame[1] & UINT64_C(0x200200);
    first &= UINT64_C(0x200200);
    second &= UINT64_C(0x200200);
    printf("if-id frame=%06llx/%06llx resumed=%06llx/%06llx\n", (unsigned long long)frame0,
           (unsigned long long)frame1, (unsigned long long)first, (unsigned long long)second);
    return delivered == 2 && frame0 == UINT64_C(0x200200) && frame1 == UINT64_C(0x000200) &&
                   first == UINT64_C(0x200200) && second == UINT64_C(0x200200)
               ? 0
               : 3;
}
