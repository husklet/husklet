#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t input = UINT64_C(0x89abcdef01234567);
    uint64_t output;
    __asm__ volatile("mov $128, %%ecx\n\t"
                     "1: movq %1, %%xmm0\n\t"
                     "dec %%ecx\n\t"
                     "jnz 1b\n\t"
                     "movq %%xmm0, %0"
                     : "=r"(output)
                     : "r"(input)
                     : "cc", "ecx", "xmm0");
    printf("%016llx\n", (unsigned long long)output);
    return output == input ? 0 : 1;
}
