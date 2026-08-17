#include "pidmap.h"

#include <stdatomic.h>
#include <string.h>
#include <sys/mman.h>

#ifndef MAP_ANONYMOUS
#define MAP_ANONYMOUS MAP_ANON
#endif

typedef struct hl_linux_pidmap_slot {
    atomic_ullong identity;
    atomic_uint generation;
} hl_linux_pidmap_slot;

struct hl_linux_pidmap_storage {
    atomic_int next_guest;
    hl_linux_pidmap_slot slot[HL_LINUX_PIDMAP_CAPACITY];
};

static hl_linux_pidmap_storage *pidmap_storage(hl_linux_pidmap *map) {
    if (map->storage != NULL) return map->storage;
    void *memory = mmap(NULL, sizeof(hl_linux_pidmap_storage), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS,
                        -1, 0);
    if (memory == MAP_FAILED) return NULL;
    memset(memory, 0, sizeof(hl_linux_pidmap_storage));
    atomic_init(&((hl_linux_pidmap_storage *)memory)->next_guest, 1);
    map->storage = memory;
    return map->storage;
}

static void pidmap_raise_next(hl_linux_pidmap_storage *storage, int32_t guest) {
    int current = atomic_load_explicit(&storage->next_guest, memory_order_relaxed);
    while (current <= guest &&
           !atomic_compare_exchange_weak_explicit(&storage->next_guest, &current, guest == INT32_MAX ? INT32_MAX : guest + 1,
                                                  memory_order_relaxed, memory_order_relaxed)) {}
}

static uint64_t identity_pack(int32_t guest, int32_t host) {
    return ((uint64_t)(uint32_t)guest << 32) | (uint32_t)host;
}

static int slot_snapshot(const hl_linux_pidmap_slot *slot, int32_t *guest, int32_t *host) {
    uint64_t identity = atomic_load_explicit(&slot->identity, memory_order_acquire);
    if (identity == 0) return 0;
    *guest = (int32_t)(identity >> 32);
    *host = (int32_t)identity;
    return *guest > 0 && *host > 0;
}

static int slot_update_host(hl_linux_pidmap_slot *slot, int32_t guest, int32_t host) {
    uint64_t current = atomic_load_explicit(&slot->identity, memory_order_acquire);
    if ((int32_t)(current >> 32) != guest) return 0;
    if (!atomic_compare_exchange_strong_explicit(&slot->identity, &current, identity_pack(guest, host),
                                                 memory_order_acq_rel, memory_order_acquire))
        return 0;
    atomic_fetch_add_explicit(&slot->generation, 1, memory_order_release);
    return 1;
}

static int slot_claim(hl_linux_pidmap_slot *slot, int32_t guest, int32_t host) {
    uint64_t empty = 0;
    if (!atomic_compare_exchange_strong_explicit(&slot->identity, &empty, identity_pack(guest, host),
                                                 memory_order_acq_rel, memory_order_acquire))
        return 0;
    atomic_fetch_add_explicit(&slot->generation, 1, memory_order_release);
    return 1;
}

void hl_linux_pidmap_init(hl_linux_pidmap *map) {
    if (map != NULL) memset(map, 0, sizeof *map);
}

int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host) {
    if (map == NULL || guest <= 0 || host <= 0) return -1;
    hl_linux_pidmap_storage *storage = pidmap_storage(map);
    if (storage == NULL) return -1;
    for (;;) {
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
            int32_t current_guest, current_host;
            if (slot_snapshot(&storage->slot[index], &current_guest, &current_host) && current_guest == guest) {
                if (slot_update_host(&storage->slot[index], guest, host)) {
                    pidmap_raise_next(storage, guest);
                    return 0;
                }
                break;
            }
        }
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index)
            if (slot_claim(&storage->slot[index], guest, host)) {
                // A concurrent same-guest claim is possible. Keep the lowest slot canonical and retire ours.
                for (uint32_t prior = 0; prior < index; ++prior) {
                    int32_t prior_guest, prior_host;
                    if (slot_snapshot(&storage->slot[prior], &prior_guest, &prior_host) && prior_guest == guest) {
                        (void)slot_update_host(&storage->slot[prior], guest, host);
                        uint64_t duplicate = identity_pack(guest, host);
                        if (atomic_compare_exchange_strong_explicit(&storage->slot[index].identity, &duplicate, 0,
                                                                    memory_order_acq_rel, memory_order_acquire))
                            atomic_fetch_add_explicit(&storage->slot[index].generation, 1, memory_order_release);
                        pidmap_raise_next(storage, guest);
                        return 0;
                    }
                }
                pidmap_raise_next(storage, guest);
                return 0;
            }
        return -1;
    }
}

