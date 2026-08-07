#include "../include/executor.h"
#include "executor.h"

#include "arena.h"
#include "../cache/cache.h"
#include "fault/provenance.h"
#include "dispatch/exit.h"
#include "arch/aarch64/entry.h"
#include "arch/aarch64/fault.h"
#include "arch/aarch64/projection.h"
#include "arch/aarch64/source.h"
#include "arch/aarch64/trace.h"
#include "arch/x86_64/run.h"

#include <stdlib.h>
#include <stdatomic.h>
#include <string.h>
#if defined(__linux__)
/* Strict C11 hides this non-POSIX Linux interface unless feature macros are
 * set before the build's forced toolchain header.  Keep the Linux ABI local. */
extern int madvise(void *, size_t, int);
#define HL_MADV_DONTNEED 4
#endif

_Static_assert(ATOMIC_LLONG_LOCK_FREE == 2, "executor admission requires lock-free uint64 atomics");

#define FORK_RUNS_CLOSED (UINT64_C(1) << 63)
#define FORK_RUNS_COUNT_MASK (FORK_RUNS_CLOSED - 1)

static hl_native_status run_lifecycle_enter(hl_native_executor *executor) {
    uint64_t current = atomic_load_explicit(&executor->fork_runs, memory_order_acquire);
    for (;;) {
        if ((current & FORK_RUNS_CLOSED) != 0) return HL_NATIVE_STATE;
        if ((current & FORK_RUNS_COUNT_MASK) == FORK_RUNS_COUNT_MASK) return HL_NATIVE_CAPACITY;
        if (atomic_compare_exchange_weak_explicit(&executor->fork_runs, &current, current + 1,
                                                  memory_order_acquire, memory_order_relaxed))
            return HL_NATIVE_OK;
    }
}

static void run_lifecycle_leave(hl_native_executor *executor) {
    uint64_t prior = atomic_fetch_sub_explicit(&executor->fork_runs, 1, memory_order_release);
    (void)prior;
}

#if defined(__aarch64__)
static void a64_fallback_word(hl_native_executor *executor, uint32_t word, int entry_rejection) {
    _Atomic uint64_t *counter;
    _Atomic uint64_t *form;
    if (!executor->diagnostics) return;
    atomic_fetch_add_explicit(entry_rejection ? &executor->a64_fallback_entry_rejection
                                              : &executor->a64_fallback_generated,
                              1, memory_order_relaxed);
    if ((word & UINT32_C(0xfc000000)) == UINT32_C(0x94000000) ||
        (word & UINT32_C(0xfffffc1f)) == UINT32_C(0xd63f0000))
        form = &executor->a64_fallback_call;
    else if ((word & UINT32_C(0xfffffc1f)) == UINT32_C(0xd65f0000) ||
             word == UINT32_C(0xd65f0bff) || word == UINT32_C(0xd65f0fff))
        form = &executor->a64_fallback_return;
    else if ((word & UINT32_C(0xfffffc1f)) == UINT32_C(0xd61f0000))
        form = &executor->a64_fallback_indirect;
    else if ((word & UINT32_C(0xff000000)) == UINT32_C(0xd4000000) ||
             (word & UINT32_C(0xff000000)) == UINT32_C(0xd5000000))
        form = &executor->a64_fallback_system;
    else if ((word & UINT32_C(0x0a000000)) == UINT32_C(0x08000000))
        form = &executor->a64_fallback_form_memory;
    else
        form = &executor->a64_fallback_form_other;
    atomic_fetch_add_explicit(form, 1, memory_order_relaxed);
    if ((word & UINT32_C(0x0e000000)) == UINT32_C(0x0e000000))
        counter = &executor->a64_fallback_simd_fp;
    else if ((word & UINT32_C(0x0a000000)) == UINT32_C(0x08000000))
        counter = &executor->a64_fallback_memory;
    else if ((word & UINT32_C(0x1c000000)) == UINT32_C(0x14000000) ||
             (word & UINT32_C(0xff000000)) == UINT32_C(0xd4000000))
        counter = &executor->a64_fallback_control;
    else
        counter = &executor->a64_fallback_other;
    atomic_fetch_add_explicit(counter, 1, memory_order_relaxed);
}
#endif

enum mutation_state {
    MUTATION_OPEN,
    MUTATION_ACTIVE,
    MUTATION_FORK,
};

#define ADMISSION_STATE_SHIFT 32u
#define ADMISSION(state, count) (((uint64_t)(state) << ADMISSION_STATE_SHIFT) | (uint32_t)(count))
#define ADMISSION_STATE(value) ((uint32_t)((value) >> ADMISSION_STATE_SHIFT))
#define ADMISSION_COUNT(value) ((uint32_t)(value))

struct hl_native_interrupt_token {
    _Atomic uint64_t value;
};

struct hl_native_direct_token {
    hl_native_executor *owner;
    uint64_t generation;
    uint64_t identity;
    hl_native_direct_authority authority;
};

static void ibtc_clear(hl_native_executor *executor);

static uint64_t direct_generation(hl_native_executor *executor) {
    executor->direct_generation++;
    if (executor->direct_generation == 0) executor->direct_generation = 1;
    return executor->direct_generation;
}

static uint64_t authority_identity(hl_native_executor *executor) {
    executor->next_authority_identity++;
    if (executor->next_authority_identity == 0) executor->next_authority_identity = 1;
    return executor->next_authority_identity;
}

uint64_t hl_native_certificate_cache_identity_issue(hl_native_executor *executor) {
    if (executor == NULL) return 0;
    uint64_t current = atomic_load_explicit(&executor->next_certificate_cache_identity,
                                            memory_order_relaxed);
    for (;;) {
        if (current == UINT64_MAX) return 0;
        uint64_t desired = current + 1;
        if (atomic_compare_exchange_weak_explicit(
                &executor->next_certificate_cache_identity, &current, desired,
                memory_order_relaxed, memory_order_relaxed))
            return desired;
    }
}

hl_native_status hl_native_direct_register(hl_native_executor *executor,
                                           const hl_native_direct_authority *authority,
                                           hl_native_direct_token **output) {
    hl_native_direct_token *token;
    hl_native_status status;
    if (output == NULL) return HL_NATIVE_ARGUMENT;
    *output = NULL;
    if (executor == NULL || authority == NULL || authority->abi != HL_NATIVE_ABI ||
        authority->size != sizeof(*authority) || authority->reserved != 0 ||
        authority->permissions == 0 || (authority->permissions & ~7u) != 0 ||
        authority->guest_last <= authority->guest_first ||
        authority->host_first > UINT64_MAX - (authority->guest_last - authority->guest_first))
        return HL_NATIVE_ARGUMENT;
    token = calloc(1, sizeof(*token));
    if (token == NULL) return HL_NATIVE_MEMORY;
    status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) {
        free(token);
        return status;
    }
    if (executor->direct_authority != NULL) {
        status = HL_NATIVE_STATE;
    } else {
        token->owner = executor;
        token->generation = direct_generation(executor);
        token->authority = *authority;
        if (!executor->retained_authority_valid ||
            memcmp(&executor->retained_authority, authority, sizeof(*authority)) != 0) {
            executor->retained_authority = *authority;
            executor->retained_authority_identity = authority_identity(executor);
            executor->retained_authority_valid = 1;
        }
        token->identity = executor->retained_authority_identity;
        executor->direct_authority = token;
        *output = token;
    }
    hl_native_executor_gate_leave(executor);
    if (status != HL_NATIVE_OK) free(token);
    return status;
}

int hl_native_direct_validate(const hl_native_executor *executor,
                              const hl_native_direct_token *token,
                              hl_native_direct_authority *output) {
    if (executor == NULL || token == NULL || output == NULL ||
        executor->direct_authority != token || token->owner != executor ||
        token->generation == 0 || token->generation != executor->direct_generation)
        return 0;
    *output = token->authority;
    return 1;
}

uint64_t hl_native_direct_generation(const hl_native_executor *executor,
                                     const hl_native_direct_token *token) {
    return executor != NULL && token != NULL && executor->direct_authority == token &&
                   token->owner == executor && token->generation == executor->direct_generation
        ? token->generation : 0;
}

uint64_t hl_native_direct_identity(const hl_native_executor *executor,
                                   const hl_native_direct_token *token) {
    return hl_native_direct_generation(executor, token) != 0 ? token->identity : 0;
}

int hl_native_direct_request_valid(const hl_native_executor *executor,
                                   const hl_native_direct_token *token, uint64_t generation,
                                   uint64_t identity,
                                   const hl_native_projection *projection) {
    if (hl_native_direct_generation(executor, token) != generation || identity == 0 ||
        token->identity != identity || projection == NULL ||
        projection->active >= projection->count)
        return 0;
    const hl_native_direct_authority *authority = &token->authority;
    const hl_native_projection_view *view = &projection->views[projection->active];
    return authority->mapping_incarnation == projection->mapping_incarnation &&
        authority->mapping_incarnation == view->mapping_incarnation &&
        authority->guest_first == view->guest_first && authority->guest_last == view->guest_last &&
        authority->host_first == view->host_first &&
        (view->permissions & authority->permissions) == authority->permissions;
}

int hl_native_direct_request_snapshot(const hl_native_executor *executor,
                                      const hl_native_direct_token *token, uint64_t generation,
                                      uint64_t identity,
                                      const hl_native_projection *projection,
                                      hl_native_direct_authority *output) {
    if (output == NULL || !hl_native_direct_request_valid(executor, token, generation, identity, projection)) return 0;
    *output = token->authority;
    return 1;
}

hl_native_status hl_native_direct_unregister(hl_native_executor *executor,
                                             hl_native_direct_token *token) {
    hl_native_status status;
    if (executor == NULL || token == NULL || token->owner != executor) return HL_NATIVE_ARGUMENT;
    status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    if (executor->direct_authority == token && token->generation == executor->direct_generation) {
        executor->direct_authority = NULL;
        (void)direct_generation(executor);
    }
    token->owner = NULL;
    hl_native_executor_gate_leave(executor);
    free(token);
    return HL_NATIVE_OK;
}

