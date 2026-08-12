#include "pidmap.h"

#include <string.h>

void hl_linux_pidmap_init(hl_linux_pidmap *map) {
    if (map != NULL) memset(map, 0, sizeof *map);
}

int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host) {
    uint32_t index;

    if (map == NULL || guest <= 0 || host <= 0) return -1;
    for (index = 0; index < map->count; ++index)
        if (map->entries[index].guest == guest) {
            map->entries[index].host = host;
            return 0;
        }
    if (map->count >= HL_LINUX_PIDMAP_CAPACITY) return -1;
    map->entries[map->count++] = (hl_linux_pidmap_entry){guest, host};
    return 0;
}

int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host) {
    uint32_t index;

    if (map == NULL || host <= 0) return -1;
    for (index = 0; index < map->count; ++index)
        if (map->entries[index].host == host) {
            map->entries[index] = map->entries[--map->count];
            return 0;
        }
    return -1;
}

int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest) {
    uint32_t index;

    if (map == NULL || guest <= 0) return guest;
    for (index = 0; index < map->count; ++index)
        if (map->entries[index].guest == guest) return map->entries[index].host;
    return guest;
}

int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host) {
    uint32_t index;

    if (map == NULL || host <= 0) return host;
    for (index = 0; index < map->count; ++index)
        if (map->entries[index].host == host) return map->entries[index].guest;
    return host;
}

uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map) {
    return map != NULL ? map->count : 0;
}
