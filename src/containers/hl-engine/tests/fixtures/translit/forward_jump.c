#include <stdio.h>

// Page alignment makes the admission condition a fixture invariant instead of a linker-layout accident.
// The skipped UD2 is the corrupt-target clamp: a wrong fall-through or displacement executes it loudly.
__attribute__((naked, noinline, aligned(4096))) static long forward_jump(void) {
    __asm__ volatile("mov $1, %eax\n"
                     "stc\n"
                     "jmp 1f\n"
                     "ud2\n"
                     "1: clc\n"
                     "adc $41, %eax\n"
                     "ret\n");
}

int main(void) {
    printf("%ld\n", forward_jump());
    return 0;
}
