#include "private.h"

#include <stdlib.h>
#include <stdatomic.h>
#include <string.h>

#define HL_UINT64_LOCK_FREE                                                                                             \
    _Generic((uint64_t)0, unsigned long: ATOMIC_LONG_LOCK_FREE, unsigned long long: ATOMIC_LLONG_LOCK_FREE, default: 0)
_Static_assert(HL_UINT64_LOCK_FREE == 2, "fault provenance requires lock-free uint64_t atomics");
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "fault provenance requires lock-free 32-bit atomics");

enum { RELOCATION_CAPACITY = 1u << 16 };

static uint64_t address_shape(const hl_native_address *address) {
    return (uint64_t)address->kind | (uint64_t)address->bits << 8 | (uint64_t)address->base << 16 |
           (uint64_t)address->index << 24 | (uint64_t)address->shift << 32 |
           (uint64_t)address->extend << 40;
}

static void provenance_store(provenance_entry *entry, uint64_t first, uint64_t last,
                             const hl_native_provenance *record, uint32_t generation) {
    uint64_t sequence = atomic_load_explicit(&entry->sequence, memory_order_relaxed);
    if ((sequence & 1) != 0) sequence++;
    atomic_store_explicit(&entry->sequence, sequence + 1, memory_order_release);
    atomic_store_explicit(&entry->code_first, first, memory_order_relaxed);
    atomic_store_explicit(&entry->code_last, last, memory_order_relaxed);
    atomic_store_explicit(&entry->guest, record->guest, memory_order_relaxed);
    atomic_store_explicit(&entry->address_displacement, (uint64_t)record->address.displacement,
                          memory_order_relaxed);
    atomic_store_explicit(&entry->address_constant, record->address.constant, memory_order_relaxed);
    atomic_store_explicit(&entry->address_shape, address_shape(&record->address), memory_order_relaxed);
    atomic_store_explicit(&entry->access, record->access, memory_order_relaxed);
    atomic_store_explicit(&entry->width, record->width, memory_order_relaxed);
    atomic_store_explicit(&entry->generation, generation, memory_order_relaxed);
    atomic_store_explicit(&entry->sequence, sequence + 2, memory_order_release);
}

static void provenance_clear(provenance_entry *entry) {
    const hl_native_provenance empty = {0};
    provenance_store(entry, 0, 0, &empty, 0);
}

static int address_valid(const hl_native_address *address) {
    if (address->kind > HL_NATIVE_ADDRESS_INDEXED || (address->bits != 0 && address->bits != 32 && address->bits != 64) ||
        address->shift > 63 || address->extend > HL_NATIVE_EXTEND_S32 || address->reserved != 0)
        return 0;
    if (address->kind == HL_NATIVE_ADDRESS_NONE) return address->bits == 0;
    if (address->bits == 0) return 0;
    if (address->kind == HL_NATIVE_ADDRESS_CONSTANT) return 1;
    if (address->base >= 32) return 0;
    return address->kind != HL_NATIVE_ADDRESS_INDEXED || address->index < 32;
}

static int power_of_two(uint32_t value) {
    return value != 0 && (value & (value - 1)) == 0;
}

static uint32_t home(const hl_native_cache *cache, uint64_t guest) {
    return (uint32_t)((guest >> cache->hash_shift) * 2654435761u) & (cache->capacity - 1);
}

static int insertion(const hl_native_cache *cache, uint64_t guest) {
    uint32_t start = home(cache, guest), tombstone = UINT32_MAX;
    for (uint32_t probe = 0; probe < cache->capacity; probe++) {
        uint32_t slot = (start + probe) & (cache->capacity - 1);
        if (cache->entries[slot].generation != cache->generation || cache->entries[slot].state == ENTRY_EMPTY)
            return tombstone == UINT32_MAX ? (int)slot : (int)tombstone;
        if (cache->entries[slot].state == ENTRY_TOMBSTONE && tombstone == UINT32_MAX) tombstone = slot;
        if (cache->entries[slot].state != ENTRY_TOMBSTONE && cache->entries[slot].guest == guest) return -1;
    }
    return tombstone == UINT32_MAX ? -1 : (int)tombstone;
}

