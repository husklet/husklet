#include "lineage_registry.h"

#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>

#define HL_LINEAGE_ABI UINT64_C(0x484c4c494e454102)
#define HL_LINEAGE_KIND_MASK UINT64_C(3)
#define HL_LINEAGE_BANK_SHIFT 2u
#define HL_LINEAGE_GENERATION_SHIFT 3u
#define HL_LINEAGE_GENERATION_MAX (UINT64_MAX >> HL_LINEAGE_GENERATION_SHIFT)

static uint64_t lineage_state(uint64_t generation, unsigned bank, uint64_t kind) {
    return (generation << HL_LINEAGE_GENERATION_SHIFT) | ((uint64_t)bank << HL_LINEAGE_BANK_SHIFT) | kind;
}

static uint64_t lineage_kind(uint64_t state) {
    return state & HL_LINEAGE_KIND_MASK;
}

static uint64_t lineage_generation(uint64_t state) {
    return state >> HL_LINEAGE_GENERATION_SHIFT;
}

static unsigned lineage_bank(uint64_t state) {
    return (unsigned)((state >> HL_LINEAGE_BANK_SHIFT) & UINT64_C(1));
}

static const hl_lineage_record *lineage_record_view(const hl_lineage_slot *slot, uint64_t state) {
    return &slot->records[lineage_bank(state)];
}

static hl_lineage_record *lineage_record_write(hl_lineage_slot *slot, uint64_t state) {
    return &slot->records[lineage_bank(state) ^ 1u];
}

static uint64_t lineage_hash(hl_lineage_identity identity) {
    uint64_t value = identity.domain ^ (identity.sequence + UINT64_C(0x9e3779b97f4a7c15));
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value *= UINT64_C(0x94d049bb133111eb);
    return value ^ (value >> 31);
}

static int lineage_initialized(const hl_lineage_registry *registry) {
    return registry != NULL && registry->abi == HL_LINEAGE_ABI && registry->domain != 0 &&
           registry->capacity >= 2 && (registry->capacity & (registry->capacity - 1u)) == 0;
}

size_t hl_lineage_registry_size(uint64_t capacity) {
    if (capacity < 2 || (capacity & (capacity - 1u)) != 0 ||
        capacity > (SIZE_MAX - sizeof(hl_lineage_registry)) / sizeof(hl_lineage_slot))
        return 0;
    return sizeof(hl_lineage_registry) + (size_t)capacity * sizeof(hl_lineage_slot);
}

int hl_lineage_registry_init(hl_lineage_registry *registry, size_t size, uint64_t capacity, uint64_t domain,
                             int storage_is_zeroed) {
    size_t required = hl_lineage_registry_size(capacity);
    if (registry == NULL || required == 0 || size < required || domain == 0) return EINVAL;
    if (!storage_is_zeroed) {
        memset(registry, 0, required);
    } else if (registry->abi == HL_LINEAGE_ABI) {
        return EALREADY;
    }
    registry->size = required;
    registry->capacity = capacity;
    registry->domain = domain;
    atomic_store_explicit(&registry->next_identity, 1, memory_order_relaxed);
    atomic_store_explicit(&registry->next_generation, 1, memory_order_relaxed);
    atomic_store_explicit(&registry->writer_epoch, 1, memory_order_relaxed);
    atomic_store_explicit(&registry->clean_epoch, 1, memory_order_relaxed);
    registry->abi = HL_LINEAGE_ABI;
    return 0;
}

static int lineage_next(_Atomic uint64_t *counter, uint64_t maximum, uint64_t *value) {
    uint64_t current = atomic_load_explicit(counter, memory_order_relaxed);
#ifndef HL_LINEAGE_MUTATE_ALLOW_GENERATION_WRAP
    if (current == 0 || current > maximum) return EOVERFLOW;
#else
    if (current == 0 || current > maximum) current = 1;
#endif
    atomic_store_explicit(counter, current + 1u, memory_order_relaxed);
    *value = current;
    return 0;
}

#ifdef HL_LINEAGE_TEST_HOOKS
static uint64_t lineage_crash_after;
static int lineage_crash_point(void) {
    if (lineage_crash_after == 0) return 0;
    if (--lineage_crash_after == 0) return EINTR;
    return 0;
}
#else
static int lineage_crash_point(void) {
    return 0;
}
#endif

static int lineage_read_record(const hl_lineage_record *record, hl_lineage_identity *identity,
                               hl_lineage_value *value) {
    identity->domain = atomic_load_explicit(&record->identity_domain, memory_order_relaxed);
    identity->sequence = atomic_load_explicit(&record->identity_sequence, memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_REFERENCE_COUNT; ++index)
        value->references[index] = atomic_load_explicit(&record->references[index], memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_PAYLOAD_WORDS; ++index)
        value->payload[index] = atomic_load_explicit(&record->payload[index], memory_order_relaxed);
    return 0;
}

static int lineage_write_record(hl_lineage_record *record, hl_lineage_identity identity,
                                uint64_t token_generation, const hl_lineage_value *value) {
    atomic_store_explicit(&record->identity_domain, identity.domain, memory_order_relaxed);
    if (lineage_crash_point() != 0) return EINTR;
    atomic_store_explicit(&record->identity_sequence, identity.sequence, memory_order_relaxed);
    if (lineage_crash_point() != 0) return EINTR;
    atomic_store_explicit(&record->token_generation, token_generation, memory_order_relaxed);
    if (lineage_crash_point() != 0) return EINTR;
    for (unsigned index = 0; index < HL_LINEAGE_REFERENCE_COUNT; ++index) {
        atomic_store_explicit(&record->references[index], value->references[index], memory_order_relaxed);
        if (lineage_crash_point() != 0) return EINTR;
    }
    for (unsigned index = 0; index < HL_LINEAGE_PAYLOAD_WORDS; ++index) {
        atomic_store_explicit(&record->payload[index], value->payload[index], memory_order_relaxed);
        if (lineage_crash_point() != 0) return EINTR;
    }
    return 0;
}

