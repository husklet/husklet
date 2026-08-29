#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>

static uint64_t target(void) { return UINT64_C(0x5a17c0de12345678); }

__attribute__((naked, noinline)) static uint64_t via_base(const void *cell) {
    __asm__("jmp *(%rdi)");
}
__attribute__((naked, noinline)) static uint64_t via_index(const void *cells) {
    __asm__("mov $1,%ecx\n\tjmp *8(%rdi,%rcx,8)");
}
__attribute__((naked, noinline)) static uint64_t via_rsp(void) {
    __asm__("lea 1f(%rip),%rax\n\tpush %rax\n\tjmp *(%rsp)\n\t"
            "1: add $8,%rsp\n\tmovabs $0x5a17c0de12345678,%rax\n\tret");
}
__attribute__((naked, noinline)) static uint64_t via_r12(void) {
    __asm__("push %r12\n\tlea 1f(%rip),%rax\n\tpush %rax\n\tmov %rsp,%r12\n\t"
            "jmp *(%r12)\n\t1: add $8,%rsp\n\tpop %r12\n\t"
            "movabs $0x5a17c0de12345678,%rax\n\tret");
}
__attribute__((naked, noinline)) static uint64_t via_rbp_disp(void) {
    __asm__("push %rbp\n\tlea 1f(%rip),%rax\n\tpush %rax\n\tlea -8(%rsp),%rbp\n\t"
            "jmp *8(%rbp)\n\t1: add $8,%rsp\n\tpop %rbp\n\t"
            "movabs $0x5a17c0de12345678,%rax\n\tret");
}
__attribute__((naked, noinline)) static uint64_t via_r13_disp(void) {
    __asm__("push %r13\n\tlea 1f(%rip),%rax\n\tpush %rax\n\tlea -128(%rsp),%r13\n\t"
            "jmp *128(%r13)\n\t1: add $8,%rsp\n\tpop %r13\n\t"
            "movabs $0x5a17c0de12345678,%rax\n\tret");
}
int main(void) {
    const uint64_t expected = UINT64_C(0x5a17c0de12345678);
    uint64_t cells[3] = {0, 0, (uint64_t)(uintptr_t)target};
    uint64_t direct = (uint64_t)(uintptr_t)target;
    uint64_t a = via_base(&direct);
    uint64_t b = via_index(cells);
    uint64_t d = via_rsp();
    uint64_t e = via_r12();
    uint64_t f = via_rbp_disp();
    uint64_t g = via_r13_disp();
    printf("base=%d index=%d rsp=%d r12=%d rbp=%d r13=%d\n",
           a == expected, b == expected, d == expected, e == expected, f == expected,
           g == expected);
    return a == expected && b == expected && d == expected && e == expected &&
                   f == expected && g == expected
               ? 0
               : 1;
}