hl_native_status hl_native_cache_create(hl_native_cache **output, hl_native_arena *arena, uint32_t capacity,
                                        uint32_t provenance_capacity, uint32_t hash_shift, uint64_t mapping_epoch,
                                        const hl_native_cache_observer *observer) {
    hl_native_cache *cache;
    if (output == NULL) return HL_NATIVE_ARGUMENT;
    *output = NULL;
    if (arena == NULL || arena->memory.reserve == NULL || !power_of_two(capacity) || capacity > (1u << 19) ||
        !power_of_two(provenance_capacity) || provenance_capacity > (1u << 18) || hash_shift > 2)
        return HL_NATIVE_ARGUMENT;
    cache = calloc(1, sizeof(*cache));
    if (cache == NULL) return HL_NATIVE_MEMORY;
    cache->entries = calloc(capacity, sizeof(*cache->entries));
    cache->provenance = calloc(provenance_capacity, sizeof(*cache->provenance));
    cache->live = calloc(capacity, sizeof(*cache->live));
    cache->relocations = calloc(RELOCATION_CAPACITY, sizeof(*cache->relocations));
    cache->resolved = calloc(RELOCATION_CAPACITY, sizeof(*cache->resolved));
    cache->certificates = calloc(HL_NATIVE_CERTIFICATE_CAPACITY, sizeof(*cache->certificates));
    cache->certificate_valid = calloc(HL_NATIVE_CERTIFICATE_CAPACITY, sizeof(*cache->certificate_valid));
    if (cache->entries == NULL || cache->provenance == NULL || cache->live == NULL || cache->relocations == NULL ||
        cache->resolved == NULL || cache->certificates == NULL || cache->certificate_valid == NULL) {
        hl_native_cache_destroy(cache);
        return HL_NATIVE_MEMORY;
    }
    cache->arena = arena;
    cache->capacity = capacity;
    cache->provenance_capacity = provenance_capacity;
    cache->relocation_capacity = RELOCATION_CAPACITY;
    cache->resolved_capacity = RELOCATION_CAPACITY;
    atomic_init(&cache->certificate_used, 0);
    if (observer != NULL) cache->observer = *observer;
    cache->generation = 1;
    atomic_init(&cache->published_generation, 1);
    atomic_init(&cache->provenance_epoch, 0);
    cache->hash_shift = hash_shift;
    atomic_init(&cache->mapping_epoch, mapping_epoch);
    atomic_init(&cache->instruction_epoch, 0);
    atomic_init(&cache->memory_mode, 0);
    atomic_init(&cache->authority_generation, 0);
    cache->next_token = 1;
    *output = cache;
    return HL_NATIVE_OK;
}

#define PROBE_KEY_VALID(EPOCH_SINK, MISS_SINK, HIT_SINK) do { \
    if (mapping_epoch != cache->mapping_epoch || memory_mode != cache->memory_mode || \
        authority_generation != cache->authority_generation) { EPOCH_SINK; return HL_NATIVE_EPOCH; } \
    slot = hl_native_cache_find_identity(cache, guest, memory_mode, authority_generation); \
    if (slot < 0) { MISS_SINK; return HL_NATIVE_MISS; } \
    entry = &cache->entries[slot]; \
    if (entry->instruction_epoch != instruction_epoch) { MISS_SINK; return HL_NATIVE_MISS; } \
    output->entry = cache->arena->executable + entry->code_offset; \
    output->body = cache->arena->executable + entry->body_offset; \
    output->admitted = cache->arena->executable + entry->admitted_offset; \
    output->code_size = entry->code_size; \
    output->generation = cache->generation; \
    output->source_first = entry->source_first; \
    output->source_last = entry->source_last; \
    output->instruction_count = entry->instruction_count; \
    output->relocation_count = entry->relocation_count; \
    output->conditional_self_loop = entry->conditional_self_loop; \
    output->cycle_safe = entry->cycle_safe; \
    output->decoded_count = entry->decoded_count; \
    output->loop_pc = entry->loop_pc; \
    output->identity_token = entry->token; \
    output->mapping_epoch = entry->mapping_epoch; \
    output->instruction_epoch = entry->instruction_epoch; \
    output->memory_mode = entry->memory_mode; \
    output->authority_generation = entry->authority_generation; \
    output->certificate_identity = entry->certificate_identity; \
    HIT_SINK; \
    return HL_NATIVE_HIT; \
} while (0)

