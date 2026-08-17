#ifndef _XOPEN_SOURCE
#define _XOPEN_SOURCE 700
#endif

#include "pidmap.h"
#include "hl/base.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MAP_ANONYMOUS
#define MAP_ANONYMOUS MAP_ANON
#endif

enum {
    PIDMAP_BANKS = 2,
    PIDMAP_KINDS = 3,
    PIDMAP_JOURNAL_CAPACITY = 2 * HL_LINUX_PIDMAP_CAPACITY + 1,
    PIDMAP_SEMANTIC_NONE = 0,
    PIDMAP_SEMANTIC_SETPGID = 1,
    PIDMAP_SEMANTIC_SETSID = 2,
};

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
    int32_t semantic_guest_process;
    int32_t semantic_host_process;
    int32_t semantic_guest_group;
    int32_t semantic_host_group;
    int32_t semantic_original_group;
    int32_t semantic_original_session;
    atomic_uint semantic;
    atomic_uint poisoned;
    hl_linux_pidmap_storage map[PIDMAP_KINDS];
};

static pthread_mutex_t g_pidmap_thread_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t g_pidmap_atfork_once = PTHREAD_ONCE_INIT;

#if defined(HL_NATIVE_TEST_HOOKS)
static int g_pidmap_test_crash_phase;

static void pidmap_test_crash(int phase) {
    if (g_pidmap_test_crash_phase == phase) _exit(190 + phase);
}
#else
static void pidmap_test_crash(int phase) {
    (void)phase;
}
#endif

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
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind)
        atomic_init(&storage->map[kind].next_guest, 1);
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

static void registry_recover_banks_locked(hl_linux_identity_registry_storage *registry) {
    unsigned count = atomic_load_explicit(&registry->journal_count, memory_order_acquire);
    if (count > PIDMAP_JOURNAL_CAPACITY) count = PIDMAP_JOURNAL_CAPACITY;
    uint64_t word = atomic_load_explicit(&registry->commit_word, memory_order_acquire);
    unsigned active = (unsigned)(word & 1u);
    unsigned inactive = active ^ 1u;
    for (unsigned position = 0; position < count; ++position) {
        hl_linux_identity_journal_entry entry = registry->journal[position];
        if (entry.kind >= PIDMAP_KINDS || entry.index >= HL_LINUX_PIDMAP_CAPACITY) continue;
        uint64_t value =
            atomic_load_explicit(&registry->map[entry.kind].bank[active][entry.index].identity, memory_order_acquire);
        atomic_store_explicit(&registry->map[entry.kind].bank[inactive][entry.index].identity, value,
                              memory_order_release);
        if (position == 0) pidmap_test_crash(5);
    }
    atomic_store_explicit(&registry->journal_count, 0, memory_order_release);
}

static int registry_apply_staged_locked(hl_linux_identity_registry *owner, const hl_linux_pidmap_update *updates,
                                        size_t count);

