#ifndef HL_NATIVE_EXECUTOR_H
#define HL_NATIVE_EXECUTOR_H

#include <stddef.h>
#include <stdint.h>

#include "cpu.h"

#define HL_NATIVE_ABI 1u
#define HL_NATIVE_DUAL_PREFERRED 1u
#define HL_NATIVE_DUAL_REQUIRED 2u
#define HL_NATIVE_DIAGNOSTICS 4u
#define HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION UINT64_C(1)
#define HL_NATIVE_A64_BRANCH_FORM_COLD_RELOCATION UINT64_C(2)
#define HL_NATIVE_A64_BRANCH_FORM_NONRELOCATABLE UINT64_C(4)
#define HL_NATIVE_A64_BRANCH_FORM_UNIDENTIFIED UINT64_C(8)
#define HL_NATIVE_WRITE_EXACT 1u

typedef uint64_t hl_native_handle;

typedef enum hl_native_status {
    HL_NATIVE_OK = 0,
    HL_NATIVE_ARGUMENT = 1,
    HL_NATIVE_MEMORY = 2,
    HL_NATIVE_PLATFORM = 3,
    HL_NATIVE_STATE = 4,
    HL_NATIVE_CAPACITY = 5,
} hl_native_status;

typedef struct hl_native_mapping {
    uint32_t abi;
    uint32_t size;
    hl_native_handle handle;
    uint64_t writable;
    uint64_t executable;
    uint64_t capacity;
    uint64_t content;
} hl_native_mapping;

typedef struct hl_native_memory {
    uint32_t abi;
    uint32_t size;
    void *context;
    hl_native_status (*reserve)(void *, uint64_t, uint64_t, uint32_t, hl_native_mapping *);
    hl_native_status (*release)(void *, hl_native_handle);
    hl_native_status (*publish)(void *, hl_native_handle, uint64_t, uint64_t);
    hl_native_status (*repair)(void *, hl_native_mapping *, uint32_t);
    hl_native_status (*write_begin)(void *);
    hl_native_status (*write_end)(void *);
} hl_native_memory;

/* The table is copied during create. Its context and every mapping it returns
 * remain owned by the application and valid until destroy consumes the handle. */

typedef struct hl_native_config {
    uint32_t abi;
    uint32_t size;
    uint64_t capacity;
    uint64_t alignment;
    uint32_t flags;
    uint32_t reserved;
    const hl_native_memory *memory;
} hl_native_config;

typedef struct hl_native_diagnostics {
    uint32_t abi;
    uint32_t size;
    uint64_t capacity;
    uint64_t used;
    uint64_t publications;
    uint64_t write_transitions;
    uint32_t dual_alias;
    uint32_t writing;
    uint64_t cache_lookups;
    uint64_t cache_hits;
    uint64_t cache_misses;
    uint64_t epoch_rejections;
    uint64_t invalidations;
    uint64_t live_blocks;
    uint64_t cache_generation;
    uint64_t mapping_epoch;
    uint64_t ibtc_fills;
    uint64_t ibtc_site_collisions;
    uint64_t ibtc_shared_collisions;
    uint64_t boundary_branch;
    uint64_t boundary_syscall;
    uint64_t boundary_fallback;
    uint64_t boundary_yield;
    uint64_t completed;
    uint64_t operand_callbacks;
    uint64_t operand_cache_hits;
    uint64_t x86_public_exits;
    uint64_t x86_public_syscalls;
    uint64_t x86_syscall_vector_dirty;
    uint64_t a64_guard_fast;
    uint64_t a64_guard_full;
    uint64_t a64_guard_fallback;
    uint64_t a64_dirty_reserved;
    uint64_t a64_dirty_overflow;
    uint64_t a64_dirty_committed;
    uint64_t a64_dirty_merged;
    uint64_t x86_cold_builds;
    uint64_t x86_cold_quota_exits;
    uint64_t relocation_cold_targets;
    uint64_t relocation_cycles;
    uint64_t relocation_capacity;
    uint64_t relocation_invalidations;
    uint64_t ibtc_site_misses;
    uint64_t ibtc_shared_misses;
    uint64_t a64_fallback_guard_read;
    uint64_t a64_fallback_guard_write;
    uint64_t a64_fallback_simd_fp;
    uint64_t a64_fallback_memory;
    uint64_t a64_fallback_control;
    uint64_t a64_fallback_other;
    uint64_t a64_fallback_entry_rejection;
    uint64_t a64_fallback_generated;
    uint64_t a64_fallback_call;
    uint64_t a64_fallback_return;
    uint64_t a64_fallback_indirect;
    uint64_t a64_fallback_system;
    uint64_t a64_fallback_form_memory;
    uint64_t a64_fallback_form_other;
    uint64_t x86_public_epochs;
    uint64_t a64_branch_exhaustion;
    uint64_t a64_branch_cold_relocation;
    uint64_t a64_branch_nonrelocatable;
    uint64_t a64_branch_unidentified;
    /* First observed AArch64 branch return since executor creation. `pc` is
     * the current guest PC at return; source_first/source_last are the
     * half-open interval of the generated block that returned. Form zero
     * means no sample. Reset, invalidation, and fork repair do not relatch it. */
    uint64_t a64_branch_sample_pc;
    uint64_t a64_branch_sample_source_first;
    uint64_t a64_branch_sample_source_last;
    uint64_t a64_branch_sample_form;
    /* Dormant authenticated-ingress diagnostic ABI. These fields remain zero
     * until authenticated publication, consumption, and rejection land as one
     * coherent capability. Local entries are then derived by checked
     * subtraction: authenticated_entries - shared_hits. */
    uint64_t ibtc_authenticated_entries;
    uint64_t ibtc_shared_hits;
    uint64_t ibtc_auth_rejections;
} hl_native_diagnostics;

