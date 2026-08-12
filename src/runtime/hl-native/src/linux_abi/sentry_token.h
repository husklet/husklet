#ifndef HL_LINUX_ABI_SENTRY_TOKEN_H
#define HL_LINUX_ABI_SENTRY_TOKEN_H

#include <stdatomic.h>
#include <stdint.h>

static inline uint32_t hl_sentry_token_next(_Atomic uint32_t *counter) {
    uint32_t token;
    do {
        token = atomic_fetch_add_explicit(counter, 1, memory_order_relaxed) + 1u;
    } while (token == 0);
    return token;
}

#endif
