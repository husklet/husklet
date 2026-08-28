#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
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

int main(int argc, char **argv) {
    static const uint8_t input[16] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    int ready[2];
    if (pipe(ready) != 0) return 2;
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) {
        close(ready[0]);
        uint8_t output[16] = {0};
        uint64_t value = child_mixed(input, output);
        if (value != UINT64_C(0x112233445566778f) || __builtin_memcmp(input, output, 16) != 0) _Exit(4);
        if (write(ready[1], "x", 1) != 1) _Exit(5);
        usleep(250000);
        ssize_t escaped = write(STDOUT_FILENO, "escaped-child\n", 14);
        (void)escaped;
        _Exit(0);
    }
    close(ready[1]);
    char byte = 0;
    int started = read(ready[0], &byte, 1) == 1 && byte == 'x';
    close(ready[0]);
    const char record[] = "mixed-child=1\n";
    if (started && write(STDOUT_FILENO, record, sizeof record - 1) != (ssize_t)(sizeof record - 1)) return 7;
    if (argc == 2 && __builtin_strcmp(argv[1], "fatal-root") == 0) {
        *(volatile int *)(uintptr_t)1 = 1;
        return 8;
    }
    /* Intentionally do not reap: the engine root's PID/birth barrier must settle this descendant before
       publishing the fork-shared census. Without that barrier the delayed line escapes after root exit. */
    return started ? 0 : 6;
}