typedef enum hl_native_change_kind {
    HL_NATIVE_INVALIDATE = 1,
    HL_NATIVE_REPLACE = 2,
} hl_native_change_kind;

typedef struct hl_native_change {
    uint32_t abi;
    uint32_t size;
    uint32_t kind;
    uint32_t reserved;
    uint64_t first;
    uint64_t last;
    uint64_t mapping_epoch;
} hl_native_change;

typedef enum hl_native_access {
    HL_NATIVE_ACCESS_UNKNOWN = 0,
    HL_NATIVE_ACCESS_READ = 1,
    HL_NATIVE_ACCESS_WRITE = 2,
    HL_NATIVE_ACCESS_EXECUTE = 3,
} hl_native_access;

typedef struct hl_native_fault {
    uint32_t abi;
    uint32_t size;
    uint32_t access;
    uint32_t precise;
    uint64_t host_pc;
    uint64_t host_address;
    uint64_t guest_pc;
} hl_native_fault;

#define HL_NATIVE_PROVENANCE_MAX 64u

typedef enum hl_native_address_kind {
    HL_NATIVE_ADDRESS_NONE = 0,
    HL_NATIVE_ADDRESS_CONSTANT = 1,
    HL_NATIVE_ADDRESS_BASE = 2,
    HL_NATIVE_ADDRESS_INDEXED = 3,
} hl_native_address_kind;

typedef enum hl_native_address_extend {
    HL_NATIVE_EXTEND_U64 = 0,
    HL_NATIVE_EXTEND_U32 = 1,
    HL_NATIVE_EXTEND_S32 = 2,
} hl_native_address_extend;

/* Bounded affine effective-address description. Register numbers name the
 * captured host register file, not guest architectural registers. CONSTANT
 * covers literal/RIP-relative forms, BASE covers folded immediates, and
 * INDEXED covers base + extended-index << shift + displacement. */
typedef struct hl_native_address {
    int64_t displacement;
    uint64_t constant;
    uint8_t kind;
    uint8_t bits;
    uint8_t base;
    uint8_t index;
    uint8_t shift;
    uint8_t extend;
    uint16_t reserved;
} hl_native_address;

typedef struct hl_native_provenance {
    uint64_t code_offset;
    uint64_t code_size;
    uint64_t guest;
    hl_native_address address;
    uint32_t access;
    uint32_t width;
} hl_native_provenance;

typedef enum hl_native_exit_kind {
    HL_NATIVE_EXIT_BRANCH = 1,
    HL_NATIVE_EXIT_SYSCALL = 2,
    HL_NATIVE_EXIT_FALLBACK = 3,
    HL_NATIVE_EXIT_FAULT = 4,
    HL_NATIVE_EXIT_INTERRUPT = 5,
    HL_NATIVE_EXIT_EPOCH = 6,
    HL_NATIVE_EXIT_YIELD = 7,
    HL_NATIVE_EXIT_FATAL = 8,
} hl_native_exit_kind;

typedef struct hl_native_exit {
    uint32_t abi;
    uint32_t size;
    uint32_t kind;
    uint32_t access;
    uint64_t instruction;
    uint64_t next;
    uint64_t address;
    uint64_t code;
} hl_native_exit;

typedef enum hl_native_architecture {
    HL_NATIVE_AARCH64 = 1,
    HL_NATIVE_X86_64 = 2,
} hl_native_architecture;