int hl_lineage_registry_recover(hl_lineage_registry *registry) {
    if (!lineage_initialized(registry)) return EINVAL;
    uint64_t occupied = 0, tombstones = 0, maximum_generation = 0, maximum_identity = 0;
    for (uint64_t index = 0; index < registry->capacity; ++index) {
        const hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        uint64_t kind = lineage_kind(state);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EINVAL;
        if (kind == HL_LINEAGE_STATE_ACTIVE) {
            const hl_lineage_record *record = lineage_record_view(slot, state);
            uint64_t domain = atomic_load_explicit(&record->identity_domain, memory_order_relaxed);
            uint64_t sequence = atomic_load_explicit(&record->identity_sequence, memory_order_relaxed);
            if (domain != registry->domain || sequence == 0) return EINVAL;
            uint64_t token_generation = atomic_load_explicit(&record->token_generation, memory_order_relaxed);
            if (token_generation == 0) return EINVAL;
            if (sequence > maximum_identity) maximum_identity = sequence;
            if (token_generation > maximum_generation) maximum_generation = token_generation;
            ++occupied;
        } else if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
            ++tombstones;
        } else if (kind != HL_LINEAGE_STATE_EMPTY) {
            return EINVAL;
        }
        uint64_t generation = lineage_generation(state);
        if (generation > maximum_generation) maximum_generation = generation;
    }
    uint64_t next_identity = atomic_load_explicit(&registry->next_identity, memory_order_relaxed);
    if (next_identity != 0 && next_identity <= maximum_identity)
        atomic_store_explicit(&registry->next_identity, maximum_identity + 1u, memory_order_relaxed);
    uint64_t next_generation = atomic_load_explicit(&registry->next_generation, memory_order_relaxed);
    if (next_generation != 0 && next_generation <= maximum_generation)
        atomic_store_explicit(&registry->next_generation, maximum_generation + 1u, memory_order_relaxed);
#ifndef HL_LINEAGE_MUTATE_SKIP_RECOVERY_COUNTERS
    atomic_store_explicit(&registry->occupied, occupied, memory_order_relaxed);
    atomic_store_explicit(&registry->tombstones, tombstones, memory_order_relaxed);
#endif
    uint64_t epoch = atomic_load_explicit(&registry->writer_epoch, memory_order_acquire);
    atomic_store_explicit(&registry->clean_epoch, epoch, memory_order_release);
    return occupied <= registry->capacity / 2u ? 0 : EINVAL;
}

static int lineage_mutation_begin(hl_lineage_registry *registry, uint64_t *epoch) {
    uint64_t dirty = atomic_load_explicit(&registry->writer_epoch, memory_order_acquire);
    uint64_t clean = atomic_load_explicit(&registry->clean_epoch, memory_order_acquire);
    if (dirty != clean) {
        int error = hl_lineage_registry_recover(registry);
        if (error != 0) return error;
    }
    int error = lineage_next(&registry->writer_epoch, UINT64_MAX - 1u, epoch);
    if (error != 0) return error;
    if (lineage_crash_point() != 0) return EINTR;
    return 0;
}

static int lineage_recover_if_dirty(hl_lineage_registry *registry) {
    return atomic_load_explicit(&registry->writer_epoch, memory_order_acquire) ==
                   atomic_load_explicit(&registry->clean_epoch, memory_order_acquire)
               ? 0
               : hl_lineage_registry_recover(registry);
}

static int lineage_mutation_finish(hl_lineage_registry *registry, uint64_t epoch) {
    atomic_store_explicit(&registry->clean_epoch, epoch + 1u, memory_order_release);
    return lineage_crash_point();
}

static int lineage_find(const hl_lineage_registry *registry, hl_lineage_identity identity, uint64_t *found) {
    uint64_t start = lineage_hash(identity) & (registry->capacity - 1u);
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t index = (start + probe) & (registry->capacity - 1u);
        const hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        uint64_t kind = lineage_kind(state);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
        if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
#ifndef HL_LINEAGE_MUTATE_STOP_AT_TOMBSTONE
            continue;
#else
            return ENOENT;
#endif
        }
        if (kind == HL_LINEAGE_STATE_EMPTY) return ENOENT;
        const hl_lineage_record *record = lineage_record_view(slot, state);
        if (atomic_load_explicit(&record->identity_domain, memory_order_relaxed) == identity.domain &&
            atomic_load_explicit(&record->identity_sequence, memory_order_relaxed) == identity.sequence) {
            if (atomic_load_explicit(&slot->state, memory_order_acquire) != state) return EAGAIN;
            *found = index;
            return 0;
        }
    }
    return ENOENT;
}

static int lineage_reusable(const hl_lineage_registry *registry, hl_lineage_identity identity, uint64_t *index,
                            uint64_t *state) {
    uint64_t remembered = UINT64_MAX;
    uint64_t start = lineage_hash(identity) & (registry->capacity - 1u);
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t candidate = (start + probe) & (registry->capacity - 1u);
        uint64_t observed = atomic_load_explicit(&registry->slots[candidate].state, memory_order_acquire);
        uint64_t kind = lineage_kind(observed);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
        if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
#ifdef HL_LINEAGE_MUTATE_STOP_AT_TOMBSTONE
            *index = candidate;
            *state = observed;
            return 0;
#else
            if (remembered == UINT64_MAX) remembered = candidate;
            continue;
#endif
        }
        if (kind == HL_LINEAGE_STATE_EMPTY) {
            *index = remembered == UINT64_MAX ? candidate : remembered;
            *state = atomic_load_explicit(&registry->slots[*index].state, memory_order_acquire);
            return 0;
        }
    }
    if (remembered == UINT64_MAX) return ENOSPC;
    *index = remembered;
    *state = atomic_load_explicit(&registry->slots[remembered].state, memory_order_acquire);
    return lineage_kind(*state) == HL_LINEAGE_STATE_TOMBSTONE ? 0 : EAGAIN;
}

