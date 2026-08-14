#ifndef HL_CHECKPOINT_OBJECT_BOUNDS_H
#define HL_CHECKPOINT_OBJECT_BOUNDS_H

#include <stddef.h>
#include <stdint.h>

#define CKPT_INOTIFY_IMAGE_LIMIT (64u * 1024u * 1024u)

static inline int ckpt_bounded_object_size(int64_t stored, size_t minimum, size_t maximum, size_t *size) {
    if (size == NULL || stored < 0 || (uint64_t)stored < minimum || (uint64_t)stored > maximum) return -1;
    *size = (size_t)stored;
    return 0;
}

static inline int ckpt_counted_object_size(size_t stored, size_t header, uint64_t count, size_t element,
                                           uint64_t count_limit) {
    if (element == 0 || count > count_limit || count > (SIZE_MAX - header) / element) return -1;
    return header + (size_t)count * element == stored ? 0 : -1;
}

static inline int ckpt_record_object_size(int64_t stored, size_t element, uint64_t count_limit, size_t *size,
                                          size_t *count) {
    if (size == NULL || count == NULL || stored < 0 || element == 0 || (uint64_t)stored > SIZE_MAX ||
        (uint64_t)stored % element != 0 || (uint64_t)stored / element > count_limit)
        return -1;
    *size = (size_t)stored;
    *count = (size_t)((uint64_t)stored / element);
    return 0;
}

static inline int ckpt_minimum_counted_object_size(int64_t stored, uint64_t count, size_t element,
                                                   uint64_t count_limit) {
    if (stored < 0 || count > count_limit || element == 0 || count > SIZE_MAX / element ||
        count * element > (uint64_t)stored)
        return -1;
    return 0;
}

static inline int ckpt_capacity_object_size(int64_t stored, size_t capacity, size_t *size) {
    if (capacity == 0) return -1;
    return ckpt_bounded_object_size(stored, 0, capacity, size);
}

static inline int ckpt_decimal_capacity(const char *text, size_t fallback, size_t maximum, size_t *capacity) {
    if (text == NULL || capacity == NULL || maximum == 0 || fallback == 0 || fallback > maximum || *text == '\0')
        return -1;
    size_t value = 0;
    for (const unsigned char *cursor = (const unsigned char *)text; *cursor != '\0'; ++cursor) {
        if (*cursor < '0' || *cursor > '9') return -1;
        size_t digit = (size_t)(*cursor - '0');
        if (digit > maximum || value > (maximum - digit) / 10u) return -1;
        value = value * 10u + digit;
    }
    *capacity = value == 0 ? fallback : value;
    return 0;
}

static inline int ckpt_inotify_object_size(int64_t stored, size_t *size) {
    return ckpt_bounded_object_size(stored, 1, CKPT_INOTIFY_IMAGE_LIMIT, size);
}

#endif
