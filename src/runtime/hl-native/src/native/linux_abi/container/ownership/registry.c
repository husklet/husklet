#include "registry.h"

#include <assert.h>
#include <errno.h>
#include <limits.h>
#include <string.h>

#define HL_OWNER_REGISTRY_ABI UINT64_C(0x484c4f574e455201)
#define HL_OWNER_STATE_KIND_MASK UINT64_C(3)
#define HL_OWNER_STATE_EMPTY UINT64_C(0)
#define HL_OWNER_STATE_CLAIMING UINT64_C(1)
#define HL_OWNER_STATE_LIVE UINT64_C(2)
#define HL_OWNER_STATE_TOMBSTONE UINT64_C(3)
#define HL_OWNER_STATE_SEQUENCE_MAX (UINT64_MAX >> 2)

static uint64_t hl_owner_state(uint64_t sequence, uint64_t kind) {
    return (sequence << 2) | kind;
}

static uint64_t hl_owner_state_kind(uint64_t state) {
    return state & HL_OWNER_STATE_KIND_MASK;
}

static uint64_t hl_owner_state_sequence(uint64_t state) {
    return state >> 2;
}

static uint64_t hl_owner_hash(hl_owner_key key) {
    uint64_t value = key.device ^ (key.object + UINT64_C(0x9e3779b97f4a7c15) + (key.device << 6) + (key.device >> 2));
    value ^= key.birth_ns + UINT64_C(0x9e3779b97f4a7c15) + (value << 6) + (value >> 2);
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value *= UINT64_C(0x94d049bb133111eb);
    return value ^ (value >> 31);
}

static int hl_owner_key_equal(const hl_owner_registry_slot *slot, hl_owner_key key) {
    return atomic_load_explicit(&slot->device, memory_order_relaxed) == key.device &&
           atomic_load_explicit(&slot->object, memory_order_relaxed) == key.object &&
           atomic_load_explicit(&slot->birth_ns, memory_order_relaxed) == key.birth_ns;
}

static int hl_owner_key_valid(hl_owner_key key) {
    return key.birth_ns != 0;
}

static int hl_owner_writer_valid(hl_owner_namespace namespace, hl_owner_writer writer) {
    int valid = namespace.generation != NULL && namespace.owner != NULL && writer.identity != 0 &&
                writer.generation != HL_OWNER_NAMESPACE_POISON && (writer.generation & 1u) != 0 &&
                atomic_load_explicit(namespace.generation, memory_order_acquire) == writer.generation &&
                atomic_load_explicit(namespace.owner, memory_order_acquire) == writer.identity;
#if defined(HL_OWNER_REGISTRY_ASSERT_WRITER)
    assert(valid && "owner registry mutation requires the namespace transaction writer");
#endif
    return valid;
}

static int hl_owner_initialized(const hl_owner_registry *registry) {
    return registry != NULL && registry->abi == HL_OWNER_REGISTRY_ABI && registry->capacity >= 2 &&
           (registry->capacity & (registry->capacity - 1u)) == 0 && registry->epoch != 0;
}

size_t hl_owner_registry_size(uint64_t capacity) {
    if (capacity < 2 || (capacity & (capacity - 1u)) != 0 ||
        capacity > (SIZE_MAX - sizeof(hl_owner_registry)) / sizeof(hl_owner_registry_slot))
        return 0;
    return sizeof(hl_owner_registry) + (size_t)capacity * sizeof(hl_owner_registry_slot);
}

static int hl_owner_registry_initialize(hl_owner_registry *registry, size_t size, uint64_t capacity, uint64_t epoch,
                                        int storage_is_zeroed) {
    size_t required = hl_owner_registry_size(capacity);
    if (registry == NULL || required == 0 || size < required || epoch == 0) return EINVAL;
    /* Lifecycle is init-only: replacing a published shared table would invalidate forked readers. */
    if (registry->abi == HL_OWNER_REGISTRY_ABI) return EALREADY;
    if (!storage_is_zeroed) memset(registry, 0, required);
    registry->size = required;
    registry->capacity = capacity;
    registry->epoch = epoch;
    atomic_store_explicit(&registry->next_sequence, 1, memory_order_relaxed);
    registry->abi = HL_OWNER_REGISTRY_ABI;
    return 0;
}

int hl_owner_registry_init(hl_owner_registry *registry, size_t size, uint64_t capacity, uint64_t epoch) {
    return hl_owner_registry_initialize(registry, size, capacity, epoch, 0);
}

int hl_owner_registry_init_zeroed(hl_owner_registry *registry, size_t size, uint64_t capacity, uint64_t epoch) {
    return hl_owner_registry_initialize(registry, size, capacity, epoch, 1);
}

