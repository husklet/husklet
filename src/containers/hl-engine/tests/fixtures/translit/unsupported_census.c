#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t input = UINT64_C(0x89abcdef01234567);
    uint64_t output;
    __asm__ volatile("movq %1, %%xmm0; movq %%xmm0, %0" : "=r"(output) : "r"(input) : "xmm0");
    printf("%016llx\n", (unsigned long long)output);
    return output == input ? 0 : 1;
}
