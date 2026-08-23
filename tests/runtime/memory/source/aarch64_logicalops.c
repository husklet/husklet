#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Native admission needs the same site observed twice, so the probes run in a hot loop. */
#define REPEATS 4000

#if defined(__aarch64__)
int main(void) {
    const size_t page = 4096;
    int fd = (int)syscall(SYS_memfd_create, "logical-ops", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(page * 2)) != 0) return 2;
    unsigned char *memory = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)page);
    if (memory == MAP_FAILED) return 3;

    uint64_t *word = (uint64_t *)(void *)(memory + 128);
    uint64_t *pair = (uint64_t *)(void *)(memory + 256);
    unsigned char *line = memory + 512;
    int lse = 1, cas = 1, casp = 1, exclusive_pair = 1, zva = 1;

    /* Each probe re-runs from the same state so the printed verdicts are identical to a single
       pass; the repeat count is what makes the block hot enough for native admission. */
    volatile int repeats = REPEATS;
    for (int round = 0; round < repeats; ++round) {
        *word = 10;
        uint64_t add = 7, old;
        __asm__ volatile(".arch_extension lse\n"
                         "ldadd %x[add], %x[old], [%x[address]]"
                         : [old] "=&r"(old)
                         : [add] "r"(add), [address] "r"(word)
                         : "memory");
        lse &= old == 10 && *word == 17;

        uint64_t expected = 17, replacement = 31;
        __asm__ volatile(".arch_extension lse\n"
                         "cas %x[expected], %x[replacement], [%x[address]]"
                         : [expected] "+r"(expected)
                         : [replacement] "r"(replacement), [address] "r"(word)
                         : "memory");
        cas &= expected == 17 && *word == 31;

        pair[0] = 40;
        pair[1] = 41;
        unsigned status;
        uint64_t first, second;
        do {
            __asm__ volatile("ldxp %x[first], %x[second], [%x[address]]"
                             : [first] "=&r"(first), [second] "=&r"(second)
                             : [address] "r"(pair)
                             : "memory");
            first += 2;
            second += 3;
            __asm__ volatile("stxp %w[status], %x[first], %x[second], [%x[address]]"
                             : [status] "=&r"(status)
                             : [first] "r"(first), [second] "r"(second), [address] "r"(pair)
                             : "memory");
        } while (status);
        exclusive_pair &= pair[0] == 42 && pair[1] == 44;

        pair[0] = 50;
        pair[1] = 51;
        register uint64_t expect0 __asm__("x0") = 50;
        register uint64_t expect1 __asm__("x1") = 51;
        register uint64_t new0 __asm__("x2") = 60;
        register uint64_t new1 __asm__("x3") = 61;
        register uint64_t *pair_address __asm__("x4") = pair;
        __asm__ volatile(".arch_extension lse\n"
                         "casp x0, x1, x2, x3, [x4]"
                         : "+r"(expect0), "+r"(expect1)
                         : "r"(new0), "r"(new1), "r"(pair_address)
                         : "memory");
        casp &= expect0 == 50 && expect1 == 51 && pair[0] == 60 && pair[1] == 61;

        memset(line, 0x5a, 64);
        __asm__ volatile("dc zva, %x[address]" : : [address] "r"(line) : "memory");
        for (int i = 0; i < 64; ++i)
            if (line[i] != 0) zva = 0;
    }

    printf("aarch64-logical-ops lse=%d cas=%d casp=%d exclusive-pair=%d zva=%d\n", lse, cas, casp, exclusive_pair, zva);
    return lse && cas && casp && exclusive_pair && zva ? 0 : 1;
}
#else
int main(void) {
    return 0;
}
#endif
