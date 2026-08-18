#include "lineage_registry.h"

#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>

#define HL_LINEAGE_ABI UINT64_C(0x484c4c494e454101)
#define HL_LINEAGE_KIND_MASK UINT64_C(3)
#define HL_LINEAGE_GENERATION_MAX (UINT64_MAX >> 2)

static uint64_t lineage_state(uint64_t generation, uint64_t kind) {
    return (generation << 2) | kind;
}

static uint64_t lineage_kind(uint64_t state) {
    return state & HL_LINEAGE_KIND_MASK;
}

static uint64_t lineage_generation(uint64_t state) {
    return state >> 2;
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
                             int zero_storage) {
    size_t required = hl_lineage_registry_size(capacity);
    if (registry == NULL || required == 0 || size < required || domain == 0) return EINVAL;
    if (registry->abi == HL_LINEAGE_ABI) return EALREADY;
    if (!zero_storage) memset(registry, 0, required);
    registry->size = required;
    registry->capacity = capacity;
    registry->domain = domain;
    atomic_store_explicit(&registry->next_identity, 1, memory_order_relaxed);
    atomic_store_explicit(&registry->next_generation, 1, memory_order_relaxed);
    registry->abi = HL_LINEAGE_ABI;
    return 0;
}

static int lineage_next(_Atomic uint64_t *counter, uint64_t maximum, uint64_t *value) {
    uint64_t current = atomic_load_explicit(counter, memory_order_relaxed);
    if (current == 0 || current > maximum) return EOVERFLOW;
    atomic_store_explicit(counter, current + 1u, memory_order_relaxed);
    *value = current;
    return 0;
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
        if (atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) == identity.domain &&
            atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) == identity.sequence) {
            if (atomic_load_explicit(&slot->state, memory_order_acquire) != state) return EAGAIN;
            *found = index;
            return 0;
        }
    }
    return ENOENT;
}

static int lineage_reusable(const hl_lineage_registry *registry, hl_lineage_identity identity, uint64_t *index,
                            uint64_t *state) {
    uint64_t start = lineage_hash(identity) & (registry->capacity - 1u);
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t candidate = (start + probe) & (registry->capacity - 1u);
        uint64_t observed = atomic_load_explicit(&registry->slots[candidate].state, memory_order_acquire);
        uint64_t kind = lineage_kind(observed);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
        /* create() owns a never-before-issued identity, so it need not traverse
         * a tombstone chain to disprove a duplicate. */
        if (kind == HL_LINEAGE_STATE_EMPTY || kind == HL_LINEAGE_STATE_TOMBSTONE) {
            *index = candidate;
            *state = observed;
            return 0;
        }
    }
    return ENOSPC;
}

int hl_lineage_registry_create(hl_lineage_registry *registry, hl_lineage_value value, hl_lineage_token *token) {
    uint64_t identity_sequence, generation, reusable, prior;
    if (!lineage_initialized(registry) || token == NULL) return EINVAL;
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) >= registry->capacity / 2u) return ENOSPC;
    int error = lineage_next(&registry->next_identity, UINT64_MAX - 1u, &identity_sequence);
    if (error != 0) return error;
    hl_lineage_identity identity = {registry->domain, identity_sequence};
    error = lineage_reusable(registry, identity, &reusable, &prior);
    if (error != 0) return error;
    error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[reusable];
    if (!atomic_compare_exchange_strong_explicit(&slot->state, &prior,
                                                 lineage_state(generation, HL_LINEAGE_STATE_CLAIMING),
                                                 memory_order_acq_rel, memory_order_acquire))
        return EAGAIN;
    atomic_store_explicit(&slot->identity_domain, identity.domain, memory_order_relaxed);
    atomic_store_explicit(&slot->identity_sequence, identity.sequence, memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_REFERENCE_COUNT; ++index)
        atomic_store_explicit(&slot->references[index], value.references[index], memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_PAYLOAD_WORDS; ++index)
        atomic_store_explicit(&slot->payload[index], value.payload[index], memory_order_relaxed);
    atomic_store_explicit(&slot->state, lineage_state(generation, HL_LINEAGE_STATE_ACTIVE), memory_order_release);
    atomic_fetch_add_explicit(&registry->occupied, 1, memory_order_relaxed);
    if (lineage_kind(prior) == HL_LINEAGE_STATE_TOMBSTONE)
        atomic_fetch_sub_explicit(&registry->tombstones, 1, memory_order_relaxed);
    *token = (hl_lineage_token){identity, reusable, generation};
    return 0;
}

