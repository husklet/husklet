#define _GNU_SOURCE
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

struct result {
    uint64_t tls_word;
    unsigned done;
};

static long legacy_clone(unsigned long flags, void *stack, int *parent_tid, int *child_tid,
                         void *tls, struct result *result) {
    register long a3 __asm__("r10") = (long)child_tid;
    register long a4 __asm__("r8") = (long)tls;
    register struct result *saved __asm__("r9") = result;
    long returned;
    __asm__ volatile("mov $56, %%rax\n\t"
                     "syscall\n\t"
                     "test %%rax, %%rax\n\t"
                     "jnz 1f\n\t"
                     "movq %%fs:0, %%rdi\n\t"
                     "movq %%rdi, (%%r9)\n\t"
                     "movl $1, 8(%%r9)\n\t"
                     "mov $60, %%rax\n\t"
                     "xor %%rdi, %%rdi\n\t"
                     "syscall\n\t"
                     "ud2\n"
                     "1:"
                     : "=a"(returned)
                     : "D"(flags), "S"(stack), "d"(parent_tid), "r"(a3), "r"(a4), "r"(saved)
                     : "rcx", "r11", "memory");
    return returned;
}

int main(void) {
    static const uint64_t marker = UINT64_C(0x73616d652d746c73);
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *stack = mmap(NULL, page * 2, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint64_t *tls = mmap(NULL, page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (stack == MAP_FAILED || tls == MAP_FAILED) return 2;
    *tls = marker;
    int child_tid_slot = 0x12345678;
    struct result result = {0};
    unsigned long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND |
                          CLONE_THREAD | CLONE_SYSVSEM | CLONE_SETTLS;
    long tid = legacy_clone(flags, stack + page * 2, NULL, &child_tid_slot, tls, &result);
    if (tid < 0) return 3;
    for (unsigned waits = 0; !__atomic_load_n(&result.done, __ATOMIC_ACQUIRE) && waits < 1000000; ++waits)
        sched_yield();
    int ok = result.done && result.tls_word == marker && child_tid_slot == 0x12345678;
    printf("legacy-clone-tls ok=%d\n", ok);
    return ok ? 0 : 4;
}