static int lineage_publish(hl_lineage_registry *registry, hl_lineage_identity identity, hl_lineage_value value,
                           hl_lineage_token *token) {
    uint64_t generation, reusable, prior, epoch;
#ifndef HL_LINEAGE_MUTATE_DISABLE_QUOTA
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) >= registry->capacity / 2u) return ENOSPC;
#endif
    int error = lineage_reusable(registry, identity, &reusable, &prior);
    if (error != 0) return error;
    error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[reusable];
    error = lineage_mutation_begin(registry, &epoch);
    if (error != 0) return error;
    error = lineage_write_record(lineage_record_write(slot, prior), identity, generation, &value);
    if (error != 0) return error;
    uint64_t published = lineage_state(generation, lineage_bank(prior) ^ 1u, HL_LINEAGE_STATE_ACTIVE);
    if (!atomic_compare_exchange_strong_explicit(&slot->state, &prior, published,
                                                 memory_order_release, memory_order_acquire))
        return EAGAIN;
    if (lineage_crash_point() != 0) return EINTR;
#ifndef HL_LINEAGE_MUTATE_SKIP_OCCUPIED_TRANSITION
    atomic_fetch_add_explicit(&registry->occupied, 1, memory_order_relaxed);
#endif
    if (lineage_crash_point() != 0) return EINTR;
    if (lineage_kind(prior) == HL_LINEAGE_STATE_TOMBSTONE)
#ifndef HL_LINEAGE_MUTATE_SKIP_TOMBSTONE_TRANSITION
        atomic_fetch_sub_explicit(&registry->tombstones, 1, memory_order_relaxed);
#else
        (void)0;
#endif
    if (lineage_crash_point() != 0) return EINTR;
    error = lineage_mutation_finish(registry, epoch);
    if (error != 0) return error;
    *token = (hl_lineage_token){identity, reusable, generation};
    return 0;
}

int hl_lineage_registry_create(hl_lineage_registry *registry, hl_lineage_value value, hl_lineage_token *token) {
    uint64_t identity_sequence;
    if (!lineage_initialized(registry) || token == NULL) return EINVAL;
    int error = lineage_recover_if_dirty(registry);
    if (error != 0) return error;
    error = lineage_next(&registry->next_identity, UINT64_MAX, &identity_sequence);
    if (error != 0) return error;
    return lineage_publish(registry, (hl_lineage_identity){registry->domain, identity_sequence}, value, token);
}

int hl_lineage_registry_insert(hl_lineage_registry *registry, hl_lineage_identity identity,
                               hl_lineage_value value, hl_lineage_token *token) {
    uint64_t found;
    if (!lineage_initialized(registry) || token == NULL || identity.domain != registry->domain ||
        identity.sequence == 0)
        return EINVAL;
    int error = lineage_recover_if_dirty(registry);
    if (error != 0) return error;
    error = lineage_find(registry, identity, &found);
    if (error != ENOENT) return error == 0 ? EEXIST : error;
    error = lineage_publish(registry, identity, value, token);
    if (error != 0) return error;
    uint64_t next = atomic_load_explicit(&registry->next_identity, memory_order_relaxed);
    if (next != 0 && next <= identity.sequence)
        atomic_store_explicit(&registry->next_identity, identity.sequence + 1u, memory_order_relaxed);
    return 0;
}

