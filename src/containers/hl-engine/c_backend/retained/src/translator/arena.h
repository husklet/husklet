#ifndef HL_TRANSLATOR_ARENA_H
#define HL_TRANSLATOR_ARENA_H

#include <stddef.h>
#include <stdint.h>

#include "emit.h"

int hl_arena_reserve(const hl_host_services *services, uint64_t size, uint64_t alignment, int dual_alias,
                     hl_host_code_mapping *mapping);
void hl_arena_bind(hl_emit_state *state, const hl_host_code_mapping *mapping);
int hl_arena_repair(const hl_host_services *services, hl_emit_state *state, int preserve);
void hl_arena_release(const hl_host_services *services, hl_host_handle handle);

#endif
