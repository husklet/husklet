#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#if defined(__x86_64__)
int main(void) {
    unsigned char *area = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) return 2;
    memset(area, 0xa5, 832);
    __asm__ volatile("xsave64 (%0)" : : "r"(area), "a"(3u), "d"(0u) : "memory");
    int saved = area[0] == 0x7f && area[1] == 0x03;
    printf("x86-projection-xsave saved=%d\n", saved);
    return !saved;
}
#else
int main(void) { return 0; }
#endif
