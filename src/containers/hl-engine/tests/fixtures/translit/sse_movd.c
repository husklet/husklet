#include <stdint.h>
#include <stdio.h>

static uint64_t half(const uint8_t value[16], unsigned base) {
    uint64_t result = 0;
    for (unsigned i = 0; i < 8; ++i) result |= (uint64_t)value[base + i] << (i * 8u);
    return result;
}

int main(void) {
    uint8_t d32[16], q64[16], high32[16], high64[16];
    uint64_t before, after;
    __asm__ volatile("movabs $0x1122334455667788, %%rax\n\t"
                     "movabs $0x8899aabbccddeeff, %%r9\n\t"
                     "pushq $0xc97; popfq\n\t"
                     "pushfq; pop %0\n\t"
                     "movd %%eax, %%xmm0\n\t"
                     "movq %%rax, %%xmm1\n\t"
                     "movd %%r9d, %%xmm10\n\t"
                     "movq %%r9, %%xmm11\n\t"
                     "pushfq; pop %1\n\t"
                     "cld\n\t"
                     "movdqu %%xmm0, %2\n\t"
                     "movdqu %%xmm1, %3\n\t"
                     "movdqu %%xmm10, %4\n\t"
                     "movdqu %%xmm11, %5"
                     : "=r"(before), "=r"(after), "=m"(d32), "=m"(q64), "=m"(high32), "=m"(high64)
                     :
                     : "cc", "rax", "r9", "xmm0", "xmm1", "xmm10", "xmm11", "memory");
    printf("d32=%016llx:%016llx q64=%016llx:%016llx high32=%016llx:%016llx high64=%016llx:%016llx"
           " flags=%04llx:%04llx:%04llx\n",
           (unsigned long long)half(d32, 0), (unsigned long long)half(d32, 8),
           (unsigned long long)half(q64, 0), (unsigned long long)half(q64, 8),
           (unsigned long long)half(high32, 0), (unsigned long long)half(high32, 8),
           (unsigned long long)half(high64, 0), (unsigned long long)half(high64, 8),
           (unsigned long long)(before & UINT64_C(0xcd5)),
           (unsigned long long)(after & UINT64_C(0xcd5)),
           (unsigned long long)((before ^ after) & UINT64_C(0xcd5)));
    return 0;
}
