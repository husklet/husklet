#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

extern uint64_t child_mixed(const uint8_t *, uint8_t *);

__asm__(".text\n"
        ".global child_mixed\n"
        ".type child_mixed,@function\n"
        "child_mixed: jmp .Lchild_mixed\n"
        ".Lchild_mixed:\n"
        "movabs $0x1122334455667788,%r10\n"
        "movdqu (%rdi),%xmm9\n"
        "lea 7(%r10),%r11\n"
        "pxor %xmm10,%xmm10\n"
        "movdqa %xmm9,%xmm10\n"
        "movdqu %xmm10,(%rsi)\n"
        "mov %r11,%rax\n"
        "ret\n"
        ".size child_mixed,.-child_mixed\n");

int main(void) {
    static const uint8_t input[16] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    pid_t child = fork();
    if (child < 0) return 2;
    if (child == 0) {
        uint8_t output[16] = {0};
        uint64_t value = child_mixed(input, output);
        _Exit(value == UINT64_C(0x112233445566778f) && __builtin_memcmp(input, output, 16) == 0 ? 0 : 3);
    }
    int status = 0;
    int passed = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("mixed-child=%d\n", passed);
    return passed ? 0 : 4;
}