static int lineage_token_slot(const hl_lineage_registry *registry, hl_lineage_token token,
                              const hl_lineage_slot **output) {
    if (!lineage_initialized(registry) || token.slot >= registry->capacity || token.generation == 0 ||
        token.identity.domain == 0 || token.identity.sequence == 0)
        return EINVAL;
    const hl_lineage_slot *slot = &registry->slots[token.slot];
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
    if (lineage_kind(state) == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE ||
#ifndef HL_LINEAGE_MUTATE_SKIP_GENERATION_RECHECK
        lineage_generation(state) != token.generation ||
#endif
#ifndef HL_LINEAGE_MUTATE_SKIP_IDENTITY_RECHECK
        atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) != token.identity.domain ||
        atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) != token.identity.sequence ||
#endif
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return ESTALE;
    *output = slot;
    return 0;
}

static int lineage_read_value(const hl_lineage_slot *slot, uint64_t state, hl_lineage_value *value) {
    for (unsigned index = 0; index < HL_LINEAGE_REFERENCE_COUNT; ++index)
        value->references[index] = atomic_load_explicit(&slot->references[index], memory_order_relaxed);
    for (unsigned index = 0; index < HL_LINEAGE_PAYLOAD_WORDS; ++index)
        value->payload[index] = atomic_load_explicit(&slot->payload[index], memory_order_relaxed);
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
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE) return EAGAIN;
    error = lineage_read_value(slot, state, value);
    if (error != 0 || atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) != identity.domain ||
        atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) != identity.sequence ||
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return EAGAIN;
    *token = (hl_lineage_token){identity, found, lineage_generation(state)};
    return 0;
}

int hl_lineage_registry_lookup(const hl_lineage_registry *registry, hl_lineage_token token,
                               hl_lineage_value *value) {
    const hl_lineage_slot *slot;
    if (value == NULL) return EINVAL;
    int error = lineage_token_slot(registry, token, &slot);
    if (error != 0) return error;
#ifndef HL_LINEAGE_MUTATE_SKIP_GENERATION_RECHECK
    uint64_t state = lineage_state(token.generation, HL_LINEAGE_STATE_ACTIVE);
#else
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
#endif
    return lineage_read_value(slot, state, value);
}

int hl_lineage_registry_reference(hl_lineage_registry *registry, hl_lineage_token token,
                                  hl_lineage_reference reference,
                                  int64_t delta) {
    const hl_lineage_slot *view;
    if ((unsigned)reference >= HL_LINEAGE_REFERENCE_COUNT) return EINVAL;
    int error = lineage_token_slot(registry, token, &view);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[token.slot];
    uint64_t active = lineage_state(token.generation, HL_LINEAGE_STATE_ACTIVE);
    uint64_t claiming = lineage_state(token.generation, HL_LINEAGE_STATE_CLAIMING);
    if (!atomic_compare_exchange_strong_explicit(&slot->state, &active, claiming, memory_order_acq_rel,
                                                 memory_order_acquire))
        return lineage_generation(active) == token.generation ? EAGAIN : ESTALE;
    uint64_t current = atomic_load_explicit(&slot->references[reference], memory_order_relaxed);
    uint64_t magnitude = delta < 0 ? UINT64_C(0) - (uint64_t)delta : (uint64_t)delta;
    if ((delta < 0 && magnitude > current) || (delta > 0 && magnitude > UINT64_MAX - current)) {
        atomic_store_explicit(&slot->state, lineage_state(token.generation, HL_LINEAGE_STATE_ACTIVE),
                              memory_order_release);
        return ERANGE;
    }
    uint64_t updated = delta < 0 ? current - magnitude : current + magnitude;
    atomic_store_explicit(&slot->references[reference], updated, memory_order_relaxed);
    atomic_store_explicit(&slot->state, lineage_state(token.generation, HL_LINEAGE_STATE_ACTIVE),
                          memory_order_release);
    return 0;
}

