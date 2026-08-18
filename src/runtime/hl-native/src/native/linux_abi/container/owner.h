#ifndef HL_LINUX_OWNER_H
#define HL_LINUX_OWNER_H

#include <stdatomic.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "../host_mman.h"
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "ownership/registry.h"
#include "vfs/namespace_transaction.h"

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

static hl_owner_registry *g_socket_owner_registry;
static size_t g_socket_owner_registry_size;

static const hl_host_services *effective_host_services(void);

static int hl_socket_owner_namespace(hl_owner_namespace *namespace) {
    return namespace_transaction_namespace(&namespace->generation, &namespace->owner) == 0 ? 0 : errno;
}

static int hl_socket_owner_runtime_init(void) {
#if defined(_WIN32)
    return 0;
#else
    size_t registry_size;
    struct timespec now;
    uint64_t epoch;
    if (g_socket_owner_registry != NULL) return 0;
    if (namespace_transaction_init(effective_host_services()) != 0) return -1;
    registry_size = hl_owner_registry_size(HL_OWNER_REGISTRY_DEFAULT_CAPACITY);
    if (registry_size == 0) return -1;
    g_socket_owner_registry_size = registry_size;
    g_socket_owner_registry = mmap(NULL, registry_size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (g_socket_owner_registry == MAP_FAILED) {
        g_socket_owner_registry = NULL;
        g_socket_owner_registry_size = 0;
        return -1;
    }
    memset(&now, 0, sizeof now);
    (void)clock_gettime(CLOCK_MONOTONIC, &now);
    epoch = ((uint64_t)(uint32_t)getpid() << 32) ^ (uint64_t)now.tv_sec ^ (uint64_t)now.tv_nsec;
    if (epoch == 0) epoch = 1;
    if (hl_owner_registry_init_zeroed(g_socket_owner_registry, registry_size, HL_OWNER_REGISTRY_DEFAULT_CAPACITY,
                                      epoch) != 0) {
        (void)munmap(g_socket_owner_registry, g_socket_owner_registry_size);
        g_socket_owner_registry = NULL;
        g_socket_owner_registry_size = 0;
        return -1;
    }
    return 0;
#endif
}

static int hl_socket_owner_writer_begin(hl_owner_writer *writer) {
    struct namespace_transaction_writer transaction_writer;
    if (writer == NULL || g_socket_owner_registry == NULL) return EINVAL;
    if (namespace_transaction_begin() != 0) return errno;
    if (namespace_transaction_writer(&transaction_writer) != 0) {
        int saved = errno;
        namespace_transaction_end();
        return saved;
    }
    writer->generation = transaction_writer.writer_generation;
    writer->identity = transaction_writer.writer_identity;
    return 0;
}

static void hl_socket_owner_writer_end(hl_owner_writer writer) {
    (void)writer;
    namespace_transaction_end();
}

static int hl_socket_owner_writer_context(hl_owner_writer *writer, hl_owner_namespace *namespace) {
    int error = hl_socket_owner_writer_begin(writer);
    if (error != 0) return error;
    error = hl_socket_owner_namespace(namespace);
    if (error != 0) hl_socket_owner_writer_end(*writer);
    return error;
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
    struct namespace_transaction_read read;
    hl_owner_value value;
    int result;
    if (g_socket_owner_registry == NULL || birth_ns == 0) return 0;
    hl_owner_namespace namespace;
    if (hl_socket_owner_namespace(&namespace) != 0) return -1;
    for (unsigned retry = 0; retry < 64; ++retry) {
        if (namespace_transaction_read_begin(&read) != 0) return -1;
        result = hl_owner_registry_lookup(g_socket_owner_registry, namespace,
                                          hl_socket_owner_key(status, birth_ns), &value);
        if (result < 0 && result != -EAGAIN) return -1;
        if (result != -EAGAIN && namespace_transaction_read_validate(&read) == 0) {
            if (result != HL_OWNER_FOUND) return 0;
            *uid = value.uid;
            *gid = value.gid;
            return 1;
        }
        if (errno != EAGAIN) return -1;
    }
    return -1;
}

typedef struct hl_socket_owner_publication {
    hl_owner_writer writer;
    hl_owner_namespace namespace;
    hl_owner_ticket ticket;
    int active;
    int ticket_active;
} hl_socket_owner_publication;

static int hl_socket_owner_finish(hl_socket_owner_publication *publication, int poison);

static int hl_socket_owner_prepare(hl_socket_owner_publication *publication) {
    int error;
    if (publication == NULL || g_socket_owner_registry == NULL) return EINVAL;
    memset(publication, 0, sizeof *publication);
    error = hl_socket_owner_writer_context(&publication->writer, &publication->namespace);
    if (error != 0) return error;
    publication->active = 1;
    error = hl_owner_registry_reserve(g_socket_owner_registry, publication->namespace, publication->writer,
                                      &publication->ticket);
    if (error != 0) {
        hl_socket_owner_writer_end(publication->writer);
        publication->active = 0;
    } else {
        publication->ticket_active = 1;
    }
    return error;
}

static int hl_socket_owner_publish(hl_socket_owner_publication *publication, const struct stat *status,
                                   uint64_t birth_ns, uint32_t uid, uint32_t gid, uint32_t descriptors) {
    int error;
    if (publication == NULL || !publication->active) return EINVAL;
    if (status == NULL || birth_ns == 0 || descriptors == 0) {
        (void)hl_socket_owner_finish(publication, 1);
        return EINVAL;
    }
    error = hl_owner_registry_commit(g_socket_owner_registry, publication->namespace, publication->writer,
                                     publication->ticket, hl_socket_owner_key(status, birth_ns),
                                     (hl_owner_value){uid, gid, 1, descriptors});
    if (error == 0 || error == EEXIST || error == EINVAL) publication->ticket_active = 0;
    if (error != 0 && error != EEXIST && error != EINVAL) {
        int cancel_error = hl_owner_registry_cancel(g_socket_owner_registry, publication->namespace,
                                                    publication->writer, publication->ticket);
        if (cancel_error != 0) {
            namespace_transaction_poison();
            error = EOWNERDEAD;
        } else {
            publication->ticket_active = 0;
        }
    }
    return error;
}

static int hl_socket_owner_finish(hl_socket_owner_publication *publication, int poison) {
    if (publication == NULL || !publication->active) return 0;
    int error = 0;
    if (publication->ticket_active)
        error = hl_owner_registry_cancel(g_socket_owner_registry, publication->namespace, publication->writer,
                                         publication->ticket);
    if (error != 0) namespace_transaction_poison();
    if (poison) namespace_transaction_poison();
    hl_socket_owner_writer_end(publication->writer);
    publication->active = 0;
    publication->ticket_active = 0;
    return error;
}

static int hl_socket_owner_reference(hl_owner_key key, int64_t delta, int descriptor) {
    hl_owner_writer writer;
    hl_owner_namespace namespace;
    int error = hl_socket_owner_writer_context(&writer, &namespace);
    if (error != 0) return error;
    error = descriptor ? hl_owner_registry_descriptor(g_socket_owner_registry, namespace, writer, key, delta)
                       : hl_owner_registry_link(g_socket_owner_registry, namespace, writer, key, delta);
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

static int hl_owner_set_metadata(const struct stat *status, uint64_t birth_ns, int64_t uid, int64_t gid) {
    int socket_fallback_writer = 0;
    hl_owner_writer socket_writer;
    if (S_ISSOCK(status->st_mode) && g_socket_owner_registry != NULL) {
        hl_owner_namespace namespace;
        int error = hl_socket_owner_writer_context(&socket_writer, &namespace);
        if (error != 0) return errno = error, -1;
        hl_owner_value value;
        hl_owner_key key = hl_socket_owner_key(status, birth_ns);
        int found = hl_owner_registry_writer_lookup(g_socket_owner_registry, namespace, socket_writer, key, &value);
        if (found == 0) {
            error = hl_owner_registry_update(g_socket_owner_registry, namespace, socket_writer, key,
                                             uid < 0 ? value.uid : (uint32_t)uid,
                                             gid < 0 ? value.gid : (uint32_t)gid);
            hl_socket_owner_writer_end(socket_writer);
            return error == 0 ? 0 : (errno = error, -1);
        }
        if (found != ENOENT) {
            hl_socket_owner_writer_end(socket_writer);
            return errno = found, -1;
        }
        socket_fallback_writer = 1;
    }
    hl_owner_entry *entry = hl_owner_slot((uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns, 1);
    if (entry == NULL) {
        if (socket_fallback_writer) hl_socket_owner_writer_end(socket_writer);
        return 0;
    }
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
    if (socket_fallback_writer) hl_socket_owner_writer_end(socket_writer);
    return 0;
}

static int hl_owner_set_path(const char *path, int64_t uid, int64_t gid, int nofollow) {
    struct stat status;
    if (path == NULL) return errno = EINVAL, -1;
    if ((nofollow ? lstat(path, &status) : stat(path, &status)) != 0) return -1;
    return hl_owner_set_metadata(&status, hl_owner_birth(path, -1, nofollow, &status), uid, gid);
}

static int hl_owner_set_fd(int fd, int64_t uid, int64_t gid) {
    struct stat status;
    if (fd < 0) return errno = EBADF, -1;
    if (fstat(fd, &status) != 0) return -1;
    return hl_owner_set_metadata(&status, hl_owner_birth(NULL, fd, 0, &status), uid, gid);
}

static int hl_owner_get(const char *path, int fd, const struct stat *status, int nofollow, int64_t *uid, int64_t *gid) {
    uint64_t birth_ns;
    hl_owner_entry *entry;
    *uid = -1;
    *gid = -1;
    if (status == NULL) return 0;
    birth_ns = hl_owner_birth(path, fd, nofollow, status);
    if (S_ISSOCK(status->st_mode)) {
        int socket_owner = hl_socket_owner_lookup(status, birth_ns, uid, gid);
        if (socket_owner > 0) return 1;
        if (socket_owner < 0) abort();
    }
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
