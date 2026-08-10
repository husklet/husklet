#include "reloc.h"

#include <string.h>

void hl_reloc_init(hl_reloc_table *table, hl_reloc *storage, int capacity) {
    table->records = storage;
    table->count = 0;
    table->capacity = capacity;
}

void hl_reloc_reset(hl_reloc_table *table) {
    table->count = 0;
}

int hl_reloc_add(hl_reloc_table *table, uint32_t off, uint32_t info) {
    if (table->count < 0 || table->count >= table->capacity) return 0;
    table->records[table->count].off = off;
    table->records[table->count].info = info;
    table->count++;
    return 1;
}

int hl_reloc_import(hl_reloc_table *table, const hl_reloc *records, size_t count) {
    if (count > (size_t)table->capacity) return 0;
    memmove(table->records, records, count * sizeof(*records));
    table->count = (int)count;
    return 1;
}

void hl_reloc_slide(uint32_t words[4], uint64_t slide) {
    uint32_t rd = words[0] & UINT32_C(0x1f);
    uint64_t old =
        (uint64_t)((words[0] >> 5) & UINT32_C(0xffff)) | ((uint64_t)((words[1] >> 5) & UINT32_C(0xffff)) << 16) |
        ((uint64_t)((words[2] >> 5) & UINT32_C(0xffff)) << 32) | ((uint64_t)((words[3] >> 5) & UINT32_C(0xffff)) << 48);
    uint64_t value = old + slide;
    words[0] = UINT32_C(0xd2800000) | (((uint32_t)value & UINT32_C(0xffff)) << 5) | rd;
    words[1] = UINT32_C(0xf2800000) | (1u << 21) | (((uint32_t)(value >> 16) & UINT32_C(0xffff)) << 5) | rd;
    words[2] = UINT32_C(0xf2800000) | (2u << 21) | (((uint32_t)(value >> 32) & UINT32_C(0xffff)) << 5) | rd;
    words[3] = UINT32_C(0xf2800000) | (3u << 21) | (((uint32_t)(value >> 48) & UINT32_C(0xffff)) << 5) | rd;
}
