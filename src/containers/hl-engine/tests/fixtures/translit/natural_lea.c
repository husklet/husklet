#include <stdint.h>
#include <stdio.h>

static const unsigned char marker;

__attribute__((naked, noinline)) static uintptr_t address(void) {
    __asm__ volatile("lea marker(%rip),%r10\n\t"
                     "mov %r10,%rax\n\t"
                     "ret");
}

int main(void) {
    uintptr_t actual = address();
    uintptr_t expected = (uintptr_t)&marker;
    printf("natural-lea exact=%d\n", actual == expected);
    return actual == expected ? 0 : 2;
}
