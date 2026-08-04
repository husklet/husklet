#ifndef HL_NATIVE_CACHE_H
#define HL_NATIVE_CACHE_H

#include "../src/arena.h"

typedef struct hl_native_cache hl_native_cache;

typedef enum hl_native_lookup {
    HL_NATIVE_MISS = 0,
    HL_NATIVE_HIT = 1,
    HL_NATIVE_EPOCH = 2,
} hl_native_lookup;

typedef struct hl_native_block {
    uint64_t guest;
    uint64_t source_first;
    uint64_t source_last;
    uint64_t code_offset;
    uint64_t code_size;
    uint64_t body_offset;
    uint64_t mapping_epoch;
    uint64_t instruction_epoch;
    uint64_t memory_mode;
    uint64_t authority_generation;
    uint64_t token;
    uint32_t instruction_count;
    uint32_t conditional_self_loop;
    uint32_t cycle_safe;
    uint64_t loop_pc;
    uint64_t identity_token;
    uint32_t slot;
} hl_native_block;

typedef struct hl_native_code {
    void *entry;
    void *body;
    uint64_t code_size;
    uint64_t generation;
    uint64_t source_first;
    uint64_t source_last;
    uint32_t instruction_count;
    uint32_t conditional_self_loop;
    uint32_t cycle_safe;
    uint64_t loop_pc;
    uint64_t identity_token;
    uint64_t mapping_epoch;
    uint64_t instruction_epoch;
    uint64_t memory_mode;
    uint64_t authority_generation;
} hl_native_code;

typedef struct hl_native_cache_stats {
    uint64_t lookups;
    uint64_t hits;
    uint64_t misses;
    uint64_t epoch_rejections;
    uint64_t publications;
    uint64_t invalidations;
    uint64_t live_blocks;
    uint64_t generation;
    uint64_t mapping_epoch;
} hl_native_cache_stats;

typedef struct hl_native_relocation {
    uint64_t code_offset;
    uint64_t target_guest;
    uint64_t target_instruction_epoch;
    uint32_t target_epoch_known;
    uint32_t expected;
    uint32_t reserved;
} hl_native_relocation;

hl_native_status hl_native_cache_create(hl_native_cache **, hl_native_arena *, uint32_t, uint32_t, uint32_t,
                                        uint64_t);
hl_native_lookup hl_native_cache_lookup(hl_native_cache *, uint64_t, uint64_t, hl_native_code *);
hl_native_lookup hl_native_cache_lookup_key(hl_native_cache *, uint64_t, uint64_t, uint64_t,
                                             uint64_t, uint64_t, hl_native_code *);
int hl_native_cache_available(const hl_native_cache *);
hl_native_status hl_native_cache_write_begin(hl_native_cache *);
hl_native_status hl_native_cache_write_end(hl_native_cache *);
hl_native_status hl_native_cache_reserve(hl_native_cache *, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t,
                                         hl_native_block *);
hl_native_status hl_native_cache_reserve_key(hl_native_cache *, uint64_t, uint64_t, uint64_t,
                                             uint64_t, uint64_t, uint64_t, uint64_t,
                                             uint64_t, hl_native_block *);
hl_native_status hl_native_cache_writable(hl_native_cache *, const hl_native_block *, void **, uint64_t *);
hl_native_status hl_native_cache_publish(hl_native_cache *, hl_native_block *, uint64_t, uint64_t);
hl_native_status hl_native_cache_publish_map(hl_native_cache *, hl_native_block *, uint64_t, uint64_t,
                                             const hl_native_provenance *, uint32_t);
void hl_native_cache_cancel(hl_native_cache *, hl_native_block *);
hl_native_status hl_native_cache_relocate(hl_native_cache *, uint64_t, uint64_t,
                                          const hl_native_relocation *, uint32_t);
hl_native_status hl_native_cache_relocate_site(hl_native_cache *, const void *, int64_t,
                                               uint64_t, uint64_t, uint64_t, uint32_t *);
hl_native_status hl_native_cache_resolve(hl_native_cache *, uint64_t, uint64_t);
void hl_native_cache_relocations_clear(hl_native_cache *);
hl_native_status hl_native_cache_relocations_invalidate(hl_native_cache *, uint64_t, uint64_t);
hl_native_status hl_native_cache_reset(hl_native_cache *, uint64_t);
hl_native_status hl_native_cache_reset_epoch(hl_native_cache *, uint64_t, uint64_t);
hl_native_status hl_native_cache_reset_identity(hl_native_cache *, uint64_t, uint64_t,
                                                uint64_t, uint64_t);
int hl_native_cache_epoch_matches(const hl_native_cache *, uint64_t, uint64_t,
                                  uint64_t, uint64_t);
int hl_native_cache_address_identity_matches(const hl_native_cache *, uint64_t,
                                             uint64_t, uint64_t);
int hl_native_cache_execution(const hl_native_cache *, uint64_t, hl_native_code *);
hl_native_status hl_native_cache_invalidate(hl_native_cache *, uint64_t, uint64_t, uint32_t *);
int hl_native_cache_provenance(const hl_native_cache *, const void *, uint64_t *);
int hl_native_cache_provenance_record(const hl_native_cache *, const void *, hl_native_provenance *);
int hl_native_address_reconstruct(const hl_native_address *, const uint64_t *, uint32_t, uint64_t *);
void hl_native_cache_diagnose(const hl_native_cache *, hl_native_cache_stats *);
void hl_native_cache_destroy(hl_native_cache *);

#endif
