#ifndef _XOPEN_SOURCE
#define _XOPEN_SOURCE 700
#endif

#include "pidmap.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_ANONYMOUS
#define MAP_ANONYMOUS MAP_ANON
#endif

enum { PIDMAP_BANKS = 2, PIDMAP_KINDS = 3, PIDMAP_JOURNAL_CAPACITY = 16 };

typedef struct hl_linux_pidmap_slot {
    atomic_ullong identity;
} hl_linux_pidmap_slot;

struct hl_linux_pidmap_storage {
    atomic_int next_guest;
    hl_linux_pidmap_slot bank[PIDMAP_BANKS][HL_LINUX_PIDMAP_CAPACITY];
};

typedef struct hl_linux_identity_journal_entry {
    uint32_t kind;
    uint32_t index;
} hl_linux_identity_journal_entry;

struct hl_linux_identity_registry_storage {
    // generation << 1 | active bank. This is the sole publication point for every map.
    atomic_ullong commit_word;
    atomic_uint active;
    atomic_uint journal_count;
    hl_linux_identity_journal_entry journal[PIDMAP_JOURNAL_CAPACITY];
    hl_linux_pidmap_storage map[PIDMAP_KINDS];
};

static pthread_mutex_t g_pidmap_thread_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t g_pidmap_atfork_once = PTHREAD_ONCE_INIT;

static void pidmap_after_fork(void) {
    // POSIX record locks are not inherited. Match that property for the process-local thread lock.
    g_pidmap_thread_lock = (pthread_mutex_t)PTHREAD_MUTEX_INITIALIZER;
}

static void pidmap_register_atfork(void) {
    (void)pthread_atfork(NULL, NULL, pidmap_after_fork);
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

static int registry_file(void) {
    char path[] = "/tmp/husklet-pidmap-XXXXXX";
    int descriptor = mkstemp(path);
    if (descriptor < 0) return -1;
    (void)unlink(path);
    int flags = fcntl(descriptor, F_GETFD);
    if (flags < 0 || fcntl(descriptor, F_SETFD, flags | FD_CLOEXEC) != 0) {
        int saved = errno;
        (void)close(descriptor);
        errno = saved;
        return -1;
    }
    return descriptor;
}

static hl_linux_identity_registry_storage *registry_storage(void) {
    void *memory = mmap(NULL, sizeof(hl_linux_identity_registry_storage), PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (memory == MAP_FAILED) return NULL;
    memset(memory, 0, sizeof(hl_linux_identity_registry_storage));
    hl_linux_identity_registry_storage *storage = memory;
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind) atomic_init(&storage->map[kind].next_guest, 1);
    return storage;
}

static int registry_lock(hl_linux_identity_registry *registry) {
    if (registry == NULL || registry->storage == NULL || registry->lock_fd < 0) {
        errno = EINVAL;
        return -1;
    }
    (void)pthread_once(&g_pidmap_atfork_once, pidmap_register_atfork);
    if (pthread_mutex_lock(&g_pidmap_thread_lock) != 0) {
        errno = EDEADLK;
        return -1;
    }
    struct flock lock = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 1};
    while (fcntl(registry->lock_fd, F_SETLKW, &lock) != 0) {
        if (errno == EINTR) continue;
        (void)pthread_mutex_unlock(&g_pidmap_thread_lock);
        return -1;
    }
    return 0;
}

static void registry_unlock(hl_linux_identity_registry *registry) {
    struct flock lock = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 1};
    (void)fcntl(registry->lock_fd, F_SETLK, &lock);
    (void)pthread_mutex_unlock(&g_pidmap_thread_lock);
}

static void registry_recover_locked(hl_linux_identity_registry_storage *registry) {
    unsigned count = atomic_load_explicit(&registry->journal_count, memory_order_acquire);
    if (count > PIDMAP_JOURNAL_CAPACITY) count = PIDMAP_JOURNAL_CAPACITY;
    uint64_t word = atomic_load_explicit(&registry->commit_word, memory_order_acquire);
    unsigned active = (unsigned)(word & 1u);
    unsigned inactive = active ^ 1u;
    for (unsigned position = 0; position < count; ++position) {
        hl_linux_identity_journal_entry entry = registry->journal[position];
        if (entry.kind >= PIDMAP_KINDS || entry.index >= HL_LINUX_PIDMAP_CAPACITY) continue;
        uint64_t value = atomic_load_explicit(&registry->map[entry.kind].bank[active][entry.index].identity,
                                              memory_order_acquire);
        atomic_store_explicit(&registry->map[entry.kind].bank[inactive][entry.index].identity, value,
                              memory_order_release);
    }
    atomic_store_explicit(&registry->journal_count, 0, memory_order_release);
}

static int registry_publish_journal(hl_linux_identity_registry_storage *registry, uint32_t kind, uint32_t index,
                                    unsigned *count) {
    if (*count >= PIDMAP_JOURNAL_CAPACITY) {
        errno = ENOSPC;
        return -1;
    }
    registry->journal[*count] = (hl_linux_identity_journal_entry){.kind = kind, .index = index};
    ++*count;
    atomic_store_explicit(&registry->journal_count, *count, memory_order_release);
    return 0;
}

