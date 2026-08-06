#ifndef HL_NATIVE_CACHE_PRIVATE_H
#define HL_NATIVE_CACHE_PRIVATE_H

#include "cache.h"

#include <stdatomic.h>

enum entry_state { ENTRY_EMPTY, ENTRY_RESERVED, ENTRY_LIVE, ENTRY_TOMBSTONE };

/* Probe keys and the hit path's code pointers occupy the leading 64 bytes so a
 * probe step reads one cache line instead of the three the old order spanned. */
typedef struct cache_entry {
    uint64_t guest, memory_mode, authority_generation, instruction_epoch;
    uint32_t generation, state;
    uint64_t code_offset, body_offset, admitted_offset;
    uint64_t source_first, source_last, code_size, mapping_epoch, token;
    uint64_t loop_pc, certificate_identity;
    uint32_t instruction_count;
    uint16_t relocation_count, decoded_count;
    uint32_t conditional_self_loop, cycle_safe;
} cache_entry;

typedef struct provenance_entry {
    _Atomic uint64_t sequence, code_first, code_last, guest;
    _Atomic uint64_t address_displacement, address_constant, address_shape;
    _Atomic uint32_t access, width;
    _Atomic uint32_t generation;
} provenance_entry;

typedef struct pending_relocation {
    uint64_t source_guest, source_instruction_epoch, source_code_offset;
    uint64_t source_certificate_identity, target_certificate_identity;
    hl_native_relocation relocation;
    uint32_t generation;
} pending_relocation;

typedef struct resolved_relocation {
    uint64_t source_guest, source_instruction_epoch, source_code_offset;
    uint64_t source_certificate_identity, target_certificate_identity;
    hl_native_relocation relocation;
    uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS];
    uint32_t generation;
    uint32_t target_epoch_wildcard;
} resolved_relocation;

_Static_assert(sizeof(cache_entry) == 136, "cache entry footprint drifted");
_Static_assert(sizeof(pending_relocation) == 160, "pending relocation footprint drifted");
_Static_assert(sizeof(resolved_relocation) == 224, "resolved relocation footprint drifted");

struct hl_native_cache {
    hl_native_arena *arena;
    cache_entry *entries;
    provenance_entry *provenance;
    uint32_t *live;
    uint32_t capacity, provenance_capacity, provenance_next, live_count, generation;
    _Atomic uint32_t published_generation;
    _Atomic uint64_t provenance_epoch;
    uint32_t hash_shift, hash_rotate;
    _Atomic uint64_t mapping_epoch, instruction_epoch;
    _Atomic uint64_t memory_mode, authority_generation;
    uint64_t next_token, active_token;
    uint32_t poisoned;
    hl_native_cache_stats stats;
    pending_relocation *relocations;
    uint32_t relocation_count, relocation_capacity;
    resolved_relocation *resolved;
    uint32_t resolved_count, resolved_capacity;
    hl_native_certificate_record *certificates;
    _Atomic unsigned char *certificate_valid;
    _Atomic uint32_t certificate_used;
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

/* Fibonacci hash read from the product's high bits. The low bits carry no
 * mixing, and aligned guest PCs left the bottom index bits permanently zero. */
static inline uint32_t hl_native_cache_home(const hl_native_cache *cache, uint64_t guest) {
    return (uint32_t)(((guest >> cache->hash_shift) * UINT64_C(0x9E3779B97F4A7C15)) >> cache->hash_rotate) &
           (cache->capacity - 1);
}

static inline int hl_native_cache_find_identity(const hl_native_cache *cache, uint64_t guest,
                                                uint64_t memory_mode, uint64_t authority_generation) {
    uint32_t start = hl_native_cache_home(cache, guest);
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
