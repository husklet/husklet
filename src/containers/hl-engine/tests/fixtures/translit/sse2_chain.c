#include <stdint.h>
#include <stdio.h>

extern uint32_t sse2_chain(const unsigned char *, const unsigned char *);

__asm__(".text\n"
        ".global sse2_chain\n"
        ".type sse2_chain,@function\n"
        "sse2_chain:\n"
        "movdqu (%rsi), %xmm1\n"
        // The unsupported setup belongs to its own interpreter block.  This branch makes the target
        // PAND a real descriptor entry instead of letting the interpreter consume the entire function.
        "jmp .Lsse2_target\n"
        ".Lsse2_target:\n"
        // The six admitted encodings are deliberately contiguous.  Changing any middle opcode must
        // split the descriptor and the receipt assertion in translit_differential.rs must turn red.
        "movdqu (%rdi), %xmm0\n"
        "pand %xmm1, %xmm0\n"
        "movdqa %xmm0, %xmm2\n"
        "psrldq $4, %xmm2\n"
        "por %xmm0, %xmm2\n"
        "movd %xmm2, %eax\n"
        "ret\n"
        ".size sse2_chain,.-sse2_chain\n");

int main(void) {
    static const unsigned char a[16] __attribute__((aligned(16))) = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    static const unsigned char b[16] __attribute__((aligned(16))) = {
        0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0};
    uint32_t value = sse2_chain(a, b);
    printf("sse2=%08x\n", value);
    return value == UINT32_C(0x00070005) ? 0 : 1;
}
