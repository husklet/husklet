#include <stdint.h>
#include <stdio.h>

uint64_t observed_rsp;
uint64_t observed_rflags;

#if defined(__x86_64__)
__asm__(".text\n"
        ".globl run_iretq_context\n"
        ".type run_iretq_context,@function\n"
        "run_iretq_context:\n"
        "mov %rsp,%r11\n"
        "xor %eax,%eax\n"
        "mov %ss,%ax\n"
        "push %rax\n"
        "push %r11\n"
        "mov $0x200ec5,%eax\n"
        "push %rax\n"
        "xor %eax,%eax\n"
        "mov %cs,%ax\n"
        "push %rax\n"
        "lea 1f(%rip),%rax\n"
        "push %rax\n"
        "iretq\n"
        "1:\n"
        "mov %rsp,observed_rsp(%rip)\n"
        "pushfq\n"
        "pop observed_rflags(%rip)\n"
        "cld\n"
        "cmp %r11,%rsp\n"
        "sete %al\n"
        "movzbl %al,%eax\n"
        "ret\n"
        ".size run_iretq_context,.-run_iretq_context\n");

extern int run_iretq_context(void);
#endif

int main(void) {
#if defined(__x86_64__)
    uint64_t before;
    __asm__ volatile("mov %%rsp,%0" : "=r"(before));
    int restored = run_iretq_context();
    uint64_t mask = UINT64_C(0x200cc5);
    int stack_ok = restored && observed_rsp == before - 8;
    printf("iretq stack=%d flags=%llx\n", stack_ok, (unsigned long long)(observed_rflags & mask));
    return !(stack_ok && (observed_rflags & mask) == mask);
#else
    return 0;
#endif
}