static int registry_recover_semantic_locked(hl_linux_identity_registry *owner) {
    hl_linux_identity_registry_storage *registry = owner->storage;
    unsigned semantic = atomic_load_explicit(&registry->semantic, memory_order_acquire);
    if (semantic == PIDMAP_SEMANTIC_NONE) return 0;
    int32_t host_process = registry->semantic_host_process;
    int32_t guest_group = registry->semantic_guest_group;
    int32_t host_group = registry->semantic_host_group;
    hl_linux_pidmap_update updates[2];
    size_t count = 0;
    if (semantic == PIDMAP_SEMANTIC_SETPGID) {
        pid_t actual = getpgid((pid_t)host_process);
        if (actual == (pid_t)host_group)
            updates[count++] = (hl_linux_pidmap_update){
                .map = owner->map[1],
                .guest = guest_group,
                .host = host_group,
            };
        else if (actual != (pid_t)registry->semantic_original_group) {
            atomic_store_explicit(&registry->poisoned, 1, memory_order_release);
            return -1;
        }
    } else if (semantic == PIDMAP_SEMANTIC_SETSID) {
        pid_t actual_group = getpgid((pid_t)host_process);
        pid_t actual_session = getsid((pid_t)host_process);
        if (actual_group == (pid_t)host_process && actual_session == (pid_t)host_process) {
            updates[count++] = (hl_linux_pidmap_update){
                .map = owner->map[1], .guest = registry->semantic_guest_process, .host = host_process};
            updates[count++] = (hl_linux_pidmap_update){
                .map = owner->map[2], .guest = registry->semantic_guest_process, .host = host_process};
        } else if (actual_group != (pid_t)registry->semantic_original_group ||
                   actual_session != (pid_t)registry->semantic_original_session) {
            atomic_store_explicit(&registry->poisoned, 1, memory_order_release);
            return -1;
        }
    }
    if (count != 0 && registry_apply_staged_locked(owner, updates, count) != 0) {
        atomic_store_explicit(&registry->poisoned, 1, memory_order_release);
        return -1;
    }
    atomic_store_explicit(&registry->semantic, PIDMAP_SEMANTIC_NONE, memory_order_release);
    return 0;
}

static int registry_recover_locked(hl_linux_identity_registry *owner) {
    registry_recover_banks_locked(owner->storage);
    return registry_recover_semantic_locked(owner);
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
    while (current <= guest && !atomic_compare_exchange_weak_explicit(&storage->next_guest, &current,
                                                                      guest == INT32_MAX ? INT32_MAX : guest + 1,
                                                                      memory_order_relaxed, memory_order_relaxed)) {}
}

static int registry_apply_staged_locked(hl_linux_identity_registry *owner, const hl_linux_pidmap_update *updates,
                                        size_t count) {
    hl_linux_identity_registry_storage *registry = owner->storage;
    uint64_t base = atomic_load_explicit(&registry->commit_word, memory_order_acquire);
    unsigned active = (unsigned)(base & 1u);
    unsigned inactive = active ^ 1u;
    unsigned journaled = 0;
    for (size_t position = 0; position < count; ++position) {
        const hl_linux_pidmap_update *update = &updates[position];
        hl_linux_pidmap_storage *map = update->map->storage;
        // Recovery has made the banks identical. Search the staged bank so a later update in this same
        // transaction observes every slot reserved by an earlier update.
        int slot = map_find_guest(map, inactive, update->guest);
        if (slot < 0 && update->host > 0) slot = map_find_empty(map, inactive);
        if (slot < 0) {
            registry_recover_banks_locked(registry);
            errno = update->host == 0 ? ESRCH : ENOSPC;
            return -1;
        }
        if (registry_publish_journal(registry, update->map->kind, (uint32_t)slot, &journaled) != 0) {
            registry_recover_banks_locked(registry);
            return -1;
        }
        if (position == 0) pidmap_test_crash(2);
        atomic_store_explicit(&map->bank[inactive][slot].identity,
                              update->host == 0 ? 0 : identity_pack(update->guest, update->host), memory_order_release);
        if (position == 0) pidmap_test_crash(3);
        if (update->host > 0) pidmap_raise_next(map, update->guest);
    }
    uint64_t committed = ((base >> 1) + 1u) << 1 | inactive;
    atomic_store_explicit(&registry->commit_word, committed, memory_order_release);
    pidmap_test_crash(4);
    registry_recover_banks_locked(registry);
    return 0;
}

