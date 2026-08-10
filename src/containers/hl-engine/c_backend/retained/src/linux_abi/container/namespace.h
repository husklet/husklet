#ifndef HL_LINUX_VFS_NAMESPACE_H
#define HL_LINUX_VFS_NAMESPACE_H

#include <stddef.h>

#define HL_LINUX_VFS_LOWER_CAPACITY 8
#define HL_LINUX_VFS_LOWER_PATH_CAPACITY 1024

struct hl_linux_vfs_lower {
    char canon[HL_LINUX_VFS_LOWER_PATH_CAPACITY];
    size_t clen;
};

struct hl_linux_vfs_namespace {
    const char *root_canonical;
    const size_t *root_length;
    const struct hl_linux_vfs_lower *lowers;
    const int *lower_count;
};

static inline size_t hl_linux_vfs_root_length(const struct hl_linux_vfs_namespace *view) {
    return *view->root_length;
}

static inline int hl_linux_vfs_lower_count(const struct hl_linux_vfs_namespace *view) {
    return *view->lower_count;
}

#endif
