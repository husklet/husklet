#include "limits.h"

#include <stddef.h>

void hl_limit_table_init(hl_limit_table *table) {
    if (table == NULL) return;
    for (int resource = 0; resource < HL_LIMIT_COUNT; ++resource) {
        atomic_store_explicit(&table->entries[resource].current, 0, memory_order_seq_cst);
        atomic_store_explicit(&table->entries[resource].maximum, 0, memory_order_seq_cst);
        atomic_store_explicit(&table->entries[resource].sequence, 0, memory_order_seq_cst);
    }
}

int hl_limit_table_set(hl_limit_table *table, int resource, uint64_t current, uint64_t maximum) {
    hl_limit_entry *entry;
    uint32_t sequence;
    uint32_t published;
    if (table == NULL || resource < 0 || resource >= HL_LIMIT_COUNT) return -1;
    entry = &table->entries[resource];
    sequence = atomic_load_explicit(&entry->sequence, memory_order_seq_cst);
    for (;;) {
        if ((sequence & 1u) != 0) {
            sequence = atomic_load_explicit(&entry->sequence, memory_order_seq_cst);
            continue;
        }
        if (atomic_compare_exchange_weak_explicit(&entry->sequence, &sequence, sequence + 1u, memory_order_seq_cst,
                                                  memory_order_seq_cst))
            break;
    }
    atomic_store_explicit(&entry->current, current, memory_order_seq_cst);
    atomic_store_explicit(&entry->maximum, maximum, memory_order_seq_cst);
    published = sequence + 2u;
    if (published == 0) published = 2u;
    atomic_store_explicit(&entry->sequence, published, memory_order_seq_cst);
    return 0;
}

int hl_limit_table_get(const hl_limit_table *table, int resource, uint64_t *current, uint64_t *maximum) {
    const hl_limit_entry *entry;
    uint32_t before;
    uint32_t after;
    uint64_t read_current;
    uint64_t read_maximum;
    if (table == NULL || resource < 0 || resource >= HL_LIMIT_COUNT) return 0;
    entry = &table->entries[resource];
    for (;;) {
        before = atomic_load_explicit(&entry->sequence, memory_order_seq_cst);
        if (before == 0) return 0;
        if ((before & 1u) != 0) continue;
        read_current = atomic_load_explicit(&entry->current, memory_order_seq_cst);
        read_maximum = atomic_load_explicit(&entry->maximum, memory_order_seq_cst);
        after = atomic_load_explicit(&entry->sequence, memory_order_seq_cst);
        if (before == after && (after & 1u) == 0) break;
    }
    if (current != NULL) *current = read_current;
    if (maximum != NULL) *maximum = read_maximum;
    return 1;
}