static int registry_apply_locked(hl_linux_identity_registry *owner, const hl_linux_pidmap_update *updates,
                                 size_t count) {
    pidmap_test_crash(1);
    if (registry_recover_locked(owner) != 0 ||
        atomic_load_explicit(&owner->storage->poisoned, memory_order_acquire) != 0) {
        errno = EIO;
        return -1;
    }
    return registry_apply_staged_locked(owner, updates, count);
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
    int result = registry_apply_locked(owner, updates, count);
    registry_unlock(owner);
    return result;
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
        registry->map[kind] = maps[kind];
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

static int registry_semantic_begin(hl_linux_identity_registry *owner, unsigned semantic, int32_t guest_process,
                                   int32_t host_process, int32_t guest_group, int32_t host_group) {
    if (registry_lock(owner) != 0) return -1;
    if (registry_recover_locked(owner) != 0 ||
        atomic_load_explicit(&owner->storage->poisoned, memory_order_acquire) != 0) {
        registry_unlock(owner);
        errno = EIO;
        return -1;
    }
    int32_t original_group = (int32_t)getpgid((pid_t)host_process);
    int32_t original_session = (int32_t)getsid((pid_t)host_process);
    if (original_group <= 0 || original_session <= 0) {
        int saved = errno;
        registry_unlock(owner);
        errno = saved;
        return -1;
    }
    owner->storage->semantic_guest_process = guest_process;
    owner->storage->semantic_host_process = host_process;
    owner->storage->semantic_guest_group = guest_group;
    owner->storage->semantic_host_group = host_group;
    owner->storage->semantic_original_group = original_group;
    owner->storage->semantic_original_session = original_session;
    atomic_store_explicit(&owner->storage->semantic, semantic, memory_order_release);
    pidmap_test_crash(8);
    return 0;
}

int hl_linux_identity_registry_setsid(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, hl_linux_pidmap *sid, int32_t guest,
                                      int32_t *host_sid) {
    if (pid == NULL || pgid == NULL || sid == NULL || host_sid == NULL || guest <= 0 || pid->registry == NULL ||
        pgid->registry != pid->registry || sid->registry != pid->registry) {
        errno = EINVAL;
        return -1;
    }
    hl_linux_identity_registry *owner = pid->registry;
    int32_t host = (int32_t)getpid();
    if (registry_semantic_begin(owner, PIDMAP_SEMANTIC_SETSID, guest, host, guest, host) != 0) return -1;
    pid_t result = setsid();
    if (result < 0) {
        int saved = errno;
        atomic_store_explicit(&owner->storage->semantic, PIDMAP_SEMANTIC_NONE, memory_order_release);
        registry_unlock(owner);
        errno = saved;
        return -1;
    }
    pidmap_test_crash(9);
    int recovered = registry_recover_semantic_locked(owner);
    if (recovered != 0) atomic_store_explicit(&owner->storage->poisoned, 1, memory_order_release);
    registry_unlock(owner);
    if (recovered != 0) {
        errno = EIO;
        return -1;
    }
    *host_sid = (int32_t)result;
    return 0;
}

int hl_linux_identity_registry_setpgid(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, int32_t guest_process,
                                       int32_t host_process, int32_t guest_group, int32_t host_group) {
    if (pid == NULL || pgid == NULL || guest_process <= 0 || guest_group <= 0 || host_process < 0 || host_group < 0 ||
        pid->registry == NULL || pgid->registry != pid->registry) {
        errno = EINVAL;
        return -1;
    }
    int32_t concrete_process = host_process == 0 ? (int32_t)getpid() : host_process;
    int32_t concrete_group = host_group == 0 ? concrete_process : host_group;
    hl_linux_identity_registry *owner = pid->registry;
    if (registry_semantic_begin(owner, PIDMAP_SEMANTIC_SETPGID, guest_process, concrete_process, guest_group,
                                concrete_group) != 0)
        return -1;
    if (setpgid((pid_t)host_process, (pid_t)host_group) != 0) {
        int saved = errno;
        atomic_store_explicit(&owner->storage->semantic, PIDMAP_SEMANTIC_NONE, memory_order_release);
        registry_unlock(owner);
        errno = saved;
        return -1;
    }
    pidmap_test_crash(9);
    int recovered = registry_recover_semantic_locked(owner);
    if (recovered != 0) atomic_store_explicit(&owner->storage->poisoned, 1, memory_order_release);
    registry_unlock(owner);
    if (recovered != 0) {
        errno = EIO;
        return -1;
    }
    return 0;
}

static int registry_host_identity_referenced(const hl_linux_pidmap *pid, unsigned bank, int32_t removed_host,
                                             int32_t typed_host, int session) {
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t guest, host;
        if (!slot_snapshot(&pid->storage->bank[bank][index], &guest, &host) || host == removed_host) continue;
        pid_t actual = session ? getsid((pid_t)host) : getpgid((pid_t)host);
        if (actual == (pid_t)typed_host) return 1;
    }
    return 0;
}