static int hl_owner_find(const hl_owner_registry *registry, hl_owner_key key, uint64_t *index_out, uint64_t *state_out,
                         uint64_t *reusable_out) {
    uint64_t mask = registry->capacity - 1u;
    uint64_t start = hl_owner_hash(key) & mask;
    uint64_t reusable = UINT64_MAX;
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t index = (start + probe) & mask;
        const hl_owner_registry_slot *slot = &registry->slots[index];
        uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
        uint64_t kind = hl_owner_state_kind(state);
        if (kind == HL_OWNER_STATE_EMPTY) {
            if (reusable == UINT64_MAX) reusable = index;
            break;
        }
        if (kind == HL_OWNER_STATE_TOMBSTONE) {
            if (reusable == UINT64_MAX) reusable = index;
            continue;
        }
        if (kind == HL_OWNER_STATE_LIVE && hl_owner_key_equal(slot, key)) {
            *index_out = index;
            *state_out = state;
            if (reusable_out != NULL) *reusable_out = reusable;
            return 1;
        }
    }
    if (reusable_out != NULL) *reusable_out = reusable;
    return 0;
}

static int hl_owner_new_sequences(hl_owner_registry *registry, uint64_t count, uint64_t *sequence) {
    uint64_t current = atomic_load_explicit(&registry->next_sequence, memory_order_relaxed);
    if (count == 0 || current == 0 || count > HL_OWNER_STATE_SEQUENCE_MAX ||
        current > HL_OWNER_STATE_SEQUENCE_MAX - count + 1u)
        return EOVERFLOW;
    atomic_store_explicit(&registry->next_sequence, current + count, memory_order_relaxed);
    *sequence = current;
    return 0;
}

int hl_owner_registry_reserve(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                              hl_owner_ticket *ticket) {
    uint64_t reusable = UINT64_MAX, sequence;
    if (!hl_owner_initialized(registry) || ticket == NULL) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    if (atomic_load_explicit(&registry->occupied, memory_order_relaxed) >= registry->capacity / 2u) return ENOSPC;
    uint64_t start = atomic_load_explicit(&registry->reserve_cursor, memory_order_relaxed);
    for (uint64_t probe = 0; probe < registry->capacity; ++probe) {
        uint64_t index = (start + probe) & (registry->capacity - 1u);
        uint64_t kind = hl_owner_state_kind(atomic_load_explicit(&registry->slots[index].state, memory_order_acquire));
        if (kind == HL_OWNER_STATE_EMPTY || kind == HL_OWNER_STATE_TOMBSTONE) {
            reusable = index;
            atomic_store_explicit(&registry->reserve_cursor, index + 1u, memory_order_relaxed);
            break;
        }
    }
    if (reusable == UINT64_MAX) return ENOSPC;
    int error = hl_owner_new_sequences(registry, 2, &sequence);
    if (error != 0) return error;
    hl_owner_registry_slot *slot = &registry->slots[reusable];
    /* Clear payload before publishing CLAIMING; a stable reader never sees stale reservation bytes. */
    atomic_store_explicit(&slot->device, 0, memory_order_relaxed);
    atomic_store_explicit(&slot->object, 0, memory_order_relaxed);
    atomic_store_explicit(&slot->birth_ns, 0, memory_order_relaxed);
    atomic_store_explicit(&slot->owner, 0, memory_order_relaxed);
    atomic_store_explicit(&slot->references, 0, memory_order_relaxed);
    atomic_store_explicit(&slot->state, hl_owner_state(sequence, HL_OWNER_STATE_CLAIMING), memory_order_release);
    atomic_fetch_add_explicit(&registry->occupied, 1, memory_order_relaxed);
    *ticket = (hl_owner_ticket){registry->epoch, sequence, sequence + 1u, reusable};
    return 0;
}

static hl_owner_registry_slot *hl_owner_ticket_slot(hl_owner_registry *registry, hl_owner_ticket ticket,
                                                    uint64_t kind) {
    if (ticket.epoch != registry->epoch || ticket.sequence == 0 || ticket.sequence >= HL_OWNER_STATE_SEQUENCE_MAX ||
        ticket.publication_sequence == 0 || ticket.publication_sequence > HL_OWNER_STATE_SEQUENCE_MAX ||
        ticket.publication_sequence != ticket.sequence + 1u || ticket.slot >= registry->capacity)
        return NULL;
    hl_owner_registry_slot *slot = &registry->slots[ticket.slot];
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_acquire);
    return state == hl_owner_state(ticket.sequence, kind) ? slot : NULL;
}

