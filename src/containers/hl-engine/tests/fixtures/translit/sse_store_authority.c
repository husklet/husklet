#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

extern void authority_store(const uint8_t source[16], uint8_t target[16]);

__asm__(".text\n"
        ".type authority_store,@function\n"
        "authority_store:\n"
        "movdqu (%rdi), %xmm9\n"
        "movups %xmm9, (%rsi)\n"
        "ret\n"
        ".size authority_store, .-authority_store\n");

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *mapping = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE | PROT_EXEC,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) return 93;
    _Alignas(16) uint8_t source[16];
    for (unsigned i = 0; i < 16; i++) source[i] = (uint8_t)(11 * i + 5);
    authority_store(source, mapping);
    unsigned sum = 0;
    for (unsigned i = 0; i < 16; i++) sum += mapping[i] == source[i];
    printf("authority=%u\n", sum);
    return sum == 16 ? 0 : 94;
}