hl_native_status hl_native_interrupt_create(hl_native_interrupt_token **output) {
    if (output == NULL) return HL_NATIVE_ARGUMENT;
    *output = calloc(1, sizeof(**output));
    if (*output == NULL) return HL_NATIVE_MEMORY;
    atomic_init(&(*output)->value, 0);
    return HL_NATIVE_OK;
}

hl_native_status hl_native_interrupt_set(hl_native_interrupt_token *token, uint64_t value) {
    if (token == NULL) return HL_NATIVE_ARGUMENT;
    atomic_store_explicit(&token->value, value, memory_order_release);
    return HL_NATIVE_OK;
}

void hl_native_interrupt_destroy(hl_native_interrupt_token *token) {
    if (token == NULL) return;
    atomic_store_explicit(&token->value, 0, memory_order_relaxed);
    free(token);
}

static void ibtc_clear(hl_native_executor *executor) {
    if (executor == NULL || executor->ibtc == NULL) return;
    const size_t bytes = HL_NATIVE_IBTC_COUNT * sizeof(*executor->ibtc);
#if defined(__linux__)
    /* The table is page-aligned anonymous storage.  Discarding its pages gives
     * the next reader zero-filled entries without faulting every page into the
     * process during construction, reset, or fork repair. */
    if (madvise(executor->ibtc, bytes, HL_MADV_DONTNEED) == 0) return;
#endif
    memset(executor->ibtc, 0, bytes);
}

static void cache_observe(void *context, hl_native_cache_event event) {
    hl_native_executor *executor = context;
    _Atomic uint64_t *counter = NULL;
    if (executor == NULL) return;
    switch (event) {
        case HL_NATIVE_CACHE_RELOCATION_COLD_TARGET: counter = &executor->relocation_cold_targets; break;
        case HL_NATIVE_CACHE_RELOCATION_CYCLE: counter = &executor->relocation_cycles; break;
        case HL_NATIVE_CACHE_RELOCATION_CAPACITY: counter = &executor->relocation_capacity; break;
        case HL_NATIVE_CACHE_RELOCATION_INVALIDATION: counter = &executor->relocation_invalidations; break;
        default: return;
    }
    atomic_fetch_add_explicit(counter, 1, memory_order_relaxed);
}

static void ibtc_publish(hl_native_ibtc_entry *, uint64_t, void *);

static void ibtc_invalidate(hl_native_executor *executor, uint64_t first, uint64_t last) {
    if (executor == NULL || executor->ibtc == NULL || last <= first) return;
    for (size_t index = 0; index < HL_NATIVE_IBTC_COUNT; ++index) {
        hl_native_ibtc_entry *entry = &executor->ibtc[index];
        uint64_t target = __atomic_load_n(&entry->target, __ATOMIC_ACQUIRE);
        if (target >= first && target < last) ibtc_publish(entry, 0, NULL);
    }
}

static void ibtc_publish(hl_native_ibtc_entry *entry, uint64_t target, void *body) {
#if defined(__aarch64__)
    __asm__ volatile("dmb ish\n\tstp %1, %2, [%0]" : : "r"(entry), "r"(target), "r"(body) : "memory");
#else
    entry->body = body;
    atomic_thread_fence(memory_order_release);
    entry->target = target;
#endif
}

void hl_native_ibtc_fill_shared(hl_native_executor *executor, uint64_t target, void *body) {
    ibtc_publish(&executor->ibtc[(target >> 2) & (HL_NATIVE_IBTC_COUNT - 1)], target, body);
}

#if defined(__aarch64__)
static hl_native_status ibtc_fill(hl_native_executor *executor, hl_native_aarch64_cpu *cpu,
                                  uint64_t target, const hl_native_code *code) {
    uintptr_t site = (uintptr_t)cpu->indirect_site;
    uintptr_t rx = (uintptr_t)executor->arena.executable;
    /* A reset re-reserves the arena, so a site recorded before it addresses the
     * old mapping. That is a dead site, not a broken invariant. */
    if (site == 0) return HL_NATIVE_OK;
    if ((site & 15u) != 0 || site < rx || site > rx + executor->arena.mapping.content - 16) {
        cpu->indirect_site = 0;
        return HL_NATIVE_OK;
    }
    if (executor->diagnostics) {
        uint64_t previous;
        hl_native_ibtc_entry *shared;
        memcpy(&previous, executor->arena.writable + (site - rx), sizeof(previous));
        shared = &executor->ibtc[(target >> 2) & (HL_NATIVE_IBTC_COUNT - 1)];
        executor->ibtc_fills++;
        atomic_fetch_add_explicit(&executor->ibtc_site_misses, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&executor->ibtc_shared_misses, 1, memory_order_relaxed);
        if (previous != 0 && previous != target) executor->ibtc_site_collisions++;
        if (shared->target != 0 && shared->target != target) executor->ibtc_shared_collisions++;
    }
    int64_t branch_delta;
    memcpy(&branch_delta, executor->arena.writable + (site - rx) + 8, sizeof(branch_delta));
    hl_native_status status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) {
        /* A peer is executing.  Keep the unfilled local site and retry through
         * ordinary dispatch; never turn harmless patch contention fatal. */
        cpu->indirect_site = 0;
        return HL_NATIVE_OK;
    }
    int writing = 0;
    status = hl_native_cache_write_begin(executor->cache);
    if (status == HL_NATIVE_OK) writing = 1;
    uint32_t patched = 0;
    if (status == HL_NATIVE_OK)
        status = hl_native_cache_relocate_site(executor->cache, (const void *)site, branch_delta,
                                               target, code->instruction_epoch,
                                               code->identity_token, &patched);
    if (status == HL_NATIVE_OK) {
        uint64_t offset = site - rx;
        memcpy(executor->arena.writable + offset, &target, sizeof(target));
        status = executor->arena.memory.publish(executor->arena.memory.context,
                                                executor->arena.mapping.handle, offset, 8);
    }
    if (status == HL_NATIVE_OK) hl_native_ibtc_fill_shared(executor, target, code->body);
    if (writing) {
        hl_native_status end = hl_native_cache_write_end(executor->cache);
        if (status == HL_NATIVE_OK) status = end;
    }
    hl_native_executor_gate_leave(executor);
    if (status != HL_NATIVE_OK) return status;
    (void)patched;
    cpu->indirect_site = 0;
    return HL_NATIVE_OK;
}
#endif

hl_native_status hl_native_executor_gate_enter(hl_native_executor *executor) {
    if (executor == NULL) return HL_NATIVE_ARGUMENT;
    uint64_t expected = ADMISSION(MUTATION_OPEN, 0);
    if (!atomic_compare_exchange_strong_explicit(&executor->admission, &expected,
                                                 ADMISSION(MUTATION_ACTIVE, 0),
                                                 memory_order_seq_cst, memory_order_relaxed))
        return HL_NATIVE_STATE;
    return HL_NATIVE_OK;
}

void hl_native_executor_gate_leave(hl_native_executor *executor) {
    if (executor == NULL) return;
    atomic_store_explicit(&executor->admission, ADMISSION(MUTATION_OPEN, 0), memory_order_release);
}

static uint64_t activation_generation(hl_native_executor *executor) {
    uint64_t current = atomic_load_explicit(&executor->activation_generation,
                                            memory_order_relaxed);
    for (;;) {
        if (current == UINT64_MAX) return 0;
        uint64_t desired = current + 1;
        if (atomic_compare_exchange_weak_explicit(&executor->activation_generation,
                                                  &current, desired,
                                                  memory_order_relaxed,
                                                  memory_order_relaxed))
            return desired;
    }
}

hl_native_status hl_native_execution_enter(hl_native_executor *executor, hl_native_execution *execution) {
    if (executor == NULL || execution == NULL || execution->owner != NULL) return HL_NATIVE_ARGUMENT;
    uint64_t current = atomic_load_explicit(&executor->admission, memory_order_acquire);
    for (;;) {
        if (ADMISSION_STATE(current) != MUTATION_OPEN) return HL_NATIVE_STATE;
        if (ADMISSION_COUNT(current) == UINT32_MAX) return HL_NATIVE_CAPACITY;
        uint64_t desired = ADMISSION(MUTATION_OPEN, ADMISSION_COUNT(current) + 1);
        if (atomic_compare_exchange_weak_explicit(&executor->admission, &current, desired,
                                                  memory_order_acquire, memory_order_relaxed))
            break;
    }
    execution->generation = activation_generation(executor);
    execution->certificate_token = NULL;
    execution->owner = executor;
    return HL_NATIVE_OK;
}

uint64_t hl_native_execution_generation(const hl_native_execution *execution) {
    return execution != NULL && execution->owner != NULL ? execution->generation : 0;
}

