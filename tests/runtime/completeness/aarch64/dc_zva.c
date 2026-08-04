#include <stdint.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    _Alignas(64) unsigned char bytes[128];
    memset(bytes, 0xa5, sizeof bytes);

    void *inside_first_block = bytes + 37;
    __asm__ volatile("dc zva, %0" : : "r"(inside_first_block) : "memory");

    int first_zero = 1;
    int second_preserved = 1;
    for (size_t index = 0; index < 64; ++index)
        first_zero &= bytes[index] == 0;
    for (size_t index = 64; index < sizeof bytes; ++index)
        second_preserved &= bytes[index] == 0xa5;

    printf("dc-zva first=%d neighbor=%d\n", first_zero, second_preserved);
    return first_zero && second_preserved ? 0 : 1;
}