static int lineage_token_slot(const hl_lineage_registry *registry, hl_lineage_token token,
                              const hl_lineage_slot **output) {
    if (!lineage_initialized(registry) || token.slot >= registry->capacity || token.generation == 0 ||
        token.identity.domain == 0 || token.identity.sequence == 0)
        return EINVAL;
    const hl_lineage_slot *slot = &registry->slots[token.slot];
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
    const hl_lineage_record *record = lineage_record_view(slot, state);
    (void)record;
    if (lineage_kind(state) == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE ||
#if !defined(HL_LINEAGE_MUTATE_SKIP_GENERATION_RECHECK) && !defined(HL_LINEAGE_MUTATE_ACCEPT_STALE_TOKEN)
        atomic_load_explicit(&record->token_generation, memory_order_relaxed) != token.generation ||
#endif
#if !defined(HL_LINEAGE_MUTATE_SKIP_IDENTITY_RECHECK) && !defined(HL_LINEAGE_MUTATE_ACCEPT_STALE_TOKEN)
        atomic_load_explicit(&record->identity_domain, memory_order_relaxed) != token.identity.domain ||
        atomic_load_explicit(&record->identity_sequence, memory_order_relaxed) != token.identity.sequence ||
#endif
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return ESTALE;
    *output = slot;
    return 0;
}

static int lineage_read_value(const hl_lineage_slot *slot, uint64_t state, hl_lineage_value *value) {
    const hl_lineage_record *record = lineage_record_view(slot, state);
    for (unsigned index = 0; index < HL_LINEAGE_REFERENCE_COUNT; ++index)
        value->references[index] = atomic_load_explicit(&record->references[index], memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_PAYLOAD_WORDS; ++index)
        value->payload[index] = atomic_load_explicit(&record->payload[index], memory_order_relaxed);
    return atomic_load_explicit(&slot->state, memory_order_acquire) == state ? 0 : EAGAIN;
}

int hl_lineage_registry_find(const hl_lineage_registry *registry, hl_lineage_identity identity,
                             hl_lineage_token *token, hl_lineage_value *value) {
    uint64_t found;
    if (!lineage_initialized(registry) || identity.domain == 0 || identity.sequence == 0 || token == NULL ||
        value == NULL)
        return EINVAL;
    int error = lineage_find(registry, identity, &found);
    if (error != 0) return error;
    const hl_lineage_slot *slot = &registry->slots[found];
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
    const hl_lineage_record *record = lineage_record_view(slot, state);
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE) return EAGAIN;
    error = lineage_read_value(slot, state, value);
    if (error != 0 || atomic_load_explicit(&record->identity_domain, memory_order_relaxed) != identity.domain ||
        atomic_load_explicit(&record->identity_sequence, memory_order_relaxed) != identity.sequence ||
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return EAGAIN;
    *token = (hl_lineage_token){identity, found,
                                atomic_load_explicit(&record->token_generation, memory_order_relaxed)};
    return 0;
}

int hl_lineage_registry_lookup(const hl_lineage_registry *registry, hl_lineage_token token,
                               hl_lineage_value *value) {
    const hl_lineage_slot *slot;
    if (value == NULL) return EINVAL;
    int error = lineage_token_slot(registry, token, &slot);
    if (error != 0) return error;
#if !defined(HL_LINEAGE_MUTATE_SKIP_GENERATION_RECHECK) && !defined(HL_LINEAGE_MUTATE_ACCEPT_STALE_TOKEN)
    uint64_t observed = atomic_load_explicit(&slot->state, memory_order_acquire);
    uint64_t state = observed;
#else
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
#endif
    return lineage_read_value(slot, state, value);
}

int hl_lineage_registry_token_at(const hl_lineage_registry *registry, uint64_t index,
                                 hl_lineage_token *token, hl_lineage_value *value) {
    if (!lineage_initialized(registry) || index >= registry->capacity || token == NULL || value == NULL)
        return EINVAL;
    const hl_lineage_slot *slot = &registry->slots[index];
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
    const hl_lineage_record *record = lineage_record_view(slot, state);
    if (lineage_kind(state) == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE) return ENOENT;
    hl_lineage_identity identity = {
        atomic_load_explicit(&record->identity_domain, memory_order_relaxed),
        atomic_load_explicit(&record->identity_sequence, memory_order_relaxed),
    };
    int error = lineage_read_value(slot, state, value);
    if (error != 0 || identity.domain == 0 || identity.sequence == 0 ||
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return EAGAIN;
    *token = (hl_lineage_token){identity, index,
                                atomic_load_explicit(&record->token_generation, memory_order_relaxed)};
    return 0;
}

int hl_lineage_registry_replace(hl_lineage_registry *registry, hl_lineage_token token,
                                const hl_lineage_value *expected, hl_lineage_value replacement) {
    const hl_lineage_slot *view;
    if (expected == NULL) return EINVAL;
    int error = lineage_recover_if_dirty(registry);
    if (error != 0) return error;
    error = lineage_token_slot(registry, token, &view);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[token.slot];
    uint64_t active = atomic_load_explicit(&slot->state, memory_order_acquire);
    if (lineage_kind(active) != HL_LINEAGE_STATE_ACTIVE ||
        atomic_load_explicit(&lineage_record_view(slot, active)->token_generation, memory_order_relaxed) !=
            token.generation)
        return ESTALE;
    hl_lineage_value observed;
    hl_lineage_identity identity;
    lineage_read_record(lineage_record_view(slot, active), &identity, &observed);
#ifndef HL_LINEAGE_MUTATE_REPLACE_WITHOUT_EXPECTED
    if (memcmp(&observed, expected, sizeof observed) != 0) return EAGAIN;
#endif
    uint64_t epoch;
    error = lineage_mutation_begin(registry, &epoch);
    if (error != 0) return error;
    uint64_t revision;
#ifndef HL_LINEAGE_MUTATE_REUSE_REVISION
    error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &revision);
    if (error != 0) return error;
#else
    revision = lineage_generation(active);
#endif
    error = lineage_write_record(lineage_record_write(slot, active), identity, token.generation, &replacement);
    if (error != 0) return error;
    uint64_t published = lineage_state(revision, lineage_bank(active) ^ 1u, HL_LINEAGE_STATE_ACTIVE);
    if (!atomic_compare_exchange_strong_explicit(&slot->state, &active, published, memory_order_release,
                                                 memory_order_acquire))
        return EAGAIN;
    if (lineage_crash_point() != 0) return EINTR;
    return lineage_mutation_finish(registry, epoch);
}

int hl_lineage_registry_reference(hl_lineage_registry *registry, hl_lineage_token token,
                                  hl_lineage_reference reference,
                                  int64_t delta) {
    const hl_lineage_slot *view;
    if ((unsigned)reference >= HL_LINEAGE_REFERENCE_COUNT) return EINVAL;
    int error = lineage_recover_if_dirty(registry);
    if (error != 0) return error;
    error = lineage_token_slot(registry, token, &view);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[token.slot];
#ifndef HL_LINEAGE_MUTATE_ACCEPT_STALE_TOKEN
    uint64_t active = atomic_load_explicit(&slot->state, memory_order_acquire);
#else
    uint64_t active = atomic_load_explicit(&slot->state, memory_order_acquire);
#endif
    hl_lineage_identity identity;
    hl_lineage_value replacement;
    lineage_read_record(lineage_record_view(slot, active), &identity, &replacement);
    uint64_t current = replacement.references[reference];
    uint64_t magnitude = delta < 0 ? UINT64_C(0) - (uint64_t)delta : (uint64_t)delta;
    if ((delta < 0 && magnitude > current) || (delta > 0 && magnitude > UINT64_MAX - current)) {
        return ERANGE;
    }
    uint64_t updated = delta < 0 ? current - magnitude : current + magnitude;
    replacement.references[reference] = updated;
    uint64_t epoch;
    error = lineage_mutation_begin(registry, &epoch);
    if (error != 0) return error;
    uint64_t revision;
#ifndef HL_LINEAGE_MUTATE_REUSE_REVISION
    error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &revision);
    if (error != 0) return error;
#else
    revision = lineage_generation(active);
#endif
    error = lineage_write_record(lineage_record_write(slot, active), identity, token.generation, &replacement);
    if (error != 0) return error;
    uint64_t published = lineage_state(revision, lineage_bank(active) ^ 1u,
                                       HL_LINEAGE_STATE_ACTIVE);
    if (!atomic_compare_exchange_strong_explicit(&slot->state, &active, published, memory_order_release,
                                                 memory_order_acquire))
        return EAGAIN;
    if (lineage_crash_point() != 0) return EINTR;
    return lineage_mutation_finish(registry, epoch);
}

