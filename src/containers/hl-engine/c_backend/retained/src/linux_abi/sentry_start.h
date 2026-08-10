#ifndef HL_LINUX_ABI_SENTRY_START_H
#define HL_LINUX_ABI_SENTRY_START_H

#include <errno.h>
#include <stdint.h>

struct hl_sentry_start {
    const void *thread;
    uint32_t token;
};

static inline int hl_sentry_start_reserve(
    struct hl_sentry_start *starts, uint32_t count, const void *thread, uint32_t token) {
    if (!thread || token == 0) return -EINVAL;
    for (uint32_t i = 0; i < count; i++)
        if (starts[i].thread == thread) return -EEXIST;
    for (uint32_t i = 0; i < count; i++)
        if (!starts[i].thread) {
            starts[i] = (struct hl_sentry_start){.thread = thread, .token = token};
            return 0;
        }
    return -EAGAIN;
}

static inline int hl_sentry_start_take(
    struct hl_sentry_start *starts, uint32_t count, const void *thread, uint32_t *token) {
    for (uint32_t i = 0; i < count; i++)
        if (starts[i].thread == thread) {
            *token = starts[i].token;
            starts[i] = (struct hl_sentry_start){0};
            return 0;
        }
    return -ENOENT;
}

#endif