int hl_linux_identity_registry_reap(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, hl_linux_pidmap *sid,
                                    int32_t host_process) {
    if (pid == NULL || pgid == NULL || sid == NULL || host_process <= 0 || pid->registry == NULL ||
        pgid->registry != pid->registry || sid->registry != pid->registry) {
        errno = EINVAL;
        return -1;
    }
    hl_linux_identity_registry *owner = pid->registry;
    if (registry_lock(owner) != 0) return -1;
    if (registry_recover_locked(owner) != 0) {
        registry_unlock(owner);
        return -1;
    }
    uint64_t word = atomic_load_explicit(&owner->storage->commit_word, memory_order_acquire);
    unsigned bank = (unsigned)(word & 1u);
    int32_t guest_process = -1;
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t guest, host;
        if (slot_snapshot(&pid->storage->bank[bank][index], &guest, &host) && host == host_process) {
            guest_process = guest;
            break;
        }
    }
    if (guest_process <= 0) {
        registry_unlock(owner);
        errno = ESRCH;
        return -1;
    }
    hl_linux_pidmap_update removals[PIDMAP_JOURNAL_CAPACITY];
    size_t count = 0;
    removals[count++] = (hl_linux_pidmap_update){.map = pid, .guest = guest_process, .host = 0};
    hl_linux_pidmap *derived[2] = {pgid, sid};
    for (uint32_t kind = 0; kind < 2 && count < PIDMAP_JOURNAL_CAPACITY; ++kind) {
        hl_linux_pidmap *map = derived[kind];
        for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY && count < PIDMAP_JOURNAL_CAPACITY; ++index) {
            int32_t guest, host;
            if (!slot_snapshot(&map->storage->bank[bank][index], &guest, &host)) continue;
            if (!registry_host_identity_referenced(pid, bank, host_process, host, kind == 1))
                removals[count++] = (hl_linux_pidmap_update){.map = map, .guest = guest, .host = 0};
        }
    }
    int result = registry_apply_staged_locked(owner, removals, count);
    if (result != 0) atomic_store_explicit(&owner->storage->poisoned, 1, memory_order_release);
    registry_unlock(owner);
    return result;
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
    hl_linux_identity_registry *owner = map->registry;
    if (registry_lock(owner) != 0) return -1;
    if (registry_recover_locked(owner) != 0) {
        registry_unlock(owner);
        return -1;
    }
    uint64_t word = atomic_load_explicit(&owner->storage->commit_word, memory_order_acquire);
    unsigned bank = (unsigned)(word & 1u);
    for (uint32_t index = 0; index < HL_LINUX_PIDMAP_CAPACITY; ++index) {
        int32_t guest, current_host;
        if (slot_snapshot(&map->storage->bank[bank][index], &guest, &current_host) && current_host == host) {
            registry_unlock(owner);
            return guest;
        }
    }
    int32_t guest = atomic_fetch_add_explicit(&map->storage->next_guest, 1, memory_order_relaxed);
    const hl_linux_pidmap_update update = {.map = map, .guest = guest, .host = host};
    int result = guest > 0 && guest < INT32_MAX ? registry_apply_locked(owner, &update, 1) : -1;
    registry_unlock(owner);
    return result == 0 ? guest : -1;
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
        if (atomic_load_explicit(&map->registry->storage->poisoned, memory_order_acquire) != 0) {
            errno = EIO;
            return -1;
        }
        if (atomic_load_explicit(&map->registry->storage->semantic, memory_order_acquire) != PIDMAP_SEMANTIC_NONE) {
            errno = EAGAIN;
            return -1;
        }
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
            if (atomic_load_explicit(&map->registry->storage->poisoned, memory_order_acquire) != 0) {
                errno = EIO;
                return -1;
            }
            if (atomic_load_explicit(&map->registry->storage->semantic, memory_order_acquire) != PIDMAP_SEMANTIC_NONE) {
                errno = EAGAIN;
                return -1;
            }
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

