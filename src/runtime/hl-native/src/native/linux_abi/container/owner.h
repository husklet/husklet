#ifndef HL_LINUX_OWNER_H
#define HL_LINUX_OWNER_H

#include <stdatomic.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "../host_mman.h"
#include <sys/stat.h>
#include <unistd.h>
#include <time.h>

#include "ownership/registry.h"

#if defined(__linux__)
#include <fcntl.h>
#include <linux/stat.h>
#include <sys/syscall.h>
#endif

typedef struct hl_owner_entry {
    _Atomic uint32_t active;
    _Atomic uint32_t metadata;
    uint64_t device;
    uint64_t object;
    uint64_t birth_ns;
    _Atomic uint32_t uid;
    _Atomic uint32_t gid;
} hl_owner_entry;

typedef struct hl_owner_table {
    uint64_t capacity;
    hl_owner_entry entries[];
} hl_owner_table;

static hl_owner_table *g_owner_table;
static size_t g_owner_table_size;

typedef struct hl_socket_owner_runtime {
    _Atomic uint64_t generation;
    _Atomic uint64_t writer;
    _Atomic uint64_t next_writer;
    unsigned char registry[];
} hl_socket_owner_runtime;

static hl_socket_owner_runtime *g_socket_owner_runtime;
static hl_owner_registry *g_socket_owner_registry;
static size_t g_socket_owner_runtime_size;

static hl_owner_namespace hl_socket_owner_namespace(void) {
    hl_owner_namespace namespace = {0};
    if (g_socket_owner_runtime != NULL) {
        namespace.generation = &g_socket_owner_runtime->generation;
        namespace.owner = &g_socket_owner_runtime->writer;
    }
    return namespace;
}

static int hl_socket_owner_runtime_init(void) {
    size_t registry_size;
    struct timespec now;
    uint64_t epoch;
    if (g_socket_owner_registry != NULL) return 0;
    registry_size = hl_owner_registry_size(HL_OWNER_REGISTRY_DEFAULT_CAPACITY);
    if (registry_size == 0 || registry_size > SIZE_MAX - sizeof(hl_socket_owner_runtime)) return -1;
    g_socket_owner_runtime_size = sizeof(hl_socket_owner_runtime) + registry_size;
    g_socket_owner_runtime = mmap(NULL, g_socket_owner_runtime_size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON,
                                  -1, 0);
    if (g_socket_owner_runtime == MAP_FAILED) {
        g_socket_owner_runtime = NULL;
        g_socket_owner_runtime_size = 0;
        return -1;
    }
    g_socket_owner_registry = (hl_owner_registry *)g_socket_owner_runtime->registry;
    memset(&now, 0, sizeof now);
    (void)clock_gettime(CLOCK_MONOTONIC, &now);
    epoch = ((uint64_t)(uint32_t)getpid() << 32) ^ (uint64_t)now.tv_sec ^ (uint64_t)now.tv_nsec;
    if (epoch == 0) epoch = 1;
    if (hl_owner_registry_init(g_socket_owner_registry, registry_size, HL_OWNER_REGISTRY_DEFAULT_CAPACITY, epoch) !=
        0) {
        (void)munmap(g_socket_owner_runtime, g_socket_owner_runtime_size);
        g_socket_owner_runtime = NULL;
        g_socket_owner_registry = NULL;
        g_socket_owner_runtime_size = 0;
        return -1;
    }
    return 0;
}

