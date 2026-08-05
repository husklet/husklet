#ifndef HL_NATIVE_AARCH64_TRACE_H
#define HL_NATIVE_AARCH64_TRACE_H

#include "source.h"
#include "../../translation.h"

#define HL_A64_TRACE_MAX_WORDS 32u
#define HL_A64_TRACE_MAX_BYTES 16384u
#define HL_A64_TRACE_MAX_CONDITIONALS 3u
#define HL_A64_TRACE_MAX_RELOCATIONS (HL_A64_TRACE_MAX_CONDITIONALS + 2u)

typedef struct hl_a64_trace_result {
    uint64_t source_first;
    uint64_t source_last;
    uint64_t code_size;
    uint64_t body_offset;
    uint64_t admitted_offset;
    uint64_t instruction_count;
    uint32_t provenance_count;
    uint32_t terminal;
    uint32_t relocation_count;
    uint32_t conditional_self_loop;
    uint64_t loop_pc;
    hl_native_relocation relocations[HL_A64_TRACE_MAX_RELOCATIONS];
    hl_native_provenance provenance[HL_A64_SOURCE_MAX_WORDS];
} hl_a64_trace_result;

typedef enum hl_a64_density_family {
    HL_A64_DENSITY_PAIR_READ,
    HL_A64_DENSITY_PAIR_WRITE,
    HL_A64_DENSITY_SCALAR_MEMORY,
    HL_A64_DENSITY_CONTROL,
    HL_A64_DENSITY_OTHER,
    HL_A64_DENSITY_FAMILY_COUNT,
} hl_a64_density_family;

typedef struct hl_a64_density_count {
    uint64_t guest_instructions;
    uint64_t hot_words;
    uint64_t cold_words;
} hl_a64_density_count;

typedef struct hl_a64_trace_density {
    hl_a64_density_count families[HL_A64_DENSITY_FAMILY_COUNT];
    uint64_t overhead_words;
    uint64_t total_words;
    uint64_t saturated;
} hl_a64_trace_density;

typedef struct hl_a64_loop_range {
    int64_t minimum;
    int64_t maximum;
    uint64_t step;
    uint8_t base;
    uint8_t permissions;
    uint8_t writes_contiguous;
    uint8_t reserved[5];
} hl_a64_loop_range;

typedef struct hl_a64_loop_plan {
    uint64_t loop_pc;
    uint64_t decrement;
    uint32_t instruction_count;
    uint32_t memory_count;
    uint8_t trip;
    uint8_t range_count;
    uint8_t reserved[6];
    hl_a64_loop_range ranges[2];
} hl_a64_loop_plan;

typedef enum hl_a64_loop_preflight_status {
    HL_A64_LOOP_PREFLIGHT_REJECT = 0,
    HL_A64_LOOP_PREFLIGHT_READY = 1,
    HL_A64_LOOP_PREFLIGHT_EPOCH = 2,
} hl_a64_loop_preflight_status;

typedef struct hl_a64_loop_entry {
    /* envelope first/last, owner-view first/last, delta, permissions */
    uint64_t views[2][6];
    uint64_t mapping_incarnation;
    uint64_t authority;
    uint64_t trip;
    uint64_t decrement;
    uint64_t instruction_count;
    uint64_t iterations;
    uint64_t budget_iterations;
    uint64_t executable;
    uint32_t view_count;
} hl_a64_loop_entry;

int hl_a64_trace_loop_plan(const uint32_t *, size_t, uint64_t, hl_a64_loop_plan *);
hl_a64_loop_preflight_status hl_a64_trace_loop_preflight(const hl_native_aarch64_cpu *,
    const hl_a64_loop_plan *, uint64_t, uint64_t, uint64_t, hl_a64_loop_entry *);

/* Internal instruction analysis used by trace validation.  Bit 31 denotes SP;
 * bits 0..30 denote the architectural X registers. */
typedef struct hl_a64_instruction_effect {
    uint32_t gpr_definitions;
    uint8_t terminal;
    uint8_t control;
    uint8_t reserved[2];
} hl_a64_instruction_effect;

hl_a64_instruction_effect hl_a64_trace_effect(uint32_t, uint64_t);

typedef struct hl_a64_range_plan {
    int64_t minimum;
    int64_t maximum;
    uint32_t members;
    uint8_t base;
    uint8_t permissions;
    uint8_t access_count;
    uint8_t reserved;
} hl_a64_range_plan;

int hl_a64_trace_range_plan(const uint32_t *, size_t, uint64_t, hl_a64_range_plan *);

int hl_a64_trace_certificate_check(const hl_native_aarch64_cpu *, uint64_t, int64_t, int64_t,
                                   uint32_t, uint64_t, uint64_t, uint64_t, uint64_t *);

int hl_a64_trace_build(const hl_a64_source *, uint64_t, size_t, void *, size_t, hl_a64_trace_result *);
int hl_a64_trace_build_density(const hl_a64_source *, uint64_t, size_t, void *, size_t,
                               hl_a64_trace_result *, hl_a64_trace_density *);
/* expected_authority is authenticated independently of translated bytes and
 * is part of the published translation identity. Zero always fails closed. */
int hl_a64_trace_build_direct(const hl_a64_source *, uint64_t, size_t, void *, size_t,
                              const hl_native_direct_authority *, uint64_t, hl_a64_trace_result *);
hl_native_status hl_a64_trace_cache(hl_native_executor *, const hl_a64_source *, uint64_t, size_t,
                                    void *, size_t, hl_native_code *, uint32_t *);
hl_native_status hl_a64_trace_cache_direct(hl_native_executor *, const hl_a64_source *, uint64_t, size_t,
                                           void *, size_t, const hl_native_direct_authority *, uint64_t,
                                           hl_native_code *, uint32_t *);

#endif
