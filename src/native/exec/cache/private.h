#ifndef HL_NATIVE_CACHE_PRIVATE_H
#define HL_NATIVE_CACHE_PRIVATE_H

#include "cache.h"

#include <stdatomic.h>

enum entry_state { ENTRY_EMPTY, ENTRY_RESERVED, ENTRY_LIVE, ENTRY_TOMBSTONE };

typedef struct cache_entry {
    uint64_t guest, source_first, source_last, code_offset, code_size, body_offset, admitted_offset;
    uint64_t mapping_epoch, instruction_epoch, token;
    uint64_t memory_mode, authority_generation;
    uint64_t loop_pc;
    uint32_t instruction_count, conditional_self_loop, cycle_safe, generation, state;
} cache_entry;

typedef struct provenance_entry {
    _Atomic uint64_t sequence, code_first, code_last, guest;
    _Atomic uint64_t address_displacement, address_constant, address_shape;
    _Atomic uint32_t access, width;
    _Atomic uint32_t generation;
} provenance_entry;

typedef struct pending_relocation {
    uint64_t source_guest, source_instruction_epoch, source_code_offset;
    hl_native_relocation relocation;
    uint32_t generation;
} pending_relocation;

typedef struct resolved_relocation {
    uint64_t source_guest, source_instruction_epoch, source_code_offset;
    hl_native_relocation relocation;
    uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS];
    uint32_t generation;
    uint32_t target_epoch_wildcard;
} resolved_relocation;

struct hl_native_cache {
    hl_native_arena *arena;
    cache_entry *entries;
    provenance_entry *provenance;
    uint32_t *live;
    uint32_t capacity, provenance_capacity, provenance_next, live_count, generation;
    _Atomic uint32_t published_generation;
    _Atomic uint64_t provenance_epoch;
    uint32_t hash_shift;
    _Atomic uint64_t mapping_epoch, instruction_epoch;
    _Atomic uint64_t memory_mode, authority_generation;
    uint64_t next_token, active_token;
    uint32_t poisoned;
    hl_native_cache_stats stats;
    pending_relocation *relocations;
    uint32_t relocation_count, relocation_capacity;
    resolved_relocation *resolved;
    uint32_t resolved_count, resolved_capacity;
    hl_native_cache_observer observer;
};

static inline void hl_native_cache_observe(hl_native_cache *cache, hl_native_cache_event event) {
    if (cache->observer.observe != NULL) cache->observer.observe(cache->observer.context, event);
}

static inline int hl_native_cache_live(const hl_native_cache *cache, uint32_t slot) {
    return cache->entries[slot].generation == cache->generation && cache->entries[slot].state == ENTRY_LIVE;
}

static inline int hl_native_cache_occupied(const hl_native_cache *cache, uint32_t slot) {
    return cache->entries[slot].generation == cache->generation && cache->entries[slot].state != ENTRY_EMPTY;
}

static inline int hl_native_cache_find_identity(const hl_native_cache *cache, uint64_t guest,
                                                uint64_t memory_mode, uint64_t authority_generation) {
    uint32_t start = (uint32_t)((guest >> cache->hash_shift) * 2654435761u) & (cache->capacity - 1);
    for (uint32_t probe = 0; probe < cache->capacity; probe++) {
        uint32_t slot = (start + probe) & (cache->capacity - 1);
        if (!hl_native_cache_occupied(cache, slot)) return -1;
        if (hl_native_cache_live(cache, slot) && cache->entries[slot].guest == guest &&
            cache->entries[slot].memory_mode == memory_mode &&
            cache->entries[slot].authority_generation == authority_generation)
            return (int)slot;
    }
    return -1;
}

static inline int hl_native_cache_find(const hl_native_cache *cache, uint64_t guest) {
    return hl_native_cache_find_identity(
        cache, guest,
        atomic_load_explicit(&cache->memory_mode, memory_order_acquire),
        atomic_load_explicit(&cache->authority_generation, memory_order_acquire));
}

#endif
