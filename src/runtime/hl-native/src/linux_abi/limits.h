#ifndef HL_LINUX_LIMITS_H
#define HL_LINUX_LIMITS_H

#include <stdint.h>
#include <stdatomic.h>

#define HL_LIMIT_COUNT 16

typedef struct hl_limit_entry {
    _Atomic uint64_t current;
    _Atomic uint64_t maximum;
    _Atomic uint32_t sequence;
} hl_limit_entry;

typedef struct hl_limit_table {
    hl_limit_entry entries[HL_LIMIT_COUNT];
} hl_limit_table;

void hl_limit_table_init(hl_limit_table *table);
int hl_limit_table_set(hl_limit_table *table, int resource, uint64_t current, uint64_t maximum);
int hl_limit_table_get(const hl_limit_table *table, int resource, uint64_t *current, uint64_t *maximum);

#endif
