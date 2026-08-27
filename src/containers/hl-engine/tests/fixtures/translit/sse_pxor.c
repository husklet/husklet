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
    const uint8_t right[16] = {0x01, 0x32, 0x23, 0x54, 0x45, 0x76, 0x67, 0x98,
                               0x89, 0xba, 0xab, 0xdc, 0xcd, 0xfe, 0xef, 0x00};
    const uint8_t high_left[16] = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
                                   0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f};
    const uint8_t high_right[16] = {0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8,
                                    0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1, 0xf0};
    uint8_t distinct[16], high[16], zero[16];
    uint64_t before, after;
    __asm__ volatile("movdqu %5, %%xmm0\n\t"
                     "movdqu %6, %%xmm1\n\t"
                     "movdqu %7, %%xmm8\n\t"
                     "movdqu %8, %%xmm9\n\t"
                     "movdqu %5, %%xmm12\n\t"
                     "pushq $0xc97; popfq\n\t"
                     "pushfq; pop %0\n\t"
                     "pxor %%xmm1, %%xmm0\n\t"
                     "pxor %%xmm9, %%xmm8\n\t"
                     "pxor %%xmm12, %%xmm12\n\t"
                     "pushfq; pop %1\n\t"
                     "cld\n\t"
                     "movdqu %%xmm0, %2\n\t"
                     "movdqu %%xmm8, %3\n\t"
                     "movdqu %%xmm12, %4"
                     : "=r"(before), "=r"(after), "=m"(distinct), "=m"(high), "=m"(zero)
                     : "m"(left), "m"(right), "m"(high_left), "m"(high_right)
                     : "xmm0", "xmm1", "xmm8", "xmm9", "xmm12", "memory");
    printf("distinct=%016llx:%016llx high=%016llx:%016llx zero=%016llx:%016llx flags=%04llx:%04llx:%04llx\n",
           (unsigned long long)half(distinct, 0), (unsigned long long)half(distinct, 8),
           (unsigned long long)half(high, 0), (unsigned long long)half(high, 8),
           (unsigned long long)half(zero, 0), (unsigned long long)half(zero, 8),
           (unsigned long long)(before & UINT64_C(0xcd5)),
           (unsigned long long)(after & UINT64_C(0xcd5)),
           (unsigned long long)((before ^ after) & UINT64_C(0xcd5)));
    return 0;
}
