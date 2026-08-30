#include "persistence.h"

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
