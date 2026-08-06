#ifndef HL_NATIVE_TRANSLATION_H
#define HL_NATIVE_TRANSLATION_H

#include "executor.h"

typedef struct hl_native_translation_key {
    uint64_t guest;
    uint64_t mapping_incarnation;
    uint64_t instruction_epoch;
    uint64_t source_first;
    uint64_t source_last;
    uint64_t memory_mode;
    uint64_t authority_generation;
    uint64_t architecture;
    uint64_t direct_token;
    uint64_t direct_generation;
} hl_native_translation_key;

typedef struct hl_native_emission {
    const uint8_t *bytes;
    uint64_t size;
    uint64_t body_offset;
    uint64_t admitted_offset;
    const hl_native_provenance *provenance;
    uint32_t provenance_count;
    const hl_native_relocation *relocations;
    uint32_t relocation_count;
    uint32_t instruction_count;
    uint32_t conditional_self_loop;
    uint32_t cycle_safe;
    uint64_t loop_pc;
} hl_native_emission;

hl_native_lookup hl_native_translation_lookup(hl_native_executor *, const hl_native_translation_key *,
                                              hl_native_code *);
hl_native_status hl_native_translation_publish(hl_native_executor *, const hl_native_translation_key *,
                                               const hl_native_emission *);

#endif