hl_native_status hl_native_execution_bind_certificate(hl_native_execution *execution,
                                                       uint64_t *token) {
    if (execution == NULL || execution->owner == NULL || token == NULL)
        return HL_NATIVE_ARGUMENT;
    __atomic_store_n(token, 0, __ATOMIC_RELEASE);
    execution->certificate_token = token;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_execution_leave(hl_native_execution *execution) {
    hl_native_executor *executor;
    if (execution == NULL || execution->owner == NULL) return HL_NATIVE_ARGUMENT;
    executor = execution->owner;
    if (execution->certificate_token != NULL)
        __atomic_store_n(execution->certificate_token, 0, __ATOMIC_RELEASE);
    execution->certificate_token = NULL;
    execution->generation = 0;
    execution->owner = NULL;
    uint64_t current = atomic_load_explicit(&executor->admission, memory_order_relaxed);
    for (;;) {
        if (ADMISSION_STATE(current) != MUTATION_OPEN || ADMISSION_COUNT(current) == 0)
            return HL_NATIVE_STATE;
        uint64_t desired = ADMISSION(MUTATION_OPEN, ADMISSION_COUNT(current) - 1);
        if (atomic_compare_exchange_weak_explicit(&executor->admission, &current, desired,
                                                  memory_order_release, memory_order_relaxed))
            break;
    }
    return HL_NATIVE_OK;
}

hl_native_status hl_native_fault_scope_enter(hl_native_executor *executor, hl_native_cpu *cpu,
                                             hl_native_fault_scope *scope) {
    hl_native_execution execution = {0};
    if (executor == NULL || cpu == NULL || scope == NULL || scope->executor != NULL || scope->reserved != 0 ||
        cpu->abi != HL_NATIVE_ABI || cpu->size != sizeof(*cpu) || cpu->state.opaque == NULL ||
        (cpu->architecture != HL_NATIVE_AARCH64 && cpu->architecture != HL_NATIVE_X86_64))
        return HL_NATIVE_ARGUMENT;
    hl_native_status status = hl_native_execution_enter(executor, &execution);
    if (status != HL_NATIVE_OK) return status;
    scope->abi = HL_NATIVE_ABI;
    scope->size = sizeof(*scope);
    scope->architecture = cpu->architecture;
    scope->reserved = 0;
    scope->cpu = cpu;
    /* Publish the owner last.  The consumer is responsible for publishing the
     * completed scope to its signal owner before native entry. */
    scope->executor = execution.owner;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_fault_scope_leave(hl_native_fault_scope *scope) {
    if (scope == NULL || scope->abi != HL_NATIVE_ABI || scope->size != sizeof(*scope) ||
        scope->reserved != 0 || scope->executor == NULL || scope->cpu == NULL)
        return HL_NATIVE_ARGUMENT;
    hl_native_execution execution = {.owner = scope->executor};
    scope->executor = NULL;
    scope->cpu = NULL;
    scope->architecture = 0;
    return hl_native_execution_leave(&execution);
}

int hl_native_fault_scope_provenance(const hl_native_fault_scope *scope, uint64_t host_pc,
                                     hl_native_provenance *output) {
    if (scope == NULL || output == NULL || scope->abi != HL_NATIVE_ABI ||
        scope->size != sizeof(*scope) || scope->reserved > 1 ||
        scope->executor == NULL || scope->cpu == NULL ||
        host_pc == 0)
        return 0;
    return hl_native_cache_provenance_record(scope->executor->cache,
                                             (const void *)(uintptr_t)host_pc, output);
}

int hl_native_fault_scope_contains(const hl_native_fault_scope *scope, uint64_t host_pc) {
    hl_native_provenance record;
    return hl_native_fault_scope_provenance(scope, host_pc, &record);
}

int hl_native_fault_scope_prepare_return(const hl_native_fault_scope *scope, uint64_t host_pc,
                                         uint64_t host_fault, void *host_context) {
    hl_native_provenance record;
    if (host_context == NULL || !hl_native_fault_scope_provenance(scope, host_pc, &record)) return 0;
#if defined(__linux__) && defined(__aarch64__)
    if (scope->architecture == HL_NATIVE_AARCH64) {
        hl_a64_host_context context;
        if (!hl_a64_linux_context(host_context, &context) || context.program != host_pc) return 0;
        return hl_a64_linux_fault_return(scope->cpu->state.aarch64, host_context, &record,
                                         host_fault);
    }
#elif defined(__APPLE__) && defined(__aarch64__)
    if (scope->architecture == HL_NATIVE_AARCH64) {
        hl_a64_host_context context;
        if (!hl_a64_darwin_context(host_context, &context) || context.program != host_pc) return 0;
        return hl_a64_darwin_fault_return(scope->cpu->state.aarch64, host_context, &record,
                                          host_fault);
    }
#else
    (void)host_fault;
#endif
    return 0;
}

hl_native_status hl_native_execution_exit(hl_native_execution *execution, hl_native_exit *output,
                                          uint32_t kind, uint32_t access, uint64_t instruction,
                                          uint64_t next, uint64_t address, uint64_t code) {
    hl_native_status status;
    hl_native_status release;
    if (execution == NULL || execution->owner == NULL) return HL_NATIVE_ARGUMENT;
    status = hl_native_exit_build(output, kind, access, instruction, next, address, code);
    release = hl_native_execution_leave(execution);
    return status == HL_NATIVE_OK ? release : status;
}

hl_native_status hl_native_create(const hl_native_config *config, hl_native_executor **output) {
    hl_native_executor *executor;
    hl_native_status status;
    if (output == NULL) return HL_NATIVE_ARGUMENT;
    *output = NULL;
    executor = calloc(1, sizeof(*executor));
    if (executor == NULL) return HL_NATIVE_MEMORY;
    /* Dynamically allocated atomic state is initialized with atomic_init before
     * its first operation; zeroed storage alone is not a C11 initializer. */
    atomic_init(&executor->admission, ADMISSION(MUTATION_OPEN, 0));
    atomic_init(&executor->fork_runs, 0);
    atomic_init(&executor->activation_generation, 0);
    atomic_init(&executor->next_certificate_cache_identity, 0);
    executor->diagnostics = (config->flags & HL_NATIVE_DIAGNOSTICS) != 0;
    atomic_init(&executor->a64_guard_fast, 0);
    atomic_init(&executor->a64_guard_full, 0);
    atomic_init(&executor->a64_guard_fallback, 0);
    atomic_init(&executor->a64_dirty_reserved, 0);
    atomic_init(&executor->a64_dirty_overflow, 0);
    atomic_init(&executor->a64_dirty_committed, 0);
    atomic_init(&executor->a64_dirty_merged, 0);
    atomic_init(&executor->relocation_cold_targets, 0);
    atomic_init(&executor->relocation_cycles, 0);
    atomic_init(&executor->relocation_capacity, 0);
    atomic_init(&executor->relocation_invalidations, 0);
    atomic_init(&executor->ibtc_site_misses, 0);
    atomic_init(&executor->ibtc_shared_misses, 0);
    atomic_init(&executor->a64_fallback_guard_read, 0);
    atomic_init(&executor->a64_fallback_guard_write, 0);
    atomic_init(&executor->a64_fallback_simd_fp, 0);
    atomic_init(&executor->a64_fallback_memory, 0);
    atomic_init(&executor->a64_fallback_control, 0);
    atomic_init(&executor->a64_fallback_other, 0);
    atomic_init(&executor->a64_fallback_entry_rejection, 0);
    atomic_init(&executor->a64_fallback_generated, 0);
    atomic_init(&executor->a64_fallback_call, 0);
    atomic_init(&executor->a64_fallback_return, 0);
    atomic_init(&executor->a64_fallback_indirect, 0);
    atomic_init(&executor->a64_fallback_system, 0);
    atomic_init(&executor->a64_fallback_form_memory, 0);
    atomic_init(&executor->a64_fallback_form_other, 0);
    atomic_init(&executor->a64_branch_exhaustion, 0);
    atomic_init(&executor->a64_branch_cold_relocation, 0);
    atomic_init(&executor->a64_branch_nonrelocatable, 0);
    atomic_init(&executor->a64_branch_unidentified, 0);
    atomic_init(&executor->a64_branch_sample_claim, 0);
    atomic_init(&executor->a64_branch_sample_form, 0);
    status = hl_native_arena_create(&executor->arena, config);
    if (status != HL_NATIVE_OK) {
        free(executor);
        return status;
    }
    hl_native_cache_observer observer = {executor, executor->diagnostics ? cache_observe : NULL};
    status = hl_native_cache_create(&executor->cache, &executor->arena, 1u << 19, 1u << 18, 0, 0,
                                    &observer);
    if (status != HL_NATIVE_OK) {
        hl_native_arena_destroy(&executor->arena);
        free(executor);
        return status;
    }
    executor->ibtc = hl_native_ibtc_storage_create();
    if (executor->ibtc == NULL) {
        hl_native_cache_destroy(executor->cache);
        hl_native_arena_destroy(&executor->arena);
        free(executor);
        return HL_NATIVE_MEMORY;
    }
    ibtc_clear(executor);
    *output = executor;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_before_fork(hl_native_executor *executor) {
    uint64_t runs_expected = 0;
    uint64_t expected = ADMISSION(MUTATION_OPEN, 0);
    if (executor == NULL) return HL_NATIVE_ARGUMENT;
    if (!atomic_compare_exchange_strong_explicit(&executor->fork_runs, &runs_expected,
                                                 FORK_RUNS_CLOSED,
                                                 memory_order_seq_cst, memory_order_relaxed))
        return HL_NATIVE_STATE;
    if (!atomic_compare_exchange_strong_explicit(&executor->admission, &expected,
                                                 ADMISSION(MUTATION_FORK, 0),
                                                 memory_order_seq_cst, memory_order_relaxed)) {
        atomic_store_explicit(&executor->fork_runs, 0, memory_order_release);
        return HL_NATIVE_STATE;
    }
    if (!hl_native_cache_available(executor->cache)) {
        atomic_store_explicit(&executor->admission, ADMISSION(MUTATION_OPEN, 0), memory_order_release);
        atomic_store_explicit(&executor->fork_runs, 0, memory_order_release);
        return HL_NATIVE_STATE;
    }
    return HL_NATIVE_OK;
}

hl_native_status hl_native_after_fork(hl_native_executor *executor, uint32_t preserve) {
    hl_native_status status;
    uint64_t expected = ADMISSION(MUTATION_FORK, 0);
    if (executor == NULL || preserve > 1) return HL_NATIVE_ARGUMENT;
    if (!atomic_compare_exchange_strong_explicit(&executor->admission, &expected,
                                                 ADMISSION(MUTATION_ACTIVE, 0),
                                                 memory_order_acquire, memory_order_relaxed))
        return HL_NATIVE_STATE;
    status = hl_native_arena_repair(&executor->arena, preserve);
    if (status != HL_NATIVE_OK) {
        /* Repair may have replaced or invalidated the inherited executable
         * mapping before reporting failure. Nothing address-bearing may
         * remain admissible when the fork mutation lease is released. */
        hl_native_cache_fail(executor->cache);
        ibtc_clear(executor);
    }
    if (status == HL_NATIVE_OK && preserve == 0) status = hl_native_cache_reset(executor->cache, 0);
    if (status == HL_NATIVE_OK && preserve != 0) {
        int writing = 0;
        status = hl_native_cache_write_begin(executor->cache);
        if (status == HL_NATIVE_OK) writing = 1;
        if (status == HL_NATIVE_OK) status = hl_native_cache_relocations_restore(executor->cache);
        if (writing) {
            hl_native_status end = hl_native_cache_write_end(executor->cache);
            if (status == HL_NATIVE_OK) status = end;
        }
        if (status == HL_NATIVE_OK)
            hl_native_cache_certificates_clear(executor->cache);
        else
            hl_native_cache_fail(executor->cache);
        /* A failed cold restoration poisons cache admission; inherited IBTC
         * ingress must still be removed before the fork lease is released. */
        ibtc_clear(executor);
    }
    if (status == HL_NATIVE_OK && preserve == 0) ibtc_clear(executor);
    if (status == HL_NATIVE_OK && executor->memory_mode != 0) {
        hl_native_cache_stats identity;
        hl_native_cache_diagnose(executor->cache, &identity);
        status = hl_native_cache_reset(executor->cache, identity.mapping_epoch);
        executor->memory_mode = 0;
        executor->authority_generation = 0;
    }
    /* Neither parent nor child may reuse an authority across the host-fork
     * boundary. Each process retires its COW copy through unregister. */
    executor->direct_authority = NULL;
    executor->retained_authority_valid = 0;
    executor->retained_authority_identity = 0;
    (void)direct_generation(executor);
    atomic_store_explicit(&executor->admission, ADMISSION(MUTATION_OPEN, 0), memory_order_release);
    atomic_store_explicit(&executor->fork_runs, 0, memory_order_release);
    return status;
}

hl_native_status hl_native_diagnose(const hl_native_executor *executor, hl_native_diagnostics *output) {
    const hl_native_arena *arena;
    hl_native_cache_stats stats;
    if (executor == NULL || output == NULL || output->abi != HL_NATIVE_ABI ||
        output->size < offsetof(hl_native_diagnostics, x86_public_exits))
        return HL_NATIVE_ARGUMENT;
    arena = &executor->arena;
    output->capacity = arena->mapping.capacity;
    output->used = arena->mapping.content;
    output->publications = arena->publications;
    output->write_transitions = arena->write_transitions;
    output->dual_alias = arena->mapping.writable != arena->mapping.executable;
    output->writing = arena->writing;
    hl_native_cache_diagnose(executor->cache, &stats);
    output->cache_lookups = stats.lookups;
    output->cache_hits = stats.hits;
    output->cache_misses = stats.misses;
    output->epoch_rejections = stats.epoch_rejections;
    output->invalidations = stats.invalidations;
    output->live_blocks = stats.live_blocks;
    output->cache_generation = stats.generation;
    output->mapping_epoch = stats.mapping_epoch;
    output->ibtc_fills = executor->ibtc_fills;
    output->ibtc_site_collisions = executor->ibtc_site_collisions;
    output->ibtc_shared_collisions = executor->ibtc_shared_collisions;
    output->boundary_branch = executor->boundary_branch;
    output->boundary_syscall = executor->boundary_syscall;
    output->boundary_fallback = executor->boundary_fallback;
    output->boundary_yield = executor->boundary_yield;
    output->completed = executor->completed;
    output->operand_callbacks = executor->operand_callbacks;
    output->operand_cache_hits = executor->operand_cache_hits;
    if (output->size >= offsetof(hl_native_diagnostics, x86_public_syscalls)) {
        output->x86_public_exits = executor->x86_public_exits;
    }
    if (output->size >= offsetof(hl_native_diagnostics, x86_syscall_vector_dirty)) {
        output->x86_public_syscalls = executor->x86_public_syscalls;
    }
    if (output->size >= offsetof(hl_native_diagnostics, a64_guard_fast)) {
        output->x86_syscall_vector_dirty = executor->x86_syscall_vector_dirty;
    }
    if (output->size >= offsetof(hl_native_diagnostics, relocation_cold_targets)) {
        output->a64_guard_fast = atomic_load_explicit(&executor->a64_guard_fast, memory_order_relaxed);
        output->a64_guard_full = atomic_load_explicit(&executor->a64_guard_full, memory_order_relaxed);
        output->a64_guard_fallback = atomic_load_explicit(&executor->a64_guard_fallback, memory_order_relaxed);
        output->a64_dirty_reserved = atomic_load_explicit(&executor->a64_dirty_reserved, memory_order_relaxed);
        output->a64_dirty_overflow = atomic_load_explicit(&executor->a64_dirty_overflow, memory_order_relaxed);
        output->a64_dirty_committed = atomic_load_explicit(&executor->a64_dirty_committed, memory_order_relaxed);
        output->a64_dirty_merged = atomic_load_explicit(&executor->a64_dirty_merged, memory_order_relaxed);
        output->x86_cold_builds = atomic_load_explicit(&executor->x86_cold_builds, memory_order_relaxed);
        output->x86_cold_quota_exits = atomic_load_explicit(&executor->x86_cold_quota_exits, memory_order_relaxed);
    }
    if (output->size >= offsetof(hl_native_diagnostics, a64_fallback_guard_read)) {
        output->relocation_cold_targets = atomic_load_explicit(&executor->relocation_cold_targets, memory_order_relaxed);
        output->relocation_cycles = atomic_load_explicit(&executor->relocation_cycles, memory_order_relaxed);
        output->relocation_capacity = atomic_load_explicit(&executor->relocation_capacity, memory_order_relaxed);
        output->relocation_invalidations = atomic_load_explicit(&executor->relocation_invalidations, memory_order_relaxed);
        output->ibtc_site_misses = atomic_load_explicit(&executor->ibtc_site_misses, memory_order_relaxed);
        output->ibtc_shared_misses = atomic_load_explicit(&executor->ibtc_shared_misses, memory_order_relaxed);
    }
    if (output->size >= offsetof(hl_native_diagnostics, a64_fallback_entry_rejection)) {
        output->a64_fallback_guard_read = atomic_load_explicit(&executor->a64_fallback_guard_read, memory_order_relaxed);
        output->a64_fallback_guard_write = atomic_load_explicit(&executor->a64_fallback_guard_write, memory_order_relaxed);
        output->a64_fallback_simd_fp = atomic_load_explicit(&executor->a64_fallback_simd_fp, memory_order_relaxed);
        output->a64_fallback_memory = atomic_load_explicit(&executor->a64_fallback_memory, memory_order_relaxed);
        output->a64_fallback_control = atomic_load_explicit(&executor->a64_fallback_control, memory_order_relaxed);
        output->a64_fallback_other = atomic_load_explicit(&executor->a64_fallback_other, memory_order_relaxed);
    }
    if (output->size >= offsetof(hl_native_diagnostics, x86_public_epochs)) {
        output->a64_fallback_entry_rejection = atomic_load_explicit(&executor->a64_fallback_entry_rejection, memory_order_relaxed);
        output->a64_fallback_generated = atomic_load_explicit(&executor->a64_fallback_generated, memory_order_relaxed);
        output->a64_fallback_call = atomic_load_explicit(&executor->a64_fallback_call, memory_order_relaxed);
        output->a64_fallback_return = atomic_load_explicit(&executor->a64_fallback_return, memory_order_relaxed);
        output->a64_fallback_indirect = atomic_load_explicit(&executor->a64_fallback_indirect, memory_order_relaxed);
        output->a64_fallback_system = atomic_load_explicit(&executor->a64_fallback_system, memory_order_relaxed);
        output->a64_fallback_form_memory = atomic_load_explicit(&executor->a64_fallback_form_memory, memory_order_relaxed);
        output->a64_fallback_form_other = atomic_load_explicit(&executor->a64_fallback_form_other, memory_order_relaxed);
    }
    if (output->size >= offsetof(hl_native_diagnostics, a64_branch_exhaustion)) {
        output->x86_public_epochs = executor->x86_public_epochs;
    }
    if (output->size >= offsetof(hl_native_diagnostics, ibtc_authenticated_entries)) {
        output->a64_branch_exhaustion = atomic_load_explicit(&executor->a64_branch_exhaustion, memory_order_relaxed);
        output->a64_branch_cold_relocation = atomic_load_explicit(&executor->a64_branch_cold_relocation, memory_order_relaxed);
        output->a64_branch_nonrelocatable = atomic_load_explicit(&executor->a64_branch_nonrelocatable, memory_order_relaxed);
        output->a64_branch_unidentified = atomic_load_explicit(&executor->a64_branch_unidentified, memory_order_relaxed);
        uint64_t sample_form = atomic_load_explicit(&executor->a64_branch_sample_form, memory_order_acquire);
        if (sample_form != 0) {
            output->a64_branch_sample_pc = executor->a64_branch_sample_pc;
            output->a64_branch_sample_source_first = executor->a64_branch_sample_source_first;
            output->a64_branch_sample_source_last = executor->a64_branch_sample_source_last;
        }
        output->a64_branch_sample_form = sample_form;
    }
    if (output->size >= sizeof(*output)) {
        output->ibtc_authenticated_entries = 0;
        output->ibtc_shared_hits = 0;
        output->ibtc_auth_rejections = 0;
    }
    return HL_NATIVE_OK;
}

hl_native_status hl_native_changed(hl_native_executor *executor, const hl_native_change *changes, size_t count) {
    if (executor == NULL || changes == NULL || count == 0 || count > 1024) return HL_NATIVE_ARGUMENT;
    int replaced = 0;
    for (size_t index = 0; index < count; index++) {
        const hl_native_change *change = &changes[index];
        if (change->abi != HL_NATIVE_ABI || change->size < sizeof(*change) || change->reserved != 0 ||
            (change->kind != HL_NATIVE_REPLACE && change->kind != HL_NATIVE_INVALIDATE) ||
            (change->kind == HL_NATIVE_INVALIDATE && change->last <= change->first) ||
            (change->kind == HL_NATIVE_REPLACE && (change->first != 0 || change->last != 0)))
            return HL_NATIVE_ARGUMENT;
    }
    hl_native_status status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    int writing = 0;
    status = hl_native_cache_write_begin(executor->cache);
    if (status == HL_NATIVE_OK) writing = 1;
    for (size_t index = 0; index < count; index++) {
        if (status != HL_NATIVE_OK) break;
        const hl_native_change *change = &changes[index];
        if (change->kind == HL_NATIVE_REPLACE) {
            status = hl_native_cache_reset(executor->cache, change->mapping_epoch);
            replaced = 1;
        } else {
            status = (change->mapping_epoch == 0 ||
                      hl_native_cache_address_identity_matches(executor->cache,
                                                               change->mapping_epoch,
                                                               executor->memory_mode,
                                                               executor->authority_generation))
                ? hl_native_cache_relocations_invalidate(executor->cache, change->first, change->last)
                : HL_NATIVE_STATE;
            if (status == HL_NATIVE_OK)
                status = hl_native_cache_invalidate(executor->cache, change->first, change->last, NULL);
        }
    }
    if (writing) {
        hl_native_status end = hl_native_cache_write_end(executor->cache);
        if (status == HL_NATIVE_OK) status = end;
    }
    if (status == HL_NATIVE_OK) {
        if (replaced)
            ibtc_clear(executor);
        else
            for (size_t index = 0; index < count; ++index)
                ibtc_invalidate(executor, changes[index].first, changes[index].last);
        if (replaced) {
            executor->memory_mode = 0;
            executor->authority_generation = 0;
            executor->retained_authority_valid = 0;
            executor->retained_authority_identity = 0;
        }
    }
    hl_native_executor_gate_leave(executor);
    return status;
}

hl_native_status hl_native_resolve_fault(const hl_native_executor *executor, hl_native_fault *fault) {
    uint64_t guest_pc;
    if (executor == NULL || fault == NULL || fault->abi != HL_NATIVE_ABI || fault->size < sizeof(*fault) ||
        fault->access > HL_NATIVE_ACCESS_EXECUTE)
        return HL_NATIVE_ARGUMENT;
    fault->precise = hl_native_fault_guest(executor->cache, fault->host_pc, &guest_pc);
    fault->guest_pc = fault->precise ? guest_pc : 0;
    return HL_NATIVE_OK;
}

static hl_native_status run_exit(hl_native_execution *execution, hl_native_exit *output,
                                 uint32_t kind, uint64_t instruction) {
    return hl_native_execution_exit(execution, output, kind, HL_NATIVE_ACCESS_UNKNOWN,
                                    instruction, instruction, 0, 0);
}

#if defined(__aarch64__)
/* Every invariant reports its own code; a shared code cannot be classified. */
enum a64_fatal_code {
    A64_FATAL_QUANTUM_GRANT = 1,
    A64_FATAL_ENTER_BUDGET = 2,
    A64_FATAL_EXECUTION_IDENTITY = 3,
    A64_FATAL_OPERAND_IDENTITY = 4,
    A64_FATAL_OPERAND_COMPLETED = 5,
    A64_FATAL_OPERAND_CHARGED = 6,
    A64_FATAL_OPERAND_REFUND = 7,
    A64_FATAL_LOOP_BUDGET = 8,
};

static hl_native_status run_fatal(hl_native_execution *execution, hl_native_exit *output, uint64_t code) {
    return hl_native_execution_exit(execution, output, HL_NATIVE_EXIT_FATAL, HL_NATIVE_ACCESS_UNKNOWN,
                                    0, 0, 0, code == 0 ? 1 : code);
}
#endif

hl_native_status hl_native_synchronize_epoch(hl_native_executor *executor, uint64_t mapping_epoch,
                                             uint64_t instruction_epoch, uint64_t memory_mode,
                                             uint64_t authority_generation) {
    hl_native_status status;
    if (hl_native_cache_epoch_matches(executor->cache, mapping_epoch, instruction_epoch,
                                      memory_mode, authority_generation))
        return HL_NATIVE_OK;
    status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    if (!hl_native_cache_epoch_matches(executor->cache, mapping_epoch, instruction_epoch,
                                       memory_mode, authority_generation))
        status = hl_native_cache_reset_identity(executor->cache, mapping_epoch, instruction_epoch,
                                                memory_mode, authority_generation);
    if (status == HL_NATIVE_OK) ibtc_clear(executor);
    if (status == HL_NATIVE_OK) {
        executor->memory_mode = memory_mode;
        executor->authority_generation = authority_generation;
    }
    hl_native_executor_gate_leave(executor);
    return status;
}

hl_native_status hl_native_synchronize_direct(hl_native_executor *executor, uint64_t mapping_epoch,
                                              uint64_t instruction_epoch,
                                              const hl_native_direct_token *token, uint64_t generation,
                                              uint64_t identity,
                                              const hl_native_projection *projection) {
    hl_native_status status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    if (!hl_native_direct_request_valid(executor, token, generation, identity, projection)) {
        status = HL_NATIVE_STATE;
    } else if (!hl_native_cache_epoch_matches(executor->cache, mapping_epoch, instruction_epoch, 1, identity)) {
        status = hl_native_cache_reset_identity(executor->cache, mapping_epoch, instruction_epoch, 1, identity);
        if (status == HL_NATIVE_OK) ibtc_clear(executor);
    }
    if (status == HL_NATIVE_OK) {
        executor->memory_mode = 1;
        executor->authority_generation = identity;
    }
    hl_native_executor_gate_leave(executor);
    return status;
}

hl_native_status hl_native_executor_rollover(hl_native_executor *executor, uint64_t mapping_epoch,
                                              uint64_t instruction_epoch) {
    hl_native_status status;
    if (executor == NULL || atomic_load_explicit(&executor->admission, memory_order_acquire) !=
                                ADMISSION(MUTATION_ACTIVE, 0))
        return HL_NATIVE_STATE;
    status = hl_native_cache_reset_identity(executor->cache, mapping_epoch, instruction_epoch,
                                            executor->memory_mode,
                                            executor->authority_generation);
    if (status != HL_NATIVE_OK) return status;
    ibtc_clear(executor);
    status = hl_native_arena_rotate(&executor->arena);
    return status;
}

#if defined(__aarch64__)
#ifndef HL_A64_RUN_VIEW_CACHE
#define HL_A64_RUN_VIEW_CACHE 1
#endif
/* A retry that completes no instruction refunds its whole charge, so an operand
 * the guard never accepts would otherwise spin without consuming budget. */
#define HL_A64_OPERAND_RETRY_LIMIT 8u

static int run_view_contains(const hl_a64_view *view, uint64_t address,
                             uint64_t size, uint32_t required) {
    return view != NULL && size != 0 && address <= UINT64_MAX - size &&
           address >= view->guest_first && address + size <= view->guest_last &&
           (view->permissions & required) == required;
}

static void run_view_promote(hl_a64_run_views *cache, size_t index) {
    hl_a64_view selected;
    if (cache == NULL || index >= cache->count || index == 0) return;
    selected = cache->entries[index];
    memmove(&cache->entries[1], &cache->entries[0], index * sizeof(cache->entries[0]));
    cache->entries[0] = selected;
}

static int run_view_resolve(hl_a64_run_views *cache, hl_native_aarch64_cpu *cpu,
                            uint64_t address, uint64_t size, uint32_t required) {
    size_t index;
    if (cache == NULL) return 0;
    for (index = 0; index < cache->count; index++) {
        hl_a64_projection projection;
        if (!run_view_contains(&cache->entries[index], address, size, required)) continue;
        run_view_promote(cache, index);
        projection = (hl_a64_projection){&cache->entries[0], 1,
                                         cache->entries[0].mapping_incarnation, 0};
        return hl_a64_projection_resolve(&projection, cpu, address, size, required);
    }
    return 0;
}

static void run_view_install(hl_a64_run_views *cache, const hl_a64_view *view) {
    size_t index;
    if (cache == NULL || view == NULL) return;
    for (index = 0; index < cache->count; index++) {
        if (memcmp(&cache->entries[index], view, sizeof(*view)) == 0) {
            run_view_promote(cache, index);
            return;
        }
    }
    if (cache->count < HL_A64_RUN_VIEW_COUNT) cache->count++;
    if (cache->count > 1)
        memmove(&cache->entries[1], &cache->entries[0],
                (cache->count - 1) * sizeof(cache->entries[0]));
    cache->entries[0] = *view;
}

void hl_a64_run_view_publish(const hl_a64_run_views *cache, hl_native_aarch64_cpu *cpu,
                             uint64_t mapping_incarnation) {
    size_t count = cache != NULL ? cache->count : 0;
    size_t published = 0;
    __atomic_store_n(&cpu->read_token, 0, __ATOMIC_RELEASE);
    cpu->read_count = 0;
    /* Retire both halves of every slot, so no republication can leave a dead
     * incarnation's delta above the new count. */
    memset(cpu->read_views, 0, sizeof(cpu->read_views));
    memset(cpu->read_view_publication, 0, sizeof(cpu->read_view_publication));
    if (mapping_incarnation == 0 || count > HL_A64_RUN_VIEW_COUNT) return;
    for (size_t index = 0; index < count; ++index) {
        const hl_a64_view *view = &cache->entries[index];
        if (view->guest_last <= view->guest_first || view->write_policy != HL_NATIVE_WRITE_EXACT ||
            view->mapping_incarnation != mapping_incarnation || view->permissions == 0 ||
            (view->permissions & ~7u) != 0 ||
            view->host_first > UINT64_MAX - (view->guest_last - view->guest_first))
            continue;
        cpu->read_views[published][0] = view->guest_first;
        cpu->read_views[published][1] = view->guest_last;
        cpu->read_views[published][2] = view->host_first - view->guest_first;
        cpu->read_views[published][3] = view->permissions;
        cpu->read_view_publication[published][0] = view->write_policy;
        cpu->read_view_publication[published][1] = view->write_index;
        published++;
    }
    if (published == 0) return;
    cpu->read_count = published;
    cpu->read_incarnation = mapping_incarnation;
    __atomic_store_n(&cpu->read_token, mapping_incarnation, __ATOMIC_RELEASE);
}

static void active_view_clear(hl_native_aarch64_cpu *cpu) {
    cpu->active_view_incarnation = 0;
    cpu->active_view_authority = 0;
}

static void active_view_publish(hl_native_aarch64_cpu *cpu, uint64_t mapping_incarnation,
                                uint64_t authority) {
    active_view_clear(cpu);
    if (mapping_incarnation == 0 || authority == 0 || cpu->memory_last <= cpu->memory_first ||
        cpu->memory_permissions == 0 || (cpu->memory_permissions & ~UINT64_C(7)) != 0)
        return;
    cpu->active_view_incarnation = mapping_incarnation;
    cpu->active_view_authority = authority;
}

static hl_native_status a64_execution_enter(hl_native_executor *executor,
                                            hl_native_execution *execution,
                                            hl_native_aarch64_cpu *cpu) {
    hl_native_status status = hl_native_execution_enter(executor, execution);
    if (status != HL_NATIVE_OK) return status;
    status = hl_native_execution_bind_certificate(execution, &cpu->certificate_token);
    if (status != HL_NATIVE_OK) (void)hl_native_execution_leave(execution);
    return status;
}

static hl_native_status run_aarch64(hl_native_executor *executor, hl_native_cpu *cpu_handle,
                                    hl_native_aarch64_cpu *cpu,
                                    const hl_native_run_request *request, hl_native_exit *output) {
    const hl_a64_source *source = request->source;
    const hl_a64_source *active_source = source;
    hl_a64_source_span resolved_span;
    hl_a64_source resolved_source;
    const hl_a64_projection *projection = request->projection;
    uint8_t scratch[HL_A64_TRACE_MAX_BYTES];
    uint64_t budget = request->budget;
    uint64_t cumulative_budget = request->budget;
    hl_native_quantum_poll quantum_poll = NULL;
    void *quantum_context = NULL;
    uint64_t quantum_grant = 0;
    int translated = 0;
    hl_native_execution execution = {0};
    hl_native_source_resolve resolver = NULL;
    void *resolver_context = NULL;
    hl_native_operand_resolve operand_resolver = NULL;
    void *operand_context = NULL;
    hl_a64_run_views operand_views = {0};
    uint64_t operand_retry_pc = 0;
    unsigned operand_retry_count = 0;
    hl_native_fault_publish fault_publish = NULL;
    hl_native_fault_unpublish fault_unpublish = NULL;
    void *fault_context = NULL;
    uint64_t memory_mode = 0;
    uint64_t authority_generation = 0;
    uint64_t authority_identity = 0;
    uint64_t expected_authority = 0;
    const hl_native_direct_token *direct_token = NULL;
    hl_native_direct_authority direct_authority = {0};
    cpu->read_token = 0;
    cpu->read_count = 0;
    memset(cpu->read_views, 0, sizeof(cpu->read_views));
    memset(cpu->read_view_publication, 0, sizeof(cpu->read_view_publication));
    cpu->memory_write_policy = 0;
    cpu->memory_write_index = 0;
    cpu->active_authority = 0;
    active_view_clear(cpu);
    cpu->loop_valid = 0;
    cpu->loop_view_count = 0;
    memset(cpu->loop_views, 0, sizeof(cpu->loop_views));
    cpu->loop_mapping_incarnation = 0;
    cpu->loop_authority = 0;
    cpu->loop_trip = 0;
    cpu->loop_decrement = 0;
    cpu->loop_instruction_count = 0;
    cpu->loop_iterations = 0;
    cpu->loop_budget_iterations = 0;
    cpu->loop_executable = 0;
    if (request->size >= offsetof(hl_native_run_request, operand_context)) {
        resolver = request->source_resolve;
        resolver_context = request->source_context;
    }
    if (request->size >= offsetof(hl_native_run_request, memory_mode)) {
        operand_resolver = request->operand_resolve;
        operand_context = request->operand_context;
        fault_publish = request->fault_publish;
        fault_unpublish = request->fault_unpublish;
        fault_context = request->fault_context;
    }
    if (request->size >= offsetof(hl_native_run_request, direct_token)) {
        memory_mode = request->memory_mode;
        authority_generation = request->authority_generation;
    }
    if (request->size >= offsetof(hl_native_run_request, authority_identity))
        direct_token = request->direct_token;
    if (request->size >= offsetof(hl_native_run_request, certificate))
        authority_identity = request->authority_identity;
    if (request->size >= offsetof(hl_native_run_request, quantum_grant)) {
        quantum_context = request->quantum_context;
        quantum_poll = request->quantum_poll;
    }
    if (request->size >= sizeof(*request)) quantum_grant = request->quantum_grant;
    if ((quantum_poll == NULL) != (quantum_grant == 0)) return HL_NATIVE_ARGUMENT;
    if ((fault_publish == NULL) != (fault_unpublish == NULL)) return HL_NATIVE_ARGUMENT;
    if (source == NULL || !hl_a64_source_validate(source) ||
        source->mapping_incarnation != request->mapping_epoch ||
        (projection != NULL && (!hl_a64_projection_validate(projection) ||
                                projection->mapping_incarnation != request->mapping_epoch)))
        return HL_NATIVE_ARGUMENT;
    if ((memory_mode == 0) != (authority_generation == 0) ||
        (memory_mode == 0) != (authority_identity == 0)) return HL_NATIVE_ARGUMENT;
    expected_authority = memory_mode != 0 ? authority_identity : request->mapping_epoch;
    if (expected_authority == 0) return HL_NATIVE_ARGUMENT;
    hl_native_status epoch_status = memory_mode == 0
        ? hl_native_synchronize_epoch(executor, source->mapping_incarnation, 0, 0, expected_authority)
        : hl_native_synchronize_direct(executor, source->mapping_incarnation, 0,
                                       direct_token, authority_generation, authority_identity, projection);
    if (epoch_status != HL_NATIVE_OK) return epoch_status;
    if (projection != NULL) {
        const hl_a64_view *view = &projection->views[projection->active];
        if (!hl_a64_projection_resolve(projection, cpu, view->guest_first,
                                       view->guest_last - view->guest_first, view->permissions))
            return HL_NATIVE_ARGUMENT;
        if (HL_A64_RUN_VIEW_CACHE)
            for (size_t index = projection->count;
                 index > 0 && operand_views.count < HL_A64_RUN_VIEW_COUNT; index--)
                run_view_install(&operand_views, &projection->views[index - 1]);
    }
    hl_a64_run_view_publish(&operand_views, cpu, request->mapping_epoch);
    cpu->budget = budget;
    cpu->executed = 0;
    for (;;) {
        hl_native_code code;
        hl_native_code executed_code;
        hl_native_translation_key key;
        size_t count;
        uint32_t hit;
        hl_native_status status;
        uint64_t instruction = cpu->program;
        if (execution.owner == NULL) {
            status = a64_execution_enter(executor, &execution, cpu);
            if (status != HL_NATIVE_OK) return status;
            if (memory_mode != 0 && !hl_native_direct_request_snapshot(
                    executor, direct_token, authority_generation, authority_identity, projection,
                    &direct_authority)) {
                (void)hl_native_execution_leave(&execution);
                return HL_NATIVE_STATE;
            }
        }
        if (cpu->interrupt != 0)
            return run_exit(&execution, output, HL_NATIVE_EXIT_INTERRUPT, instruction);
        if (budget == 0) {
            /* The spilled loop head is the same architecturally precise point a
             * yield would return from, so a still-current grant extends here. */
            if (quantum_poll == NULL || !quantum_poll(quantum_context, cpu->executed, cumulative_budget))
                return run_exit(&execution, output, HL_NATIVE_EXIT_YIELD, instruction);
            if (cumulative_budget > UINT64_MAX - quantum_grant || cpu->executed > cumulative_budget)
                return run_fatal(&execution, output, A64_FATAL_QUANTUM_GRANT);
            cumulative_budget += quantum_grant;
            budget = quantum_grant;
            cpu->budget = budget;
        }
        size_t limit = budget < HL_A64_SOURCE_MAX_WORDS ? (size_t)budget : HL_A64_SOURCE_MAX_WORDS;
        if (limit > HL_A64_TRACE_MAX_WORDS) limit = HL_A64_TRACE_MAX_WORDS;
        count = hl_a64_source_available(active_source, instruction, limit);
        if (count == 0 && translated && resolver != NULL) {
            memset(&resolved_span, 0, sizeof(resolved_span));
            if (resolver(resolver_context, instruction, source->mapping_incarnation,
                         source->instruction_epoch, &resolved_span)) {
                resolved_source = (hl_a64_source){&resolved_span, 1,
                                                  source->mapping_incarnation,
                                                  resolved_span.instruction_epoch};
                if (!hl_a64_source_validate(&resolved_source)) return HL_NATIVE_ARGUMENT;
                active_source = &resolved_source;
                count = hl_a64_source_available(active_source, instruction, limit);
            }
        }
        if (count == 0) {
            return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, instruction);
        }
        key = (hl_native_translation_key){
            .guest = instruction,
            .mapping_incarnation = active_source->mapping_incarnation,
            .instruction_epoch = active_source->instruction_epoch,
            .source_first = instruction,
            .source_last = instruction + count * 4,
            .memory_mode = memory_mode,
            .authority_generation = expected_authority,
            .architecture = HL_NATIVE_AARCH64,
            .direct_token = (uint64_t)(uintptr_t)direct_token,
            .direct_generation = authority_generation,
        };
        if (hl_native_translation_lookup(executor, &key, &code) != HL_NATIVE_HIT ||
            code.decoded_count > count) {
            status = hl_native_execution_leave(&execution);
            if (status != HL_NATIVE_OK) return status;
            const hl_a64_source *build_source = active_source;
            hl_a64_source_span region_spans[HL_A64_SOURCE_MAX_SPANS];
            hl_a64_source region_source;
            size_t build_count = count;
            hl_a64_fetch_result tail;
            /* The resolver refills one borrowed buffer, so a resolved primary
             * span's bytes die the moment the successor is resolved. */
            if (resolver != NULL && active_source == source && count < limit &&
                active_source->span_count < HL_A64_SOURCE_MAX_SPANS &&
                hl_a64_source_fetch(active_source, instruction + (count - 1) * 4, 1, &tail) &&
                (tail.words[0] & UINT32_C(0xfc000000)) == UINT32_C(0x14000000)) {
                int64_t displacement = (int64_t)((uint64_t)(tail.words[0] & UINT32_C(0x03ffffff)) << 38) >> 36;
                uint64_t branch_pc = instruction + (count - 1) * 4;
                uint64_t magnitude = displacement < 0 ? UINT64_C(0) - (uint64_t)displacement
                                                      : (uint64_t)displacement;
                int target_valid = displacement < 0 ? magnitude <= branch_pc
                                                    : magnitude <= UINT64_MAX - branch_pc;
                uint64_t target = displacement < 0 ? branch_pc - magnitude : branch_pc + magnitude;
                hl_native_source_span successor = {0};
                if (target_valid && target != instruction && resolver(resolver_context, target, source->mapping_incarnation,
                        source->instruction_epoch, &successor)) {
                    size_t insert = 0;
                    for (size_t index = 0; index < active_source->span_count; ++index)
                        region_spans[index] = active_source->spans[index];
                    while (insert < active_source->span_count &&
                           region_spans[insert].guest_first < successor.guest_first) insert++;
                    memmove(&region_spans[insert + 1], &region_spans[insert],
                            (active_source->span_count - insert) * sizeof(region_spans[0]));
                    region_spans[insert] = successor;
                    region_source = (hl_a64_source){region_spans, active_source->span_count + 1,
                        source->mapping_incarnation, source->instruction_epoch};
                    if (hl_a64_source_validate(&region_source)) {
                        size_t successor_count = hl_a64_source_available(&region_source, target, limit - count);
                        if (successor_count != 0) {
                            build_source = &region_source;
                            build_count += successor_count;
                        }
                    }
                }
            }
            status = hl_a64_trace_cache_direct(executor, build_source, instruction, build_count, scratch,
                                               sizeof(scratch), memory_mode != 0 ? &direct_authority : NULL,
                                               expected_authority,
                                               &code, &hit);
            count = build_count;
            if (status != HL_NATIVE_OK) {
                if (executor->diagnostics) {
                    hl_a64_fetch_result declined;
                    if (hl_a64_source_fetch(active_source, instruction, 1, &declined))
                        a64_fallback_word(executor, declined.words[0], 1);
                }
                status = a64_execution_enter(executor, &execution, cpu);
                if (status != HL_NATIVE_OK) return status;
                return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, instruction);
            }
            status = a64_execution_enter(executor, &execution, cpu);
            if (status != HL_NATIVE_OK) return status;
            /* Publication and reset both require exclusive mutation admission.
             * Resolve only after re-admission, so no pointer selected before
             * that boundary can survive an epoch change or fork repair. */
            if (hl_native_translation_lookup(executor, &key, &code) != HL_NATIVE_HIT ||
                code.decoded_count > count)
                return run_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, instruction);
        }
        if (cpu->indirect_site != 0) {
            status = hl_native_execution_leave(&execution);
            if (status != HL_NATIVE_OK) return status;
            status = ibtc_fill(executor, cpu, instruction, &code);
            if (status != HL_NATIVE_OK) return status;
            status = a64_execution_enter(executor, &execution, cpu);
            if (status != HL_NATIVE_OK) return status;
            if (hl_native_translation_lookup(executor, &key, &code) != HL_NATIVE_HIT ||
                code.decoded_count > count)
                return run_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, instruction);
        }
        uint64_t executed_before = cpu->executed;
        hl_native_fault_scope fault_scope = {
            .abi = HL_NATIVE_ABI,
            .size = sizeof(fault_scope),
            .architecture = HL_NATIVE_AARCH64,
            .reserved = 1,
            .executor = executor,
            .cpu = cpu_handle,
        };
        /* This value comes from the independently authenticated run request,
         * never from the translation key baked into generated code.  The
         * execution gate keeps it current until the fully spilled return. */
        cpu->active_authority = memory_mode != 0 ? authority_identity : request->mapping_epoch;
        active_view_publish(cpu, request->mapping_epoch, expected_authority);
        if (fault_publish != NULL && fault_publish(fault_context, &fault_scope) != HL_NATIVE_OK) {
            cpu->active_authority = 0;
            active_view_clear(cpu);
            status = hl_native_execution_leave(&execution);
            return status == HL_NATIVE_OK ? HL_NATIVE_STATE : status;
        }
        cpu->diagnostic_guard_fast = cpu->diagnostic_guard_full = cpu->diagnostic_guard_fallback = 0;
        cpu->diagnostic_dirty_reserved = cpu->diagnostic_dirty_overflow = cpu->diagnostic_dirty_committed = 0;
        cpu->diagnostic_dirty_merged = 0;
        hl_native_aarch64_enter(cpu, code.entry);
        if (fault_unpublish != NULL) fault_unpublish(fault_context, &fault_scope);
        cpu->active_authority = 0;
        active_view_clear(cpu);
        if (cpu->budget > cumulative_budget) return run_fatal(&execution, output, A64_FATAL_ENTER_BUDGET);
        cpu->executed = cumulative_budget - cpu->budget;
        if (executor->diagnostics) {
            atomic_fetch_add_explicit(&executor->a64_guard_fast, cpu->diagnostic_guard_fast, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_guard_full, cpu->diagnostic_guard_full, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_guard_fallback, cpu->diagnostic_guard_fallback, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_dirty_reserved, cpu->diagnostic_dirty_reserved, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_dirty_overflow, cpu->diagnostic_dirty_overflow, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_dirty_committed, cpu->diagnostic_dirty_committed, memory_order_relaxed);
            atomic_fetch_add_explicit(&executor->a64_dirty_merged, cpu->diagnostic_dirty_merged, memory_order_relaxed);
            cpu->diagnostic_guard_fast = cpu->diagnostic_guard_full = cpu->diagnostic_guard_fallback = 0;
            cpu->diagnostic_dirty_reserved = cpu->diagnostic_dirty_overflow = cpu->diagnostic_dirty_committed = 0;
            cpu->diagnostic_dirty_merged = 0;
        }
        int executed_identity = 0;
        if ((cpu->execution_identity & 3u) == 1u) {
            if (!hl_native_cache_execution(executor->cache, cpu->execution_identity, &executed_code))
                return run_fatal(&execution, output, A64_FATAL_EXECUTION_IDENTITY);
            cpu->execution_identity = 0;
            executed_identity = 1;
        }
        if (executor->diagnostics) {
            executor->completed += cpu->executed - executed_before;
            switch (cpu->reason) {
                case HL_NATIVE_EXIT_BRANCH: {
                    uint64_t form;
                    executor->boundary_branch++;
                    if (!executed_identity) {
                        atomic_fetch_add_explicit(&executor->a64_branch_unidentified, 1, memory_order_relaxed);
                        form = HL_NATIVE_A64_BRANCH_FORM_UNIDENTIFIED;
                    } else if (executed_code.relocation_count != 0) {
                        atomic_fetch_add_explicit(&executor->a64_branch_cold_relocation, 1, memory_order_relaxed);
                        form = HL_NATIVE_A64_BRANCH_FORM_COLD_RELOCATION;
                    } else if (cpu->program == executed_code.source_last) {
                        atomic_fetch_add_explicit(&executor->a64_branch_exhaustion, 1, memory_order_relaxed);
                        form = HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION;
                    } else {
                        atomic_fetch_add_explicit(&executor->a64_branch_nonrelocatable, 1, memory_order_relaxed);
                        form = HL_NATIVE_A64_BRANCH_FORM_NONRELOCATABLE;
                    }
                    /* This tuple is a lifetime latch, not pending-relocation
                     * state. Resolution may change later exits, but never
                     * rewrites the first observed return. */
                    uint64_t unclaimed = 0;
                    if (atomic_compare_exchange_strong_explicit(&executor->a64_branch_sample_claim,
                            &unclaimed, 1, memory_order_relaxed, memory_order_relaxed)) {
                        executor->a64_branch_sample_pc = cpu->program;
                        executor->a64_branch_sample_source_first =
                            executed_identity ? executed_code.source_first : instruction;
                        executor->a64_branch_sample_source_last =
                            executed_identity ? executed_code.source_last : instruction;
                        atomic_store_explicit(&executor->a64_branch_sample_form, form, memory_order_release);
                    }
                    break;
                }
                case HL_NATIVE_EXIT_SYSCALL: executor->boundary_syscall++; break;
                case HL_NATIVE_EXIT_FALLBACK: executor->boundary_fallback++; break;
                case HL_NATIVE_EXIT_YIELD: executor->boundary_yield++; break;
                default: break;
            }
        }
        translated = 1;
        if ((cpu->executable_written & 4u) != 0)
            return run_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, cpu->program);
        if (cpu->reason == HL_NATIVE_EXIT_FALLBACK && cpu->fault_access != 0 && cpu->fault_size != 0) {
            if (executor->diagnostics) {
                _Atomic uint64_t *counter = cpu->fault_access == HL_NATIVE_ACCESS_WRITE
                    ? &executor->a64_fallback_guard_write
                    : &executor->a64_fallback_guard_read;
                atomic_fetch_add_explicit(counter, 1, memory_order_relaxed);
                atomic_fetch_add_explicit(&executor->a64_fallback_generated, 1, memory_order_relaxed);
                atomic_fetch_add_explicit(&executor->a64_fallback_form_memory, 1, memory_order_relaxed);
            }
        }
        if (cpu->reason == HL_NATIVE_EXIT_FALLBACK && cpu->fault_access != 0 &&
            cpu->fault_size != 0 && operand_resolver != NULL) {
            uint64_t completed;
            hl_a64_view view = {0};
            hl_a64_projection resolved_projection;
            uint32_t result;
            if (!executed_identity) {
                return run_fatal(&execution, output, A64_FATAL_OPERAND_IDENTITY);
            }
            completed = cpu->fault_completed;
            if (completed != 0 || cpu->program != operand_retry_pc) {
                operand_retry_pc = cpu->program;
                operand_retry_count = 0;
            }
            if (++operand_retry_count > HL_A64_OPERAND_RETRY_LIMIT)
                return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, cpu->program);
            if (completed > budget) return run_fatal(&execution, output, A64_FATAL_OPERAND_COMPLETED);
            uint64_t charged = executed_code.instruction_count;
            if (charged < completed || cpu->executed < charged)
                return run_fatal(&execution, output, A64_FATAL_OPERAND_CHARGED);
            if (cpu->budget > cumulative_budget - (charged - completed))
                return run_fatal(&execution, output, A64_FATAL_OPERAND_REFUND);
            uint64_t refund = charged - completed;
            cpu->executed -= refund;
            cpu->budget += refund;
            if (executor->diagnostics) executor->completed -= refund;
            budget = cpu->budget;
            if (HL_A64_RUN_VIEW_CACHE && run_view_resolve(&operand_views, cpu, cpu->fault_address,
                                                         cpu->fault_size, (uint32_t)cpu->fault_access)) {
                if (executor->diagnostics) executor->operand_cache_hits++;
                /* run_view_resolve promoted the selected generation-qualified
                 * view and installed it as the active owner. Publish that same
                 * bounded ordering before retry so generated guards can select
                 * subsequent cached owners without another dispatcher exit. */
                hl_a64_run_view_publish(&operand_views, cpu, source->mapping_incarnation);
                continue;
            }
            status = hl_native_execution_leave(&execution);
            if (status != HL_NATIVE_OK) return status;
            if (cpu->interrupt != 0 || budget == 0) continue;
            if (executor->diagnostics) executor->operand_callbacks++;
            result = operand_resolver(operand_context, cpu->fault_address, cpu->fault_size,
                                      (uint32_t)cpu->fault_access, executed_code.mapping_epoch,
                                      executed_code.instruction_epoch, &view);
            if (result == HL_NATIVE_OPERAND_RESOLVED) {
                resolved_projection = (hl_a64_projection){&view, 1, source->mapping_incarnation, 0};
                if (!hl_a64_projection_validate(&resolved_projection) ||
                    !hl_a64_projection_resolve(&resolved_projection, cpu, cpu->fault_address,
                                               cpu->fault_size, (uint32_t)cpu->fault_access))
                    return HL_NATIVE_ARGUMENT;
                if (HL_A64_RUN_VIEW_CACHE) run_view_install(&operand_views, &view);
                hl_a64_run_view_publish(&operand_views, cpu, source->mapping_incarnation);
                continue;
            }
            status = a64_execution_enter(executor, &execution, cpu);
            if (status != HL_NATIVE_OK) return status;
            if (result == HL_NATIVE_OPERAND_FAULT)
                return hl_native_execution_exit(&execution, output, HL_NATIVE_EXIT_FAULT,
                                                (uint32_t)cpu->fault_access, cpu->program, cpu->program,
                                                cpu->fault_address, 1);
            if (result == HL_NATIVE_OPERAND_EPOCH)
                return run_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, cpu->program);
            if (result != HL_NATIVE_OPERAND_DECLINED) {
                status = hl_native_execution_leave(&execution);
                return status == HL_NATIVE_OK ? HL_NATIVE_ARGUMENT : status;
            }
            return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, cpu->program);
        }
        if (cpu->budget > budget || cpu->executed > cumulative_budget)
            return run_fatal(&execution, output, A64_FATAL_LOOP_BUDGET);
        budget = cpu->budget;
        if (cpu->reason == HL_NATIVE_EXIT_BRANCH) {
            continue;
        }
        if (cpu->reason == HL_NATIVE_EXIT_FAULT)
            return hl_native_execution_exit(&execution, output, HL_NATIVE_EXIT_FAULT,
                                            (uint32_t)cpu->fault_access, cpu->program, cpu->program,
                                            cpu->fault_address, 1);
        if (cpu->reason != HL_NATIVE_EXIT_SYSCALL && cpu->reason != HL_NATIVE_EXIT_FALLBACK &&
            cpu->reason != HL_NATIVE_EXIT_INTERRUPT && cpu->reason != HL_NATIVE_EXIT_YIELD)
            return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, instruction);
        if (executor->diagnostics && cpu->reason == HL_NATIVE_EXIT_FALLBACK && cpu->fault_access == 0) {
            hl_a64_fetch_result fallback_word;
            if (hl_a64_source_fetch(source, cpu->program, 1, &fallback_word))
                a64_fallback_word(executor, fallback_word.words[0], 0);
        }
        return run_exit(&execution, output, (uint32_t)cpu->reason, cpu->program);
    }
}
#endif

