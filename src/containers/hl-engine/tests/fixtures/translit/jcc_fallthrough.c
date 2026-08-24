#include <stdio.h>

// Alignment keeps the JCC and its fall-through on one source page. Both paths are observable: the
// false condition must continue inside the descriptor, while the true condition must still take the
// ordinary dispatcher side exit and land at the same target as the interpreter.
__attribute__((naked, noinline, aligned(4096))) static long conditional(long take) {
    __asm__ volatile("mov $40, %eax\n"
                     "test %rdi, %rdi\n"
                     "jnz 1f\n"
                     "add $1, %eax\n"
                     "1: add $1, %eax\n"
                     "ret\n");
}

int main(void) {
    printf("fall=%ld taken=%ld\n", conditional(0), conditional(1));
    return 0;
}
