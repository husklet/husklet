#include "persistence.h"

#include <string.h>

void x64_pc_put16(uint8_t **cursor, uint16_t value) {
    (*cursor)[0] = (uint8_t)value;
    (*cursor)[1] = (uint8_t)(value >> 8);
    *cursor += 2;
}

void x64_pc_put32(uint8_t **cursor, uint32_t value) {
    for (unsigned i = 0; i < 4; i++) (*cursor)[i] = (uint8_t)(value >> (8 * i));
    *cursor += 4;
}

void x64_pc_put64(uint8_t **cursor, uint64_t value) {
    for (unsigned i = 0; i < 8; i++) (*cursor)[i] = (uint8_t)(value >> (8 * i));
    *cursor += 8;
}

uint16_t x64_pc_get16(const uint8_t *cursor) {
    return (uint16_t)(cursor[0] | ((uint16_t)cursor[1] << 8));
}

uint32_t x64_pc_get32(const uint8_t *cursor) {
    uint32_t value = 0;
    for (unsigned i = 0; i < 4; i++) value |= (uint32_t)cursor[i] << (8 * i);
    return value;
}

uint64_t x64_pc_get64(const uint8_t *cursor) {
    uint64_t value = 0;
    for (unsigned i = 0; i < 8; i++) value |= (uint64_t)cursor[i] << (8 * i);
    return value;
}

int x64_pc_header_validate(const uint8_t *bytes, size_t size, uint64_t abi, uint64_t cpu_size,
                           uint64_t map_slots, const uint8_t identity[32], uint64_t entry,
                           uint64_t modes, uint64_t matches[10]) {
    uint64_t local[10] = {
        size >= X64_PC_HEADER_SIZE,
        size >= 8 && x64_pc_get64(bytes) == X64_PC_MAGIC,
        size >= 16 && x64_pc_get64(bytes + 8) == X64_PC_VERSION,
        size >= 24 && x64_pc_get64(bytes + 16) == X64_PC_ENDIAN,
        size >= 32 && x64_pc_get64(bytes + 24) == abi,
        size >= 40 && x64_pc_get64(bytes + 32) == cpu_size,
        size >= 48 && x64_pc_get64(bytes + 40) == map_slots,
        size >= 80 && memcmp(bytes + 48, identity, 32) == 0,
        size >= 88 && x64_pc_get64(bytes + 80) == entry,
        size >= 200 && x64_pc_get64(bytes + 192) == modes,
    };
    int valid = 1;
    for (unsigned i = 0; i < 10; i++) {
        if (matches != NULL) matches[i] = local[i];
        valid = valid && local[i];
    }
    return valid;
}
