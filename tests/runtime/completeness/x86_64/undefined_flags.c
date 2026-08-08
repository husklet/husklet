// bsf/bsr/imul leave CF, OF, SF, AF and PF undefined. Undefined still has to be the SAME on the
// interpreter and on a translated block, or a guest's behaviour would depend on whether its code
// happened to get translated. This pins CF/OF/SF/ZF, which the QEMU oracle agrees on; PF and AF
// are deliberately absent because both of our paths preserve them where QEMU clears them.
#include <stdio.h>
#include <stdint.h>

#define OBSERVED 0x8c1u /* CF | ZF | SF | OF */

static uint64_t seeded(uint64_t seed) { return (seed & OBSERVED) | 2u; }

static uint64_t bsf64(uint64_t source, uint64_t seed, uint64_t *destination) {
    uint64_t flags, value = *destination;
    __asm__ volatile("push %3\n\t popfq\n\t bsf %2, %1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(source), "r"(seeded(seed)) : "cc");
    *destination = value;
    return flags & OBSERVED;
}

static uint64_t bsr64(uint64_t source, uint64_t seed, uint64_t *destination) {
    uint64_t flags, value = *destination;
    __asm__ volatile("push %3\n\t popfq\n\t bsr %2, %1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(source), "r"(seeded(seed)) : "cc");
    *destination = value;
    return flags & OBSERVED;
}

static uint64_t bsf32(uint32_t source, uint64_t seed, uint64_t *destination) {
    uint64_t flags, value = *destination;
    __asm__ volatile("push %3\n\t popfq\n\t bsf %2, %k1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(source), "r"(seeded(seed)) : "cc");
    *destination = value;
    return flags & OBSERVED;
}

static uint64_t bsr16(uint16_t source, uint64_t seed, uint64_t *destination) {
    uint64_t flags, value = *destination;
    __asm__ volatile("push %3\n\t popfq\n\t bsr %2, %w1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(source), "r"(seeded(seed)) : "cc");
    *destination = value;
    return flags & OBSERVED;
}

static uint64_t bsf16(uint16_t source, uint64_t seed, uint64_t *destination) {
    uint64_t flags, value = *destination;
    __asm__ volatile("push %3\n\t popfq\n\t bsf %2, %w1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(source), "r"(seeded(seed)) : "cc");
    *destination = value;
    return flags & OBSERVED;
}

static uint64_t imul64(uint64_t left, uint64_t right, uint64_t seed, uint64_t *product) {
    uint64_t flags, value = left;
    __asm__ volatile("push %3\n\t popfq\n\t imul %2, %1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(right), "r"(seeded(seed)) : "cc");
    *product = value;
    return flags & OBSERVED;
}

static uint64_t imul32(uint32_t left, uint32_t right, uint64_t seed, uint64_t *product) {
    uint64_t flags, value = left;
    __asm__ volatile("push %3\n\t popfq\n\t imul %2, %k1\n\t pushfq\n\t pop %0"
                     : "=r"(flags), "+r"(value) : "r"(right), "r"(seeded(seed)) : "cc");
    *product = value;
    return flags & OBSERVED;
}

static uint64_t mix(uint64_t hash, uint64_t value) {
    return (hash ^ value) * 1099511628211ULL;
}

int main(void) {
    static const uint64_t sources[] = {
        0, 1, 0x10, 0x8000, 0xffff, 0x8000000000000000ULL, 0xffffffffffffffffULL,
        0x0000000080000000ULL, 0x00000000ffffffffULL, 0x0123456789abcdefULL,
    };
    static const uint64_t seeds[] = {0, OBSERVED, 0x041};
    uint64_t hash = 1469598103934665603ULL;
    size_t index, seed;

    for (seed = 0; seed < sizeof seeds / sizeof seeds[0]; ++seed) {
        for (index = 0; index < sizeof sources / sizeof sources[0]; ++index) {
            uint64_t source = sources[index];
            uint64_t destination = 0x5555aaaa5555aaaaULL;
            hash = mix(hash, bsf64(source, seeds[seed], &destination));
            hash = mix(hash, destination);
            destination = 0x5555aaaa5555aaaaULL;
            hash = mix(hash, bsr64(source, seeds[seed], &destination));
            hash = mix(hash, destination);
            destination = 0x5555aaaa5555aaaaULL;
            hash = mix(hash, bsf32((uint32_t)source, seeds[seed], &destination));
            hash = mix(hash, destination);
            destination = 0x5555aaaa5555aaaaULL;
            hash = mix(hash, bsf16((uint16_t)source, seeds[seed], &destination));
            hash = mix(hash, destination);
            destination = 0x5555aaaa5555aaaaULL;
            hash = mix(hash, bsr16((uint16_t)source, seeds[seed], &destination));
            hash = mix(hash, destination);

            {
                uint64_t product = 0;
                hash = mix(hash, imul64(source, 5, seeds[seed], &product));
                hash = mix(hash, product);
                hash = mix(hash, imul64(source, source, seeds[seed], &product));
                hash = mix(hash, product);
                hash = mix(hash, imul32((uint32_t)source, 3, seeds[seed], &product));
                hash = mix(hash, product);
            }
        }
    }
    printf("undefined-flags=%016llx\n", (unsigned long long)hash);
    return 0;
}
