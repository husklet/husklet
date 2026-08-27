#include <stdint.h>
#include <stdio.h>

static uint64_t half(const uint8_t value[16], unsigned base) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 8; ++i) result |= (uint64_t)value[base + i] << (i * 8u);
    return result;
}

int main(void) {
    const uint8_t left[16] = {0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                              0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x10};
    const uint8_t right[16] = {0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01,
                               0xb9, 0x8a, 0x9b, 0xec, 0xfd, 0xce, 0xdf, 0x30};
    uint8_t distinct[16], high[16], alias[16];
    uint64_t before, after;
    __asm__ volatile("movdqu %5, %%xmm0\n\t"
                     "movdqu %6, %%xmm1\n\t"
                     "movdqu %5, %%xmm15\n\t"
                     "movdqu %6, %%xmm14\n\t"
                     "movdqu %5, %%xmm12\n\t"
                     "pushq $0xc97; popfq\n\t"
                     "pushfq; pop %0\n\t"
                     "jmp 1f\n\t"
                     "1:\n\t"
                     "movhlps %%xmm1, %%xmm0\n\t"
                     "movhlps %%xmm14, %%xmm15\n\t"
                     "movhlps %%xmm12, %%xmm12\n\t"
                     "pushfq; pop %1\n\t"
                     "cld\n\t"
                     "movdqu %%xmm0, %2\n\t"
                     "movdqu %%xmm15, %3\n\t"
                     "movdqu %%xmm12, %4"
                     : "=r"(before), "=r"(after), "=m"(distinct), "=m"(high), "=m"(alias)
                     : "m"(left), "m"(right)
                     : "cc", "xmm0", "xmm1", "xmm12", "xmm14", "xmm15", "memory");
    printf("distinct=%016llx:%016llx high=%016llx:%016llx alias=%016llx:%016llx flags=%04llx:%04llx:%04llx\n",
           (unsigned long long)half(distinct, 0), (unsigned long long)half(distinct, 8),
           (unsigned long long)half(high, 0), (unsigned long long)half(high, 8),
           (unsigned long long)half(alias, 0), (unsigned long long)half(alias, 8),
           (unsigned long long)(before & UINT64_C(0xcd5)),
           (unsigned long long)(after & UINT64_C(0xcd5)),
           (unsigned long long)((before ^ after) & UINT64_C(0xcd5)));
    return 0;
}
