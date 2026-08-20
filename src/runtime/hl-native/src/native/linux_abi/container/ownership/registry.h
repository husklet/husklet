#ifndef HL_LINUX_OWNERSHIP_REGISTRY_H
#define HL_LINUX_OWNERSHIP_REGISTRY_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

/* 1,048,576 sparse slots consume 48 MiB; admission stops at 50% to retain bounded probes. */
#define HL_OWNER_REGISTRY_DEFAULT_CAPACITY (UINT64_C(1) << 20)
/* Fatal execution-domain poison: no registry operation may recover or reuse an abandoned claim. */
#define HL_OWNER_NAMESPACE_POISON UINT64_MAX

typedef struct hl_owner_key {
    uint64_t device;
    uint64_t object;
    /* Nonzero stable filesystem birth time or caller-assigned incarnation. */
    uint64_t birth_ns;
} hl_owner_key;

typedef struct hl_owner_value {
    uint32_t uid;
    uint32_t gid;
    uint32_t links;
    uint32_t descriptors;
} hl_owner_value;

typedef struct hl_owner_ticket {
    uint64_t epoch;
    uint64_t sequence;
    uint64_t publication_sequence;
    uint64_t slot;
} hl_owner_ticket;

typedef struct hl_owner_writer {
    uint64_t generation;
    uint64_t identity;
} hl_owner_writer;

/* Process-local binding to the shared namespace transaction authority. */
typedef struct hl_owner_namespace {
    _Atomic uint64_t *generation;
    _Atomic uint64_t *owner;
} hl_owner_namespace;

typedef struct hl_owner_registry_slot {
    _Atomic uint64_t state;
    _Atomic uint64_t device;
    _Atomic uint64_t object;
    _Atomic uint64_t birth_ns;
    _Atomic uint64_t owner;
    _Atomic uint64_t references;
} hl_owner_registry_slot;

typedef struct hl_owner_registry {
    uint64_t abi;
    uint64_t size;
    uint64_t capacity;
    uint64_t epoch;
    _Atomic uint64_t next_sequence;
    _Atomic uint64_t occupied;
    _Atomic uint64_t reserve_cursor;
    hl_owner_registry_slot slots[];
} hl_owner_registry;

enum {
    HL_OWNER_ABSENT = 0,
    HL_OWNER_FOUND = 1,
};

size_t hl_owner_registry_size(uint64_t capacity);
int hl_owner_registry_init(hl_owner_registry *registry, size_t size, uint64_t capacity, uint64_t epoch);
/* Initialize storage supplied by a fresh zero-filled shared mapping without
 * eagerly dirtying every sparse slot page. */
int hl_owner_registry_init_zeroed(hl_owner_registry *registry, size_t size, uint64_t capacity, uint64_t epoch);
/* Reserve quota before an operation creates an object whose stable key does not exist yet. */
int hl_owner_registry_reserve(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                              hl_owner_ticket *ticket);
/* Commit consumes the reservation on success, EEXIST, or an invalid key; other errors leave it cancellable. */
int hl_owner_registry_commit(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_ticket ticket, hl_owner_key key, hl_owner_value value);
int hl_owner_registry_cancel(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_ticket ticket);
int hl_owner_registry_lookup(const hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_key key,
                             hl_owner_value *value);
/* Read a live value while holding the exact namespace writer token. */
int hl_owner_registry_writer_lookup(const hl_owner_registry *registry, hl_owner_namespace namespace,
                                    hl_owner_writer writer, hl_owner_key key, hl_owner_value *value);
int hl_owner_registry_update(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_key key, uint32_t uid, uint32_t gid);
int hl_owner_registry_link(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                           hl_owner_key key, int64_t delta);
int hl_owner_registry_descriptor(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                                 hl_owner_key key, int64_t delta);
int hl_owner_registry_retire(hl_owner_registry *registry, hl_owner_namespace namespace, hl_owner_writer writer,
                             hl_owner_key key);

#endif
