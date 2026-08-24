#include <stdio.h>

// The two-byte JCC begins at the last byte of its page, so its architectural fall-through belongs to
// the next page and must remain a dispatcher boundary. The long NOP prefix makes that placement a link-
// independent invariant; this fixture runs only once per arm.
__attribute__((naked, noinline, aligned(4096))) static long boundary(long take) {
    __asm__ volatile(".fill 4093, 1, 0x90\n"
                     "test %edi, %edi\n"
                     "jnz 1f\n"
                     "mov $5, %eax\n"
                     "ret\n"
                     "1: mov $6, %eax\n"
                     "ret\n");
}

// Here the JCC itself ends on the original page, but its five-byte fall-through instruction begins at
// the final byte and straddles the next page. Merely checking `next` is therefore insufficient.
__attribute__((naked, noinline, aligned(4096))) static long straddling_successor(long take) {
    __asm__ volatile(".fill 4091, 1, 0x90\n"
                     "test %edi, %edi\n"
                     "jnz 1f\n"
                     "mov $5, %eax\n"
                     "ret\n"
                     "1: mov $6, %eax\n"
                     "ret\n");
}

int main(void) {
    printf("fall=%ld taken=%ld straddle=%ld\n", boundary(0), boundary(1), straddling_successor(0));
    return 0;
}
