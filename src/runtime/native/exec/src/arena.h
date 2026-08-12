#ifndef HL_NATIVE_ARENA_H
#define HL_NATIVE_ARENA_H

#include "../include/executor.h"

typedef struct hl_native_arena {
    hl_native_memory memory;
    hl_native_mapping mapping;
    uint8_t *writable;
    uint8_t *executable;
    uint8_t *cursor;
    uint8_t *limit;
    uint64_t capacity;
    uint64_t alignment;
    uint64_t publications;
    uint64_t write_transitions;
    uint32_t writing;
} hl_native_arena;

typedef struct hl_native_span {
    uint8_t *writable;
    uint8_t *executable;
    uint64_t offset;
    uint64_t capacity;
} hl_native_span;

hl_native_status hl_native_arena_create(hl_native_arena *, const hl_native_config *);
hl_native_status hl_native_arena_begin(hl_native_arena *);
hl_native_status hl_native_arena_allocate(hl_native_arena *, uint64_t, uint64_t, hl_native_span *);
hl_native_status hl_native_arena_publish(hl_native_arena *, const hl_native_span *, uint64_t);
hl_native_status hl_native_arena_end(hl_native_arena *);
hl_native_status hl_native_arena_repair(hl_native_arena *, uint32_t);
hl_native_status hl_native_arena_rotate(hl_native_arena *);
void hl_native_arena_destroy(hl_native_arena *);

#endif
