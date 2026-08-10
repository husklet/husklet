#ifndef HL_LINUX_READONLY_H
#define HL_LINUX_READONLY_H

#include <stdatomic.h>

#define HL_READONLY_TABLE_CAPACITY 16
#define HL_READONLY_PATH_CAPACITY 256

typedef struct hl_readonly_table {
    char paths[HL_READONLY_TABLE_CAPACITY][HL_READONLY_PATH_CAPACITY];
    _Atomic int count;
} hl_readonly_table;

void hl_readonly_table_init(hl_readonly_table *table);
int hl_readonly_table_add(hl_readonly_table *table, const char *absolute_path);
int hl_readonly_table_denies(const hl_readonly_table *table, const char *absolute_path);
int hl_readonly_table_empty(const hl_readonly_table *table);

#endif