static hl_native_status run_inner(hl_native_executor *executor, hl_native_cpu *cpu,
                                  const hl_native_run_request *request, hl_native_exit *output) {
    hl_native_execution execution = {0};
    uint64_t instruction;
    volatile uint64_t *interrupt;
    hl_native_status status;

    switch (cpu->architecture) {
        case HL_NATIVE_AARCH64:
            if (cpu->state.aarch64 == NULL) return HL_NATIVE_ARGUMENT;
            cpu->state.aarch64->executable_written = 0;
            /* Authenticated-ingress admission is dormant. Caller storage is
             * untrusted across run, fallback, reset, and fork boundaries, so
             * no preloaded arena pointer or entry identity may survive. */
            cpu->state.aarch64->code_arena_lower = 0;
            cpu->state.aarch64->code_arena_upper = 0;
            cpu->state.aarch64->entry_certificate_identity = 0;
            /* Executor-owned, so a caller-supplied value can never steer a probe. */
            cpu->state.aarch64->ibtc_base = (uint64_t)(uintptr_t)executor->ibtc;
            instruction = cpu->state.aarch64->program;
            interrupt = &cpu->state.aarch64->interrupt;
            break;
        case HL_NATIVE_X86_64:
            if (cpu->state.x86_64 == NULL) return HL_NATIVE_ARGUMENT;
            cpu->state.x86_64->executable_written = 0;
            instruction = cpu->state.x86_64->program;
            interrupt = &cpu->state.x86_64->interrupt;
            break;
        default:
            return HL_NATIVE_ARGUMENT;
    }

#if defined(__aarch64__)
    if (cpu->architecture == HL_NATIVE_X86_64 &&
        request->size >= offsetof(hl_native_run_request, source_context) &&
        request->source != NULL) {
        return hl_native_x86_64_run(executor, cpu->state.x86_64, request, output);
    }
    if (cpu->architecture == HL_NATIVE_AARCH64 &&
        request->size >= offsetof(hl_native_run_request, source_context) &&
        request->source != NULL)
        return run_aarch64(executor, cpu, cpu->state.aarch64, request, output);
#endif

    status = hl_native_execution_enter(executor, &execution);
    if (status != HL_NATIVE_OK) return status;
    uint64_t *certificate_token = cpu->architecture == HL_NATIVE_AARCH64
        ? &cpu->state.aarch64->certificate_token
        : &cpu->state.x86_64->certificate_token;
    status = hl_native_execution_bind_certificate(&execution, certificate_token);
    if (status != HL_NATIVE_OK) {
        (void)hl_native_execution_leave(&execution);
        return status;
    }
    if (request->size >= offsetof(hl_native_run_request, direct_token) &&
        ((request->memory_mode == 0) != (request->authority_generation == 0))) {
        (void)hl_native_execution_leave(&execution);
        return HL_NATIVE_ARGUMENT;
    }
    if (request->size >= offsetof(hl_native_run_request, direct_token) && request->memory_mode != 0) {
        const hl_native_direct_token *token = request->size >= offsetof(hl_native_run_request, authority_identity)
            ? request->direct_token : NULL;
        uint64_t identity = request->size >= offsetof(hl_native_run_request, certificate)
            ? request->authority_identity : 0;
        if (!hl_native_direct_request_valid(executor, token, request->authority_generation,
                                            identity, request->projection)) {
            (void)hl_native_execution_leave(&execution);
            return HL_NATIVE_STATE;
        }
    }
    if (*interrupt != 0)
        return run_exit(&execution, output, HL_NATIVE_EXIT_INTERRUPT, instruction);
    if (request->budget == 0)
        return run_exit(&execution, output, HL_NATIVE_EXIT_YIELD, instruction);

    /* M1 has no production translation service.  Returning fallback before
     * cache lookup is exact: no architectural state or guest PC was changed. */
#if defined(__aarch64__)
    if (cpu->architecture == HL_NATIVE_AARCH64)
        hl_native_aarch64_enter(cpu->state.aarch64, hl_native_aarch64_fallback);
#endif
    return run_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, instruction);
}