int hl_lineage_registry_reclaim(hl_lineage_registry *registry, uint64_t budget, uint64_t *reclaimed) {
    if (!lineage_initialized(registry) || reclaimed == NULL || budget > registry->capacity) return EINVAL;
    int error = lineage_recover_if_dirty(registry);
    if (error != 0) return error;
    uint64_t epoch;
    error = lineage_mutation_begin(registry, &epoch);
    if (error != 0) return error;
    uint64_t count = 0;
    uint64_t cursor = atomic_load_explicit(&registry->reclaim_cursor, memory_order_relaxed);
    for (uint64_t step = 0; step < budget; ++step) {
        uint64_t index = (cursor + step) & (registry->capacity - 1u);
        hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE) continue;
        const hl_lineage_record *record = lineage_record_view(slot, state);
        int retained = 0;
        for (unsigned reference = 0; reference < HL_LINEAGE_REFERENCE_COUNT; ++reference) {
#ifdef HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX
            if (reference == HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX) continue;
#endif
            retained |= atomic_load_explicit(&record->references[reference], memory_order_relaxed) != 0;
        }
        if (retained) continue;
        uint64_t generation;
        error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
        if (error != 0) return error;
        uint64_t tombstone = lineage_state(generation, lineage_bank(state), HL_LINEAGE_STATE_TOMBSTONE);
        if (!atomic_compare_exchange_strong_explicit(&slot->state, &state, tombstone, memory_order_release,
                                                     memory_order_acquire))
            continue;
        if (lineage_crash_point() != 0) return EINTR;
#ifndef HL_LINEAGE_MUTATE_SKIP_OCCUPIED_TRANSITION
        atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
#endif
        if (lineage_crash_point() != 0) return EINTR;
#ifndef HL_LINEAGE_MUTATE_SKIP_TOMBSTONE_TRANSITION
        atomic_fetch_add_explicit(&registry->tombstones, 1, memory_order_relaxed);
#endif
        if (lineage_crash_point() != 0) return EINTR;
        ++count;
    }
#ifndef HL_LINEAGE_MUTATE_SKIP_CURSOR_ADVANCE
    atomic_store_explicit(&registry->reclaim_cursor, cursor + budget, memory_order_relaxed);
#endif
    if (lineage_crash_point() != 0) return EINTR;
    error = lineage_mutation_finish(registry, epoch);
    if (error != 0) return error;
    *reclaimed = count;
    return 0;
}

int hl_lineage_registry_validate(const hl_lineage_registry *registry) {
    if (!lineage_initialized(registry)) return EINVAL;
    uint64_t occupied = 0, tombstones = 0;
    for (uint64_t index = 0; index < registry->capacity; ++index) {
        const hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        uint64_t kind = lineage_kind(state);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
        if (kind == HL_LINEAGE_STATE_ACTIVE) {
            const hl_lineage_record *record = lineage_record_view(slot, state);
            if (lineage_generation(state) == 0 ||
                atomic_load_explicit(&record->identity_domain, memory_order_relaxed) == 0 ||
                atomic_load_explicit(&record->identity_sequence, memory_order_relaxed) == 0 ||
                atomic_load_explicit(&record->token_generation, memory_order_relaxed) == 0)
                return EINVAL;
            ++occupied;
        } else if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
            if (lineage_generation(state) == 0) return EINVAL;
            ++tombstones;
        } else if (kind != HL_LINEAGE_STATE_EMPTY) {
            return EINVAL;
        }
        if (atomic_load_explicit(&slot->state, memory_order_acquire) != state) return EAGAIN;
    }
    return atomic_load_explicit(&registry->writer_epoch, memory_order_acquire) ==
                       atomic_load_explicit(&registry->clean_epoch, memory_order_acquire) &&
                   occupied == atomic_load_explicit(&registry->occupied, memory_order_relaxed) &&
                   tombstones == atomic_load_explicit(&registry->tombstones, memory_order_relaxed) &&
                   occupied <= registry->capacity / 2u
               ? 0
               : EINVAL;
}

#ifdef HL_LINEAGE_TEST_HOOKS
static uint64_t lineage_fixture_sequence(const hl_lineage_registry *registry, uint64_t slot, uint64_t first) {
    for (uint64_t sequence = first; sequence != 0; ++sequence)
        if ((lineage_hash((hl_lineage_identity){registry->domain, sequence}) & (registry->capacity - 1u)) == slot)
            return sequence;
    return 0;
}

