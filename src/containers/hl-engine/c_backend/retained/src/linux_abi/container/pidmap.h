#ifndef HL_LINUX_ABI_CONTAINER_PIDMAP_H
#define HL_LINUX_ABI_CONTAINER_PIDMAP_H

#include <stdint.h>

#define HL_LINUX_PIDMAP_CAPACITY 4096

typedef struct hl_linux_pidmap_entry {
    int32_t guest;
    int32_t host;
} hl_linux_pidmap_entry;

typedef struct hl_linux_pidmap {
    hl_linux_pidmap_entry entries[HL_LINUX_PIDMAP_CAPACITY];
    uint32_t count;
} hl_linux_pidmap;

void hl_linux_pidmap_init(hl_linux_pidmap *map);
int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host);
int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host);
int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest);
int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host);
uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map);

#endif