typedef struct hl_native_source_span {
    uint64_t guest_first;
    const uint8_t *bytes;
    size_t size;
    uint64_t mapping_incarnation;
    uint64_t instruction_epoch;
} hl_native_source_span;

typedef struct hl_native_source {
    const hl_native_source_span *spans;
    size_t span_count;
    uint64_t mapping_incarnation;
    uint64_t instruction_epoch;
} hl_native_source;

typedef struct hl_native_projection_view {
    uint64_t guest_first;
    uint64_t guest_last;
    uint64_t host_first;
    uint64_t mapping_incarnation;
    uint32_t permissions;
    uint16_t write_policy;
    uint16_t write_index;
} hl_native_projection_view;

typedef struct hl_native_projection {
    const hl_native_projection_view *views;
    size_t count;
    uint64_t mapping_incarnation;
    size_t active;
} hl_native_projection;

typedef int (*hl_native_source_resolve)(void *context, uint64_t guest_pc,
                                        uint64_t mapping_incarnation,
                                        uint64_t instruction_epoch,
                                        hl_native_source_span *output);

typedef enum hl_native_operand_result {
    HL_NATIVE_OPERAND_DECLINED = 0,
    HL_NATIVE_OPERAND_RESOLVED = 1,
    HL_NATIVE_OPERAND_FAULT = 2,
    HL_NATIVE_OPERAND_EPOCH = 3,
} hl_native_operand_result;

typedef uint32_t (*hl_native_operand_resolve)(void *context, uint64_t address,
                                              uint64_t size, uint32_t access,
                                              uint64_t mapping_incarnation,
                                              uint64_t instruction_epoch,
                                              hl_native_projection_view *output);
typedef uint32_t (*hl_native_quantum_poll)(void *context, uint64_t executed,
                                           uint64_t cumulative_budget);

typedef struct hl_native_fault_scope hl_native_fault_scope;
typedef uint32_t (*hl_native_fault_publish)(void *, const hl_native_fault_scope *);
typedef void (*hl_native_fault_unpublish)(void *, const hl_native_fault_scope *);
typedef struct hl_native_direct_token hl_native_direct_token;

typedef struct hl_native_run_certificate {
    uint32_t abi;
    uint32_t size;
    uint32_t architecture;
    uint32_t data_permissions;
    uint32_t mapped_executable;
    uint32_t view_index;
    uint16_t write_policy;
    uint16_t reserved;
    uint32_t reserved2;
    uint64_t guest_first;
    uint64_t guest_last;
    uint64_t host_first;
    uint64_t mapping_incarnation;
    uint64_t mapping_generation;
    uint64_t instruction_generation;
    uint64_t authority_identity;
    uint64_t authority_generation;
    uint64_t run_generation;
    const hl_native_direct_token *direct_token;
} hl_native_run_certificate;

/* A run request is deliberately bounded. The optional source resolver is
 * called only after a translated control transfer leaves the immutable batch;
 * its borrowed span remains owned by the caller for the duration of the run.
 * `budget` is the maximum number of guest instructions that may be entered
 * during this activation. Zero is an immediate, precise yield. Requests whose
 * size includes source/projection may carry immutable architecture-owned batch
 * descriptors; older requests and other architectures ignore those fields. */
typedef struct hl_native_run_request {
    uint32_t abi;
    uint32_t size;
    uint32_t architecture;
    uint32_t reserved;
    uint64_t mapping_epoch;
    uint64_t budget;
    const hl_native_source *source;
    const hl_native_projection *projection;
    void *source_context;
    hl_native_source_resolve source_resolve;
    void *operand_context;
    hl_native_operand_resolve operand_resolve;
    void *fault_context;
    hl_native_fault_publish fault_publish;
    hl_native_fault_unpublish fault_unpublish;
    uint64_t memory_mode;
    uint64_t authority_generation;
    const hl_native_direct_token *direct_token;
    uint64_t authority_identity;
    void *quantum_context;
    hl_native_quantum_poll quantum_poll;
    uint64_t quantum_grant;
    const hl_native_run_certificate *certificate;
} hl_native_run_request;

typedef struct hl_native_cpu {
    uint32_t abi;
    uint32_t size;
    uint32_t architecture;
    uint32_t reserved;
    union {
        hl_native_aarch64_cpu *aarch64;
        hl_native_x86_64_cpu *x86_64;
        void *opaque;
    } state;
} hl_native_cpu;
typedef struct hl_native_executor hl_native_executor;
typedef struct hl_native_interrupt_token hl_native_interrupt_token;