hl_native_status hl_native_run(hl_native_executor *executor, hl_native_cpu *cpu,
                               const hl_native_run_request *request, hl_native_exit *output) {
    hl_native_status status;
    if (executor == NULL || cpu == NULL || request == NULL || output == NULL ||
        cpu->abi != HL_NATIVE_ABI || cpu->size < sizeof(*cpu) || cpu->reserved != 0 ||
        request->abi != HL_NATIVE_ABI || request->size < offsetof(hl_native_run_request, source) ||
        request->reserved != 0 || output->abi != HL_NATIVE_ABI || output->size < sizeof(*output) ||
        cpu->architecture != request->architecture ||
        /* Generated admission tests the budget by borrow, which matches an
         * unsigned comparison only while the budget stays below the sign bit. */
        request->budget > (uint64_t)INT64_MAX)
        return HL_NATIVE_ARGUMENT;
    switch (cpu->architecture) {
        case HL_NATIVE_AARCH64:
            if (cpu->state.aarch64 == NULL) return HL_NATIVE_ARGUMENT;
            break;
        case HL_NATIVE_X86_64:
            if (cpu->state.x86_64 == NULL) return HL_NATIVE_ARGUMENT;
            break;
        default:
            return HL_NATIVE_ARGUMENT;
    }
    status = run_lifecycle_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    status = run_inner(executor, cpu, request, output);
    run_lifecycle_leave(executor);
    return status;
}

