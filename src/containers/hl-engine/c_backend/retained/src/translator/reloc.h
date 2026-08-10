#ifndef HL_TRANSLATOR_RELOC_H
#define HL_TRANSLATOR_RELOC_H

#include <stddef.h>
#include <stdint.h>

typedef struct hl_reloc {
    uint32_t off;
    uint32_t info;
} hl_reloc;

typedef struct hl_reloc_table {
    hl_reloc *records;
    int count;
    int capacity;
} hl_reloc_table;

void hl_reloc_init(hl_reloc_table *table, hl_reloc *storage, int capacity);
void hl_reloc_reset(hl_reloc_table *table);
int hl_reloc_add(hl_reloc_table *table, uint32_t off, uint32_t info);
int hl_reloc_import(hl_reloc_table *table, const hl_reloc *records, size_t count);
void hl_reloc_slide(uint32_t words[4], uint64_t slide);

#endif