int32_t hl_linux_pidmap_register_host(hl_linux_pidmap *map, int32_t host) {
    if (map == NULL || host <= 0) return -1;
    hl_linux_pidmap_storage *storage = pidmap_storage(map);
    if (storage == NULL) return -1;
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t guest, current_host;
        if (slot_snapshot(&storage->slot[index], &guest, &current_host) && current_host == host) return guest;
    }
    int guest = atomic_fetch_add_explicit(&storage->next_guest, 1, memory_order_relaxed);
    if (guest <= 0 || guest == INT32_MAX) return -1;
    return hl_linux_pidmap_add(map, guest, host) == 0 ? guest : -1;
}

int32_t hl_linux_pidmap_allocate_guest(hl_linux_pidmap *map) {
    if (map == NULL) return -1;
    hl_linux_pidmap_storage *storage = pidmap_storage(map);
    if (storage == NULL) return -1;
    int guest = atomic_fetch_add_explicit(&storage->next_guest, 1, memory_order_relaxed);
    return guest > 0 && guest < INT32_MAX ? guest : -1;
}

void hl_linux_pidmap_activate(hl_linux_pidmap *map) {
    if (map != NULL) map->active = 1;
}

int hl_linux_pidmap_host_checked(const hl_linux_pidmap *map, int32_t guest, int32_t *host) {
    if (host == NULL || guest <= 0) return -1;
    if (map != NULL && map->storage != NULL)
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
            int32_t current_guest, current_host;
            if (slot_snapshot(&map->storage->slot[index], &current_guest, &current_host) && current_guest == guest) {
                *host = current_host;
                return 0;
            }
        }
    if (map == NULL || !map->active) {
        *host = guest;
        return 0;
    }
    return -1;
}

int hl_linux_pidmap_guest_checked(const hl_linux_pidmap *map, int32_t host, int32_t *guest) {
    if (guest == NULL || host <= 0) return -1;
    if (map != NULL && map->storage != NULL)
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
            int32_t current_guest, current_host;
            if (slot_snapshot(&map->storage->slot[index], &current_guest, &current_host) && current_host == host) {
                *guest = current_guest;
                return 0;
            }
        }
    if (map == NULL || !map->active) {
        *guest = host;
        return 0;
    }
    return -1;
}

int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host) {
    if (map == NULL || map->storage == NULL || host <= 0) return -1;
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        hl_linux_pidmap_slot *slot = &map->storage->slot[index];
        int32_t guest, current_host;
        if (!slot_snapshot(slot, &guest, &current_host) || current_host != host) continue;
        uint64_t identity = identity_pack(guest, current_host);
        if (!atomic_compare_exchange_strong_explicit(&slot->identity, &identity, 0, memory_order_acq_rel,
                                                     memory_order_acquire))
            continue;
        atomic_fetch_add_explicit(&slot->generation, 1, memory_order_release);
        return 0;
    }
    return -1;
}

int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest) {
    int32_t host;
    return hl_linux_pidmap_host_checked(map, guest, &host) == 0 ? host : guest;
}

int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host) {
    int32_t guest;
    return hl_linux_pidmap_guest_checked(map, host, &guest) == 0 ? guest : host;
}

uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map) {
    uint32_t count = 0;
    if (map == NULL || map->storage == NULL) return 0;
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t guest, host;
        count += (uint32_t)slot_snapshot(&map->storage->slot[index], &guest, &host);
    }
    return count;
}
