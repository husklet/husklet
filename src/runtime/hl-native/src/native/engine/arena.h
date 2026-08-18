#ifndef HL_ENGINE_ARENA_H
#define HL_ENGINE_ARENA_H

#include <stdatomic.h>
#include <stdint.h>

#define HL_ARENA_MANIFEST_VERSION UINT32_C(2)
#define HL_ARENA_MANIFEST_MAGIC UINT64_C(0x484c4152454e4131)
#define HL_ARENA_MAX_RESERVATIONS UINT32_C(1024)
#define HL_ARENA_AUTHORITY_INIT {0}

typedef enum hl_arena_lifecycle {
    HL_ARENA_EMPTY = 0,
    HL_ARENA_INITIALIZING = 1,
    HL_ARENA_READY = 2,
    HL_ARENA_RETIRED = 3,
} hl_arena_lifecycle;

typedef enum hl_arena_zone {
    HL_ARENA_NORMAL = 1,
    HL_ARENA_LOW32 = 2,
} hl_arena_zone;

typedef enum hl_arena_reservation_state {
    HL_ARENA_RESERVATION_UNUSED = 0,
    HL_ARENA_RESERVATION_OWNED = 1,
    HL_ARENA_RESERVATION_RELEASED = 2,
} hl_arena_reservation_state;

typedef enum hl_arena_protection {
    HL_ARENA_PROTECTION_READ = 1u << 0,
    HL_ARENA_PROTECTION_WRITE = 1u << 1,
    HL_ARENA_PROTECTION_EXECUTE = 1u << 2,
} hl_arena_protection;

typedef struct hl_arena_manifest {
    uint64_t magic;
    uint32_t version;
    uint32_t size;
    uint64_t granule;
    uint64_t normal_base;
    uint64_t normal_limit;
    uint64_t normal_cursor;
    uint64_t low32_base;
    uint64_t low32_limit;
    uint64_t low32_cursor;
    uint64_t generation;
    uint64_t authority_nonce;
    uint64_t authority_identity;
    uint64_t next_identity;
    uint32_t reservation_count;
    uint32_t reserved;
} hl_arena_manifest;

typedef struct hl_arena_reservation {
    uint64_t authority_nonce;
    uint64_t authority_identity;
    uint64_t identity;
    uint64_t address;
    uint64_t length;
    uint32_t zone;
    uint32_t state;
} hl_arena_reservation;

typedef struct hl_arena_config {
    uint64_t granule;
    uint64_t normal_base;
    uint64_t normal_limit;
    uint64_t low32_base;
    uint64_t low32_limit;
} hl_arena_config;

typedef struct hl_arena_authority {
    /* Process-private state. It may be inherited through fork as COW memory,
     * but must never be placed in shared memory. Forks must use the
     * prepare/parent/child barrier below. */
    hl_arena_manifest manifest;
    hl_arena_reservation reservations[HL_ARENA_MAX_RESERVATIONS];
    uint32_t reservation_count;
    uint32_t active_transaction;
    uint64_t transaction_normal_cursor;
    uint64_t transaction_low32_cursor;
    uint32_t transaction_reservation_count;
    uint32_t materialization_count;
    uint32_t transaction_materialization_count;
    uint64_t materialized_identities[HL_ARENA_MAX_RESERVATIONS];
    _Atomic uint32_t fork_phase;
    uint64_t fork_process;
    _Atomic uint32_t lifecycle;
    uint64_t claimed_normal_base;
    uint64_t claimed_normal_limit;
    uint64_t claimed_low32_base;
    uint64_t claimed_low32_limit;
    _Atomic uint32_t lock;
} hl_arena_authority;

typedef struct hl_arena_persisted_state {
    hl_arena_manifest manifest;
    hl_arena_reservation reservations[HL_ARENA_MAX_RESERVATIONS];
    uint64_t checksum;
} hl_arena_persisted_state;

typedef struct hl_arena_transaction {
    hl_arena_authority *authority;
    uint64_t generation;
    uint32_t active;
    uint32_t reserved;
} hl_arena_transaction;

typedef struct hl_arena_fork_context {
    uint64_t parent_process;
    uint64_t child_nonce;
    uint32_t active;
    uint32_t reserved;
} hl_arena_fork_context;

int hl_arena_authority_init(hl_arena_authority *authority, const hl_arena_config *config);
int hl_arena_authority_destroy(hl_arena_authority *authority);
int hl_arena_authority_fork_prepare(hl_arena_authority *authority);
int hl_arena_authority_fork_parent(hl_arena_authority *authority);
int hl_arena_authority_fork_child(hl_arena_authority *authority);
int hl_arena_fork_context_prepare(hl_arena_fork_context *context);
int hl_arena_fork_context_parent(hl_arena_fork_context *context);
int hl_arena_after_fork_child(hl_arena_fork_context *context);
uint64_t hl_arena_host_granule(void);
int hl_arena_manifest_valid(const hl_arena_manifest *manifest);
int hl_arena_manifest_get(hl_arena_authority *authority, hl_arena_manifest *manifest);
int hl_arena_persisted_state_get(hl_arena_authority *authority, hl_arena_persisted_state *state);
int hl_arena_persisted_state_valid(const hl_arena_persisted_state *state);
int hl_arena_transaction_begin(hl_arena_authority *authority, hl_arena_transaction *transaction);
int hl_arena_transaction_reserve(hl_arena_transaction *transaction, hl_arena_zone zone, uint64_t length,
                                 hl_arena_reservation *reservation);
int hl_arena_transaction_materialize_anonymous(hl_arena_transaction *transaction,
                                               const hl_arena_reservation *reservation, uint32_t protection);
int hl_arena_transaction_commit(hl_arena_transaction *transaction);
int hl_arena_transaction_rollback(hl_arena_transaction *transaction);
int hl_arena_reservation_owned(hl_arena_authority *authority, const hl_arena_reservation *reservation);

#if defined(HL_NATIVE_TEST_HOOKS)
void hl_arena_test_lock(hl_arena_authority *authority);
void hl_arena_test_unlock(hl_arena_authority *authority);
void hl_arena_test_identity_sequence(uint64_t next);
void hl_arena_test_generation(hl_arena_authority *authority, uint64_t generation);
#endif

#endif