typedef struct hl_native_direct_authority {
    uint32_t abi;
    uint32_t size;
    uint32_t permissions;
    uint32_t reserved;
    uint64_t guest_first;
    uint64_t guest_last;
    uint64_t host_first;
    uint64_t mapping_incarnation;
    uint64_t mapping_generation;
    uint64_t instruction_generation;
} hl_native_direct_authority;

/* Consumer-owned host-fault scope.  Enter/leave run outside a host signal
 * handler.  While active, the executor's cache, arena, CPU state, and bounded
 * provenance records are pinned.  The remaining operations perform no
 * allocation, locking, or mutation and may be called by the application's
 * existing host-fault owner.  The application owns signal installation,
 * nesting, masking, alternate stacks, prior-handler chaining, and fork repair.
 * It must unpublish this scope from that owner before leave. */
struct hl_native_fault_scope {
    uint32_t abi;
    uint32_t size;
    uint32_t architecture;
    uint32_t reserved;
    hl_native_executor *executor;
    hl_native_cpu *cpu;
};

/* reserved=0 identifies an owning scope returned by scope_enter and accepted
 * by scope_leave. Dispatcher callbacks receive a borrowed reserved=1 view;
 * contains/provenance/prepare accept it, but leave must reject it. */

hl_native_status hl_native_interrupt_create(hl_native_interrupt_token **);
hl_native_status hl_native_interrupt_set(hl_native_interrupt_token *, uint64_t);
void hl_native_interrupt_destroy(hl_native_interrupt_token *);

_Static_assert(UINTPTR_MAX == UINT64_MAX, "native execution requires a 64-bit host ABI");
_Static_assert(sizeof(hl_native_mapping) == 48, "native mapping ABI drifted");
_Static_assert(sizeof(hl_native_memory) == 64, "native memory ABI drifted");
_Static_assert(sizeof(hl_native_config) == 40, "native config ABI drifted");
_Static_assert(offsetof(hl_native_diagnostics, x86_public_exits) == 192,
               "native diagnostics legacy prefix drifted");
_Static_assert(offsetof(hl_native_diagnostics, relocation_cold_targets) == 288,
               "native diagnostics retained prefix drifted");
_Static_assert(offsetof(hl_native_diagnostics, a64_fallback_guard_read) == 336,
               "native diagnostics fallback extension drifted");
_Static_assert(offsetof(hl_native_diagnostics, a64_fallback_entry_rejection) == 384,
               "native diagnostics fallback-form extension drifted");
_Static_assert(offsetof(hl_native_diagnostics, x86_public_epochs) == 448,
               "native diagnostics epoch extension drifted");
_Static_assert(offsetof(hl_native_diagnostics, ibtc_authenticated_entries) == 520,
               "native diagnostics append offset drifted");
_Static_assert(sizeof(hl_native_diagnostics) == 544, "native diagnostics ABI drifted");
_Static_assert(sizeof(hl_native_change) == 40, "native change ABI drifted");
_Static_assert(sizeof(hl_native_fault) == 40, "native fault ABI drifted");
_Static_assert(sizeof(hl_native_address) == 24, "native address ABI drifted");
_Static_assert(offsetof(hl_native_address, constant) == 8, "native address constant offset drifted");
_Static_assert(offsetof(hl_native_address, kind) == 16, "native address shape offset drifted");
_Static_assert(sizeof(hl_native_provenance) == 56, "native provenance ABI drifted");
_Static_assert(offsetof(hl_native_provenance, address) == 24, "native provenance address offset drifted");
_Static_assert(offsetof(hl_native_provenance, access) == 48, "native provenance access offset drifted");
_Static_assert(sizeof(hl_native_exit) == 48, "native exit ABI drifted");
_Static_assert(sizeof(hl_native_source_span) == 40, "native source span ABI drifted");
_Static_assert(sizeof(hl_native_source) == 32, "native source ABI drifted");
_Static_assert(sizeof(hl_native_projection_view) == 40, "native projection view ABI drifted");
_Static_assert(sizeof(hl_native_projection) == 32, "native projection ABI drifted");
_Static_assert(offsetof(hl_native_run_request, source) == 32, "native run prefix ABI drifted");
_Static_assert(offsetof(hl_native_run_request, operand_context) == 64, "native resolver prefix ABI drifted");
_Static_assert(sizeof(hl_native_run_certificate) == 112, "native run certificate ABI drifted");
_Static_assert(offsetof(hl_native_run_certificate, direct_token) == 104,
               "native run certificate token offset drifted");
