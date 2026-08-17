#ifndef HL_LINUX_ABI_CONTAINER_PIDMAP_H
#define HL_LINUX_ABI_CONTAINER_PIDMAP_H

#include <stdint.h>
#include <stddef.h>

#define HL_LINUX_PIDMAP_CAPACITY 4096

typedef struct hl_linux_pidmap_entry {
    int32_t guest;
    int32_t host;
} hl_linux_pidmap_entry;

typedef struct hl_linux_pidmap_storage hl_linux_pidmap_storage;
typedef struct hl_linux_identity_registry_storage hl_linux_identity_registry_storage;

typedef struct hl_linux_identity_registry {
    hl_linux_identity_registry_storage *storage;
    int lock_fd;
} hl_linux_identity_registry;

typedef struct hl_linux_pidmap {
    hl_linux_pidmap_storage *storage;
    hl_linux_identity_registry *registry;
    uint32_t kind;
    int active;
} hl_linux_pidmap;

typedef struct hl_linux_pidmap_update {
    hl_linux_pidmap *map;
    int32_t guest;
    int32_t host;
} hl_linux_pidmap_update;

void hl_linux_pidmap_init(hl_linux_pidmap *map);
int hl_linux_pidmap_prepare_shared(hl_linux_pidmap *map);
int hl_linux_identity_registry_prepare(hl_linux_identity_registry *registry, hl_linux_pidmap *pid,
                                       hl_linux_pidmap *pgid, hl_linux_pidmap *sid);
int hl_linux_identity_registry_add(const hl_linux_pidmap_update *updates, size_t count);
uint64_t hl_linux_identity_registry_commit_word(const hl_linux_identity_registry *registry);
#if defined(HL_NATIVE_TEST_HOOKS)
int hl_c_backend_identity_registry_test(uint32_t scenario, uint32_t iterations);
#endif
int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host);
int32_t hl_linux_pidmap_allocate_guest(hl_linux_pidmap *map);
int32_t hl_linux_pidmap_register_host(hl_linux_pidmap *map, int32_t host);
int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host);
void hl_linux_pidmap_activate(hl_linux_pidmap *map);
int hl_linux_pidmap_is_active(const hl_linux_pidmap *map);
int hl_linux_pidmap_host_checked(const hl_linux_pidmap *map, int32_t guest, int32_t *host);
int hl_linux_pidmap_guest_checked(const hl_linux_pidmap *map, int32_t host, int32_t *guest);
int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest);
int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host);
uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map);
size_t hl_linux_pidmap_snapshot(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity);

#endif
