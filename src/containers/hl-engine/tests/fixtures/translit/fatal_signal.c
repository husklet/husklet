#include <stdint.h>

int main(void) {
    __asm__ volatile("xor %%rax, %%rax\n\tmovl $1, (%%rax)" ::: "rax", "memory");
    return 0;
}
