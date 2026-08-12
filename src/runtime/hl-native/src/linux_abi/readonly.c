#include "readonly.h"

#include <string.h>

void hl_readonly_table_init(hl_readonly_table *table) {
    if (table == NULL) return;
    memset(table->paths, 0, sizeof(table->paths));
    atomic_store_explicit(&table->count, 0, memory_order_release);
}

int hl_readonly_table_add(hl_readonly_table *table, const char *absolute_path) {
    int count;
    size_t length;
    if (table == NULL || absolute_path == NULL || absolute_path[0] != '/') return -1;
    length = strlen(absolute_path);
    if (length >= HL_READONLY_PATH_CAPACITY) return -1;
    count = atomic_load_explicit(&table->count, memory_order_acquire);
    for (int index = 0; index < count; ++index)
        if (strcmp(table->paths[index], absolute_path) == 0) return 0;
    if (count >= HL_READONLY_TABLE_CAPACITY) return -1;
    memcpy(table->paths[count], absolute_path, length + 1);
    atomic_store_explicit(&table->count, count + 1, memory_order_release);
    return 0;
}

int hl_readonly_table_denies(const hl_readonly_table *table, const char *absolute_path) {
    int count;
    if (table == NULL || absolute_path == NULL || absolute_path[0] != '/') return 0;
    count = atomic_load_explicit(&table->count, memory_order_acquire);
    for (int index = 0; index < count; ++index) {
        size_t length = strlen(table->paths[index]);
        if (strncmp(absolute_path, table->paths[index], length) == 0 &&
            (absolute_path[length] == '\0' || absolute_path[length] == '/'))
            return 1;
    }
    return 0;
}

int hl_readonly_table_empty(const hl_readonly_table *table) {
    return table == NULL || atomic_load_explicit(&table->count, memory_order_acquire) == 0;
}
