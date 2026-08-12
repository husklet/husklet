#ifndef HL_LINUX_ABI_SENTRY_AUTHORITY_H
#define HL_LINUX_ABI_SENTRY_AUTHORITY_H

#include <stdint.h>

static inline int hl_sentry_native_fd(const int *real, const uint8_t *typed, uint32_t count, int descriptor) {
    if (descriptor < 0 || (uint32_t)descriptor >= count || real[descriptor] < 0 || typed[descriptor]) return -1;
    return real[descriptor];
}

#endif