int hl_linux_pidmap_snapshot_checked(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity,
                                     size_t *count_out) {
    if (map == NULL || map->storage == NULL || map->registry == NULL || count_out == NULL ||
        (capacity != 0 && entries == NULL)) {
        errno = EINVAL;
        return -1;
    }
    if (atomic_load_explicit(&map->registry->storage->poisoned, memory_order_acquire) != 0) {
        errno = EIO;
        return -1;
    }
    if (atomic_load_explicit(&map->registry->storage->semantic, memory_order_acquire) != PIDMAP_SEMANTIC_NONE) {
        errno = EAGAIN;
        return -1;
    }
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
        if (before == hl_linux_identity_registry_commit_word(map->registry) &&
            atomic_load_explicit(&map->registry->storage->poisoned, memory_order_acquire) == 0 &&
            atomic_load_explicit(&map->registry->storage->semantic, memory_order_acquire) == PIDMAP_SEMANTIC_NONE) {
            *count_out = count;
            return 0;
        }
        if (atomic_load_explicit(&map->registry->storage->poisoned, memory_order_acquire) != 0) {
            errno = EIO;
            return -1;
        }
        if (atomic_load_explicit(&map->registry->storage->semantic, memory_order_acquire) != PIDMAP_SEMANTIC_NONE) {
            errno = EAGAIN;
            return -1;
        }
    }
}

size_t hl_linux_pidmap_snapshot(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity) {
    size_t count = 0;
    return hl_linux_pidmap_snapshot_checked(map, entries, capacity, &count) == 0 ? count : 0;
}

uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map) {
    size_t count = hl_linux_pidmap_snapshot(map, NULL, 0);
    return count > UINT32_MAX ? UINT32_MAX : (uint32_t)count;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int pidmap_test_prepare(hl_linux_identity_registry *registry, hl_linux_pidmap maps[PIDMAP_KINDS]) {
    memset(registry, 0, sizeof *registry);
    registry->lock_fd = -1;
    memset(maps, 0, sizeof(*maps) * PIDMAP_KINDS);
    return hl_linux_identity_registry_prepare(registry, &maps[0], &maps[1], &maps[2]);
}

static int pidmap_test_values(hl_linux_pidmap maps[PIDMAP_KINDS], int32_t guest, int32_t expected) {
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind) {
        int32_t host = 0;
        if (hl_linux_pidmap_host_checked(&maps[kind], guest, &host) != 0 || host != expected) return -1;
    }
    return 0;
}

