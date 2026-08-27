#include <stdio.h>

// Alignment keeps the JCC and its fall-through on one source page. Both paths are observable: the
// false condition must continue inside the descriptor, while the true condition must still take the
// ordinary dispatcher side exit and land at the same target as the interpreter.
__attribute__((naked, noinline, aligned(4096))) static long conditional(long take) {
    __asm__ volatile("mov $40, %eax\n"
                     "test %rdi, %rdi\n"
                     "jnz 1f\n"
                     "sete %cl\n"
                     "movzbl %cl, %ecx\n"
                     "add %ecx, %eax\n"
                     "1: add $1, %eax\n"
                     "ret\n");
}

int main(void) {
    // The first taken edge materializes its interior target as a standalone descriptor. The reported call
    // below then proves the diagnostic's target-live/current-generation eligibility path is non-vacuous.
    long warm = conditional(1);
    printf("fall=%ld taken=%ld\n", conditional(0), conditional(1));
    return warm != 41;
}
