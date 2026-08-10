#ifndef HL_NATIVE_EXECUTOR_PRIVATE_H
#define HL_NATIVE_EXECUTOR_PRIVATE_H

#include "../include/executor.h"
#include "arena.h"
#include "../cache/cache.h"
#include "ibtc/storage.h"

#include <stdatomic.h>

struct hl_native_executor {
    hl_native_arena arena;
    hl_native_cache *cache;
    hl_native_ibtc_entry *ibtc;
    /* High 32 bits are mutation state; low 32 bits are execution leases.  One
     * atomic word makes close versus admission a race-free transition. */
    _Atomic uint64_t admission;
    /* High bit closes public-run admission across fork repair; remaining bits
     * count whole hl_native_run calls, including admission-free cold work. */
    _Atomic uint64_t fork_runs;
    /* Monotonic activation identity. Zero means certificate-ineligible. Once
     * exhausted it stays at UINT64_MAX and execution continues uncertified. */
    _Atomic uint64_t activation_generation;
    /* Registration identity shared by a future CPU certificate and cache
     * admission record. Zero is permanently ineligible after saturation. */
    _Atomic uint64_t next_certificate_cache_identity;
    struct hl_native_direct_token *direct_authority;
    uint64_t direct_generation;
    uint64_t next_authority_identity;
    uint64_t retained_authority_identity;
    hl_native_direct_authority retained_authority;
    uint32_t retained_authority_valid;
    uint64_t memory_mode;
    uint64_t authority_generation;
    uint32_t diagnostics;
    uint32_t write_reserve, write_commit;
    /* Emission is unconditional under the runtime gate; the emitted load reads
     * this filter out of the CPU record, which run_aarch64 seeds from here and
     * folds back. Set-only, so a concurrent seed can only lose a discovery. */
    uint32_t runtime_write_reserve;
    uint32_t dirty_overflow_continue;
    uint32_t fixed_aperture;
    uint8_t a64_reserve_filter[HL_NATIVE_A64_SATURATION_SLOTS];
    uint64_t ibtc_fills, ibtc_site_collisions, ibtc_shared_collisions;
    uint64_t boundary_branch, boundary_syscall, boundary_fallback, boundary_yield, completed;
    _Atomic uint64_t a64_branch_exhaustion, a64_branch_cold_relocation;
    _Atomic uint64_t a64_branch_nonrelocatable, a64_branch_unidentified;
    _Atomic uint64_t a64_branch_sample_claim, a64_branch_sample_form;
    uint64_t a64_branch_sample_pc, a64_branch_sample_source_first;
    uint64_t a64_branch_sample_source_last;
    uint64_t operand_callbacks, operand_cache_hits;
    uint64_t x86_public_exits, x86_public_syscalls, x86_syscall_vector_dirty, x86_public_epochs;
    _Atomic uint64_t a64_guard_fast, a64_guard_full, a64_guard_fallback;
    _Atomic uint64_t a64_dirty_reserved, a64_dirty_overflow, a64_dirty_committed;
    _Atomic uint64_t a64_dirty_merged;
    _Atomic uint64_t a64_aperture_scalar_attempts, a64_aperture_hits;
    _Atomic uint64_t x86_cold_builds, x86_cold_quota_exits;
    _Atomic uint64_t x86_guard_fast, x86_guard_full, x86_guard_fallback;
    _Atomic uint64_t x86_dirty_merged, x86_dirty_committed, x86_dirty_overflow;
    _Atomic uint64_t x86_write_cache_hit, x86_write_cache_miss;
    _Atomic uint64_t relocation_cold_targets, relocation_cycles, relocation_capacity;
    _Atomic uint64_t relocation_invalidations, ibtc_site_misses, ibtc_shared_misses;
    _Atomic uint64_t a64_fallback_guard_read, a64_fallback_guard_write;
    _Atomic uint64_t a64_fallback_simd_fp, a64_fallback_memory;
    _Atomic uint64_t a64_fallback_control, a64_fallback_other;
    _Atomic uint64_t a64_fallback_entry_rejection, a64_fallback_generated;
    _Atomic uint64_t a64_fallback_call, a64_fallback_return, a64_fallback_indirect;
    _Atomic uint64_t a64_fallback_system, a64_fallback_form_memory, a64_fallback_form_other;
    /* Absolute tail: dormant Stage B preserves every established executor
     * field offset. Production keeps this null until coherent activation. */
    hl_native_ibtc_authenticated_entry *authenticated_ibtc;
};

typedef struct hl_native_execution {
    hl_native_executor *owner;
    uint64_t generation;
    uint64_t *certificate_token;
} hl_native_execution;

/* Internal admission groundwork. An execution lease pins the current arena,
 * cache identities, and provenance against mutation. It must stay private to
 * one dispatcher activation and be released after fully spilled return. */
hl_native_status hl_native_executor_gate_enter(hl_native_executor *);
void hl_native_executor_gate_leave(hl_native_executor *);
hl_native_status hl_native_execution_enter(hl_native_executor *, hl_native_execution *);
uint64_t hl_native_execution_generation(const hl_native_execution *);
hl_native_status hl_native_execution_bind_certificate(hl_native_execution *, uint64_t *);
hl_native_status hl_native_execution_leave(hl_native_execution *);
hl_native_status hl_native_execution_exit(hl_native_execution *, hl_native_exit *, uint32_t, uint32_t,
                                          uint64_t, uint64_t, uint64_t, uint64_t);
hl_native_status hl_native_synchronize_epoch(hl_native_executor *, uint64_t, uint64_t,
                                             uint64_t, uint64_t);
hl_native_status hl_native_synchronize_direct(hl_native_executor *, uint64_t, uint64_t,
                                              uint64_t, const hl_native_direct_token *, uint64_t, uint64_t,
                                              const hl_native_projection *);
hl_native_status hl_native_executor_rollover(hl_native_executor *, uint64_t, uint64_t);
void hl_native_ibtc_fill_shared(hl_native_executor *, uint64_t, void *);
#if defined(__aarch64__)
hl_native_status hl_native_ibtc_fill(hl_native_executor *, hl_native_aarch64_cpu *, uint64_t,
                                     const hl_native_code *);
#endif
int hl_native_direct_request_valid(const hl_native_executor *, const hl_native_direct_token *,
                                   uint64_t, uint64_t, uint64_t, const hl_native_projection *);
int hl_native_direct_request_snapshot(const hl_native_executor *, const hl_native_direct_token *,
                                      uint64_t, uint64_t, uint64_t, const hl_native_projection *,
                                      hl_native_direct_authority *);
uint64_t hl_native_certificate_cache_identity_issue(hl_native_executor *);

#if defined(__aarch64__)
#ifndef HL_A64_RUN_VIEW_CACHE
#define HL_A64_RUN_VIEW_CACHE 1
#endif
#define HL_A64_RUN_VIEW_COUNT 4u

typedef struct hl_a64_run_views {
    hl_native_projection_view entries[HL_A64_RUN_VIEW_COUNT];
    size_t count;
} hl_a64_run_views;

/* Publishes the conforming prefix-compacted subset; a rejected view is skipped
 * rather than abandoning the slots behind it. */
void hl_a64_run_view_publish(const hl_a64_run_views *, hl_native_aarch64_cpu *, uint64_t);
#endif

#endif
