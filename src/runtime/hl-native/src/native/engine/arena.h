#ifndef HL_ENGINE_ARENA_H
#define HL_ENGINE_ARENA_H

#include <stdatomic.h>
#include <stdint.h>

#define HL_ARENA_MANIFEST_VERSION UINT32_C(1)
#define HL_ARENA_MANIFEST_MAGIC UINT64_C(0x484c4152454e4131)
#define HL_ARENA_MAX_RESERVATIONS UINT32_C(1024)

typedef enum hl_arena_zone {
    HL_ARENA_NORMAL = 1,
    HL_ARENA_LOW32 = 2,
} hl_arena_zone;

typedef enum hl_arena_reservation_state {
    HL_ARENA_RESERVATION_UNUSED = 0,
    HL_ARENA_RESERVATION_OWNED = 1,
    HL_ARENA_RESERVATION_RELEASED = 2,
} hl_arena_reservation_state;

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
    uint64_t next_identity;
    uint32_t reservation_count;
    uint32_t reserved;
} hl_arena_manifest;

typedef struct hl_arena_reservation {
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
    /* Process-private state. It may be inherited through fork as COW memory, but
     * must never be placed in shared memory: stale-owner recovery is pid based. */
    hl_arena_manifest manifest;
    hl_arena_reservation reservations[HL_ARENA_MAX_RESERVATIONS];
    uint32_t reservation_count;
    uint32_t active_transaction;
    uint64_t transaction_normal_cursor;
    uint64_t transaction_low32_cursor;
    uint32_t transaction_reservation_count;
    uint32_t initialized;
    uint64_t claimed_normal_base;
    uint64_t claimed_normal_limit;
    uint64_t claimed_low32_base;
    uint64_t claimed_low32_limit;
    _Atomic uint64_t owner;
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

int hl_arena_authority_init(hl_arena_authority *authority, const hl_arena_config *config);
void hl_arena_authority_destroy(hl_arena_authority *authority);
int hl_arena_manifest_valid(const hl_arena_manifest *manifest);
int hl_arena_manifest_get(hl_arena_authority *authority, hl_arena_manifest *manifest);
int hl_arena_persisted_state_get(hl_arena_authority *authority, hl_arena_persisted_state *state);
int hl_arena_persisted_state_valid(const hl_arena_persisted_state *state);
int hl_arena_transaction_begin(hl_arena_authority *authority, hl_arena_transaction *transaction);
int hl_arena_transaction_reserve(hl_arena_transaction *transaction, hl_arena_zone zone, uint64_t length,
                                 hl_arena_reservation *reservation);
int hl_arena_transaction_commit(hl_arena_transaction *transaction);
void hl_arena_transaction_rollback(hl_arena_transaction *transaction);
int hl_arena_reservation_owned(hl_arena_authority *authority, const hl_arena_reservation *reservation);

#endif