hl_native_status hl_native_destroy(hl_native_executor *executor) {
    if (executor == NULL) return HL_NATIVE_ARGUMENT;
    /* One nonblocking CAS closes admission only when no execution/fault scope,
     * mutation, or fork lease exists. STATE leaves the live handle untouched
     * and may be retried after the conflicting owner releases its lease. */
    uint64_t expected = ADMISSION(MUTATION_OPEN, 0);
    if (!atomic_compare_exchange_strong_explicit(&executor->admission, &expected,
                                                 ADMISSION(MUTATION_ACTIVE, 0),
                                                 memory_order_acq_rel, memory_order_relaxed))
        return HL_NATIVE_STATE;
    if (executor->direct_authority != NULL) {
        atomic_store_explicit(&executor->admission, ADMISSION(MUTATION_OPEN, 0), memory_order_release);
        return HL_NATIVE_STATE;
    }
    hl_native_cache_destroy(executor->cache);
    hl_native_ibtc_storage_destroy(executor->ibtc);
    hl_native_arena_destroy(&executor->arena);
    free(executor);
    return HL_NATIVE_OK;
}

void hl_native_flush(void *address, size_t size) {
    if (address == NULL || size == 0) return;
    __builtin___clear_cache((char *)address, (char *)address + size);
}
