#include <stdint.h>
#include <stdio.h>

static uint64_t fold(const uint8_t value[16]) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 8; ++i) result |= (uint64_t)value[i] << (i * 8u);
    return result;
}

int main(void) {
    const uint8_t left[16] = {0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                              0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x10};
    const uint8_t right[16] = {0x01, 0x32, 0x23, 0x54, 0x45, 0x76, 0x67, 0x98,
                               0x89, 0xba, 0xab, 0xdc, 0xcd, 0xfe, 0xef, 0x00};
    uint8_t distinct[16], high[16], zero[16];
    uint64_t before, after;
    __asm__ volatile("movdqu %5, %%xmm0\n\t"
                     "movdqu %6, %%xmm1\n\t"
                     "movdqu %5, %%xmm8\n\t"
                     "movdqu %6, %%xmm9\n\t"
                     "movdqu %5, %%xmm12\n\t"
                     "pushfq; pop %0\n\t"
                     "pxor %%xmm1, %%xmm0\n\t"
                     "pxor %%xmm9, %%xmm8\n\t"
                     "pxor %%xmm12, %%xmm12\n\t"
                     "pushfq; pop %1\n\t"
                     "movdqu %%xmm0, %2\n\t"
                     "movdqu %%xmm8, %3\n\t"
                     "movdqu %%xmm12, %4"
                     : "=r"(before), "=r"(after), "=m"(distinct), "=m"(high), "=m"(zero)
                     : "m"(left), "m"(right)
                     : "xmm0", "xmm1", "xmm8", "xmm9", "xmm12", "memory");
    printf("pxor=%016llx high=%016llx zero=%016llx flags=%llu\n",
           (unsigned long long)fold(distinct), (unsigned long long)fold(high),
           (unsigned long long)fold(zero), (unsigned long long)((before ^ after) & UINT64_C(0xcd5)));
    return 0;
}
