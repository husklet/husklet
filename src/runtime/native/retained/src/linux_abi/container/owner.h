#ifndef HL_LINUX_OWNER_H
#define HL_LINUX_OWNER_H

#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "../host_mman.h"
#include <sys/stat.h>
#include <unistd.h>

#if defined(__linux__)
#include <fcntl.h>
#include <linux/stat.h>
#include <sys/syscall.h>
#endif

typedef struct hl_owner_entry {
    _Atomic uint32_t active;
    uint32_t reserved;
    uint64_t device;
    uint64_t object;
    uint64_t birth_ns;
    _Atomic int32_t uid;
    _Atomic int32_t gid;
} hl_owner_entry;

typedef struct hl_owner_table {
    uint64_t capacity;
    hl_owner_entry entries[];
} hl_owner_table;

static hl_owner_table *g_owner_table;
static size_t g_owner_table_size;

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

static int hl_owner_reset(size_t minimum) {
    size_t capacity = 8192;
    size_t size;
    while (capacity < minimum * 2u) {
        if (capacity > (SIZE_MAX / 2u)) return -1;
        capacity *= 2u;
    }
    if (capacity > (SIZE_MAX - sizeof(hl_owner_table)) / sizeof(hl_owner_entry)) return -1;
    size = sizeof(hl_owner_table) + capacity * sizeof(hl_owner_entry);
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
        atomic_store_explicit(&entry->uid, -1, memory_order_relaxed);
        atomic_store_explicit(&entry->gid, -1, memory_order_relaxed);
        atomic_store_explicit(&entry->active, 1, memory_order_release);
        return entry;
    }
    return NULL;
}

static void hl_owner_set_metadata(const struct stat *status, uint64_t birth_ns, int uid, int gid) {
    hl_owner_entry *entry = hl_owner_slot((uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns, 1);
    if (entry == NULL) return;
    if (uid >= 0) atomic_store_explicit(&entry->uid, uid, memory_order_release);
    if (gid >= 0) atomic_store_explicit(&entry->gid, gid, memory_order_release);
}

static void hl_owner_set_path(const char *path, int uid, int gid, int nofollow) {
    struct stat status;
    if (path == NULL || (nofollow ? lstat(path, &status) : stat(path, &status)) != 0) return;
    hl_owner_set_metadata(&status, hl_owner_birth(path, -1, nofollow, &status), uid, gid);
}

static void hl_owner_set_fd(int fd, int uid, int gid) {
    struct stat status;
    if (fd < 0 || fstat(fd, &status) != 0) return;
    hl_owner_set_metadata(&status, hl_owner_birth(NULL, fd, 0, &status), uid, gid);
}

static int hl_owner_get(const char *path, int fd, const struct stat *status, int nofollow, int *uid, int *gid) {
    uint64_t birth_ns;
    hl_owner_entry *entry;
    *uid = -1;
    *gid = -1;
    if (status == NULL) return 0;
    birth_ns = hl_owner_birth(path, fd, nofollow, status);
    entry = hl_owner_slot((uint64_t)status->st_dev, (uint64_t)status->st_ino, birth_ns, 0);
    if (entry == NULL) return 0;
    *uid = atomic_load_explicit(&entry->uid, memory_order_acquire);
    *gid = atomic_load_explicit(&entry->gid, memory_order_acquire);
    return *uid >= 0 || *gid >= 0;
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
                hl_owner_set_metadata(&status, hl_owner_birth(host, -1, 1, &status), (int32_t)uid, (int32_t)gid);
        }
        line = *end == 0 ? end : end + 1;
    }
    return 0;
}

#endif
