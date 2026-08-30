#ifndef HL_X86_64_INTERP_PERSISTENCE_H
#define HL_X86_64_INTERP_PERSISTENCE_H

#include <stddef.h>
#include <stdint.h>

#define X64_PC_MAGIC UINT64_C(0x3143535034364c48)
#define X64_PC_VERSION UINT64_C(9)
#define X64_PC_ENDIAN UINT64_C(0x0807060504030201)
#define X64_PC_HEADER_SIZE 272u
#define X64_PC_MAP_SIZE 100u
#define X64_PC_OWNER_SIZE 28u
#define X64_PC_RELOC_SIZE 8u
#define X64_PC_HELPER_RELOC_SIZE 8u
#define X64_PC_LIB_SIZE 56u
#define X64_PC_CHAIN_SIZE 24u
#define X64_PC_CHECKSUM_OFFSET 264u
#define X64_PC_LIB_BASE UINT64_C(0x0000050000000000)
#define X64_PC_LIB_SPAN (UINT64_C(1) << 38)
#define X64_PC_LIB_MAX 512u
#define X64_PC_LIB_HASH_MAX (UINT64_C(512) << 20)

void x64_pc_put16(uint8_t **cursor, uint16_t value);
void x64_pc_put32(uint8_t **cursor, uint32_t value);
void x64_pc_put64(uint8_t **cursor, uint64_t value);
uint16_t x64_pc_get16(const uint8_t *cursor);
uint32_t x64_pc_get32(const uint8_t *cursor);
uint64_t x64_pc_get64(const uint8_t *cursor);
int x64_pc_header_validate(const uint8_t *bytes, size_t size, uint64_t abi, uint64_t cpu_size,
                           uint64_t map_slots, const uint8_t identity[32], uint64_t entry,
                           uint64_t modes, uint64_t matches[10]);

typedef struct x64_pc_format_limits {
    uint64_t arena_bytes, maps, owners, relocations, helper_relocations, libraries, chains;
} x64_pc_format_limits;

typedef struct x64_pc_format_layout {
    uint64_t arena, maps, owners, relocations, helper_relocations, libraries, chains;
    uint64_t map_bytes, owner_bytes, relocation_bytes, helper_relocation_bytes, library_bytes, chain_bytes;
    uint64_t image_lo, image_hi, interpreter_lo, interpreter_hi;
    const uint8_t *map_records, *owner_records, *relocation_records, *helper_relocation_records;
    const uint8_t *library_records, *chain_records, *arena_bytes;
} x64_pc_format_layout;

int x64_pc_layout_validate(const uint8_t *bytes, size_t size, const x64_pc_format_limits *limits,
                           x64_pc_format_layout *layout, uint64_t matches[8]);
int x64_pc_checksum_validate(const uint8_t *bytes, size_t size);
void x64_pc_checksum_write(uint8_t *bytes, size_t size);

typedef struct x64_pc_semantic_policy {
    uint64_t block_magic;
    uint32_t owner_preserve_mask;
    uint64_t map_slots;
    int require_census_ordinal;
} x64_pc_semantic_policy;

int x64_pc_validate_maps_owners(const x64_pc_format_layout *layout,
                                const x64_pc_semantic_policy *policy, unsigned *stage);
typedef int (*x64_pc_external_authority)(void *context, uint32_t kind);
int x64_pc_validate_relocations_authority(const x64_pc_format_layout *layout,
                                          x64_pc_external_authority external, void *context,
                                          unsigned *stage);

#endif