hl_native_lookup hl_native_cache_lookup_key(hl_native_cache *cache, uint64_t guest, uint64_t mapping_epoch,
                                            uint64_t instruction_epoch, uint64_t memory_mode,
                                            uint64_t authority_generation, hl_native_code *output) {
    int slot;
    cache_entry *entry;
    if (!hl_native_cache_available(cache) || output == NULL) return HL_NATIVE_MISS;
    memset(output, 0, sizeof(*output));
    cache->stats.lookups++;
    PROBE_KEY_VALID(cache->stats.epoch_rejections++, cache->stats.misses++, cache->stats.hits++);
}

hl_native_lookup hl_native_cache_probe_key(hl_native_cache *cache, uint64_t guest, uint64_t mapping_epoch,
                                           uint64_t instruction_epoch, uint64_t memory_mode,
                                           uint64_t authority_generation, hl_native_code *output) {
    if (!hl_native_cache_available(cache) || output == NULL) return HL_NATIVE_MISS;
    memset(output, 0, sizeof(*output));
    int slot;
    cache_entry *entry;
    PROBE_KEY_VALID((void)0, (void)0, (void)0);
}

static void count_saturating(uint64_t *value) {
    if (*value != UINT64_MAX) (*value)++;
}

hl_native_lookup hl_native_cache_probe_key_counted(hl_native_cache *cache,
                                                   hl_native_cache_lookup_counts *counts,
                                                   uint64_t guest, uint64_t mapping_epoch,
                                                   uint64_t instruction_epoch, uint64_t memory_mode,
                                                   uint64_t authority_generation, hl_native_code *output) {
    if (!hl_native_cache_available(cache) || counts == NULL || output == NULL) return HL_NATIVE_MISS;
    memset(output, 0, sizeof(*output));
    count_saturating(&counts->lookups);
    int slot;
    cache_entry *entry;
    PROBE_KEY_VALID(count_saturating(&counts->epoch_rejections),
                    count_saturating(&counts->misses), count_saturating(&counts->hits));
}

#undef PROBE_KEY_VALID

hl_native_lookup hl_native_cache_lookup(hl_native_cache *cache, uint64_t guest, uint64_t mapping_epoch,
                                        hl_native_code *output) {
    return hl_native_cache_lookup_key(cache, guest, mapping_epoch, 0, 0, 0, output);
}

int hl_native_cache_available(const hl_native_cache *cache) {
    return cache != NULL && !cache->poisoned;
}

static void certificate_revoke(hl_native_cache *cache, uint64_t identity) {
    if (cache == NULL || identity == 0) return;
    uint32_t used = atomic_load_explicit(&cache->certificate_used, memory_order_acquire);
    for (uint32_t index = 0; index < used; ++index)
        if (cache->certificates[index].identity == identity) {
            atomic_store_explicit(&cache->certificate_valid[index], 0, memory_order_release);
            return;
        }
}

static uint64_t certificate_reserve(hl_native_cache *cache,
                                  const hl_native_certificate_record *record) {
    if (record == NULL || record->identity == 0) return 0;
    uint32_t index = atomic_load_explicit(&cache->certificate_used, memory_order_relaxed);
    if (index == HL_NATIVE_CERTIFICATE_CAPACITY) return 0;
    cache->certificates[index] = *record;
    atomic_store_explicit(&cache->certificate_used, index + 1, memory_order_release);
    return record->identity;
}

static void certificate_publish(hl_native_cache *cache, uint64_t identity) {
    if (identity == 0) return;
    uint32_t used = atomic_load_explicit(&cache->certificate_used, memory_order_acquire);
    for (uint32_t index = 0; index < used; ++index)
        if (cache->certificates[index].identity == identity) {
            atomic_store_explicit(&cache->certificate_valid[index], 1, memory_order_release);
            return;
        }
}

_Static_assert(sizeof(hl_native_certificate_record) == 80,
               "certificate record footprint drifted");

int hl_native_cache_certificate(const hl_native_cache *cache, uint64_t identity,
                                hl_native_certificate_record *output) {
    if (cache == NULL || identity == 0 || output == NULL) return 0;
    uint32_t used = atomic_load_explicit(&cache->certificate_used, memory_order_acquire);
    for (uint32_t index = 0; index < used; ++index) {
        if (cache->certificates[index].identity != identity) continue;
        if (atomic_load_explicit(&cache->certificate_valid[index], memory_order_acquire) == 0) return 0;
        *output = cache->certificates[index];
        return atomic_load_explicit(&cache->certificate_valid[index], memory_order_acquire) != 0;
    }
    return 0;
}

