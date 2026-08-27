#include <stdint.h>
#include <stdio.h>

extern uint32_t sse2_displaced_memory(const unsigned char *);

__asm__(".text\n"
        ".global sse2_displaced_memory\n"
        "sse2_displaced_memory: jmp 1f\n"
        // Natural execution may lower this aligned register-address MOVDQA.  A displaced image must
        // refuse it because RDI is a low guest pointer while the bytes live at the high storage bias.
        "1: movdqa (%rdi),%xmm0; movd %xmm0,%eax; ret\n");

int main(void) {
    static const unsigned char input[16] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    uint32_t value = sse2_displaced_memory(input);
    printf("sse2-displaced=%08x\n", value);
    return value == UINT32_C(0x04030201) ? 0 : 1;
}