static int hl_socket_owner_writer_begin(hl_owner_writer *writer) {
    uint64_t generation;
    uint64_t identity;
    if (writer == NULL || g_socket_owner_runtime == NULL) return EINVAL;
    for (;;) {
        generation = atomic_load_explicit(&g_socket_owner_runtime->generation, memory_order_acquire);
        if (generation == HL_OWNER_NAMESPACE_POISON) return EOWNERDEAD;
        if ((generation & 1u) != 0) {
            uint64_t owner = atomic_load_explicit(&g_socket_owner_runtime->writer, memory_order_acquire);
            if (owner != 0) {
                pid_t writer_pid = (pid_t)(uint32_t)(owner >> 32);
                if (writer_pid > 0 && kill(writer_pid, 0) != 0 && errno == ESRCH) {
                    (void)atomic_compare_exchange_strong_explicit(
                        &g_socket_owner_runtime->generation, &generation, HL_OWNER_NAMESPACE_POISON,
                        memory_order_acq_rel, memory_order_acquire);
                }
            }
            sched_yield();
            continue;
        }
        if (generation >= HL_OWNER_NAMESPACE_POISON - 1u) return EOVERFLOW;
        if (!atomic_compare_exchange_weak_explicit(&g_socket_owner_runtime->generation, &generation, generation + 1u,
                                                   memory_order_acq_rel, memory_order_acquire))
            continue;
        identity = atomic_fetch_add_explicit(&g_socket_owner_runtime->next_writer, 1, memory_order_relaxed) + 1u;
        if (identity == 0 || identity > UINT32_MAX) {
            atomic_store_explicit(&g_socket_owner_runtime->generation, HL_OWNER_NAMESPACE_POISON,
                                  memory_order_release);
            return EOVERFLOW;
        }
        identity |= (uint64_t)(uint32_t)getpid() << 32;
        atomic_store_explicit(&g_socket_owner_runtime->writer, identity, memory_order_release);
        *writer = (hl_owner_writer){generation + 1u, identity};
        return 0;
    }
}

static void hl_socket_owner_writer_end(hl_owner_writer writer) {
    if (g_socket_owner_runtime == NULL) return;
    atomic_store_explicit(&g_socket_owner_runtime->writer, 0, memory_order_relaxed);
    atomic_store_explicit(&g_socket_owner_runtime->generation, writer.generation + 1u, memory_order_release);
}

