#include <stdint.h>
#include <string.h>

volatile int armed;
extern void exit_site(uint64_t number, int terminate);
extern void exit_continuation(void);

__asm__(".pushsection .text\n"
        ".globl exit_site\n"
        ".type exit_site,@function\n"
        "exit_site:\n"
        "test %esi,%esi\n"
        "je exit_continuation\n"
        "mov %rdi,%rax\n"
        "xor %edi,%edi\n"
        "syscall\n"
        ".globl exit_continuation\n"
        ".type exit_continuation,@function\n"
        "exit_continuation:\n"
        "cmpl $0,armed(%rip)\n"
        "je 1f\n"
        "mov $1,%eax\n"
        "mov $1,%edi\n"
        "lea 2f(%rip),%rsi\n"
        "mov $21,%edx\n"
        "syscall\n"
        "1: ret\n"
        "2: .ascii \"forbidden-after-exit\\n\"\n"
        ".popsection\n");

int main(int argc, char **argv) {
    uint64_t number = argc == 2 && strcmp(argv[1], "group") == 0 ? 231u : 60u;
    exit_continuation();
    armed = 1;
    exit_site(number, 1);
    __builtin_unreachable();
}
