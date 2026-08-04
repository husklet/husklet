#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__aarch64__)
int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "pair-writeback", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(page * 2)) != 0) return 2;

    /* A non-zero memfd offset activates the logical-VMA translation path. */
    void *logical = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)page);
    if (logical == MAP_FAILED) return 3;
    *(volatile unsigned char *)logical = 1;

    uint64_t words[6] = {0};
    uint64_t *base = &words[4];
    uint64_t first = UINT64_C(0x1122334455667788);
    uint64_t second = UINT64_C(0x99aabbccddeeff00);
    uint64_t out_first, out_second;
    __asm__ volatile(
        "stp %x[first], %x[second], [%x[base], #-16]!\n"
        "ldp %x[out_first], %x[out_second], [%x[base]], #16\n"
        : [base] "+r"(base), [out_first] "=&r"(out_first), [out_second] "=&r"(out_second)
        : [first] "r"(first), [second] "r"(second)
        : "memory");

    int values = out_first == first && out_second == second;
    int address = base == &words[4];
    int slots = words[2] == first && words[3] == second;
    printf("aarch64-pair-writeback values=%d address=%d slots=%d\n", values, address, slots);
    return values && address && slots ? 0 : 1;
}
#else
int main(void) { return 0; }
#endif