int hl_owner_registry_commit(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_ticket ticket, hl_owner_key key, hl_owner_value value) {
    uint64_t existing_index = 0, existing_state = 0, reusable = UINT64_MAX;
    if (!hl_owner_initialized(registry)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    hl_owner_registry_slot *reservation = hl_owner_ticket_slot(registry, ticket, HL_OWNER_STATE_CLAIMING);
    if (reservation == NULL) return ESTALE;
    if (!hl_owner_key_valid(key)) {
        atomic_store_explicit(&reservation->state, hl_owner_state(ticket.sequence, HL_OWNER_STATE_TOMBSTONE),
                              memory_order_release);
        atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
        return EINVAL;
    }
    if (hl_owner_find(registry, key, &existing_index, &existing_state, &reusable)) {
        atomic_store_explicit(&reservation->state, hl_owner_state(ticket.sequence, HL_OWNER_STATE_TOMBSTONE),
                              memory_order_release);
        atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
        return EEXIST;
    }
    if (reusable == UINT64_MAX) return ENOSPC;
    hl_owner_registry_slot *slot = &registry->slots[reusable];
    atomic_store_explicit(&slot->device, key.device, memory_order_relaxed);
    atomic_store_explicit(&slot->object, key.object, memory_order_relaxed);
    atomic_store_explicit(&slot->birth_ns, key.birth_ns, memory_order_relaxed);
    atomic_store_explicit(&slot->owner, ((uint64_t)value.uid << 32) | value.gid, memory_order_relaxed);
    atomic_store_explicit(&slot->references, ((uint64_t)value.links << 32) | value.descriptors, memory_order_relaxed);
    atomic_store_explicit(&slot->state, hl_owner_state(ticket.publication_sequence, HL_OWNER_STATE_LIVE),
                          memory_order_release);
    atomic_store_explicit(&reservation->state, hl_owner_state(ticket.sequence, HL_OWNER_STATE_TOMBSTONE),
                          memory_order_release);
    return 0;
}

int hl_owner_registry_cancel(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_ticket ticket) {
    if (!hl_owner_initialized(registry)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    hl_owner_registry_slot *slot = hl_owner_ticket_slot(registry, ticket, HL_OWNER_STATE_CLAIMING);
    if (slot == NULL) return ESTALE;
    atomic_store_explicit(&slot->state, hl_owner_state(ticket.sequence, HL_OWNER_STATE_TOMBSTONE),
                          memory_order_release);
    atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
    return 0;
}

int hl_owner_registry_lookup(const hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_key key,
                             hl_owner_value *value) {
    if (!hl_owner_initialized(registry) || namespace.generation == NULL || value == NULL || !hl_owner_key_valid(key))
        return -EINVAL;
    for (unsigned retry = 0; retry < 32; ++retry) {
        uint64_t before = atomic_load_explicit(namespace.generation, memory_order_acquire);
        if (before == HL_OWNER_NAMESPACE_POISON) return -EOWNERDEAD;
        if (before & 1u) return -EAGAIN;
        uint64_t index = 0, state = 0;
        int found = hl_owner_find(registry, key, &index, &state, NULL);
        hl_owner_value snapshot = {0};
        if (found && hl_owner_state_kind(state) == HL_OWNER_STATE_LIVE) {
            const hl_owner_registry_slot *slot = &registry->slots[index];
            uint64_t owner = atomic_load_explicit(&slot->owner, memory_order_relaxed);
            uint64_t references = atomic_load_explicit(&slot->references, memory_order_relaxed);
            uint64_t after_state = atomic_load_explicit(&slot->state, memory_order_acquire);
            if (after_state != state) continue;
            snapshot.uid = (uint32_t)(owner >> 32);
            snapshot.gid = (uint32_t)owner;
            snapshot.links = (uint32_t)(references >> 32);
            snapshot.descriptors = (uint32_t)references;
        }
        uint64_t after = atomic_load_explicit(namespace.generation, memory_order_acquire);
        if (before != after || (after & 1u)) continue;
        if (!found || hl_owner_state_kind(state) != HL_OWNER_STATE_LIVE) return HL_OWNER_ABSENT;
        *value = snapshot;
        return HL_OWNER_FOUND;
    }
    return -EAGAIN;
}

int hl_owner_registry_writer_lookup(const hl_owner_registry *registry, hl_owner_namespace namespace,
                                    hl_owner_writer writer, hl_owner_key key, hl_owner_value *value) {
    uint64_t index = 0, state = 0;
    if (!hl_owner_initialized(registry) || value == NULL || !hl_owner_key_valid(key)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    if (!hl_owner_find(registry, key, &index, &state, NULL) ||
        hl_owner_state_kind(state) != HL_OWNER_STATE_LIVE)
        return ENOENT;
    const hl_owner_registry_slot *slot = &registry->slots[index];
    uint64_t owner = atomic_load_explicit(&slot->owner, memory_order_relaxed);
    uint64_t references = atomic_load_explicit(&slot->references, memory_order_relaxed);
    value->uid = (uint32_t)(owner >> 32);
    value->gid = (uint32_t)owner;
    value->links = (uint32_t)(references >> 32);
    value->descriptors = (uint32_t)references;
    return 0;
}

static int hl_owner_live_slot(hl_owner_registry *registry, hl_owner_key key, hl_owner_registry_slot **output) {
    uint64_t index = 0, state = 0;
    if (!hl_owner_key_valid(key)) return EINVAL;
    if (!hl_owner_find(registry, key, &index, &state, NULL) || hl_owner_state_kind(state) != HL_OWNER_STATE_LIVE)
        return ENOENT;
    *output = &registry->slots[index];
    return 0;
}

static int hl_owner_begin_mutation(hl_owner_registry *registry, hl_owner_registry_slot *slot, uint64_t *sequence) {
    int error = hl_owner_new_sequences(registry, 1, sequence);
    if (error != 0) return error;
    atomic_store_explicit(&slot->state, hl_owner_state(*sequence, HL_OWNER_STATE_CLAIMING), memory_order_release);
    return 0;
}

int hl_owner_registry_update(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_key key, uint32_t uid, uint32_t gid) {
    hl_owner_registry_slot *slot;
    if (!hl_owner_initialized(registry)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    int error = hl_owner_live_slot(registry, key, &slot);
    if (error != 0) return error;
    uint64_t sequence;
    error = hl_owner_begin_mutation(registry, slot, &sequence);
    if (error != 0) return error;
    atomic_store_explicit(&slot->owner, ((uint64_t)uid << 32) | gid, memory_order_release);
    atomic_store_explicit(&slot->state, hl_owner_state(sequence, HL_OWNER_STATE_LIVE), memory_order_release);
    return 0;
}

static int hl_owner_reference(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                              hl_owner_key key, int64_t delta, int descriptor) {
    hl_owner_registry_slot *slot;
    if (!hl_owner_initialized(registry)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    int error = hl_owner_live_slot(registry, key, &slot);
    if (error != 0) return error;
    uint64_t refs = atomic_load_explicit(&slot->references, memory_order_relaxed);
    uint32_t links = (uint32_t)(refs >> 32), descriptors = (uint32_t)refs;
    uint32_t current = descriptor ? descriptors : links;
    if ((delta < 0 && delta < -(int64_t)current) || (delta > 0 && (uint64_t)delta > UINT32_MAX - current))
        return ERANGE;
    current = (uint32_t)((int64_t)current + delta);
    if (descriptor)
        descriptors = current;
    else
        links = current;
    uint64_t sequence;
    error = hl_owner_begin_mutation(registry, slot, &sequence);
    if (error != 0) return error;
    atomic_store_explicit(&slot->references, ((uint64_t)links << 32) | descriptors, memory_order_release);
    uint64_t kind = links == 0 && descriptors == 0 ? HL_OWNER_STATE_TOMBSTONE : HL_OWNER_STATE_LIVE;
    atomic_store_explicit(&slot->state, hl_owner_state(sequence, kind), memory_order_release);
    if (kind == HL_OWNER_STATE_TOMBSTONE) atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
    return 0;
}

int hl_owner_registry_link(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                           hl_owner_key key, int64_t delta) {
    return hl_owner_reference(registry, namespace, writer, key, delta, 0);
}

int hl_owner_registry_descriptor(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                                 hl_owner_key key, int64_t delta) {
    return hl_owner_reference(registry, namespace, writer, key, delta, 1);
}

int hl_owner_registry_retire(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_key key) {
    hl_owner_registry_slot *slot;
    if (!hl_owner_initialized(registry)) return EINVAL;
    if (!hl_owner_writer_valid(namespace, writer)) return EPERM;
    int error = hl_owner_live_slot(registry, key, &slot);
    if (error != 0) return error;
    if (atomic_load_explicit(&slot->references, memory_order_acquire) != 0) return EBUSY;
    uint64_t state = atomic_load_explicit(&slot->state, memory_order_relaxed);
    atomic_store_explicit(&slot->state, hl_owner_state(hl_owner_state_sequence(state), HL_OWNER_STATE_TOMBSTONE),
                          memory_order_release);
    atomic_fetch_sub_explicit(&registry->occupied, 1, memory_order_relaxed);
    return 0;
}
