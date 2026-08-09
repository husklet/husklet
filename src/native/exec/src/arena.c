#include "arena.h"
#include "state.h"

#include <limits.h>
#include <string.h>

#define HL_NATIVE_ARENA_BYTES (64u << 20)
#define HL_NATIVE_ALIGNMENT_MAX (2u << 20)

static int power_of_two(uint64_t value) {
    return value != 0 && (value & (value - 1)) == 0;
}

static int services_valid(const hl_native_memory *memory) {
    return memory != NULL && memory->abi == HL_NATIVE_ABI && memory->size >= sizeof(*memory) &&
           memory->reserve != NULL && memory->release != NULL && memory->publish != NULL && memory->repair != NULL &&
           memory->write_begin != NULL && memory->write_end != NULL;
}

static int mapping_valid(const hl_native_mapping *mapping, uint64_t capacity, uint64_t alignment) {
    return mapping->abi == HL_NATIVE_ABI && mapping->size >= sizeof(*mapping) && mapping->handle != 0 &&
           mapping->writable != 0 && mapping->executable != 0 && mapping->capacity == capacity &&
           mapping->content <= capacity && mapping->writable % alignment == 0 && mapping->executable % alignment == 0 &&
           mapping->writable <= UINTPTR_MAX - capacity && mapping->executable <= UINTPTR_MAX - capacity;
}

static void bind(hl_native_arena *arena) {
    arena->writable = (uint8_t *)(uintptr_t)arena->mapping.writable;
    arena->executable = (uint8_t *)(uintptr_t)arena->mapping.executable;
    arena->cursor = arena->writable + arena->mapping.content;
    arena->limit = arena->writable + arena->mapping.capacity;
}

