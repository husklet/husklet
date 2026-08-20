#ifndef HL_ENGINE_ARENA_SIDECAR_H
#define HL_ENGINE_ARENA_SIDECAR_H

#include "arena.h"

#include <stddef.h>
#include <stdint.h>

/* Little-endian wire bytes spell "HLARMAP1". */
#define HL_ARENA_SIDECAR_MAGIC UINT64_C(0x3150414d52414c48)
#define HL_ARENA_SIDECAR_VERSION UINT32_C(1)
#define HL_ARENA_SIDECAR_HEADER_SIZE UINT32_C(80)
#define HL_ARENA_SIDECAR_RECORD_SIZE UINT32_C(56)
#define HL_ARENA_SIDECAR_MAX_RECORDS UINT32_C(1024)
#define HL_ARENA_SIDECAR_MAX_SIZE \
    (HL_ARENA_SIDECAR_HEADER_SIZE + HL_ARENA_SIDECAR_MAX_RECORDS * HL_ARENA_SIDECAR_RECORD_SIZE)

typedef enum hl_arena_mapping_source_kind {
    HL_ARENA_MAPPING_ANONYMOUS = 1,
    HL_ARENA_MAPPING_FILE = 2,
} hl_arena_mapping_source_kind;

/* `source_identity` is a checkpoint-store object identity, not a host path or
 * descriptor. Restore must resolve it through the checkpoint's authenticated
 * object table. Anonymous mappings carry no source identity or offset. */
typedef struct hl_arena_mapping_source {
    uint64_t reservation_identity;
    uint64_t address;
    uint64_t length;
    uint64_t source_identity;
    uint64_t source_offset;
    uint32_t source_kind;
    uint32_t protection;
    uint32_t flags;
    uint32_t reserved;
} hl_arena_mapping_source;

typedef struct hl_arena_mapping_sidecar {
    uint32_t guest_isa;
    uint32_t record_count;
    uint64_t granule;
    uint64_t authority_nonce;
    uint64_t authority_identity;
    uint64_t generation;
    hl_arena_mapping_source records[HL_ARENA_SIDECAR_MAX_RECORDS];
} hl_arena_mapping_sidecar;

typedef struct hl_arena_expected_mapping {
    uint64_t reservation_identity;
    uint64_t address;
    uint64_t length;
} hl_arena_expected_mapping;

typedef struct hl_arena_sidecar_authority {
    uint32_t guest_isa;
    uint32_t reserved;
    uint64_t granule;
    uint64_t authority_nonce;
    uint64_t authority_identity;
    uint64_t generation;
    uint32_t mapping_count;
    uint32_t mapping_reserved;
    const hl_arena_expected_mapping *mappings;
} hl_arena_sidecar_authority;

/* Parse is a structural decoder, not a cryptographic authenticator. `expected`
 * must be derived from the already-authenticated checkpoint manifest and arena
 * persisted state, never from this sidecar. The enclosing checkpoint image
 * digest must authenticate the sidecar bytes before this parser is called.
 * Exact authority/generation and reservation identity/range matching prevents
 * replay or cross-arena substitution after that outer authentication. */

/* Publication is deliberately abstract: the checkpoint sink owns staging and
 * visibility. A failed begin must create no staging and require no abort. A
 * successful begin must remain invisible until commit. Commit
 * must make the complete object visible atomically, and may return failure only
 * while the object remains invisible. Abort after any write/commit failure must
 * discard all staging. Implementations unable to provide that contract are not
 * valid publishers. The deferred source is compiled only by its direct native
 * probe and is not yet part of the production archive or capture/restore. */
typedef struct hl_arena_sidecar_publication {
    void *context;
    int (*begin)(void *context, uint64_t size);
    int (*write)(void *context, const void *bytes, size_t size);
    int (*commit)(void *context);
    void (*abort)(void *context);
} hl_arena_sidecar_publication;

int hl_arena_mapping_sidecar_size(uint32_t record_count, size_t *size);
int hl_arena_mapping_sidecar_encode(const hl_arena_mapping_sidecar *sidecar, void *output, size_t capacity,
                                    size_t *written);
int hl_arena_mapping_sidecar_parse(const void *input, size_t size, const hl_arena_sidecar_authority *expected,
                                   hl_arena_mapping_sidecar *sidecar);
int hl_arena_mapping_sidecar_publish(const hl_arena_mapping_sidecar *sidecar, void *scratch, size_t capacity,
                                     const hl_arena_sidecar_publication *publication);

#endif