int hl_lineage_registry_reclaim(hl_lineage_registry *registry, uint64_t budget, uint64_t *reclaimed) {
    if (!lineage_initialized(registry) || reclaimed == NULL || budget > registry->capacity) return EINVAL;
    uint64_t count = 0;
    uint64_t cursor = atomic_load_explicit(&registry->reclaim_cursor, memory_order_relaxed);
    for (uint64_t step = 0; step < budget; ++step) {
        uint64_t index = (cursor + step) & (registry->capacity - 1u);
        hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE) continue;
        int retained = 0;
        for (unsigned reference = 0; reference < HL_LINEAGE_REFERENCE_COUNT; ++reference)
            retained |= atomic_load_explicit(&slot->references[reference], memory_order_relaxed) != 0;
        if (retained) continue;
        uint64_t generation;
        int error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
        if (error != 0) return error;
        uint64_t claiming = lineage_state(generation, HL_LINEAGE_STATE_CLAIMING);
        if (!atomic_compare_exchange_strong_explicit(&slot->state, &state, claiming, memory_order_acq_rel,
                                                     memory_order_acquire))
            continue;
        retained = 0;
        for (unsigned reference = 0; reference < HL_LINEAGE_REFERENCE_COUNT; ++reference)
            retained |= atomic_load_explicit(&slot->references[reference], memory_order_relaxed) != 0;
        if (retained) {
            atomic_store_explicit(&slot->state, state, memory_order_release);
            continue;
        }
        atomic_store_explicit(&slot->identity_domain, 0, memory_order_relaxed);
        atomic_store_explicit(&slot->identity_sequence, 0, memory_order_relaxed);
        for (unsigned reference = 0; reference < HL_LINEAGE_REFERENCE_COUNT; ++reference)
            atomic_store_explicit(&slot->references[reference], 0, memory_order_relaxed);
        for (unsigned payload = 0; payload < HL_LINEAGE_PAYLOAD_WORDS; ++payload)
            atomic_store_explicit(&slot->payload[payload], 0, memory_order_relaxed);
        atomic_store_explicit(&slot->state, lineage_state(generation, HL_LINEAGE_STATE_TOMBSTONE), memory_order_release);
        atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&registry->tombstones, 1, memory_order_relaxed);
        ++count;
    }
    atomic_store_explicit(&registry->reclaim_cursor, cursor + budget, memory_order_relaxed);
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
            if (lineage_generation(state) == 0 ||
                atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) == 0 ||
                atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) == 0)
                return EINVAL;
            ++occupied;
        } else if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
            if (lineage_generation(state) == 0 ||
                atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) != 0 ||
                atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) != 0)
                return EINVAL;
            for (unsigned reference = 0; reference < HL_LINEAGE_REFERENCE_COUNT; ++reference)
                if (atomic_load_explicit(&slot->references[reference], memory_order_relaxed) != 0) return EINVAL;
            for (unsigned payload = 0; payload < HL_LINEAGE_PAYLOAD_WORDS; ++payload)
                if (atomic_load_explicit(&slot->payload[payload], memory_order_relaxed) != 0) return EINVAL;
            ++tombstones;
        } else if (kind != HL_LINEAGE_STATE_EMPTY) {
            return EINVAL;
        }
        if (atomic_load_explicit(&slot->state, memory_order_acquire) != state) return EAGAIN;
    }
    return occupied == atomic_load_explicit(&registry->occupied, memory_order_relaxed) &&
                   tombstones == atomic_load_explicit(&registry->tombstones, memory_order_relaxed) &&
                   occupied <= registry->capacity / 2u
               ? 0
               : EINVAL;
}

#ifdef HL_LINEAGE_TEST_HOOKS
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
        (hl_lineage_registry_lookup(registry, sentinel, &observed) != 0 || memcmp(&observed, &live, sizeof live)))
        result = 1;
    if (!result && scenario == 1) {
        stale = sentinel;
        if (hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_DESCRIPTOR_REFERENCE, -1) != 0 ||
            hl_lineage_registry_reference(registry, sentinel, HL_LINEAGE_PROCESS_LEDGER_REFERENCE, -1) != 0)
            result = 2;
        uint64_t reclaimed = 0;
        if (!result && (hl_lineage_registry_reclaim(registry, capacity, &reclaimed) != 0 || reclaimed != 1)) result = 3;
        hl_lineage_token replacement;
        if (!result && hl_lineage_registry_create(registry, zero, &replacement) != 0) result = 4;
        if (!result && (replacement.generation == stale.generation ||
                        hl_lineage_registry_lookup(registry, stale, &observed) != ESTALE))
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
    if (!result && hl_lineage_registry_validate(registry) != 0) result = 19;
    free(registry);
    return result;
}
#endif