static int pidmap_test_crash_recovery(uint32_t scenario) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    hl_linux_pidmap_update initial[PIDMAP_KINDS];
    hl_linux_pidmap_update replacement[PIDMAP_KINDS];
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind) {
        initial[kind] = (hl_linux_pidmap_update){.map = &maps[kind], .guest = 10, .host = 110};
        replacement[kind] = (hl_linux_pidmap_update){.map = &maps[kind], .guest = 10, .host = 120};
    }
    if (registry_apply(initial, PIDMAP_KINDS) != 0) return -1;
    pid_t child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        g_pidmap_test_crash_phase = (int)scenario;
        int result = registry_apply(replacement, PIDMAP_KINDS);
        _exit(result == 0 ? 80 : 81);
    }
    int status = 0;
    while (waitpid(child, &status, 0) < 0)
        if (errno != EINTR) return -1;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 190 + (int)scenario) return -1;
    if (registry_lock(&registry) != 0) return -1;
    if (registry_recover_locked(&registry) != 0) {
        registry_unlock(&registry);
        return -1;
    }
    registry_unlock(&registry);
    return pidmap_test_values(maps, 10, scenario < 4 ? 110 : 120);
}

static int pidmap_test_concurrent(uint32_t iterations) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    hl_linux_pidmap_update updates[PIDMAP_KINDS];
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind)
        updates[kind] = (hl_linux_pidmap_update){.map = &maps[kind], .guest = 10, .host = 110};
    if (registry_apply(updates, PIDMAP_KINDS) != 0) return -1;
    pid_t child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        for (uint32_t iteration = 0; iteration < iterations; ++iteration) {
            int32_t host = (iteration & 1u) == 0 ? 120 : 110;
            for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind)
                updates[kind].host = host;
            if (registry_apply(updates, PIDMAP_KINDS) != 0) _exit(82);
        }
        _exit(0);
    }
    int status = 0;
    for (;;) {
        pid_t waited = waitpid(child, &status, WNOHANG);
        if (waited < 0) return -1;
        uint64_t before = hl_linux_identity_registry_commit_word(&registry);
        int32_t values[PIDMAP_KINDS];
        int valid = 1;
        for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind)
            valid &= hl_linux_pidmap_host_checked(&maps[kind], 10, &values[kind]) == 0;
        uint64_t after = hl_linux_identity_registry_commit_word(&registry);
        if (before == after && (!valid || values[0] != values[1] || values[1] != values[2])) {
            (void)kill(child, SIGKILL);
            (void)waitpid(child, &status, 0);
            return -1;
        }
        if (waited == child) break;
    }
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static int pidmap_test_churn(uint32_t iterations) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    for (uint32_t iteration = 0; iteration < iterations; ++iteration) {
        int32_t guest = (int32_t)iteration + 1;
        int32_t host = (int32_t)iteration + 10000;
        if (hl_linux_pidmap_add(&maps[0], guest, host) != 0 || hl_linux_pidmap_remove_host(&maps[0], host) != 0 ||
            hl_linux_pidmap_count(&maps[0]) != 0)
            return -1;
    }
    return 0;
}

