#ifndef HL_LINUX_ABI_CONTAINER_VFS_PATH_COMPOSE_H
#define HL_LINUX_ABI_CONTAINER_VFS_PATH_COMPOSE_H

#include <stddef.h>
#include <string.h>

static inline int hl_guest_path_compose(char *out, size_t capacity, const char *base, const char *relative,
                                        int leading_slash) {
    const char *right = relative ? relative : "";
    size_t base_length = strlen(base);
    size_t right_length = strlen(right);
    size_t prefix_length = leading_slash ? 1u : 0u;
    if (capacity == 0) return -1;
    size_t available = capacity - 1u;
    if (base_length > available || prefix_length + 1u > available - base_length ||
        right_length > available - base_length - prefix_length - 1u) {
        out[0] = '\0';
        return -1;
    }
    char *cursor = out;
    if (leading_slash) *cursor++ = '/';
    memcpy(cursor, base, base_length);
    cursor += base_length;
    *cursor++ = '/';
    memcpy(cursor, right, right_length);
    cursor[right_length] = '\0';
    return 0;
}

static inline int hl_guest_path_copy(char *out, size_t capacity, const char *path) {
    size_t length = strlen(path);
    if (capacity == 0) return -1;
    if (length >= capacity) {
        out[0] = '\0';
        return -1;
    }
    memcpy(out, path, length + 1u);
    return 0;
}

#endif
