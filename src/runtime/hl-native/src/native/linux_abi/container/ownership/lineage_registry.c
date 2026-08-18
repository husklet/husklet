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

static int lineage_find(const hl_lineage_registry *registry, hl_lineage_identity identity, uint64_t *found,
                        uint64_t *reusable) {
    uint64_t remembered = UINT64_MAX;
    uint64_t start = lineage_hash(identity) & (registry->capacity - 1u);
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t index = (start + probe) & (registry->capacity - 1u);
        const hl_lineage_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        uint64_t kind = lineage_kind(state);
        if (kind == HL_LINEAGE_STATE_CLAIMING) return EAGAIN;
        if (kind == HL_LINEAGE_STATE_TOMBSTONE) {
            if (remembered == UINT64_MAX) remembered = index;
            continue;
        }
        if (kind == HL_LINEAGE_STATE_EMPTY) {
            if (remembered == UINT64_MAX) remembered = index;
            *reusable = remembered;
            return ENOENT;
        }
        if (atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) == identity.domain &&
            atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) == identity.sequence) {
            if (atomic_load_explicit(&slot->state, memory_order_acquire) != state) return EAGAIN;
            *found = index;
            *reusable = remembered;
            return 0;
        }
    }
    *reusable = remembered;
    return ENOENT;
}

int hl_lineage_registry_create(hl_lineage_registry *registry, hl_lineage_value value, hl_lineage_token *token) {
    uint64_t identity_sequence, generation, found = 0, reusable = UINT64_MAX;
    if (!lineage_initialized(registry) || token == NULL) return EINVAL;
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) >= registry->capacity / 2u) return ENOSPC;
    int error = lineage_next(&registry->next_identity, UINT64_MAX - 1u, &identity_sequence);
    if (error != 0) return error;
    hl_lineage_identity identity = {registry->domain, identity_sequence};
    error = lineage_find(registry, identity, &found, &reusable);
    if (error != ENOENT) return error == 0 ? EEXIST : error;
    if (reusable == UINT64_MAX) return ENOSPC;
    error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[reusable];
    uint64_t prior = atomic_load_explicit(&slot->state, memory_order_relaxed);
    atomic_store_explicit(&slot->state, lineage_state(generation, HL_LINEAGE_STATE_CLAIMING), memory_order_release);
    atomic_store_explicit(&slot->identity_domain, identity.domain, memory_order_relaxed);
    atomic_store_explicit(&slot->identity_sequence, identity.sequence, memory_order_relaxed);
    for (unsigned index = 0; index < 2; ++index)
        atomic_store_explicit(&slot->references[index], value.references[index], memory_order_relaxed);
    for (unsigned index = 0; index < 3; ++index)
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
    if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE || lineage_generation(state) != token.generation ||
        atomic_load_explicit(&slot->identity_domain, memory_order_relaxed) != token.identity.domain ||
        atomic_load_explicit(&slot->identity_sequence, memory_order_relaxed) != token.identity.sequence ||
        atomic_load_explicit(&slot->state, memory_order_acquire) != state)
        return ESTALE;
    *output = slot;
    return 0;
}

int hl_lineage_registry_lookup(const hl_lineage_registry *registry, hl_lineage_token token,
                               hl_lineage_value *value) {
    const hl_lineage_slot *slot;
    if (value == NULL) return EINVAL;
    int error = lineage_token_slot(registry, token, &slot);
    if (error != 0) return error;
    for (unsigned index = 0; index < 2; ++index)
        value->references[index] = atomic_load_explicit(&slot->references[index], memory_order_relaxed);
    for (unsigned index = 0; index < 3; ++index)
        value->payload[index] = atomic_load_explicit(&slot->payload[index], memory_order_relaxed);
    return atomic_load_explicit(&slot->state, memory_order_acquire) ==
                   lineage_state(token.generation, HL_LINEAGE_STATE_ACTIVE)
               ? 0
               : ESTALE;
}

int hl_lineage_registry_reference(hl_lineage_registry *registry, hl_lineage_token token, unsigned reference,
                                  int64_t delta) {
    const hl_lineage_slot *view;
    if (reference >= 2) return EINVAL;
    int error = lineage_token_slot(registry, token, &view);
    if (error != 0) return error;
    hl_lineage_slot *slot = &registry->slots[token.slot];
    uint64_t current = atomic_load_explicit(&slot->references[reference], memory_order_relaxed);
    uint64_t magnitude = delta < 0 ? UINT64_C(0) - (uint64_t)delta : (uint64_t)delta;
    if ((delta < 0 && magnitude > current) || (delta > 0 && magnitude > UINT64_MAX - current)) return ERANGE;
    uint64_t updated = delta < 0 ? current - magnitude : current + magnitude;
    atomic_store_explicit(&slot->references[reference], updated, memory_order_release);
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
        if (lineage_kind(state) != HL_LINEAGE_STATE_ACTIVE ||
            atomic_load_explicit(&slot->references[0], memory_order_relaxed) != 0 ||
            atomic_load_explicit(&slot->references[1], memory_order_relaxed) != 0)
            continue;
        uint64_t generation;
        int error = lineage_next(&registry->next_generation, HL_LINEAGE_GENERATION_MAX, &generation);
        if (error != 0) return error;
        atomic_store_explicit(&slot->state, lineage_state(generation, HL_LINEAGE_STATE_CLAIMING), memory_order_release);
        if (atomic_load_explicit(&slot->references[0], memory_order_relaxed) != 0 ||
            atomic_load_explicit(&slot->references[1], memory_order_relaxed) != 0)
            return EBUSY;
        atomic_store_explicit(&slot->identity_domain, 0, memory_order_relaxed);
        atomic_store_explicit(&slot->identity_sequence, 0, memory_order_relaxed);
        for (unsigned payload = 0; payload < 3; ++payload)
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

int hl_lineage_registry_fixture(uint32_t scenario, uint64_t capacity, uint64_t iterations) {
    size_t size = hl_lineage_registry_size(capacity);
    hl_lineage_registry *registry = size ? calloc(1, size) : NULL;
    if (registry == NULL || hl_lineage_registry_init(registry, size, capacity, 7, 1) != 0) {
        free(registry);
        return 100;
    }
    hl_lineage_token sentinel, stale;
    hl_lineage_value live = {{1, 0}, {11, 13, 17}}, zero = {{0, 0}, {19, 23, 29}}, observed;
    int result = 0;
    if (hl_lineage_registry_create(registry, live, &sentinel) != 0) result = 101;
    if (!result && scenario == 0 &&
        (hl_lineage_registry_lookup(registry, sentinel, &observed) != 0 || memcmp(&observed, &live, sizeof live)))
        result = 1;
    if (!result && scenario == 1) {
        stale = sentinel;
        if (hl_lineage_registry_reference(registry, sentinel, 0, -1) != 0) result = 2;
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
            if (hl_lineage_registry_create(registry, zero, &token) != 0 ||
                hl_lineage_registry_reclaim(registry, capacity, &reclaimed) != 0 || reclaimed == 0 ||
                hl_lineage_registry_lookup(registry, sentinel, &observed) != 0 ||
                memcmp(&observed, &live, sizeof live)) {
                result = 8;
                break;
            }
        }
    }
    free(registry);
    return result;
}
