#ifndef HL_TRANSLATOR_EMIT_H
#define HL_TRANSLATOR_EMIT_H

#include <stddef.h>
#include <stdint.h>

#include "hl/host_services.h"

typedef struct hl_emit_state {
    uint8_t *base;
    uint8_t *cursor;
    uint8_t *start;
    ptrdiff_t rx_delta;
    int dual_alias;
    uint64_t wx_toggles;
    hl_host_code_mapping mapping;
} hl_emit_state;

static inline void *hl_emit_rx(const hl_emit_state *state, const void *pointer) {
    return (void *)((const uint8_t *)pointer + state->rx_delta);
}

static inline void *hl_emit_rw(const hl_emit_state *state, const void *pointer) {
    return (void *)((const uint8_t *)pointer - state->rx_delta);
}

#endif
