#ifndef HL_LINEAGE_REGISTRY_H
#define HL_LINEAGE_REGISTRY_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

#define HL_LINEAGE_DEFAULT_CAPACITY (UINT64_C(1) << 20)
#define HL_LINEAGE_STATE_EMPTY UINT64_C(0)
#define HL_LINEAGE_STATE_CLAIMING UINT64_C(1)
#define HL_LINEAGE_STATE_ACTIVE UINT64_C(2)
#define HL_LINEAGE_STATE_TOMBSTONE UINT64_C(3)

typedef struct hl_lineage_identity {
    uint64_t domain;
    uint64_t sequence;
} hl_lineage_identity;

typedef struct hl_lineage_token {
    hl_lineage_identity identity;
    uint64_t slot;
    uint64_t generation;
} hl_lineage_token;

typedef struct hl_lineage_value {
    uint64_t references[2];
    uint64_t payload[3];
} hl_lineage_value;

typedef struct hl_lineage_slot {
    _Atomic uint64_t state;
    _Atomic uint64_t identity_domain;
    _Atomic uint64_t identity_sequence;
    _Atomic uint64_t references[2];
    _Atomic uint64_t payload[3];
} hl_lineage_slot;

typedef struct hl_lineage_registry {
    uint64_t abi;
    uint64_t size;
    uint64_t capacity;
    uint64_t domain;
    _Atomic uint64_t next_identity;
    _Atomic uint64_t next_generation;
    _Atomic uint64_t occupied;
    _Atomic uint64_t tombstones;
    _Atomic uint64_t reclaim_cursor;
    hl_lineage_slot slots[];
} hl_lineage_registry;

size_t hl_lineage_registry_size(uint64_t capacity);
int hl_lineage_registry_init(hl_lineage_registry *registry, size_t size, uint64_t capacity, uint64_t domain,
                             int zero_storage);
int hl_lineage_registry_create(hl_lineage_registry *registry, hl_lineage_value value, hl_lineage_token *token);
int hl_lineage_registry_lookup(const hl_lineage_registry *registry, hl_lineage_token token,
                               hl_lineage_value *value);
int hl_lineage_registry_reference(hl_lineage_registry *registry, hl_lineage_token token, unsigned reference,
                                  int64_t delta);
int hl_lineage_registry_reclaim(hl_lineage_registry *registry, uint64_t budget, uint64_t *reclaimed);

/* Deterministic fixture entry point. Production-capacity churn callers pass an
 * explicit iteration count so the ordinary test suite can use small tables. */
int hl_lineage_registry_fixture(uint32_t scenario, uint64_t capacity, uint64_t iterations);

#endif
