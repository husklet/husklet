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

/*
 * Drop a mapping from the address space every future fork() child receives, while leaving it
 * completely intact in this process.  A retired code arena is unmapped by the child's fork hook
 * before any guest instruction runs, so the child's inherited copy is pure cost -- and on macOS an
 * executable (MAP_JIT) mapping is not free to inherit the way plain anonymous memory is: the kernel
 * charges roughly 9us per reserved MiB of it in fork(), on every subsequent fork, for as long as the
 * arena stays mapped.  Returns 1 when the address space was updated, 0 when the host does not offer
 * the operation (in which case the child simply inherits the arena as before -- the fork hook still
 * unmaps it, so this is an optimisation and never a correctness dependency).
 */
int hl_arena_drop_child_inheritance(void *base, uint64_t size);

#endif