static int pidmap_test_semantic_death(uint32_t scenario) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    for (uint32_t kind = 0; kind < PIDMAP_KINDS; ++kind)
        hl_linux_pidmap_activate(&maps[kind]);
    int descriptors[2];
    if (pipe(descriptors) != 0) return -1;
    pid_t writer = fork();
    if (writer < 0) return -1;
    if (writer == 0) {
        (void)close(descriptors[0]);
        pid_t target = fork();
        if (target < 0) _exit(84);
        if (target == 0) {
            (void)close(descriptors[1]);
            for (;;)
                pause();
        }
        ssize_t written;
        do {
            written = write(descriptors[1], &target, sizeof target);
        } while (written < 0 && errno == EINTR);
        (void)close(descriptors[1]);
        if (written != (ssize_t)sizeof target || hl_linux_pidmap_add(&maps[0], 20, (int32_t)target) != 0) _exit(85);
        g_pidmap_test_crash_phase = (int)scenario;
        (void)hl_linux_identity_registry_setpgid(&maps[0], &maps[1], 20, (int32_t)target, 20, (int32_t)target);
        _exit(86);
    }
    (void)close(descriptors[1]);
    pid_t target = -1;
    ssize_t received;
    do {
        received = read(descriptors[0], &target, sizeof target);
    } while (received < 0 && errno == EINTR);
    (void)close(descriptors[0]);
    int status = 0;
    while (waitpid(writer, &status, 0) < 0)
        if (errno != EINTR) return -1;
    if (received != (ssize_t)sizeof target || target <= 0 || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 190 + (int)scenario || kill(target, 0) != 0)
        return -1;
    if (scenario == 9) {
        // Kill the first recovery writer after it has flipped the semantic repair but before mirroring.
        pid_t recovery = fork();
        if (recovery < 0) return -1;
        if (recovery == 0) {
            g_pidmap_test_crash_phase = 5;
            if (registry_lock(&registry) != 0) _exit(87);
            (void)registry_recover_locked(&registry);
            _exit(88);
        }
        while (waitpid(recovery, &status, 0) < 0)
            if (errno != EINTR) return -1;
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 195) return -1;
    }
    if (registry_lock(&registry) != 0) return -1;
    int recovered = registry_recover_locked(&registry);
    registry_unlock(&registry);
    int32_t mapped = 0;
    int mapping = hl_linux_pidmap_host_checked(&maps[1], 20, &mapped);
    pid_t actual = getpgid(target);
    int result = scenario == 8 ? (recovered == 0 && actual != target && mapping != 0)
                               : (recovered == 0 && actual == target && mapping == 0 && mapped == target);
    (void)kill(target, SIGKILL);
    return result ? 0 : -1;
}

static int pidmap_test_multiple_new(void) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    const hl_linux_pidmap_update updates[] = {
        {.map = &maps[0], .guest = 10, .host = 110},
        {.map = &maps[0], .guest = 11, .host = 111},
    };
    int32_t first = 0, second = 0;
    return registry_apply(updates, sizeof updates / sizeof updates[0]) == 0 &&
                   hl_linux_pidmap_host_checked(&maps[0], 10, &first) == 0 && first == 110 &&
                   hl_linux_pidmap_host_checked(&maps[0], 11, &second) == 0 && second == 111 &&
                   hl_linux_pidmap_count(&maps[0]) == 2
               ? 0
               : -1;
}

static int pidmap_test_same_host_registration(uint32_t workers) {
    hl_linux_identity_registry registry;
    hl_linux_pidmap maps[PIDMAP_KINDS];
    if (pidmap_test_prepare(&registry, maps) != 0) return -1;
    if (workers == 0 || workers > 64) workers = 32;
    pid_t children[64];
    for (uint32_t index = 0; index < workers; ++index) {
        children[index] = fork();
        if (children[index] < 0) return -1;
        if (children[index] == 0) _exit(hl_linux_pidmap_register_host(&maps[0], 4242) > 0 ? 0 : 83);
    }
    for (uint32_t index = 0; index < workers; ++index) {
        int status = 0;
        while (waitpid(children[index], &status, 0) < 0)
            if (errno != EINTR) return -1;
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return -1;
    }
    int32_t guest = 0;
    return hl_linux_pidmap_guest_checked(&maps[0], 4242, &guest) == 0 && guest > 0 &&
                   hl_linux_pidmap_count(&maps[0]) == 1
               ? 0
               : -1;
}

HL_API int hl_c_backend_identity_registry_test(uint32_t scenario, uint32_t iterations) {
    if (scenario >= 1 && scenario <= 5) return pidmap_test_crash_recovery(scenario);
    if (scenario == 6) return pidmap_test_concurrent(iterations == 0 ? 10000 : iterations);
    if (scenario == 7) return pidmap_test_churn(iterations == 0 ? 2 * HL_LINUX_PIDMAP_CAPACITY + 32 : iterations);
    if (scenario == 8 || scenario == 9) return pidmap_test_semantic_death(scenario);
    if (scenario == 10) return pidmap_test_multiple_new();
    if (scenario == 11) return pidmap_test_same_host_registration(iterations);
    errno = EINVAL;
    return -1;
}
#endif
