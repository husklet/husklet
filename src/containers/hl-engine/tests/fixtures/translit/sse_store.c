#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>

static uint64_t fold(const uint8_t value[16]) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 16; i++) result = result * UINT64_C(257) ^ value[i];
    return result;
}

extern void stores(const uint8_t source[16], uint8_t aligned[16], uint8_t unaligned[40], uint64_t *flags);

/* Enter at the first SSE instruction.  A C prologue before the sequence would make the homogeneous
   translator stop at the SSE boundary while the interpreter consumes the remainder of that block. */
__asm__(".text\n"
        ".type stores,@function\n"
        "stores:\n"
        "movdqu (%rdi), %xmm9\n"
        "movups %xmm9, 1(%rdx)\n"
        "movaps %xmm9, (%rsi)\n"
        "movdqu %xmm9, 19(%rdx)\n"
        "pushfq\n"
        "pop %rax\n"
        "mov %rax, (%rcx)\n"
        "ret\n"
        ".size stores, .-stores\n");

__attribute__((noinline)) static uint64_t run_stores(const uint8_t source[16]) {
    _Alignas(16) uint8_t aligned[16] = {0};
    uint8_t unaligned[40] = {0};
    uint64_t after = 0;
    /* Seed CF/PF/AF/ZF/SF/OF and keep DF clear for the C ABI.  None of the four stores may alter them. */
    __asm__ volatile("pushfq; pop %%rax; and $-3286, %%rax; or $2261, %%rax; push %%rax; popfq"
                     ::: "rax", "cc", "memory");
    stores(source, aligned, unaligned, &after);
    return fold(aligned) ^ fold(unaligned + 1) ^ fold(unaligned + 19) ^ ((after ^ UINT64_C(0x8d5)) & UINT64_C(0xcd5));
}

int main(void) {
    _Alignas(16) uint8_t source[16];
    for (unsigned i = 0; i < sizeof source; i++) source[i] = (uint8_t)(7 * i + 3);
    printf("store=%016llx\n", (unsigned long long)run_stores(source));
    return 0;
}