static int lineage_crash_fixture(unsigned operation, uint64_t crash_after) {
    size_t size = hl_lineage_registry_size(64);
    hl_lineage_registry *registry = calloc(1, size);
    if (registry == NULL || hl_lineage_registry_init(registry, size, 64, 17, 1) != 0) {
        free(registry);
        return 1;
    }
    hl_lineage_value before = {.payload = {2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37}};
    hl_lineage_value after = before;
    after.payload[0] = 101;
    hl_lineage_token token = {0};
    if (operation != 0 && hl_lineage_registry_create(registry, before, &token) != 0) {
        free(registry);
        return 2;
    }
    lineage_crash_after = crash_after;
    int error;
    if (operation == 0) {
        error = hl_lineage_registry_create(registry, after, &token);
    } else if (operation == 1) {
        error = hl_lineage_registry_replace(registry, token, &before, after);
    } else if (operation == 2) {
        error = hl_lineage_registry_reference(registry, token, HL_LINEAGE_DESCRIPTOR_REFERENCE, 1);
    } else {
        uint64_t reclaimed = 0;
        atomic_store_explicit(&registry->reclaim_cursor, token.slot, memory_order_relaxed);
        error = hl_lineage_registry_reclaim(registry, 1, &reclaimed);
    }
    lineage_crash_after = 0;
    if (error != 0 && error != EINTR) {
        free(registry);
        return 3;
    }
    if (hl_lineage_registry_recover(registry) != 0 || hl_lineage_registry_validate(registry) != 0) {
        free(registry);
        return 4;
    }
    hl_lineage_value observed;
    hl_lineage_token found;
    int lookup = operation == 0 ? hl_lineage_registry_find(registry, (hl_lineage_identity){17, 1}, &found, &observed)
                                : hl_lineage_registry_lookup(registry, token, &observed);
    int result = 0;
    if (operation == 0) {
        if (lookup != ENOENT && (lookup != 0 || memcmp(&observed, &after, sizeof observed) != 0)) result = 5;
    } else if (operation == 1) {
        if (lookup != 0 || (memcmp(&observed, &before, sizeof observed) != 0 &&
                            memcmp(&observed, &after, sizeof observed) != 0))
            result = 6;
    } else if (operation == 2) {
        if (lookup != 0 || observed.references[HL_LINEAGE_DESCRIPTOR_REFERENCE] > 1 ||
            memcmp(observed.payload, before.payload, sizeof observed.payload) != 0)
            result = 7;
    } else if (lookup != 0 && lookup != ESTALE) {
        result = 8;
    }
    free(registry);
    return result;
}

