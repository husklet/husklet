#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

static sigjmp_buf escaped;
static volatile sig_atomic_t saw_fault, rip_exact, r11_exact;
extern const unsigned char jcc_fall_fault_load[];

static void fault_handler(int signal, siginfo_t *info, void *opaque) {
    (void)signal;
    (void)info;
    ucontext_t *context = opaque;
    saw_fault = 1;
    rip_exact = (uintptr_t)context->uc_mcontext.gregs[REG_RIP] == (uintptr_t)jcc_fall_fault_load;
    r11_exact = (uint64_t)context->uc_mcontext.gregs[REG_R11] == UINT64_C(0x1122334455667788);
    siglongjmp(escaped, 1);
}

__attribute__((naked, noinline, aligned(4096))) static long conditional_load(const void *address, long take) {
    __asm__ volatile("movabs $0x1122334455667788, %r11\n"
                     "test %rsi, %rsi\n"
                     "jnz 1f\n"
                     ".globl jcc_fall_fault_load\n"
                     "jcc_fall_fault_load:\n"
                     "mov (%rdi), %rax\n"
                     "ret\n"
                     "1: mov $7, %eax\n"
                     "ret\n");
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *unreadable = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (unreadable == MAP_FAILED) return 2;
    struct sigaction action = {.sa_sigaction = fault_handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0) return 3;
    if (sigsetjmp(escaped, 1) == 0) (void)conditional_load(unreadable, 0);
    long taken = conditional_load(unreadable, 1);
    printf("fault=%d rip=%d r11=%d taken=%ld\n", saw_fault, rip_exact, r11_exact, taken);
    return 0;
}
