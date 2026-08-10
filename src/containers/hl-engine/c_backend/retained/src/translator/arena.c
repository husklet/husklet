#include "arena.h"

#include <string.h>

int hl_arena_reserve(const hl_host_services *services, uint64_t size, uint64_t alignment, int dual_alias,
                     hl_host_code_mapping *mapping) {
    memset(mapping, 0, sizeof(*mapping));
    return services->memory
                       ->reserve_code(services->context, size, alignment, dual_alias ? HL_HOST_CODE_DUAL_ALIAS : 0,
                                      mapping)
                       .status == HL_STATUS_OK
               ? 0
               : -1;
}

void hl_arena_bind(hl_emit_state *state, const hl_host_code_mapping *mapping) {
    state->mapping = *mapping;
    state->base = state->cursor = (uint8_t *)(uintptr_t)mapping->writable_address;
    state->rx_delta = (uint8_t *)(uintptr_t)mapping->executable_address - state->base;
    state->dual_alias = state->rx_delta != 0;
}

int hl_arena_repair(const hl_host_services *services, hl_emit_state *state, int preserve) {
    int dual_alias = state->dual_alias;
    state->mapping.content_size = (uint64_t)(state->cursor - state->base);
    if (services->memory->repair_code_after_fork(services->context, &state->mapping, (uint32_t)preserve).status !=
        HL_STATUS_OK)
        return -1;
    if (!preserve) {
        hl_arena_bind(state, &state->mapping);
        state->dual_alias = dual_alias;
    }
    return 0;
}

void hl_arena_release(const hl_host_services *services, hl_host_handle handle) {
    (void)services->memory->release(services->context, handle);
}