static int map_find_guest(const hl_linux_pidmap_storage *map, unsigned bank, int32_t guest) {
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t current_guest, current_host;
        if (slot_snapshot(&map->bank[bank][index], &current_guest, &current_host) && current_guest == guest)
            return (int)index;
    }
    return -1;
}

static int map_find_empty(const hl_linux_pidmap_storage *map, unsigned bank) {
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index)
        if (atomic_load_explicit(&map->bank[bank][index].identity, memory_order_acquire) == 0) return (int)index;
    return -1;
}

static void pidmap_raise_next(hl_linux_pidmap_storage *storage, int32_t guest) {
    int current = atomic_load_explicit(&storage->next_guest, memory_order_relaxed);
    while (current <= guest &&
           !atomic_compare_exchange_weak_explicit(&storage->next_guest, &current,
                                                  guest == INT32_MAX ? INT32_MAX : guest + 1,
                                                  memory_order_relaxed, memory_order_relaxed)) {}
}

static int registry_apply(const hl_linux_pidmap_update *updates, size_t count) {
    if (updates == NULL || count == 0 || count > PIDMAP_JOURNAL_CAPACITY) {
        errno = EINVAL;
        return -1;
    }
    hl_linux_identity_registry *owner = updates[0].map == NULL ? NULL : updates[0].map->registry;
    for (size_t index = 0; index < count; ++index)
        if (updates[index].map == NULL || updates[index].map->registry != owner || updates[index].guest <= 0 ||
            updates[index].host < 0) {
            errno = EINVAL;
            return -1;
        }
    if (registry_lock(owner) != 0) return -1;
    hl_linux_identity_registry_storage *registry = owner->storage;
    registry_recover_locked(registry);
    uint64_t base = atomic_load_explicit(&registry->commit_word, memory_order_acquire);
    unsigned active = (unsigned)(base & 1u);
    unsigned inactive = active ^ 1u;
    unsigned journaled = 0;
    for (size_t position = 0; position < count; ++position) {
        const hl_linux_pidmap_update *update = &updates[position];
        hl_linux_pidmap_storage *map = update->map->storage;
        int slot = map_find_guest(map, active, update->guest);
        if (slot < 0 && update->host > 0) slot = map_find_empty(map, active);
        if (slot < 0) {
            registry_recover_locked(registry);
            registry_unlock(owner);
            errno = update->host == 0 ? ESRCH : ENOSPC;
            return -1;
        }
        if (registry_publish_journal(registry, update->map->kind, (uint32_t)slot, &journaled) != 0) {
            registry_recover_locked(registry);
            registry_unlock(owner);
            return -1;
        }
        atomic_store_explicit(&map->bank[inactive][slot].identity,
                              update->host == 0 ? 0 : identity_pack(update->guest, update->host), memory_order_release);
        if (update->host > 0) pidmap_raise_next(map, update->guest);
    }
    uint64_t committed = ((base >> 1) + 1u) << 1 | inactive;
    atomic_store_explicit(&registry->commit_word, committed, memory_order_release);
    registry_recover_locked(registry);
    registry_unlock(owner);
    return 0;
}

void hl_linux_pidmap_init(hl_linux_pidmap *map) {
    if (map != NULL) memset(map, 0, sizeof *map);
}

int hl_linux_identity_registry_prepare(hl_linux_identity_registry *registry, hl_linux_pidmap *pid,
                                       hl_linux_pidmap *pgid, hl_linux_pidmap *sid) {
    if (registry == NULL || pid == NULL || pgid == NULL || sid == NULL) {
        errno = EINVAL;
        return -1;
    }
    hl_linux_identity_registry_storage *storage = registry_storage();
    if (storage == NULL) return -1;
    int descriptor = registry_file();
    if (descriptor < 0) {
        int saved = errno;
        (void)munmap(storage, sizeof *storage);
        errno = saved;
        return -1;
    }
    registry->storage = storage;
    registry->lock_fd = descriptor;
    hl_linux_pidmap *maps[PIDMAP_KINDS] = {pid, pgid, sid};
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind) {
        maps[kind]->storage = &storage->map[kind];
        maps[kind]->registry = registry;
        maps[kind]->kind = kind;
    }
    return 0;
}

int hl_linux_pidmap_prepare_shared(hl_linux_pidmap *map) {
    if (map == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (map->storage != NULL) return 0;
    hl_linux_identity_registry *registry = calloc(1, sizeof *registry);
    hl_linux_pidmap *spares = calloc(2, sizeof *spares);
    if (registry == NULL || spares == NULL) {
        free(registry);
        free(spares);
        return -1;
    }
    registry->lock_fd = -1;
    if (hl_linux_identity_registry_prepare(registry, map, &spares[0], &spares[1]) != 0) {
        free(registry);
        free(spares);
        return -1;
    }
    return 0;
}

int hl_linux_identity_registry_add(const hl_linux_pidmap_update *updates, size_t count) {
    return registry_apply(updates, count);
}

uint64_t hl_linux_identity_registry_commit_word(const hl_linux_identity_registry *registry) {
    return registry == NULL || registry->storage == NULL
               ? 0
               : atomic_load_explicit(&registry->storage->commit_word, memory_order_acquire);
}

int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host) {
    if (map == NULL || guest <= 0 || host <= 0) return -1;
    if (map->storage == NULL && hl_linux_pidmap_prepare_shared(map) != 0) return -1;
    const hl_linux_pidmap_update update = {.map = map, .guest = guest, .host = host};
    return registry_apply(&update, 1);
}