void hl_native_cache_certificates_clear(hl_native_cache *cache) {
    if (cache == NULL) return;
    for (uint32_t index = 0; index < HL_NATIVE_CERTIFICATE_CAPACITY; ++index)
        atomic_store_explicit(&cache->certificate_valid[index], 0, memory_order_release);
    for (uint32_t index = 0; index < cache->live_count; ++index)
        cache->entries[cache->live[index]].certificate_identity = 0;
}

hl_native_status hl_native_cache_write_begin(hl_native_cache *cache) {
    if (!hl_native_cache_available(cache)) return HL_NATIVE_STATE;
    return hl_native_arena_begin(cache->arena);
}

hl_native_status hl_native_cache_write_end(hl_native_cache *cache) {
    hl_native_status status;
    if (cache == NULL) return HL_NATIVE_ARGUMENT;
    status = hl_native_arena_end(cache->arena);
    if (status != HL_NATIVE_OK) hl_native_cache_fail(cache);
    return status;
}

hl_native_status hl_native_cache_reserve_key(hl_native_cache *cache, uint64_t guest, uint64_t mapping_epoch,
                                             uint64_t instruction_epoch, uint64_t memory_mode,
                                             uint64_t authority_generation, uint64_t source_first,
                                             uint64_t source_last, uint64_t maximum,
                                             const hl_native_certificate_record *certificate,
                                             hl_native_block *block) {
    hl_native_span span;
    int slot;
    if (block == NULL) return HL_NATIVE_ARGUMENT;
    memset(block, 0, sizeof(*block));
    if (cache == NULL || mapping_epoch != cache->mapping_epoch || memory_mode != cache->memory_mode ||
        authority_generation != cache->authority_generation || source_last <= source_first || maximum == 0)
        return HL_NATIVE_ARGUMENT;
    if (cache->active_token != 0) return HL_NATIVE_STATE;
    if (hl_native_cache_find_identity(cache, guest, memory_mode, authority_generation) >= 0)
        return HL_NATIVE_STATE;
    slot = insertion(cache, guest);
    if (slot < 0 || cache->live_count >= cache->capacity) return HL_NATIVE_CAPACITY;
    hl_native_status status = hl_native_arena_allocate(cache->arena, maximum, 16, &span);
    if (status != HL_NATIVE_OK) return status;
    cache_entry *entry = &cache->entries[slot];
    memset(entry, 0, sizeof(*entry));
    entry->guest = guest;
    entry->source_first = source_first;
    entry->source_last = source_last;
    entry->code_offset = span.offset;
    entry->code_size = maximum;
    entry->body_offset = span.offset;
    entry->admitted_offset = span.offset;
    entry->mapping_epoch = mapping_epoch;
    entry->instruction_epoch = instruction_epoch;
    entry->memory_mode = memory_mode;
    entry->authority_generation = authority_generation;
    entry->certificate_identity = certificate_reserve(cache, certificate);
    entry->token = cache->next_token++;
    if (entry->token == 0) entry->token = cache->next_token++;
    entry->generation = cache->generation;
    entry->state = ENTRY_RESERVED;
    cache->active_token = entry->token;
    block->guest = guest;
    block->source_first = source_first;
    block->source_last = source_last;
    block->code_offset = span.offset;
    block->code_size = maximum;
    block->body_offset = span.offset;
    block->admitted_offset = span.offset;
    block->mapping_epoch = mapping_epoch;
    block->instruction_epoch = instruction_epoch;
    block->memory_mode = memory_mode;
    block->authority_generation = authority_generation;
    block->certificate_identity = entry->certificate_identity;
    block->token = entry->token;
    block->slot = (uint32_t)slot;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_cache_reserve(hl_native_cache *cache, uint64_t guest, uint64_t mapping_epoch,
                                         uint64_t source_first, uint64_t source_last, uint64_t maximum,
                                         hl_native_block *block) {
    return hl_native_cache_reserve_key(cache, guest, mapping_epoch, 0, 0, 0,
                                       source_first, source_last, maximum, NULL, block);
}

hl_native_status hl_native_cache_writable(hl_native_cache *cache, const hl_native_block *block, void **bytes,
                                           uint64_t *capacity) {
    cache_entry *entry;
    if (cache == NULL || block == NULL || bytes == NULL || capacity == NULL || block->slot >= cache->capacity)
        return HL_NATIVE_ARGUMENT;
    *bytes = NULL;
    *capacity = 0;
    entry = &cache->entries[block->slot];
    if (entry->generation != cache->generation || entry->state != ENTRY_RESERVED ||
        entry->token != block->token || entry->code_offset != block->code_offset ||
        entry->code_size != block->code_size || cache->active_token != block->token)
        return HL_NATIVE_STATE;
    *bytes = cache->arena->writable + block->code_offset;
    *capacity = block->code_size;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_cache_publish_map(hl_native_cache *cache, hl_native_block *block, uint64_t used,
                                             uint64_t body_offset, const hl_native_provenance *records,
                                             uint32_t record_count) {
    hl_native_span span;
    cache_entry *entry;
    hl_native_status status;
    if (cache == NULL || block == NULL || block->slot >= cache->capacity || used == 0 || used > block->code_size ||
        body_offset >= used || block->admitted_offset < block->code_offset ||
        block->admitted_offset >= block->code_offset + used || records == NULL || record_count == 0 ||
        record_count > cache->provenance_capacity)
        return HL_NATIVE_ARGUMENT;
    uint64_t previous_end = 0;
    for (uint32_t index = 0; index < record_count; index++) {
        if (records[index].code_size == 0 || records[index].code_offset >= used ||
            records[index].code_size > used - records[index].code_offset ||
            records[index].code_offset < previous_end || records[index].guest < block->source_first ||
            records[index].guest >= block->source_last || records[index].access > HL_NATIVE_ACCESS_EXECUTE ||
            records[index].width > 64 || !address_valid(&records[index].address) ||
            ((records[index].access == HL_NATIVE_ACCESS_UNKNOWN) != (records[index].width == 0)) ||
            (records[index].access != HL_NATIVE_ACCESS_UNKNOWN && records[index].address.kind == HL_NATIVE_ADDRESS_NONE))
            return HL_NATIVE_ARGUMENT;
        previous_end = records[index].code_offset + records[index].code_size;
    }
    entry = &cache->entries[block->slot];
    if (entry->generation != cache->generation || entry->state != ENTRY_RESERVED || entry->token != block->token ||
        entry->guest != block->guest || entry->mapping_epoch != cache->mapping_epoch ||
        entry->instruction_epoch != block->instruction_epoch ||
        entry->memory_mode != block->memory_mode ||
        entry->authority_generation != block->authority_generation ||
        entry->certificate_identity != block->certificate_identity ||
        entry->source_first != block->source_first || entry->source_last != block->source_last ||
        entry->code_offset != block->code_offset || entry->code_size != block->code_size)
        return HL_NATIVE_STATE;
    span.writable = cache->arena->writable + block->code_offset;
    span.executable = cache->arena->executable + block->code_offset;
    span.offset = block->code_offset;
    span.capacity = block->code_size;
    status = hl_native_arena_publish(cache->arena, &span, used);
    if (status != HL_NATIVE_OK) {
        certificate_revoke(cache, entry->certificate_identity);
        entry->certificate_identity = 0;
        entry->state = ENTRY_TOMBSTONE;
        cache->active_token = 0;
        return status;
    }
    atomic_fetch_add_explicit(&cache->provenance_epoch, 1, memory_order_acq_rel);
    atomic_store_explicit(&cache->published_generation, 0, memory_order_release);
    entry->source_first = block->source_first;
    entry->source_last = block->source_last;
    entry->code_offset = block->code_offset;
    entry->code_size = used;
    entry->body_offset = block->code_offset + body_offset;
    entry->admitted_offset = block->admitted_offset;
    entry->instruction_count = block->instruction_count != 0 ? block->instruction_count : 1u;
    entry->relocation_count = block->relocation_count;
    entry->conditional_self_loop = block->conditional_self_loop;
    entry->cycle_safe = block->cycle_safe;
    entry->decoded_count = block->decoded_count;
    entry->loop_pc = block->loop_pc;
    /* Publication makes bytes executable first. Provenance is then complete
     * before the live identity can be observed by a dispatcher lookup. */
    for (uint32_t index = 0; index < record_count; index++) {
        provenance_entry *provenance =
            &cache->provenance[cache->provenance_next++ & (cache->provenance_capacity - 1)];
        uint64_t first = block->code_offset + records[index].code_offset;
        provenance_store(provenance, first, first + records[index].code_size, &records[index],
                         cache->generation);
    }
    entry->state = ENTRY_LIVE;
    cache->live[cache->live_count++] = block->slot;
    cache->stats.publications++;
    cache->active_token = 0;
    atomic_store_explicit(&cache->published_generation, cache->generation, memory_order_release);
    atomic_fetch_add_explicit(&cache->provenance_epoch, 1, memory_order_release);
    certificate_publish(cache, entry->certificate_identity);
    memset(block, 0, sizeof(*block));
    return HL_NATIVE_OK;
}

void hl_native_cache_fail(hl_native_cache *cache) {
    if (cache != NULL) {
        cache->poisoned = 1;
        hl_native_cache_certificates_clear(cache);
    }
}

hl_native_status hl_native_cache_publish(hl_native_cache *cache, hl_native_block *block, uint64_t used,
                                         uint64_t body_offset) {
    hl_native_provenance record = {0};
    if (block == NULL) return HL_NATIVE_ARGUMENT;
    record.code_offset = 0;
    record.code_size = used;
    record.guest = block->guest;
    return hl_native_cache_publish_map(cache, block, used, body_offset, &record, 1);
}

void hl_native_cache_cancel(hl_native_cache *cache, hl_native_block *block) {
    if (cache == NULL || block == NULL || block->slot >= cache->capacity) return;
    cache_entry *entry = &cache->entries[block->slot];
    if (entry->generation == cache->generation && entry->state == ENTRY_RESERVED && entry->token == block->token) {
        certificate_revoke(cache, entry->certificate_identity);
        entry->certificate_identity = 0;
        entry->state = ENTRY_TOMBSTONE;
    }
    if (cache->active_token == block->token) cache->active_token = 0;
    memset(block, 0, sizeof(*block));
}

int hl_native_cache_epoch_matches(const hl_native_cache *cache, uint64_t mapping_epoch,
                                  uint64_t instruction_epoch, uint64_t memory_mode,
                                  uint64_t authority_generation) {
    return cache != NULL && atomic_load_explicit(&cache->mapping_epoch, memory_order_acquire) == mapping_epoch &&
        atomic_load_explicit(&cache->instruction_epoch, memory_order_acquire) == instruction_epoch &&
        atomic_load_explicit(&cache->memory_mode, memory_order_acquire) == memory_mode &&
        atomic_load_explicit(&cache->authority_generation, memory_order_acquire) == authority_generation;
}

int hl_native_cache_address_identity_matches(const hl_native_cache *cache,
                                             uint64_t mapping_epoch, uint64_t memory_mode,
                                             uint64_t authority_generation) {
    return cache != NULL && atomic_load_explicit(&cache->mapping_epoch, memory_order_acquire) == mapping_epoch &&
        atomic_load_explicit(&cache->memory_mode, memory_order_acquire) == memory_mode &&
        atomic_load_explicit(&cache->authority_generation, memory_order_acquire) == authority_generation;
}

int hl_native_cache_execution(const hl_native_cache *cache, uint64_t identity, hl_native_code *output) {
    uintptr_t executable;
    if (cache == NULL || output == NULL || (identity & 3u) != 1u) return 0;
    memset(output, 0, sizeof(*output));
    executable = (uintptr_t)(identity & ~UINT64_C(3));
    for (uint32_t index = 0; index < cache->live_count; index++) {
        const cache_entry *entry = &cache->entries[cache->live[index]];
        uintptr_t first = (uintptr_t)cache->arena->executable + entry->code_offset;
        if (entry->generation != cache->generation || entry->state != ENTRY_LIVE ||
            executable < first || executable >= first + entry->code_size)
            continue;
        output->entry = (void *)first;
        output->body = cache->arena->executable + entry->body_offset;
        output->admitted = cache->arena->executable + entry->admitted_offset;
        output->code_size = entry->code_size;
        output->generation = entry->generation;
        output->source_first = entry->source_first;
        output->source_last = entry->source_last;
        output->instruction_count = entry->instruction_count;
        output->relocation_count = entry->relocation_count;
        output->conditional_self_loop = entry->conditional_self_loop;
        output->cycle_safe = entry->cycle_safe;
        output->decoded_count = entry->decoded_count;
        output->loop_pc = entry->loop_pc;
        output->identity_token = entry->token;
        output->mapping_epoch = entry->mapping_epoch;
        output->instruction_epoch = entry->instruction_epoch;
        output->memory_mode = entry->memory_mode;
        output->authority_generation = entry->authority_generation;
        output->certificate_identity = entry->certificate_identity;
        return 1;
    }
    return 0;
}

hl_native_status hl_native_cache_reset_identity(hl_native_cache *cache, uint64_t mapping_epoch,
                                                uint64_t instruction_epoch, uint64_t memory_mode,
                                                uint64_t authority_generation) {
    if (!hl_native_cache_available(cache)) return HL_NATIVE_STATE;
    atomic_store_explicit(&cache->published_generation, 0, memory_order_release);
    atomic_fetch_add_explicit(&cache->provenance_epoch, 1, memory_order_acq_rel);
    for (uint32_t index = 0; index < cache->provenance_capacity; index++)
        provenance_clear(&cache->provenance[index]);
    hl_native_cache_certificates_clear(cache);
    cache->generation++;
    if (cache->generation == 0) {
        memset(cache->entries, 0, cache->capacity * sizeof(*cache->entries));
        cache->generation = 1;
    }
    atomic_store_explicit(&cache->mapping_epoch, mapping_epoch, memory_order_release);
    atomic_store_explicit(&cache->instruction_epoch, instruction_epoch, memory_order_release);
    atomic_store_explicit(&cache->memory_mode, memory_mode, memory_order_release);
    atomic_store_explicit(&cache->authority_generation, authority_generation, memory_order_release);
    cache->live_count = 0;
    cache->relocation_count = 0;
    cache->resolved_count = 0;
    cache->provenance_next = 0;
    atomic_store_explicit(&cache->published_generation, cache->generation, memory_order_release);
    atomic_fetch_add_explicit(&cache->provenance_epoch, 1, memory_order_release);
    return HL_NATIVE_OK;
}

hl_native_status hl_native_cache_reset_epoch(hl_native_cache *cache, uint64_t mapping_epoch,
                                             uint64_t instruction_epoch) {
    return hl_native_cache_reset_identity(cache, mapping_epoch, instruction_epoch, 0, 0);
}

hl_native_status hl_native_cache_reset(hl_native_cache *cache, uint64_t mapping_epoch) {
    return hl_native_cache_reset_epoch(cache, mapping_epoch, 0);
}

hl_native_status hl_native_cache_invalidate(hl_native_cache *cache, uint64_t first, uint64_t last,
                                            uint32_t *removed) {
    uint32_t retained = 0, count = 0;
    if (cache == NULL || last <= first) return HL_NATIVE_ARGUMENT;
    if (!hl_native_cache_available(cache)) return HL_NATIVE_STATE;
    if (cache->active_token != 0) return HL_NATIVE_STATE;
    for (uint32_t index = 0; index < cache->live_count; index++) {
        uint32_t slot = cache->live[index];
        cache_entry *entry = &cache->entries[slot];
        if (!hl_native_cache_live(cache, slot)) continue;
        if (entry->source_first < last && first < entry->source_last) {
            certificate_revoke(cache, entry->certificate_identity);
            entry->certificate_identity = 0;
            entry->state = ENTRY_TOMBSTONE;
            count++;
        } else {
            cache->live[retained++] = slot;
        }
    }
    cache->live_count = retained;
    cache->stats.invalidations += count;
    if (removed != NULL) *removed = count;
    return HL_NATIVE_OK;
}

int hl_native_cache_provenance_record(const hl_native_cache *cache, const void *executable,
                                      hl_native_provenance *output) {
    uintptr_t pc, base;
    uint64_t offset, best = 0;
    hl_native_provenance source = {0};
    int found = 0;
    if (cache == NULL || executable == NULL) return 0;
    pc = (uintptr_t)executable;
    base = (uintptr_t)cache->arena->executable;
    if (pc < base || pc >= base + cache->arena->mapping.capacity) return 0;
    offset = pc - base;
    uint64_t epoch = atomic_load_explicit(&cache->provenance_epoch, memory_order_acquire);
    uint32_t generation;
    if ((epoch & 1) != 0) return 0;
    generation = atomic_load_explicit(&cache->published_generation, memory_order_acquire);
    for (uint32_t index = 0; index < cache->provenance_capacity; index++) {
        const provenance_entry *entry = &cache->provenance[index];
        uint64_t before = atomic_load_explicit(&entry->sequence, memory_order_acquire);
        uint64_t first, last, shape;
        hl_native_provenance candidate = {0};
        uint32_t entry_generation;
        if ((before & 1) != 0) continue;
        first = atomic_load_explicit(&entry->code_first, memory_order_relaxed);
        last = atomic_load_explicit(&entry->code_last, memory_order_relaxed);
        candidate.code_offset = first;
        candidate.code_size = last - first;
        candidate.guest = atomic_load_explicit(&entry->guest, memory_order_relaxed);
        candidate.address.displacement = (int64_t)atomic_load_explicit(&entry->address_displacement, memory_order_relaxed);
        candidate.address.constant = atomic_load_explicit(&entry->address_constant, memory_order_relaxed);
        shape = atomic_load_explicit(&entry->address_shape, memory_order_relaxed);
        candidate.address.kind = (uint8_t)shape;
        candidate.address.bits = (uint8_t)(shape >> 8);
        candidate.address.base = (uint8_t)(shape >> 16);
        candidate.address.index = (uint8_t)(shape >> 24);
        candidate.address.shift = (uint8_t)(shape >> 32);
        candidate.address.extend = (uint8_t)(shape >> 40);
        candidate.access = atomic_load_explicit(&entry->access, memory_order_relaxed);
        candidate.width = atomic_load_explicit(&entry->width, memory_order_relaxed);
        entry_generation = atomic_load_explicit(&entry->generation, memory_order_relaxed);
        if (before != atomic_load_explicit(&entry->sequence, memory_order_acquire)) continue;
        if (entry_generation == generation && first <= offset && offset < last && first >= best) {
            best = first;
            source = candidate;
            found = 1;
        }
    }
    if (epoch != atomic_load_explicit(&cache->provenance_epoch, memory_order_acquire)) return 0;
    if (!found) return 0;
    if (output != NULL) *output = source;
    return 1;
}

int hl_native_cache_provenance(const hl_native_cache *cache, const void *executable, uint64_t *guest) {
    hl_native_provenance record;
    if (!hl_native_cache_provenance_record(cache, executable, &record)) return 0;
    if (guest != NULL) *guest = record.guest;
    return 1;
}

int hl_native_address_reconstruct(const hl_native_address *address, const uint64_t *registers,
                                  uint32_t register_count, uint64_t *output) {
    uint64_t value, index;
    if (address == NULL || output == NULL || !address_valid(address) || address->kind == HL_NATIVE_ADDRESS_NONE)
        return 0;
    if (address->kind == HL_NATIVE_ADDRESS_CONSTANT) {
        value = address->constant;
    } else {
        if (registers == NULL || address->base >= register_count) return 0;
        value = registers[address->base];
        if (address->kind == HL_NATIVE_ADDRESS_INDEXED) {
            if (address->index >= register_count) return 0;
            index = registers[address->index];
            if (address->extend == HL_NATIVE_EXTEND_U32) index = (uint32_t)index;
            if (address->extend == HL_NATIVE_EXTEND_S32) index = (uint64_t)(int64_t)(int32_t)index;
            value += index << address->shift;
        }
        value += (uint64_t)address->displacement;
    }
    if (address->bits == 32) value = (uint32_t)value;
    *output = value;
    return 1;
}

void hl_native_cache_diagnose(const hl_native_cache *cache, hl_native_cache_stats *output) {
    if (cache == NULL || output == NULL) return;
    *output = cache->stats;
    output->live_blocks = cache->live_count;
    output->generation = cache->generation;
    output->mapping_epoch = atomic_load_explicit(&cache->mapping_epoch, memory_order_acquire);
}

void hl_native_cache_destroy(hl_native_cache *cache) {
    if (cache == NULL) return;
    free(cache->live);
    free(cache->provenance);
    free(cache->entries);
    free(cache->relocations);
    free(cache->resolved);
    free(cache->certificates);
    free(cache->certificate_valid);
    memset(cache, 0, sizeof(*cache));
    free(cache);
}