hl_native_status hl_native_arena_create(hl_native_arena *arena, const hl_native_config *config) {
    hl_native_status status;
    uint32_t dual;
    if (arena == NULL || config == NULL || config->abi != HL_NATIVE_ABI || config->size < sizeof(*config) ||
        config->capacity != HL_NATIVE_ARENA_BYTES || !power_of_two(config->alignment) ||
        config->alignment > HL_NATIVE_ALIGNMENT_MAX || !services_valid(config->memory) ||
        (config->flags & ~(HL_NATIVE_DUAL_PREFERRED | HL_NATIVE_DUAL_REQUIRED | HL_NATIVE_DIAGNOSTICS |
                           HL_NATIVE_A64_NO_WRITE_RESERVE |
                           HL_NATIVE_A64_NO_WRITE_COMMIT |
                           HL_NATIVE_A64_RUNTIME_WRITE_RESERVE |
                           HL_NATIVE_A64_DIRTY_OVERFLOW_CONTINUE |
                           HL_NATIVE_A64_DIRTY_OVERFLOW_EXIT)) != 0 ||
        (config->flags & (HL_NATIVE_A64_DIRTY_OVERFLOW_CONTINUE |
                          HL_NATIVE_A64_DIRTY_OVERFLOW_EXIT)) ==
            (HL_NATIVE_A64_DIRTY_OVERFLOW_CONTINUE | HL_NATIVE_A64_DIRTY_OVERFLOW_EXIT) ||
        (config->flags & (HL_NATIVE_DUAL_PREFERRED | HL_NATIVE_DUAL_REQUIRED)) ==
            (HL_NATIVE_DUAL_PREFERRED | HL_NATIVE_DUAL_REQUIRED) || config->reserved != 0)
        return HL_NATIVE_ARGUMENT;
    memset(arena, 0, sizeof(*arena));
    arena->memory = *config->memory;
    arena->capacity = config->capacity;
    arena->alignment = config->alignment;
    dual = config->flags & (HL_NATIVE_DUAL_PREFERRED | HL_NATIVE_DUAL_REQUIRED);
    status = arena->memory.reserve(arena->memory.context, config->capacity, config->alignment, dual != 0,
                                   &arena->mapping);
    if (status != HL_NATIVE_OK && dual == HL_NATIVE_DUAL_PREFERRED)
        status = arena->memory.reserve(arena->memory.context, config->capacity, config->alignment, 0,
                                       &arena->mapping);
    if (status != HL_NATIVE_OK) return status;
    if (!mapping_valid(&arena->mapping, config->capacity, config->alignment)) {
        if (arena->mapping.handle != 0) arena->memory.release(arena->memory.context, arena->mapping.handle);
        memset(arena, 0, sizeof(*arena));
        return HL_NATIVE_PLATFORM;
    }
    bind(arena);
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_begin(hl_native_arena *arena) {
    hl_native_status status;
    if (arena == NULL || arena->memory.reserve == NULL || arena->writing) return HL_STATE("arena begin while writing");
    status = arena->memory.write_begin(arena->memory.context);
    if (status != HL_NATIVE_OK) return status;
    arena->writing = 1;
    arena->write_transitions++;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_allocate(hl_native_arena *arena, uint64_t size, uint64_t alignment,
                                          hl_native_span *span) {
    uintptr_t current, aligned, limit;
    if (arena == NULL || span == NULL || !arena->writing || size == 0 || !power_of_two(alignment))
        return HL_NATIVE_ARGUMENT;
    current = (uintptr_t)arena->cursor;
    if (current > UINTPTR_MAX - (alignment - 1)) return HL_NATIVE_CAPACITY;
    aligned = (current + alignment - 1) & ~(uintptr_t)(alignment - 1);
    limit = (uintptr_t)arena->limit;
    if (aligned > limit || size > limit - aligned) return HL_NATIVE_CAPACITY;
    span->writable = (uint8_t *)aligned;
    span->executable = arena->executable + (aligned - (uintptr_t)arena->writable);
    span->offset = aligned - (uintptr_t)arena->writable;
    span->capacity = size;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_publish(hl_native_arena *arena, const hl_native_span *span, uint64_t used) {
    hl_native_status status;
    uintptr_t expected;
    if (arena == NULL || span == NULL || !arena->writing || used == 0 || used > span->capacity)
        return HL_NATIVE_ARGUMENT;
    expected = (uintptr_t)arena->writable + span->offset;
    if ((uintptr_t)span->writable != expected || (uintptr_t)span->writable < (uintptr_t)arena->cursor ||
        span->offset > arena->mapping.capacity ||
        used > arena->mapping.capacity - span->offset ||
        span->executable != arena->executable + span->offset)
        return HL_NATIVE_ARGUMENT;
    status = arena->memory.publish(arena->memory.context, arena->mapping.handle, span->offset, used);
    if (status != HL_NATIVE_OK) return status;
    arena->cursor = span->writable + used;
    arena->mapping.content = (uint64_t)(arena->cursor - arena->writable);
    arena->publications++;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_end(hl_native_arena *arena) {
    hl_native_status status;
    if (arena == NULL || arena->memory.reserve == NULL || !arena->writing) return HL_STATE("arena end while not writing");
    status = arena->memory.write_end(arena->memory.context);
    if (status != HL_NATIVE_OK) return status;
    arena->writing = 0;
    arena->write_transitions++;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_repair(hl_native_arena *arena, uint32_t preserve) {
    hl_native_status status;
    if (arena == NULL || arena->memory.reserve == NULL || arena->writing || preserve > 1) return HL_STATE("arena repair while writing");
    status = arena->memory.repair(arena->memory.context, &arena->mapping, preserve);
    if (status != HL_NATIVE_OK) return status;
    if (!mapping_valid(&arena->mapping, arena->capacity, arena->alignment)) return HL_NATIVE_PLATFORM;
    bind(arena);
    return HL_NATIVE_OK;
}

hl_native_status hl_native_arena_rotate(hl_native_arena *arena) {
    hl_native_status status;
    if (arena == NULL || arena->memory.reserve == NULL || !arena->writing) return HL_STATE("arena rotate while not writing");
    status = arena->memory.write_end(arena->memory.context);
    if (status != HL_NATIVE_OK) return status;
    arena->writing = 0;
    arena->write_transitions++;
    status = arena->memory.repair(arena->memory.context, &arena->mapping, 0);
    if (status != HL_NATIVE_OK || !mapping_valid(&arena->mapping, arena->capacity, arena->alignment))
        return status != HL_NATIVE_OK ? status : HL_NATIVE_PLATFORM;
    arena->mapping.content = 0;
    bind(arena);
    status = arena->memory.write_begin(arena->memory.context);
    if (status != HL_NATIVE_OK) return status;
    arena->writing = 1;
    arena->write_transitions++;
    return HL_NATIVE_OK;
}

void hl_native_arena_destroy(hl_native_arena *arena) {
    if (arena == NULL || arena->memory.reserve == NULL) return;
    if (arena->writing && arena->memory.write_end(arena->memory.context) == HL_NATIVE_OK)
        arena->write_transitions++;
    arena->memory.release(arena->memory.context, arena->mapping.handle);
    memset(arena, 0, sizeof(*arena));
}
