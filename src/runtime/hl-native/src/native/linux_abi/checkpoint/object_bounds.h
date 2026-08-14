#ifndef HL_CHECKPOINT_OBJECT_BOUNDS_H
#define HL_CHECKPOINT_OBJECT_BOUNDS_H

#include <stddef.h>
#include <stdint.h>

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

#endif