int32_t hl_linux_pidmap_register_host(hl_linux_pidmap *map, int32_t host) {
    if (map == NULL || host <= 0) return -1;
    if (map->storage == NULL && hl_linux_pidmap_prepare_shared(map) != 0) return -1;
    int32_t guest;
    if (hl_linux_pidmap_guest_checked(map, host, &guest) == 0) return guest;
    guest = atomic_fetch_add_explicit(&map->storage->next_guest, 1, memory_order_relaxed);
    return guest > 0 && guest < INT32_MAX && hl_linux_pidmap_add(map, guest, host) == 0 ? guest : -1;
}

int32_t hl_linux_pidmap_allocate_guest(hl_linux_pidmap *map) {
    if (map == NULL) return -1;
    if (map->storage == NULL && hl_linux_pidmap_prepare_shared(map) != 0) return -1;
    int guest = atomic_fetch_add_explicit(&map->storage->next_guest, 1, memory_order_relaxed);
    return guest > 0 && guest < INT32_MAX ? guest : -1;
}

void hl_linux_pidmap_activate(hl_linux_pidmap *map) {
    if (map == NULL) return;
    if (map->registry != NULL && map->registry->storage != NULL)
        atomic_store_explicit(&map->registry->storage->active, 1, memory_order_release);
    else
        map->active = 1;
}

static int pidmap_active(const hl_linux_pidmap *map) {
    return map != NULL && ((map->registry != NULL && map->registry->storage != NULL)
                               ? atomic_load_explicit(&map->registry->storage->active, memory_order_acquire) != 0
                               : map->active != 0);
}

int hl_linux_pidmap_is_active(const hl_linux_pidmap *map) {
    return pidmap_active(map);
}

static int pidmap_checked(const hl_linux_pidmap *map, int32_t identity, int32_t *translated, int reverse) {
    if (translated == NULL || identity <= 0) return -1;
    if (map != NULL && map->storage != NULL && map->registry != NULL) {
        for (;;) {
            uint64_t before = hl_linux_identity_registry_commit_word(map->registry);
            unsigned bank = (unsigned)(before & 1u);
            int found = 0;
            int32_t value = 0;
            for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
                int32_t guest, host;
                if (!slot_snapshot(&map->storage->bank[bank][index], &guest, &host)) continue;
                int32_t key = reverse ? host : guest;
                if (key == identity) {
                    value = reverse ? guest : host;
                    found = 1;
                    break;
                }
            }
            if (before != hl_linux_identity_registry_commit_word(map->registry)) continue;
            if (found) {
                *translated = value;
                return 0;
            }
            break;
        }
    }
    if (!pidmap_active(map)) {
        *translated = identity;
        return 0;
    }
    return -1;
}

int hl_linux_pidmap_host_checked(const hl_linux_pidmap *map, int32_t guest, int32_t *host) {
    return pidmap_checked(map, guest, host, 0);
}

int hl_linux_pidmap_guest_checked(const hl_linux_pidmap *map, int32_t host, int32_t *guest) {
    return pidmap_checked(map, host, guest, 1);
}

int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host) {
    int32_t guest;
    if (hl_linux_pidmap_guest_checked(map, host, &guest) != 0) return -1;
    const hl_linux_pidmap_update update = {.map = map, .guest = guest, .host = 0};
    return registry_apply(&update, 1);
}

int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest) {
    int32_t host;
    return hl_linux_pidmap_host_checked(map, guest, &host) == 0 ? host : guest;
}

int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host) {
    int32_t guest;
    return hl_linux_pidmap_guest_checked(map, host, &guest) == 0 ? guest : host;
}

size_t hl_linux_pidmap_snapshot(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity) {
    if (map == NULL || map->storage == NULL || map->registry == NULL || (capacity != 0 && entries == NULL)) return 0;
    for (;;) {
        uint64_t before = hl_linux_identity_registry_commit_word(map->registry);
        unsigned bank = (unsigned)(before & 1u);
        size_t count = 0;
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
            int32_t guest, host;
            if (!slot_snapshot(&map->storage->bank[bank][index], &guest, &host)) continue;
            if (count < capacity) entries[count] = (hl_linux_pidmap_entry){.guest = guest, .host = host};
            ++count;
        }
        if (before == hl_linux_identity_registry_commit_word(map->registry)) return count;
    }
}

uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map) {
    size_t count = hl_linux_pidmap_snapshot(map, NULL, 0);
    return count > UINT32_MAX ? UINT32_MAX : (uint32_t)count;
}