static hl_owner_key hl_socket_owner_key(const struct stat *status, uint64_t birth_ns) {
    return (hl_owner_key){(uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns};
}

static uint64_t hl_owner_birth(const char *path, int fd, int nofollow, const struct stat *fallback) {
#if defined(__APPLE__)
    (void)path;
    (void)fd;
    (void)nofollow;
    return (uint64_t)fallback->st_birthtimespec.tv_sec * UINT64_C(1000000000) +
           (uint64_t)fallback->st_birthtimespec.tv_nsec;
#elif defined(__linux__) && defined(SYS_statx)
    struct statx status;
    int flags = fd >= 0 ? AT_EMPTY_PATH : nofollow ? AT_SYMLINK_NOFOLLOW : 0;
    const char *name = fd >= 0 ? "" : path;
    int directory = fd >= 0 ? fd : AT_FDCWD;
    memset(&status, 0, sizeof(status));
    if (name != NULL && syscall(SYS_statx, directory, name, flags, STATX_BTIME, &status) == 0 &&
        (status.stx_mask & STATX_BTIME) != 0)
        return (uint64_t)status.stx_btime.tv_sec * UINT64_C(1000000000) + (uint64_t)status.stx_btime.tv_nsec;
    (void)fallback;
    return 0;
#else
    (void)path;
    (void)fd;
    (void)nofollow;
    (void)fallback;
    return 0;
#endif
}

static uint64_t hl_owner_hash(uint64_t device, uint64_t object, uint64_t birth_ns) {
    uint64_t value = device ^ (object + UINT64_C(0x9e3779b97f4a7c15) + (device << 6) + (device >> 2));
    value ^= birth_ns + UINT64_C(0x9e3779b97f4a7c15) + (value << 6) + (value >> 2);
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value *= UINT64_C(0x94d049bb133111eb);
    return value ^ (value >> 31);
}

static int hl_socket_owner_lookup(const struct stat *status, uint64_t birth_ns, int64_t *uid, int64_t *gid) {
    hl_owner_value value;
    int result;
    if (g_socket_owner_registry == NULL || birth_ns == 0) return 0;
    for (;;) {
        result = hl_owner_registry_lookup(g_socket_owner_registry, hl_socket_owner_namespace(),
                                          hl_socket_owner_key(status, birth_ns), &value);
        if (result != -EAGAIN) break;
        uint64_t owner = atomic_load_explicit(&g_socket_owner_runtime->writer, memory_order_acquire);
        if (owner != 0) {
            pid_t writer_pid = (pid_t)(uint32_t)(owner >> 32);
            if (writer_pid > 0 && kill(writer_pid, 0) != 0 && errno == ESRCH) {
                uint64_t generation = atomic_load_explicit(&g_socket_owner_runtime->generation, memory_order_acquire);
                if ((generation & 1u) != 0)
                    (void)atomic_compare_exchange_strong_explicit(
                        &g_socket_owner_runtime->generation, &generation, HL_OWNER_NAMESPACE_POISON,
                        memory_order_acq_rel, memory_order_acquire);
            }
        }
        sched_yield();
    }
    if (result == -EOWNERDEAD) return -1;
    if (result != HL_OWNER_FOUND) return 0;
    *uid = value.uid;
    *gid = value.gid;
    return 1;
}

static int hl_socket_owner_update(const struct stat *status, uint64_t birth_ns, int64_t uid, int64_t gid) {
    hl_owner_writer writer;
    hl_owner_value value;
    hl_owner_key key;
    int found;
    int error;
    if (g_socket_owner_registry == NULL || birth_ns == 0 || (uid < 0 && gid < 0) ||
        (uid >= 0 && (uint64_t)uid > UINT32_MAX) || (gid >= 0 && (uint64_t)gid > UINT32_MAX))
        return 0;
    key = hl_socket_owner_key(status, birth_ns);
    found = hl_owner_registry_lookup(g_socket_owner_registry, hl_socket_owner_namespace(), key, &value);
    if (found != HL_OWNER_FOUND) return 0;
    error = hl_socket_owner_writer_begin(&writer);
    if (error != 0) return -error;
    error = hl_owner_registry_update(g_socket_owner_registry, hl_socket_owner_namespace(), writer, key,
                                     uid < 0 ? value.uid : (uint32_t)uid,
                                     gid < 0 ? value.gid : (uint32_t)gid);
    hl_socket_owner_writer_end(writer);
    return error == 0 ? 1 : -error;
}

typedef struct hl_socket_owner_publication {
    hl_owner_writer writer;
    hl_owner_ticket ticket;
    int active;
} hl_socket_owner_publication;

static int hl_socket_owner_prepare(hl_socket_owner_publication *publication) {
    int error;
    if (publication == NULL || g_socket_owner_registry == NULL) return EINVAL;
    memset(publication, 0, sizeof *publication);
    error = hl_socket_owner_writer_begin(&publication->writer);
    if (error != 0) return error;
    publication->active = 1;
    error = hl_owner_registry_reserve(g_socket_owner_registry, hl_socket_owner_namespace(), publication->writer,
                                      &publication->ticket);
    if (error != 0) {
        hl_socket_owner_writer_end(publication->writer);
        publication->active = 0;
    }
    return error;
}

static int hl_socket_owner_publish(hl_socket_owner_publication *publication, const struct stat *status,
                                   uint64_t birth_ns, uint32_t uid, uint32_t gid) {
    int error;
    if (publication == NULL || !publication->active || status == NULL || birth_ns == 0) return EINVAL;
    error = hl_owner_registry_commit(g_socket_owner_registry, hl_socket_owner_namespace(), publication->writer,
                                     publication->ticket, hl_socket_owner_key(status, birth_ns),
                                     (hl_owner_value){uid, gid, 1, 1});
    hl_socket_owner_writer_end(publication->writer);
    publication->active = 0;
    return error;
}

static void hl_socket_owner_cancel(hl_socket_owner_publication *publication) {
    if (publication == NULL || !publication->active) return;
    (void)hl_owner_registry_cancel(g_socket_owner_registry, hl_socket_owner_namespace(), publication->writer,
                                   publication->ticket);
    hl_socket_owner_writer_end(publication->writer);
    publication->active = 0;
}

static int hl_socket_owner_reference(hl_owner_key key, int64_t delta, int descriptor) {
    hl_owner_writer writer;
    int error = hl_socket_owner_writer_begin(&writer);
    if (error != 0) return error;
    error = descriptor ? hl_owner_registry_descriptor(g_socket_owner_registry, hl_socket_owner_namespace(), writer,
                                                      key, delta)
                       : hl_owner_registry_link(g_socket_owner_registry, hl_socket_owner_namespace(), writer, key,
                                                delta);
    hl_socket_owner_writer_end(writer);
    return error;
}

static int hl_owner_reset(size_t minimum) {
    size_t capacity = 8192;
    size_t size;
    while (capacity < minimum * 2u) {
        if (capacity > (SIZE_MAX / 2u)) return -1;
        capacity *= 2u;
    }
    if (capacity > (SIZE_MAX - sizeof(hl_owner_table)) / sizeof(hl_owner_entry)) return -1;
    size = sizeof(hl_owner_table) + capacity * sizeof(hl_owner_entry);
    if (hl_socket_owner_runtime_init() != 0) return -1;
    if (g_owner_table != NULL) (void)munmap(g_owner_table, g_owner_table_size);
    g_owner_table = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (g_owner_table == MAP_FAILED) {
        g_owner_table = NULL;
        g_owner_table_size = 0;
        return -1;
    }
    g_owner_table_size = size;
    g_owner_table->capacity = capacity;
    return 0;
}

static hl_owner_entry *hl_owner_slot(uint64_t device, uint64_t object, uint64_t birth_ns, int create) {
    size_t index;
    size_t probe;
    if (g_owner_table == NULL && (!create || hl_owner_reset(4096) != 0)) return NULL;
    index = (size_t)(hl_owner_hash(device, object, birth_ns) & (g_owner_table->capacity - 1u));
    for (probe = 0; probe < g_owner_table->capacity; ++probe) {
        hl_owner_entry *entry = &g_owner_table->entries[(index + probe) & (g_owner_table->capacity - 1u)];
    retry_entry:;
        uint32_t active = atomic_load_explicit(&entry->active, memory_order_acquire);
        if (active != 0) {
            if (active == 2) goto retry_entry;
            if (entry->device == device && entry->object == object && entry->birth_ns == birth_ns) return entry;
            continue;
        }
        if (!create) return NULL;
        {
            uint32_t expected = 0;
            if (!atomic_compare_exchange_strong_explicit(&entry->active, &expected, 2, memory_order_acq_rel,
                                                         memory_order_acquire))
                goto retry_entry;
        }
        entry->device = device;
        entry->object = object;
        entry->birth_ns = birth_ns;
        atomic_store_explicit(&entry->metadata, 0, memory_order_relaxed);
        atomic_store_explicit(&entry->uid, 0, memory_order_relaxed);
        atomic_store_explicit(&entry->gid, 0, memory_order_relaxed);
        atomic_store_explicit(&entry->active, 1, memory_order_release);
        return entry;
    }
    return NULL;
}

static void hl_owner_set_metadata(const struct stat *status, uint64_t birth_ns, int64_t uid, int64_t gid) {
    if (hl_socket_owner_update(status, birth_ns, uid, gid) != 0) return;
    hl_owner_entry *entry = hl_owner_slot((uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns, 1);
    if (entry == NULL) return;
    uint32_t metadata = 0;
    if (uid >= 0 && (uint64_t)uid <= UINT32_MAX) {
        atomic_store_explicit(&entry->uid, (uint32_t)uid, memory_order_relaxed);
        metadata |= 1u;
    }
    if (gid >= 0 && (uint64_t)gid <= UINT32_MAX) {
        atomic_store_explicit(&entry->gid, (uint32_t)gid, memory_order_relaxed);
        metadata |= 2u;
    }
    if (metadata != 0) atomic_fetch_or_explicit(&entry->metadata, metadata, memory_order_release);
}

static void hl_owner_set_path(const char *path, int64_t uid, int64_t gid, int nofollow) {
    struct stat status;
    if (path == NULL || (nofollow ? lstat(path, &status) : stat(path, &status)) != 0) return;
    hl_owner_set_metadata(&status, hl_owner_birth(path, -1, nofollow, &status), uid, gid);
}

static void hl_owner_set_fd(int fd, int64_t uid, int64_t gid) {
    struct stat status;
    if (fd < 0 || fstat(fd, &status) != 0) return;
    hl_owner_set_metadata(&status, hl_owner_birth(NULL, fd, 0, &status), uid, gid);
}

static int hl_owner_get(const char *path, int fd, const struct stat *status, int nofollow, int64_t *uid, int64_t *gid) {
    uint64_t birth_ns;
    hl_owner_entry *entry;
    *uid = -1;
    *gid = -1;
    if (status == NULL) return 0;
    birth_ns = hl_owner_birth(path, fd, nofollow, status);
    int socket_owner = hl_socket_owner_lookup(status, birth_ns, uid, gid);
    if (socket_owner > 0) return 1;
    if (socket_owner < 0) abort();
    entry = hl_owner_slot((uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns, 0);
    if (entry == NULL) return 0;
    uint32_t metadata = atomic_load_explicit(&entry->metadata, memory_order_acquire);
    if (metadata & 1u) *uid = atomic_load_explicit(&entry->uid, memory_order_relaxed);
    if (metadata & 2u) *gid = atomic_load_explicit(&entry->gid, memory_order_relaxed);
    return metadata != 0;
}

static int hl_owner_path_valid(const char *path, size_t length) {
    size_t start = 0;
    if (length == 0 || path[0] == '/') return 0;
    while (start < length) {
        size_t end = start;
        while (end < length && path[end] != '/')
            ++end;
        if (end == start || (end - start == 1 && path[start] == '.') ||
            (end - start == 2 && path[start] == '.' && path[start + 1] == '.'))
            return 0;
        start = end + 1;
    }
    return path[length - 1] != '/';
}

static int hl_owner_number(const char *begin, const char *end, uint32_t *output) {
    uint64_t value = 0;
    if (begin == end) return -1;
    while (begin < end) {
        if (*begin < '0' || *begin > '9') return -1;
        value = value * 10u + (uint64_t)(*begin++ - '0');
        if (value > UINT32_MAX) return -1;
    }
    *output = (uint32_t)value;
    return 0;
}

static int hl_owner_seed(const char *rootfs, const char *spec, const char *const *lowers, size_t lower_count) {
    const char *line;
    size_t count = 0;
    if (spec != NULL)
        for (const char *cursor = spec; *cursor != 0; ++cursor)
            if (*cursor == '\n') ++count;
    if (hl_owner_reset(count + 4096u) != 0) return -1;
    if (spec == NULL || spec[0] == 0) return 0;
    line = spec;
    while (*line != 0) {
        const char *end = strchr(line, '\n');
        const char *first;
        const char *second;
        char path[4096];
        char host[8192];
        uint32_t uid;
        uint32_t gid;
        if (end == NULL) end = line + strlen(line);
        first = memchr(line, '\t', (size_t)(end - line));
        second = first == NULL ? NULL : memchr(first + 1, '\t', (size_t)(end - first - 1));
        if (first == NULL || second == NULL || memchr(second + 1, '\t', (size_t)(end - second - 1)) != NULL ||
            (size_t)(first - line) >= sizeof(path) || !hl_owner_path_valid(line, (size_t)(first - line)) ||
            hl_owner_number(first + 1, second, &uid) != 0 || hl_owner_number(second + 1, end, &gid) != 0)
            return -1;
        memcpy(path, line, (size_t)(first - line));
        path[first - line] = 0;
        if (snprintf(host, sizeof(host), "%s/%s", rootfs, path) < 0 || strlen(host) >= sizeof(host) - 1u) return -1;
        {
            struct stat status;
            size_t layer = 0;
            while (lstat(host, &status) != 0 && layer < lower_count) {
                if (snprintf(host, sizeof(host), "%s/%s", lowers[layer++], path) < 0 ||
                    strlen(host) >= sizeof(host) - 1u)
                    return -1;
            }
            if (lstat(host, &status) == 0)
                hl_owner_set_metadata(&status, hl_owner_birth(host, -1, 1, &status), uid, gid);
        }
        line = *end == 0 ? end : end + 1;
    }
    return 0;
}

#endif