_Static_assert(offsetof(hl_native_run_request, certificate) == 160,
               "native run certificate pointer offset drifted");
_Static_assert(sizeof(hl_native_run_request) == 168, "native run request ABI drifted");
_Static_assert(sizeof(hl_native_cpu) == 24, "native CPU handle ABI drifted");
_Static_assert(sizeof(hl_native_fault_scope) == 32, "native fault scope ABI drifted");
_Static_assert(sizeof(hl_native_direct_authority) == 64, "native direct authority ABI drifted");

/* Destruction remains externally serialized. Native translated-thread
 * admission and dispatcher safepoints arrive with reusable block execution. */
hl_native_status hl_native_create(const hl_native_config *, hl_native_executor **);
/* Closes whole-public-run and cache mutation admission across the host fork
 * operation. STATE is nonblocking when either owner is live. Every
 * successful before_fork call must be completed exactly once in both the
 * parent and child copies with after_fork. */
hl_native_status hl_native_before_fork(hl_native_executor *);
hl_native_status hl_native_after_fork(hl_native_executor *, uint32_t preserve);
hl_native_status hl_native_diagnose(const hl_native_executor *, hl_native_diagnostics *);
hl_native_status hl_native_changed(hl_native_executor *, const hl_native_change *, size_t);
hl_native_status hl_native_resolve_fault(const hl_native_executor *, hl_native_fault *);
hl_native_status hl_native_fault_scope_enter(hl_native_executor *, hl_native_cpu *,
                                             hl_native_fault_scope *);
hl_native_status hl_native_fault_scope_leave(hl_native_fault_scope *);
int hl_native_fault_scope_contains(const hl_native_fault_scope *, uint64_t);
int hl_native_fault_scope_provenance(const hl_native_fault_scope *, uint64_t,
                                     hl_native_provenance *);
int hl_native_fault_scope_prepare_return(const hl_native_fault_scope *, uint64_t,
                                         uint64_t, void *);
/* Fixed native thread association for an application-owned fault dispatcher.
 * Attach/detach are coarse execution-thread operations, never entry callbacks.
 * Publish accepts only a borrowed reserved=1 scope and copies it; unpublish
 * requires equal scope contents plus the returned retirement generation.
 * Prepare is async-signal-safe only where attach succeeds. Unsupported hosts
 * return PLATFORM rather than claiming unsafe compiler/runtime TLS behavior. */
hl_native_status hl_native_fault_thread_attach(void);
hl_native_status hl_native_fault_thread_publish(const hl_native_fault_scope *, uint64_t *);
hl_native_status hl_native_fault_thread_unpublish(const hl_native_fault_scope *, uint64_t);
int hl_native_fault_thread_prepare(uint64_t, uint64_t, void *);
hl_native_status hl_native_fault_thread_after_fork_child(void);
hl_native_status hl_native_fault_thread_detach(void);
/* Process-wide POSIX fault dispatch ownership. Acquire installs one composed
 * SIGSEGV/SIGBUS dispatcher for every native executor in the process and
 * retains the dispositions until the matching release. Existing dispositions
 * are chained when a fault is not owned by the active native thread scope.
 * Thread attach remains explicit because alternate stacks and published scopes
 * are properties of the calling host thread, not of the process coordinator. */
hl_native_status hl_native_fault_process_acquire(void);
hl_native_status hl_native_fault_process_release(void);

/* Registers trusted application evidence retained by a Rust memory lease.
 * The opaque token is generation-qualified and enables no execution behavior.
 * Registration/unregistration are externally serialized with executor use;
 * after_fork invalidates inherited tokens in both process copies. */
hl_native_status hl_native_direct_register(hl_native_executor *,
                                           const hl_native_direct_authority *,
                                           hl_native_direct_token **);
int hl_native_direct_validate(const hl_native_executor *, const hl_native_direct_token *,
                              hl_native_direct_authority *);
uint64_t hl_native_direct_generation(const hl_native_executor *, const hl_native_direct_token *);
uint64_t hl_native_direct_identity(const hl_native_executor *, const hl_native_direct_token *);
hl_native_status hl_native_direct_unregister(hl_native_executor *, hl_native_direct_token *);
hl_native_status hl_native_run(hl_native_executor *, hl_native_cpu *,
                               const hl_native_run_request *, hl_native_exit *);
void hl_native_flush(void *, size_t);
/* Nonblocking close-and-destroy. STATE means a run, fault scope, mutation, or
 * fork owner is active; the handle remains live and the call may be retried.
 * OK consumes the handle exactly once. */
hl_native_status hl_native_destroy(hl_native_executor *);

#endif
