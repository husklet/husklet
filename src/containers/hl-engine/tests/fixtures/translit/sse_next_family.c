#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>

__attribute__((visibility("hidden"))) const uint8_t rip_source[16] = {
    0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01};

static uint64_t fold(const uint8_t value[16]) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 16; i++) result = result * UINT64_C(257) ^ value[i];
    return result;
}

__attribute__((noinline)) static uint64_t unaligned_chain(const uint8_t *p) {
    uint8_t out[16];
    uint64_t before, after;
    __asm__ volatile("pushfq; pop %0\n\t"
                     "jmp 1f\n\t"
                     "1:\n\t"
                     "movdqu 1(%2), %%xmm0\n\t"
                     "movdqu 17(%2), %%xmm1\n\t"
                     "pandn %%xmm1, %%xmm0\n\t"
                     "movdqu %%xmm0, %3\n\t"
                     "pushfq; pop %1"
                     : "=r"(before), "=r"(after)
                     : "r"(p), "m"(out)
                     : "xmm0", "xmm1", "memory");
    return fold(out) ^ ((before ^ after) & UINT64_C(0xcd5));
}

__attribute__((noinline)) static uint64_t high_and_rip(const uint8_t *p) {
    uint8_t out[16];
    register const uint8_t *base __asm__("r8") = p;
    __asm__ volatile("movdqu 3(%%r8), %%xmm8\n\t"
                     "movdqu rip_source(%%rip), %%xmm9\n\t"
                     "pandn %%xmm9, %%xmm8\n\t"
                     "movdqu %%xmm8, %0"
                     : "=m"(out)
                     : "r"(base)
                     : "xmm8", "xmm9", "memory");
    return fold(out);
}

__attribute__((noinline)) static uint64_t aliases(const uint8_t *p) {
    uint8_t out[16];
    __asm__ volatile("movdqu (%1), %%xmm0\n\t"
                     "movdqu %%xmm0, %%xmm0\n\t"
                     "pandn %%xmm0, %%xmm0\n\t"
                     "movdqu %%xmm0, %0"
                     : "=m"(out)
                     : "r"(p)
                     : "xmm0", "memory");
    return fold(out);
}

int main(void) {
    uint8_t input[64];
    for (unsigned i = 0; i < sizeof input; i++) input[i] = (uint8_t)(3 * i + 1);
    uint64_t a = unaligned_chain(input), b = high_and_rip(input), c = aliases(input);
    printf("%016llx %016llx %016llx\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)c);
    return 0;
}
