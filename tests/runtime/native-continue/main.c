#include <stdint.h>
#include <stdio.h>

extern void continue_loop(uint32_t remaining);

#if defined(__x86_64__)
__asm__(".text\n"
        ".globl continue_loop\n"
        ".type continue_loop,@function\n"
        "continue_loop:\n"
        "addps %xmm1,%xmm0\n"
        "subl $1,%edi\n"
        "jne continue_loop\n"
        "ret\n"
        ".size continue_loop,.-continue_loop\n");
#endif

int main(void) {
#if defined(__x86_64__)
    __asm__ volatile("pxor %%xmm0,%%xmm0\n"
                     "pxor %%xmm1,%%xmm1\n"
                     :
                     :
                     : "cc", "xmm0", "xmm1");
    continue_loop(300);
#endif

    puts("continue");
    return 0;
}