int hl_lineage_registry_fixture(uint32_t scenario, uint64_t capacity, uint64_t iterations) {
    size_t size = hl_lineage_registry_size(capacity);
    hl_lineage_registry *registry = size ? calloc(1, size) : NULL;
    if (registry == NULL || hl_lineage_registry_init(registry, size, capacity, 7, 1) != 0) {
        free(registry);
        return 100;
    }
    hl_lineage_token sentinel, stale;
    hl_lineage_value live = {
        .references = {1, 0, 1, 0, 0},
        .payload = {11, 13, 17, 19, 23, 29, 31, 37},
    };
    hl_lineage_value zero = {.payload = {41, 43, 47, 53, 59, 61, 67, 71}}, observed;
    int result = 0;
    if (hl_lineage_registry_create(registry, live, &sentinel) != 0) result = 101;
    if (!result && scenario == 0 &&
        (hl_lineage_registry_lookup(registry, sentinel, &observed) != 0 ||
         memcmp(&observed, &live, sizeof live) ||
         hl_lineage_registry_init(registry, size, capacity, 7, 1) != EALREADY))
        result = 1;
    if (!result && scenario == 1) {
        stale = sentinel;
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 2;
        uint64_t reclaimed = 0;
        if (!result && (hl_lineage_registry_reclaim(registry, capacity, &reclaimed) != 0 || reclaimed != 1)) result = 3;
        uint64_t replacement_sequence = lineage_fixture_sequence(
            registry, stale.slot, atomic_load_explicit(&registry->next_identity, memory_order_relaxed));
        if (!result && replacement_sequence == 0) result = 4;
        atomic_store_explicit(&registry->next_identity, replacement_sequence, memory_order_relaxed);
        hl_lineage_token replacement;
        hl_lineage_value replacement_value = zero;
        replacement_value.references[HL_LINEAGE_DESCRIPTOR_REFERENCE] = 5;
        replacement_value.references[HL_LINEAGE_INFLIGHT_REFERENCE] = 7;
        if (!result && hl_lineage_registry_create(registry, replacement_value, &replacement) != 0) result = 4;
        if (!result &&
            (replacement.slot != stale.slot || replacement.generation == stale.generation ||
             hl_lineage_registry_reference(registry, stale, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != ESTALE ||
             hl_lineage_registry_reference(registry, stale, HL_LINEAGE_INFLIGHT_REFERENCE, -1) != ESTALE ||
             hl_lineage_registry_lookup(registry, replacement, &observed) != 0 ||
             memcmp(&observed, &replacement_value, sizeof observed) != 0))
            result = 5;
    }
    if (!result && scenario == 2) {
        for (uint64_t index = 1; index < capacity / 2u; ++index) {
            hl_lineage_token token;
            if (hl_lineage_registry_create(registry, live, &token) != 0) { result = 6; break; }
        }
        hl_lineage_token overflow;
        if (!result && hl_lineage_registry_create(registry, live, &overflow) != ENOSPC) result = 7;
    }
    if (!result && scenario == 3) {
        for (uint64_t index = 0; index < iterations; ++index) {
            hl_lineage_token token;
            uint64_t reclaimed = 0;
            if (hl_lineage_registry_create(registry, zero, &token) != 0) {
                result = 8;
                break;
            }
            atomic_store_explicit(&registry->reclaim_cursor, token.slot, memory_order_relaxed);
            if (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1 ||
                hl_lineage_registry_lookup(registry, sentinel, &observed) != 0 ||
                memcmp(&observed, &live, sizeof live)) {
                result = 8;
                break;
            }
        }
    }
    if (!result && scenario == 4) {
        uint64_t buckets[64] = {0};
        uint64_t first_sequence = 0, second_sequence = 0;
        for (uint64_t sequence = 2; sequence < 4096 && second_sequence == 0; ++sequence) {
            uint64_t bucket = lineage_hash((hl_lineage_identity){registry->domain, sequence}) & (capacity - 1u);
            if (buckets[bucket] != 0) {
                first_sequence = buckets[bucket];
                second_sequence = sequence;
            } else {
                buckets[bucket] = sequence;
            }
        }
        if (capacity > 64 || second_sequence == 0) result = 9;
        hl_lineage_token predecessor, displaced;
        if (!result) {
            atomic_store_explicit(&registry->next_identity, first_sequence, memory_order_relaxed);
            if (hl_lineage_registry_create(registry, live, &predecessor) != 0) result = 10;
            atomic_store_explicit(&registry->next_identity, second_sequence, memory_order_relaxed);
            if (!result && hl_lineage_registry_create(registry, live, &displaced) != 0) result = 10;
        }
        if (!result &&
            (hl_lineage_registry_reference(registry, predecessor, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
             hl_lineage_registry_reference(registry, predecessor, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0))
            result = 11;
        uint64_t reclaimed = 0;
        if (!result) {
            atomic_store_explicit(&registry->reclaim_cursor, predecessor.slot, memory_order_relaxed);
            if (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1) result = 12;
        }
        hl_lineage_token found;
        if (!result && (hl_lineage_registry_find(registry, displaced.identity, &found, &observed) != 0 ||
                        found.slot != displaced.slot))
            result = 13;
    }
    if (!result && scenario == 5) {
        stale = sentinel;
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 14;
        uint64_t reclaimed = 0;
        atomic_store_explicit(&registry->reclaim_cursor, sentinel.slot, memory_order_relaxed);
        if (!result && (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1)) result = 15;
        hl_lineage_token replacement;
        if (!result && hl_lineage_registry_create(registry, zero, &replacement) != 0) result = 16;
        if (!result) {
            hl_lineage_token forged = replacement;
            forged.generation = stale.generation;
            if (hl_lineage_registry_lookup(registry, forged, &observed) != ESTALE) result = 17;
        }
    }
    if (!result && scenario == 6) {
        atomic_store_explicit(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, memory_order_relaxed);
        hl_lineage_token last, overflow;
        if (hl_lineage_registry_create(registry, zero, &last) != 0 ||
            hl_lineage_registry_create(registry, zero, &overflow) != EOVERFLOW)
            result = 18;
    }
    if (!result && scenario == 7) {
        hl_lineage_token forged = sentinel;
        forged.identity.sequence++;
        if (hl_lineage_registry_lookup(registry, forged, &observed) != ESTALE) result = 20;
    }
    if (!result && scenario == 8) {
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 21;
        for (unsigned reference = 0; !result && reference < HL_LINEAGE_REFERENCE_COUNT; ++reference) {
            uint64_t reclaimed = UINT64_MAX;
            if (hl_lineage_registry_reference(registry, sentinel, (hl_lineage_reference)reference, 1) != 0)
                result = 22;
            atomic_store_explicit(&registry->reclaim_cursor, sentinel.slot, memory_order_relaxed);
            if (!result &&
                (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 0 ||
                 hl_lineage_registry_lookup(registry, sentinel, &observed) != 0))
                result = 23;
            if (!result &&
                hl_lineage_registry_reference(registry, sentinel, (hl_lineage_reference)reference, -1) != 0)
                result = 24;
        }
        uint64_t reclaimed = 0;
        atomic_store_explicit(&registry->reclaim_cursor, sentinel.slot, memory_order_relaxed);
        if (!result && (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1)) result = 25;
    }
    if (!result && scenario == 9) {
        size_t dirty_size = hl_lineage_registry_size(8);
        hl_lineage_registry *dirty = malloc(dirty_size);
        if (dirty == NULL) {
            result = 26;
        } else {
            memset(dirty, 0xa5, dirty_size);
            if (hl_lineage_registry_init(dirty, dirty_size, 8, 99, 0) != 0 ||
                hl_lineage_registry_validate(dirty) != 0)
                result = 27;
            free(dirty);
        }
    }
    if (!result && scenario == 10) {
        uint64_t start = capacity - 2u;
        atomic_store_explicit(&registry->reclaim_cursor, start, memory_order_relaxed);
        uint64_t reclaimed = 0;
        if (hl_lineage_registry_reclaim(registry, 3, &reclaimed) != 0 || reclaimed != 0 ||
            atomic_load_explicit(&registry->reclaim_cursor, memory_order_relaxed) != start + 3u)
            result = 28;
    }
    if (!result && scenario == 11) {
        if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) != 1 ||
            atomic_load_explicit(&registry->tombstones, memory_order_relaxed) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 29;
        uint64_t reclaimed = 0;
        atomic_store_explicit(&registry->reclaim_cursor, sentinel.slot, memory_order_relaxed);
        if (!result &&
            (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1 ||
             atomic_load_explicit(&registry->occupied, memory_order_relaxed) != 0 ||
             atomic_load_explicit(&registry->tombstones, memory_order_relaxed) != 1))
            result = 30;
        uint64_t sequence = lineage_fixture_sequence(
            registry, sentinel.slot, atomic_load_explicit(&registry->next_identity, memory_order_relaxed));
        atomic_store_explicit(&registry->next_identity, sequence, memory_order_relaxed);
        hl_lineage_token replacement;
        if (!result &&
            (sequence == 0 || hl_lineage_registry_create(registry, zero, &replacement) != 0 ||
             replacement.slot != sentinel.slot ||
             atomic_load_explicit(&registry->occupied, memory_order_relaxed) != 1 ||
             atomic_load_explicit(&registry->tombstones, memory_order_relaxed) != 0))
            result = 31;
    }
    if (!result && scenario == 12) {
        hl_lineage_value restored = zero;
        restored.references[HL_LINEAGE_STAGED_RESTORE_REFERENCE] = 1;
        hl_lineage_identity identity = {registry->domain, 1000};
        hl_lineage_token imported, duplicate, future;
        if (hl_lineage_registry_insert(registry, identity, restored, &imported) != 0 ||
            hl_lineage_registry_insert(registry, identity, restored, &duplicate) != EEXIST ||
            atomic_load_explicit(&registry->next_identity, memory_order_relaxed) != 1001 ||
            hl_lineage_registry_find(registry, identity, &duplicate, &observed) != 0 ||
            memcmp(&observed, &restored, sizeof observed) != 0)
            result = 32;
        uint64_t reclaimed = UINT64_MAX;
        atomic_store_explicit(&registry->reclaim_cursor, imported.slot, memory_order_relaxed);
        if (!result &&
            (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 0 ||
             hl_lineage_registry_reference(registry, imported, HL_LINEAGE_STAGED_RESTORE_REFERENCE, -1) != 0))
            result = 33;
        atomic_store_explicit(&registry->reclaim_cursor, imported.slot, memory_order_relaxed);
        if (!result &&
            (hl_lineage_registry_reclaim(registry, 1, &reclaimed) != 0 || reclaimed != 1 ||
             hl_lineage_registry_create(registry, zero, &future) != 0 || future.identity.sequence != 1001))
            result = 34;
    }
    if (!result && scenario == 13) {
        hl_lineage_token imported, overflow;
        if (hl_lineage_registry_insert(registry, (hl_lineage_identity){registry->domain, UINT64_MAX}, zero,
                                       &imported) != 0 ||
            hl_lineage_registry_create(registry, zero, &overflow) != EOVERFLOW)
            result = 35;
    }
    if (!result && scenario == 14) {
        stale = sentinel;
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 36;
        uint64_t reclaimed = 0;
        if (!result &&
            (hl_lineage_registry_reclaim(registry, capacity, &reclaimed) != 0 || reclaimed != 1))
            result = 37;
        uint64_t claiming_slot = (stale.slot + 1u) & (capacity - 1u);
        atomic_store_explicit(&registry->slots[claiming_slot].state,
                              lineage_state(1, 0, HL_LINEAGE_STATE_CLAIMING), memory_order_release);
        uint64_t sequence = lineage_fixture_sequence(
            registry, stale.slot, atomic_load_explicit(&registry->next_identity, memory_order_relaxed));
        atomic_store_explicit(&registry->next_identity, sequence, memory_order_relaxed);
        hl_lineage_token denied, replacement;
        if (!result && (sequence == 0 || hl_lineage_registry_create(registry, zero, &denied) != EAGAIN)) result = 38;
        atomic_store_explicit(&registry->slots[claiming_slot].state, HL_LINEAGE_STATE_EMPTY, memory_order_release);
        sequence = lineage_fixture_sequence(
            registry, stale.slot, atomic_load_explicit(&registry->next_identity, memory_order_relaxed));
        atomic_store_explicit(&registry->next_identity, sequence, memory_order_relaxed);
        if (!result &&
            (sequence == 0 || hl_lineage_registry_create(registry, zero, &replacement) != 0 ||
             replacement.slot != stale.slot))
            result = 39;
    }
    if (!result && scenario == 15) {
        hl_lineage_value expected = live;
        hl_lineage_value replacement = live;
        replacement.payload[0] = 97;
        hl_lineage_token enumerated;
        if (hl_lineage_registry_replace(registry, sentinel, &expected, replacement) != 0 ||
            hl_lineage_registry_replace(registry, sentinel, &expected, live) != EAGAIN ||
            hl_lineage_registry_token_at(registry, sentinel.slot, &enumerated, &observed) != 0 ||
            enumerated.identity.sequence != sentinel.identity.sequence || enumerated.generation != sentinel.generation ||
            memcmp(&observed, &replacement, sizeof observed) != 0)
            result = 40;
    }
    if (!result && scenario == 16) {
        static const unsigned crash_points[] = {25, 23, 23, 6};
        for (unsigned operation = 0; !result && operation < 4; ++operation)
            for (uint64_t crash = 1; !result && crash <= crash_points[operation]; ++crash)
                if (lineage_crash_fixture(operation, crash) != 0) result = 41;
    }
    if (!result && scenario == 17) {
        uint64_t initial = atomic_load_explicit(&registry->slots[sentinel.slot].state, memory_order_acquire);
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_INFLIGHT_REFERENCE, 1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_INFLIGHT_REFERENCE, -1) != 0 ||
            atomic_load_explicit(&registry->slots[sentinel.slot].state, memory_order_acquire) == initial)
            result = 42;
    }
    if (!result && hl_lineage_registry_validate(registry) != 0) result = 19;
    free(registry);
    return result;
}
#endif
