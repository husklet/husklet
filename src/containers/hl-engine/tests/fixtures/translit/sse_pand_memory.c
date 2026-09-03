#include <stdint.h>
#include <stdio.h>

extern void pand_memory(const void *, const void *, void *);

__asm__(".text\n"
        ".global pand_memory\n.type pand_memory,@function\n"
        "pand_memory: movdqu (%rsi),%xmm0\n"
        "xor %eax,%eax\n"
        "jmp 1f\n"
        ".balign 4096\n"
        ".space 4093\n"
        "1: pand (%rdi,%rax),%xmm0\n"
        "movdqu %xmm0,(%rdx)\nret\n"
        ".size pand_memory,.-pand_memory\n");

static uint64_t half(const uint8_t value[16], unsigned offset) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 8; ++i) result |= (uint64_t)value[offset + i] << (i * 8u);
    return result;
}

int main(void) {
    uint8_t mask[16] __attribute__((aligned(16)));
    uint8_t initial[16] __attribute__((aligned(16))), result[16];
    for (unsigned i = 0; i < 16; ++i) {
        mask[i] = (uint8_t)(0xf0u ^ i);
        initial[i] = (uint8_t)(0xffu - i);
    }
    for (unsigned iteration = 0; iteration < 256; ++iteration) pand_memory(mask, initial, result);
    printf("pand-memory=%016llx:%016llx\n", (unsigned long long)half(result, 0),
           (unsigned long long)half(result, 8));
    return 0;
}
