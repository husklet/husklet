#ifndef HL_NATIVE_X86_64_WORD_H
#define HL_NATIVE_X86_64_WORD_H

#include <stddef.h>
#include <stdint.h>

static inline uint32_t constant_words(uint64_t value) {
    uint32_t count = 1;
    unsigned shift;

    for (shift = 16; shift < 64; shift += 16) {
        if (((value >> shift) & UINT64_C(0xffff)) != 0) ++count;
    }
    return count;
}

static inline uint32_t bit_count(uint32_t value) {
    uint32_t count = 0;

    while (value != 0) {
        count += value & 1u;
        value >>= 1;
    }
    return count;
}

static inline void emit_constant(uint32_t *words, uint32_t *cursor, unsigned destination, uint64_t value) {
    unsigned shift;

    words[(*cursor)++] = UINT32_C(0xd2800000) | ((uint32_t)value & UINT32_C(0xffff)) << 5 | destination;
    for (shift = 16; shift < 64; shift += 16) {
        uint32_t part = (uint32_t)(value >> shift) & UINT32_C(0xffff);
        if (part != 0) words[(*cursor)++] = UINT32_C(0xf2800000) | (shift / 16u) << 21 | part << 5 | destination;
    }
}

static inline uint32_t store_word(unsigned source, size_t offset) {
    return UINT32_C(0xf9000000) | ((uint32_t)(offset / 8u) & UINT32_C(0xfff)) << 10 | 28u << 5 | source;
}

static inline uint32_t load_word(unsigned destination, size_t offset) {
    return UINT32_C(0xf9400000) | ((uint32_t)(offset / 8u) & UINT32_C(0xfff)) << 10 | 28u << 5 |
           destination;
}

static inline uint32_t arm_condition(uint8_t x86) {
    static const uint8_t map[16] = {6, 7, 3, 2, 0, 1, 9, 8, 4, 5, 0, 0, 11, 10, 13, 12};

    return map[x86 & 0x0fu];
}

#endif
