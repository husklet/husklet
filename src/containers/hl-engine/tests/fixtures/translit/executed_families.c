#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static sigjmp_buf divide_fault;
static volatile sig_atomic_t divide_faults;

static void divide_handler(int signal) {
    (void)signal;
    ++divide_faults;
    siglongjmp(divide_fault, 1);
}

__attribute__((noinline)) static uint64_t jump_target(void) {
    return UINT64_C(0x123456789abcdef0);
}

__attribute__((used)) static void *jump_slot = jump_target;

// Load the address of the slot, then use FF /4 through that register. The RIP-relative FF /4 lowering is
// a different translated family; this exact register-derived memory form is the Jmem census target.
__attribute__((naked, noinline)) static uint64_t jump_memory(void) {
    __asm__ volatile("leaq jump_slot(%rip), %r11\n\t"
                     "jmp *(%r11)");
}

__attribute__((noinline)) static uint64_t unsigned32(void) {
    uint32_t quotient, remainder;
    __asm__ volatile("xorl %%edx, %%edx\n\t"
                     "movl $100, %%eax\n\t"
                     "divl %%ecx"
                     : "=a"(quotient), "=d"(remainder)
                     : "c"(7u)
                     : "cc");
    return (uint64_t)quotient * 100 + remainder;
}

__attribute__((noinline)) static uint64_t unsigned64(void) {
    uint64_t quotient, remainder;
    __asm__ volatile("xorq %%rdx, %%rdx\n\t"
                     "movq $1000, %%rax\n\t"
                     "divq %%rcx"
                     : "=a"(quotient), "=d"(remainder)
                     : "c"(13u)
                     : "cc");
    return quotient * 100 + remainder;
}

__attribute__((noinline)) static int64_t signed32(void) {
    int32_t quotient, remainder;
    __asm__ volatile("movl $-100, %%eax\n\t"
                     "cltd\n\t"
                     "idivl %%ecx"
                     : "=a"(quotient), "=d"(remainder)
                     : "c"(-7)
                     : "cc");
    return (int64_t)quotient * 100 + remainder;
}

__attribute__((noinline)) static int64_t signed64(void) {
    int64_t quotient, remainder;
    __asm__ volatile("movq $-1000, %%rax\n\t"
                     "cqto\n\t"
                     "idivq %%rcx"
                     : "=a"(quotient), "=d"(remainder)
                     : "c"(-13ll)
                     : "cc");
    return quotient * 100 + remainder;
}

__attribute__((noinline)) static void unsigned_de(void) {
    __asm__ volatile("xorl %%edx, %%edx\n\t"
                     "movl $1, %%eax\n\t"
                     "xorl %%ecx, %%ecx\n\t"
                     "divl %%ecx"
                     :
                     :
                     : "rax", "rcx", "rdx", "cc");
}

__attribute__((noinline)) static void signed_de(void) {
    __asm__ volatile("movl $-1, %%eax\n\t"
                     "cltd\n\t"
                     "xorl %%ecx, %%ecx\n\t"
                     "idivl %%ecx"
                     :
                     :
                     : "rax", "rcx", "rdx", "cc");
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = divide_handler;
    sigaction(SIGFPE, &action, NULL);
    if (sigsetjmp(divide_fault, 1) == 0) unsigned_de();
    if (sigsetjmp(divide_fault, 1) == 0) signed_de();
    printf("j=%llx u32=%llu u64=%llu i32=%lld i64=%lld de=%d\n",
           (unsigned long long)jump_memory(), (unsigned long long)unsigned32(),
           (unsigned long long)unsigned64(), (long long)signed32(), (long long)signed64(),
           (int)divide_faults);
    return divide_faults == 2 ? 0 : 1;
}
