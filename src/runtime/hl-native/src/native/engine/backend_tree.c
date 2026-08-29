/*
 * Hook-only whole-process-tree execution census.
 *
 * A guest fork is a host fork, so ordinary globals become COW snapshots and
 * cannot answer how the complete guest tree executed.  This table is created
 * before the first guest instruction, mapped MAP_SHARED by the Linux-memory
 * boundary, and inherited by every guest child.  Each process claims exactly
 * one pid slot; threads in that process update the same atomics.
 *
 * The full table is hook-only diagnostic state. Product diagnostics retain a
 * separate compact fork-shared record for the three mixed normal/SSE execution
 * facts used by untimed same-binary proof; ordinary launches allocate nothing.
 * No counter influences translation, scheduling, signal delivery, or
 * guest-visible state.
 */

#if defined(HL_NATIVE_TEST_HOOKS)

#define HL_BACKEND_TREE_SLOTS 4096u
#define HL_BACKEND_TREE_REASON_COUNT 16u
#define HL_BACKEND_SHAPE_FORM_SLOTS 4096u
#define HL_BACKEND_SHAPE_TOP_FORMS 8u

enum hl_backend_shape_translated_exit {
    HL_BACKEND_SHAPE_T_FALLTHROUGH,
    HL_BACKEND_SHAPE_T_COND_TAKEN,
    HL_BACKEND_SHAPE_T_COND_NOT_TAKEN,
    HL_BACKEND_SHAPE_T_DIRECT_JUMP,
    HL_BACKEND_SHAPE_T_DIRECT_CALL,
    HL_BACKEND_SHAPE_T_RETURN,
    HL_BACKEND_SHAPE_T_INDIRECT_BRANCH,
    HL_BACKEND_SHAPE_T_INDIRECT_CALL,
    HL_BACKEND_SHAPE_T_SYSCALL,
    HL_BACKEND_SHAPE_T_IRQ,
    HL_BACKEND_SHAPE_T_FAULT,
    HL_BACKEND_SHAPE_T_OTHER,
    HL_BACKEND_SHAPE_T_COUNT,
};
#define HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY HL_BACKEND_SHAPE_T_INDIRECT_BRANCH
#define HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY HL_BACKEND_SHAPE_T_INDIRECT_CALL

/* Why a completed transliterated descriptor used its sequential dispatcher exit.  These are execution
   facts, carried by the emitted terminal marker: counting them while building would include cold blocks
   and would misattribute a linked ingress to the descriptor through which the dispatcher entered. */
enum hl_backend_shape_fall_stop {
    HL_BACKEND_FALL_CAP,
    HL_BACKEND_FALL_DECODE,
    HL_BACKEND_FALL_NORMAL_TO_SSE2,
    HL_BACKEND_FALL_SSE2_TO_NORMAL,
    HL_BACKEND_FALL_NORMAL_TO_FS,
    HL_BACKEND_FALL_FS_TO_NORMAL,
    HL_BACKEND_FALL_SSE2_TO_FS,
    HL_BACKEND_FALL_FS_TO_SSE2,
    HL_BACKEND_FALL_TL_NO,
    HL_BACKEND_FALL_DISPLACED_UNSAFE,
    HL_BACKEND_FALL_FETCH,
    HL_BACKEND_FALL_RIPREL_LOWER,
    HL_BACKEND_FALL_FS_TRANSACTION,
    HL_BACKEND_FALL_SSE_RIPREL_LOWER,
    HL_BACKEND_FALL_OTHER,
    HL_BACKEND_FALL_COUNT,
};

enum hl_backend_shape_interpreter_entry {
    HL_BACKEND_SHAPE_I_DISABLED,
    HL_BACKEND_SHAPE_I_IMAGE,
    HL_BACKEND_SHAPE_I_DECODE,
    HL_BACKEND_SHAPE_I_UNSUPPORTED,
    HL_BACKEND_SHAPE_I_AUTHORITY,
    HL_BACKEND_SHAPE_I_RESOURCE,
    HL_BACKEND_SHAPE_I_EMIT,
    HL_BACKEND_SHAPE_I_RUNTIME_IMAGE,
    HL_BACKEND_SHAPE_I_RUNTIME_BIND,
    HL_BACKEND_SHAPE_I_OTHER,
    HL_BACKEND_SHAPE_I_COUNT,
};

enum hl_backend_shape_interpreter_stop {
    HL_BACKEND_SHAPE_S_FALLTHROUGH,
    HL_BACKEND_SHAPE_S_COND_TAKEN,
    HL_BACKEND_SHAPE_S_COND_NOT_TAKEN,
    HL_BACKEND_SHAPE_S_DIRECT_JUMP,
    HL_BACKEND_SHAPE_S_DIRECT_CALL,
    HL_BACKEND_SHAPE_S_RETURN,
    HL_BACKEND_SHAPE_S_INDIRECT_BRANCH,
    HL_BACKEND_SHAPE_S_INDIRECT_CALL,
    HL_BACKEND_SHAPE_S_SYSCALL,
    HL_BACKEND_SHAPE_S_IRQ,
    HL_BACKEND_SHAPE_S_FAULT,
    HL_BACKEND_SHAPE_S_SERVICE,
    HL_BACKEND_SHAPE_S_OTHER,
    HL_BACKEND_SHAPE_S_COUNT,
};
#define HL_BACKEND_SHAPE_S_INDIRECT_BRANCH_MEMORY HL_BACKEND_SHAPE_S_INDIRECT_BRANCH
#define HL_BACKEND_SHAPE_S_INDIRECT_CALL_MEMORY HL_BACKEND_SHAPE_S_INDIRECT_CALL

enum hl_backend_shape_edge_family {
    HL_BACKEND_SHAPE_EDGE_FALLTHROUGH,
    HL_BACKEND_SHAPE_EDGE_JCC_TAKEN,
    HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN,
    HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP,
    HL_BACKEND_SHAPE_EDGE_DIRECT_CALL,
    HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT,
};

enum hl_backend_shape_edge_resolution {
    HL_BACKEND_SHAPE_EDGE_MAPPED,
    HL_BACKEND_SHAPE_EDGE_UNMAPPED,
    HL_BACKEND_SHAPE_EDGE_INTERRUPTED,
    HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT,
};

enum hl_backend_would_link_family {
    HL_BACKEND_WOULD_LINK_FALLTHROUGH,
    HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
    HL_BACKEND_WOULD_LINK_DIRECT_CALL,
    HL_BACKEND_WOULD_LINK_FAMILY_COUNT,
};

/* Ordered, first-refusal-wins publication facts.  ELIGIBLE is deliberately last. */
enum hl_backend_would_link_disposition {
    HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED,
    HL_BACKEND_WOULD_LINK_CROSS_PAGE,
    HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED,
    HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED,
    HL_BACKEND_WOULD_LINK_GENERATION,
    HL_BACKEND_WOULD_LINK_TARGET_PAGE,
    HL_BACKEND_WOULD_LINK_REL32,
    HL_BACKEND_WOULD_LINK_ELIGIBLE,
    HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT,
};

enum hl_backend_family_div_outcome {
    HL_BACKEND_FAMILY_DIV_INLINE,
    HL_BACKEND_FAMILY_DIV_SERVICE64,
    HL_BACKEND_FAMILY_DIV_DE,
    HL_BACKEND_FAMILY_DIV_OUTCOME_COUNT,
};

enum {
    HL_BACKEND_FAMILY_DIV_UNSIGNED,
    HL_BACKEND_FAMILY_DIV_SIGNED,
    HL_BACKEND_FAMILY_DIV_KIND_COUNT,
};

struct hl_backend_shape_form {
    _Atomic uint32_t state; /* 0 empty, 1 metadata reserved, 2 published */
    uint64_t key;
    _Atomic uint64_t count;
};

#if ATOMIC_INT_LOCK_FREE != 2
#error "backend-tree signal finalization requires lock-free 32-bit atomics"
#endif
#if (defined(_WIN32) && ATOMIC_LLONG_LOCK_FREE != 2) || (!defined(_WIN32) && ATOMIC_LONG_LOCK_FREE != 2)
#error "backend-tree signal finalization requires lock-free 64-bit atomics"
#endif

enum hl_backend_tree_lifecycle {
    HL_BACKEND_TREE_CLAIMED = 1,
    HL_BACKEND_TREE_COMPLETED = 2,
    HL_BACKEND_TREE_ABNORMAL = 3,
};

struct hl_backend_tree_slot {
    _Atomic int pid;           /* 0 free, -1 being filled, positive published last */
    _Atomic uint64_t birth_ns; /* published before pid; prevents authority crossing PID reuse */
    _Atomic uint32_t lifecycle;
    _Atomic uint64_t translated_entries;
    _Atomic uint64_t interpreted_entries;
    _Atomic uint64_t translated_steps;
    _Atomic uint64_t interpreted_steps;
    _Atomic uint64_t translations;
    _Atomic uint64_t map_hits;
    _Atomic uint64_t stw_retries;
    _Atomic uint64_t irq_pending;
    _Atomic uint64_t reason[HL_BACKEND_TREE_REASON_COUNT];
    _Atomic uint64_t reason_other;
    _Atomic uint64_t translated_exit[HL_BACKEND_SHAPE_T_COUNT];
    _Atomic uint64_t translated_fall_stop[HL_BACKEND_FALL_COUNT];
    _Atomic uint64_t translated_stitch_jmp;
    _Atomic uint64_t translated_stitch_cond_fall;
    _Atomic uint64_t interpreter_entry[HL_BACKEND_SHAPE_I_COUNT];
    _Atomic uint64_t interpreter_stop[HL_BACKEND_SHAPE_S_COUNT];
    _Atomic uint64_t direct_edge[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    _Atomic uint64_t direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT]
                                                   [HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT];
    _Atomic uint64_t direct_edge_chained[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    _Atomic uint64_t direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    _Atomic uint64_t jcc_taken_same_page;
    _Atomic uint64_t jcc_taken_cross_page;
    _Atomic uint64_t jcc_taken_target_translated;
    _Atomic uint64_t jcc_taken_target_interpreted;
    _Atomic uint64_t jcc_taken_generation_current;
    _Atomic uint64_t jcc_taken_generation_retired;
    _Atomic uint64_t jcc_taken_rel32;
    _Atomic uint64_t jcc_taken_rel32_unreachable;
    _Atomic uint64_t jcc_taken_eligible;
    _Atomic uint64_t jcc_taken_ineligible;
    _Atomic uint64_t would_link[HL_BACKEND_WOULD_LINK_FAMILY_COUNT]
                               [HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT];
    _Atomic uint64_t family_jmem;
    _Atomic uint64_t family_div[HL_BACKEND_FAMILY_DIV_KIND_COUNT][HL_BACKEND_FAMILY_DIV_OUTCOME_COUNT];
    _Atomic uint64_t family_div_service64_completed[HL_BACKEND_FAMILY_DIV_KIND_COUNT];
    _Atomic uint64_t mixed_sse_executed;
    _Atomic uint64_t mixed_sse_executed_transitions;
    _Atomic uint64_t mixed_sse_disabled_boundaries;
};

struct hl_backend_tree_shared {
    _Atomic int root_pid;
    _Atomic uint64_t missing_claims;
    _Atomic uint64_t duplicate_finalize;
    _Atomic uint32_t reported;
    struct hl_backend_shape_form fallback_forms[HL_BACKEND_SHAPE_FORM_SLOTS];
    struct hl_backend_shape_form stop_forms[HL_BACKEND_SHAPE_FORM_SLOTS];
    _Atomic uint64_t fallback_form_total;
    _Atomic uint64_t fallback_form_unique;
    _Atomic uint64_t fallback_form_overflow;
    _Atomic uint64_t stop_form_total;
    _Atomic uint64_t stop_form_unique;
    _Atomic uint64_t stop_form_overflow;
    /* One shared dynamic count: every admitted link has the same proven JCC disposition. Keeping it in
       the fork-shared record makes emitted increments independent of per-process slot reassignment. */
    _Atomic uint64_t jcc_links;
    struct hl_backend_tree_slot slots[HL_BACKEND_TREE_SLOTS];
};

struct hl_backend_tree_summary {
    uint64_t root_pid;
    uint64_t claimed;
    uint64_t completed;
    uint64_t abnormal;
    uint64_t missing;
    uint64_t duplicate_finalize;
    uint64_t crossings;
    uint64_t translated_entries;
    uint64_t interpreted_entries;
    uint64_t translated_steps;
    uint64_t interpreted_steps;
    uint64_t translations;
    uint64_t map_hits;
    uint64_t stw_retries;
    uint64_t irq_pending;
    uint64_t reason[HL_BACKEND_TREE_REASON_COUNT];
    uint64_t reason_other;
    uint64_t translated_exit[HL_BACKEND_SHAPE_T_COUNT];
    uint64_t translated_fall_stop[HL_BACKEND_FALL_COUNT];
    uint64_t translated_stitch_jmp;
    uint64_t translated_stitch_cond_fall;
    uint64_t interpreter_entry[HL_BACKEND_SHAPE_I_COUNT];
    uint64_t interpreter_stop[HL_BACKEND_SHAPE_S_COUNT];
    uint64_t direct_edge[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    uint64_t direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT]
                                   [HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT];
    uint64_t direct_edge_chained[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    uint64_t direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT];
    uint64_t jcc_taken_same_page;
    uint64_t jcc_taken_cross_page;
    uint64_t jcc_taken_target_translated;
    uint64_t jcc_taken_target_interpreted;
    uint64_t jcc_taken_generation_current;
    uint64_t jcc_taken_generation_retired;
    uint64_t jcc_taken_rel32;
    uint64_t jcc_taken_rel32_unreachable;
    uint64_t jcc_taken_eligible;
    uint64_t jcc_taken_ineligible;
    uint64_t would_link[HL_BACKEND_WOULD_LINK_FAMILY_COUNT]
                        [HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT];
    uint64_t family_jmem;
    uint64_t family_div[HL_BACKEND_FAMILY_DIV_KIND_COUNT][HL_BACKEND_FAMILY_DIV_OUTCOME_COUNT];
    uint64_t family_div_service64_completed[HL_BACKEND_FAMILY_DIV_KIND_COUNT];
    uint64_t mixed_sse_executed;
    uint64_t mixed_sse_executed_transitions;
    uint64_t mixed_sse_disabled_boundaries;
    uint64_t fallback_form_total;
    uint64_t fallback_form_unique;
    uint64_t fallback_form_overflow;
    uint64_t stop_form_total;
    uint64_t stop_form_unique;
    uint64_t stop_form_overflow;
    uint64_t fallback_top_key[HL_BACKEND_SHAPE_TOP_FORMS];
    uint64_t fallback_top_count[HL_BACKEND_SHAPE_TOP_FORMS];
    uint64_t stop_top_key[HL_BACKEND_SHAPE_TOP_FORMS];
    uint64_t stop_top_count[HL_BACKEND_SHAPE_TOP_FORMS];
};

static struct hl_backend_tree_shared *g_backend_tree;
static struct hl_backend_tree_slot *g_backend_tree_self;
static int g_backend_tree_lifecycle_owned;

static uint64_t hl_backend_shape_mix(uint64_t value) {
    value ^= value >> 33;
    value *= UINT64_C(0xff51afd7ed558ccd);
    value ^= value >> 33;
    value *= UINT64_C(0xc4ceb9fe1a85ec53);
    return value ^ (value >> 33);
}

static void hl_backend_shape_form_record(struct hl_backend_shape_form forms[HL_BACKEND_SHAPE_FORM_SLOTS],
                                         _Atomic uint64_t *total, _Atomic uint64_t *unique,
                                         _Atomic uint64_t *overflow, uint64_t key) {
    if (g_backend_tree_self == NULL || g_backend_tree == NULL) return;
    atomic_fetch_add_explicit(total, 1, memory_order_relaxed);
    unsigned start = (unsigned)hl_backend_shape_mix(key) & (HL_BACKEND_SHAPE_FORM_SLOTS - 1u);
    for (unsigned probe = 0; probe < HL_BACKEND_SHAPE_FORM_SLOTS; ++probe) {
        struct hl_backend_shape_form *form = &forms[(start + probe) & (HL_BACKEND_SHAPE_FORM_SLOTS - 1u)];
        uint32_t state = atomic_load_explicit(&form->state, memory_order_acquire);
        /* A process can die after reserving a shared slot and before publishing its key.  Never wait on
           another process's transient state: retain the exact total and expose the lost form detail as
           overflow instead of allowing an abnormal child to wedge every surviving recorder. */
        if (state == 1) {
            atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
            return;
        }
        if (state == 0) {
            uint32_t expected = 0;
            if (atomic_compare_exchange_strong_explicit(&form->state, &expected, 1, memory_order_acquire,
                                                        memory_order_relaxed)) {
                form->key = key;
                atomic_store_explicit(&form->count, 1, memory_order_relaxed);
                atomic_store_explicit(&form->state, 2, memory_order_release);
                atomic_fetch_add_explicit(unique, 1, memory_order_relaxed);
                return;
            }
            state = atomic_load_explicit(&form->state, memory_order_acquire);
            if (state == 1) {
                atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
                return;
            }
        }
        if (state == 2 && form->key == key) {
            atomic_fetch_add_explicit(&form->count, 1, memory_order_relaxed);
            return;
        }
    }
    atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
}

static struct hl_backend_tree_slot *hl_backend_tree_reserve(void) {
    if (g_backend_tree == NULL) return NULL;
    for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
        int expected = 0;
        struct hl_backend_tree_slot *slot = &g_backend_tree->slots[index];
        if (!atomic_compare_exchange_strong_explicit(&slot->pid, &expected, -1, memory_order_acq_rel,
                                                     memory_order_relaxed))
            continue;
        atomic_store_explicit(&slot->lifecycle, HL_BACKEND_TREE_CLAIMED, memory_order_relaxed);
        return slot;
    }
    return NULL;
}

static void hl_backend_tree_publish(struct hl_backend_tree_slot *slot, int pid) {
    uint64_t birth_ns = 0;
    if (slot == NULL || pid <= 0 || !hl_host_process_start_time_ns(pid, &birth_ns)) return;
    atomic_store_explicit(&slot->birth_ns, birth_ns, memory_order_relaxed);
    atomic_store_explicit(&slot->pid, pid, memory_order_release);
}

static struct hl_backend_tree_slot *hl_backend_tree_claim_pid(int pid) {
    struct hl_backend_tree_slot *slot = hl_backend_tree_reserve();
    if (slot == NULL) {
        if (g_backend_tree != NULL) atomic_fetch_add_explicit(&g_backend_tree->missing_claims, 1, memory_order_relaxed);
        return NULL;
    }
    hl_backend_tree_publish(slot, pid);
    return slot;
}

size_t hl_target_backend_tree_shared_size(int enabled) {
#if defined(_WIN32)
    (void)enabled;
    return 0;
#else
    return enabled ? sizeof(struct hl_backend_tree_shared) : 0;
#endif
}

void hl_target_backend_tree_child_begin(void *shared, size_t shared_size) {
    g_backend_tree_lifecycle_owned = 1;
    g_backend_tree = shared_size == sizeof(struct hl_backend_tree_shared) ? shared : NULL;
    g_backend_tree_self = NULL;
    if (g_backend_tree == NULL) return;
    int self = (int)getpid();
    atomic_store_explicit(&g_backend_tree->root_pid, self, memory_order_release);
    g_backend_tree_self = hl_backend_tree_claim_pid(self);
}

static void hl_backend_tree_begin(int enabled, const hl_host_services *host) {
    if (g_backend_tree_lifecycle_owned) return;
    g_backend_tree = NULL;
    g_backend_tree_self = NULL;
    if (!enabled) return;
    void *mapping = NULL;
    if (hl_linux_shared_create(host, sizeof(struct hl_backend_tree_shared), &mapping) != HL_STATUS_OK) {
#if !defined(_WIN32)
        /* Exported hook tests do not construct an engine instance and therefore have no injected host-service
           table.  They still exercise the production storage primitive: a genuinely shared anonymous mapping. */
        mapping = mmap(NULL, sizeof(struct hl_backend_tree_shared), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS,
                       -1, 0);
        if (mapping == MAP_FAILED) mapping = NULL;
#endif
    }
    if (mapping == NULL) return;
    hl_target_backend_tree_child_begin(mapping, sizeof(struct hl_backend_tree_shared));
    /* The exported hook owns this fallback mapping itself and starts a fresh scenario on its next call.
       Production enters with lifecycle_owned already set and returns before allocating here. */
    g_backend_tree_lifecycle_owned = 0;
}

/* Reserve the birth before fork. The parent can then publish the returned pid even when the child is killed
   before it executes a single userspace instruction; the child inherits the same reservation and binds its
   local counter pointer before any path can _exit. */
static struct hl_backend_tree_slot *hl_backend_tree_prepare_fork(void) {
    return hl_backend_tree_reserve();
}

static void hl_backend_tree_after_fork(pid_t result, struct hl_backend_tree_slot *birth) {
    if (g_backend_tree == NULL) return;
    if (result < 0) {
        if (birth != NULL) {
            atomic_store_explicit(&birth->lifecycle, 0, memory_order_relaxed);
            atomic_store_explicit(&birth->pid, 0, memory_order_release);
        }
        return;
    }
    if (birth == NULL) {
        /* Only the parent records exhaustion: both fork return arms share the mapping, so counting in the child
           too would turn one untracked process into two missing lifecycle rows. */
        if (result > 0) atomic_fetch_add_explicit(&g_backend_tree->missing_claims, 1, memory_order_relaxed);
        if (result == 0) g_backend_tree_self = NULL;
        return;
    }
    if (result == 0) {
        g_backend_tree_self = birth;
        hl_backend_tree_publish(birth, (int)getpid());
    } else
        hl_backend_tree_publish(birth, (int)result);
}

static inline void hl_backend_tree_run_begin(int translated, uint64_t steps) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (translated) {
        atomic_fetch_add_explicit(&slot->translated_entries, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&slot->translated_steps, steps, memory_order_relaxed);
    } else {
        atomic_fetch_add_explicit(&slot->interpreted_entries, 1, memory_order_relaxed);
    }
}

static inline int hl_backend_tree_steps_enabled(void) { return g_backend_tree_self != NULL; }

static inline void hl_backend_tree_interpreted_steps(uint64_t steps) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->interpreted_steps, steps, memory_order_relaxed);
}

static inline void hl_backend_tree_reason(unsigned reason) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (reason < HL_BACKEND_TREE_REASON_COUNT)
        atomic_fetch_add_explicit(&slot->reason[reason], 1, memory_order_relaxed);
    else
        atomic_fetch_add_explicit(&slot->reason_other, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_translated_exit(unsigned kind, unsigned stitched_jmp,
                                                   unsigned stitched_cond_fall) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (kind >= HL_BACKEND_SHAPE_T_COUNT) kind = HL_BACKEND_SHAPE_T_OTHER;
    atomic_fetch_add_explicit(&slot->translated_exit[kind], 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&slot->translated_stitch_jmp, stitched_jmp, memory_order_relaxed);
    atomic_fetch_add_explicit(&slot->translated_stitch_cond_fall, stitched_cond_fall, memory_order_relaxed);
}

static inline void hl_backend_tree_translated_fall_stop(unsigned reason) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (reason >= HL_BACKEND_FALL_COUNT) reason = HL_BACKEND_FALL_OTHER;
    atomic_fetch_add_explicit(&slot->translated_fall_stop[reason], 1, memory_order_relaxed);
}

static inline void hl_backend_tree_mixed_sse_completed(uint64_t transitions, int disabled_boundary) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (disabled_boundary) {
        if (transitions == 0)
            atomic_fetch_add_explicit(&slot->mixed_sse_disabled_boundaries, 1, memory_order_relaxed);
        return;
    }
    if (transitions == 0) return;
    atomic_fetch_add_explicit(&slot->mixed_sse_executed, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&slot->mixed_sse_executed_transitions, transitions, memory_order_relaxed);
}

static inline void hl_backend_tree_interpreter_entry(unsigned kind, uint64_t fallback_form) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (kind >= HL_BACKEND_SHAPE_I_COUNT) kind = HL_BACKEND_SHAPE_I_OTHER;
    atomic_fetch_add_explicit(&slot->interpreter_entry[kind], 1, memory_order_relaxed);
    if (kind == HL_BACKEND_SHAPE_I_UNSUPPORTED)
        hl_backend_shape_form_record(g_backend_tree->fallback_forms, &g_backend_tree->fallback_form_total,
                                     &g_backend_tree->fallback_form_unique,
                                     &g_backend_tree->fallback_form_overflow, fallback_form);
}

static inline void hl_backend_tree_interpreter_stop(unsigned kind, uint64_t stop_form) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL) return;
    if (kind >= HL_BACKEND_SHAPE_S_COUNT) kind = HL_BACKEND_SHAPE_S_OTHER;
    atomic_fetch_add_explicit(&slot->interpreter_stop[kind], 1, memory_order_relaxed);
    hl_backend_shape_form_record(g_backend_tree->stop_forms, &g_backend_tree->stop_form_total,
                                 &g_backend_tree->stop_form_unique, &g_backend_tree->stop_form_overflow,
                                 stop_form);
}

static inline void hl_backend_tree_direct_edge(unsigned family, int same_page) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL || family >= HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT) return;
    atomic_fetch_add_explicit(&slot->direct_edge[family], 1, memory_order_relaxed);
    /* This call records a dispatcher-returning edge. Immutable linked JCCs use
       hl_backend_tree_jcc_links instead; the later locked lookup supplies this edge's map disposition. */
    atomic_fetch_add_explicit(&slot->direct_edge_dispatcher[family], 1, memory_order_relaxed);
    if (family == HL_BACKEND_SHAPE_EDGE_JCC_TAKEN)
        atomic_fetch_add_explicit(same_page ? &slot->jcc_taken_same_page : &slot->jcc_taken_cross_page, 1,
                                  memory_order_relaxed);
}

// Addresses baked by hook-only same-ISA x86 link stubs. Each target was resolved under the map lock at
// source publication, so these disposition columns are facts rather than a later lookup's inference.
// The address belongs to the fork-shared record rather than a lifecycle slot, so it remains authoritative
// even on the narrow fixed-pcache fork path that preserves an arena.
static inline uintptr_t hl_backend_tree_jcc_link_counter_address(void) {
    return g_backend_tree == NULL ? 0 : (uintptr_t)&g_backend_tree->jcc_links;
}

static inline void hl_backend_tree_direct_edge_resolution(unsigned family, unsigned resolution,
                                                           int target_translated, int current_generation,
                                                           int rel32_reachable, int eligible) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL || family >= HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT) return;
    if (resolution >= HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT) resolution = HL_BACKEND_SHAPE_EDGE_INTERRUPTED;
    atomic_fetch_add_explicit(&slot->direct_edge_resolution[family][resolution], 1, memory_order_relaxed);
    if (family != HL_BACKEND_SHAPE_EDGE_JCC_TAKEN || resolution == HL_BACKEND_SHAPE_EDGE_INTERRUPTED) return;
    if (resolution == HL_BACKEND_SHAPE_EDGE_UNMAPPED) {
        atomic_fetch_add_explicit(&slot->jcc_taken_ineligible, 1, memory_order_relaxed);
        return;
    }
    atomic_fetch_add_explicit(target_translated ? &slot->jcc_taken_target_translated
                                                : &slot->jcc_taken_target_interpreted,
                              1, memory_order_relaxed);
    if (!target_translated) {
        atomic_fetch_add_explicit(&slot->jcc_taken_ineligible, 1, memory_order_relaxed);
        return;
    }
    atomic_fetch_add_explicit(current_generation ? &slot->jcc_taken_generation_current
                                                 : &slot->jcc_taken_generation_retired,
                              1, memory_order_relaxed);
    atomic_fetch_add_explicit(rel32_reachable ? &slot->jcc_taken_rel32 : &slot->jcc_taken_rel32_unreachable, 1,
                              memory_order_relaxed);
    atomic_fetch_add_explicit(eligible ? &slot->jcc_taken_eligible : &slot->jcc_taken_ineligible, 1,
                              memory_order_relaxed);
}

static inline void hl_backend_tree_would_link(unsigned family, unsigned disposition) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL || family >= HL_BACKEND_WOULD_LINK_FAMILY_COUNT ||
        disposition >= HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT)
        return;
    atomic_fetch_add_explicit(&slot->would_link[family][disposition], 1, memory_order_relaxed);
}

static inline void hl_backend_tree_family_jmem(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->family_jmem, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_family_div(unsigned is_signed, unsigned outcome) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL || is_signed >= HL_BACKEND_FAMILY_DIV_KIND_COUNT ||
        outcome >= HL_BACKEND_FAMILY_DIV_OUTCOME_COUNT)
        return;
    atomic_fetch_add_explicit(&slot->family_div[is_signed][outcome], 1, memory_order_relaxed);
}

static inline void hl_backend_tree_family_div_service64_completed(unsigned is_signed) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot == NULL || is_signed >= HL_BACKEND_FAMILY_DIV_KIND_COUNT) return;
    atomic_fetch_add_explicit(&slot->family_div_service64_completed[is_signed], 1, memory_order_relaxed);
}

static inline void hl_backend_tree_translation(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->translations, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_map_hit(void) {
    if (g_backend_tree_self != NULL) atomic_fetch_add_explicit(&g_backend_tree_self->map_hits, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_stw_retry(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->stw_retries, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_irq_pending(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->irq_pending, 1, memory_order_relaxed);
}

static int hl_backend_tree_finalize_slot_in(struct hl_backend_tree_shared *shared, struct hl_backend_tree_slot *slot,
                                            int abnormal) {
    if (slot == NULL) return 0;
    uint32_t expected = HL_BACKEND_TREE_CLAIMED;
    uint32_t completed = abnormal ? HL_BACKEND_TREE_ABNORMAL : HL_BACKEND_TREE_COMPLETED;
    if (atomic_compare_exchange_strong_explicit(&slot->lifecycle, &expected, completed, memory_order_acq_rel,
                                                memory_order_acquire))
        return 1;
    if (shared != NULL) atomic_fetch_add_explicit(&shared->duplicate_finalize, 1, memory_order_relaxed);
    return 0;
}

static int hl_backend_tree_finalize_slot(struct hl_backend_tree_slot *slot, int abnormal) {
    return hl_backend_tree_finalize_slot_in(g_backend_tree, slot, abnormal);
}

static int hl_backend_tree_finalize(int abnormal) {
    return hl_backend_tree_finalize_slot(g_backend_tree_self, abnormal);
}

/* A reaper closes a child that could not execute its own finalizer (SIGKILL, host fault). */
static void hl_backend_tree_reaped(int pid) {
    if (g_backend_tree == NULL || pid <= 0) return;
    for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &g_backend_tree->slots[index];
        if (atomic_load_explicit(&slot->pid, memory_order_acquire) != pid) continue;
        uint32_t lifecycle = atomic_load_explicit(&slot->lifecycle, memory_order_acquire);
        if (lifecycle != HL_BACKEND_TREE_CLAIMED) continue;
        (void)hl_backend_tree_finalize_slot(slot, 1);
        return;
    }
}

static int hl_backend_tree_is_finalized(void) {
    return g_backend_tree_self != NULL &&
           atomic_load_explicit(&g_backend_tree_self->lifecycle, memory_order_acquire) != HL_BACKEND_TREE_CLAIMED;
}

#if !defined(_WIN32)
/* The lifecycle parent calls this only after it has reaped the initial guest. Match every remaining pid with
   its immutable birth token before signalling it, then wait until that incarnation is gone or a zombie. A
   zombie cannot execute or mutate the mapping; treating it as settled also avoids depending on an unrelated
   host init's reap cadence for a deeper descendant. */
static int hl_backend_tree_process_can_mutate(const struct hl_backend_tree_slot *slot, int pid) {
    hl_host_process_info process;
    uint64_t expected = atomic_load_explicit(&slot->birth_ns, memory_order_acquire);
    return expected != 0 && hl_host_process_read(pid, &process) && process.start_time_ns == expected &&
           process.state != 'Z' && process.state != 'X';
}

static int hl_backend_tree_parent_barrier(struct hl_backend_tree_shared *shared, int root_pid) {
    for (unsigned round = 0; round < 2000; ++round) {
        unsigned live = 0;
        for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
            struct hl_backend_tree_slot *slot = &shared->slots[index];
            int pid = atomic_load_explicit(&slot->pid, memory_order_acquire);
            if (pid == -1) {
                ++live;
                continue;
            }
            if (pid <= 0) continue;
            if (pid == root_pid || !hl_backend_tree_process_can_mutate(slot, pid)) {
                if (atomic_load_explicit(&slot->lifecycle, memory_order_acquire) == HL_BACKEND_TREE_CLAIMED)
                    (void)hl_backend_tree_finalize_slot_in(shared, slot, 1);
                continue;
            }
            (void)kill((pid_t)pid, SIGKILL);
            ++live;
        }
        if (live == 0) return 1;
        (void)poll(NULL, 0, 1);
    }
    return 0;
}
#else
#define hl_backend_tree_parent_barrier(shared, root_pid) 0
#endif

static void hl_backend_shape_top(const struct hl_backend_shape_form forms[HL_BACKEND_SHAPE_FORM_SLOTS],
                                 uint64_t top_key[HL_BACKEND_SHAPE_TOP_FORMS],
                                 uint64_t top_count[HL_BACKEND_SHAPE_TOP_FORMS]) {
    for (unsigned slot = 0; slot < HL_BACKEND_SHAPE_FORM_SLOTS; ++slot) {
        if (atomic_load_explicit(&forms[slot].state, memory_order_acquire) != 2) continue;
        uint64_t key = forms[slot].key;
        uint64_t count = atomic_load_explicit(&forms[slot].count, memory_order_relaxed);
        unsigned rank = 0;
        while (rank < HL_BACKEND_SHAPE_TOP_FORMS &&
               (top_count[rank] > count || (top_count[rank] == count && top_key[rank] <= key)))
            ++rank;
        if (rank == HL_BACKEND_SHAPE_TOP_FORMS) continue;
        for (unsigned move = HL_BACKEND_SHAPE_TOP_FORMS - 1; move > rank; --move) {
            top_key[move] = top_key[move - 1];
            top_count[move] = top_count[move - 1];
        }
        top_key[rank] = key;
        top_count[rank] = count;
    }
}

static void hl_backend_tree_summary_in(struct hl_backend_tree_shared *shared, struct hl_backend_tree_summary *summary) {
    memset(summary, 0, sizeof *summary);
    if (shared == NULL) return;
    summary->root_pid = (uint64_t)atomic_load_explicit(&shared->root_pid, memory_order_acquire);
    summary->duplicate_finalize = atomic_load_explicit(&shared->duplicate_finalize, memory_order_relaxed);
    uint64_t missing_claims = atomic_load_explicit(&shared->missing_claims, memory_order_relaxed);
    for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &shared->slots[index];
        int pid = atomic_load_explicit(&slot->pid, memory_order_acquire);
        if (pid <= 0) continue;
        ++summary->claimed;
        uint32_t lifecycle = atomic_load_explicit(&slot->lifecycle, memory_order_acquire);
        if (lifecycle == HL_BACKEND_TREE_COMPLETED) ++summary->completed;
        if (lifecycle == HL_BACKEND_TREE_ABNORMAL) ++summary->abnormal;
        summary->translated_entries += atomic_load_explicit(&slot->translated_entries, memory_order_relaxed);
        summary->interpreted_entries += atomic_load_explicit(&slot->interpreted_entries, memory_order_relaxed);
        summary->translated_steps += atomic_load_explicit(&slot->translated_steps, memory_order_relaxed);
        summary->interpreted_steps += atomic_load_explicit(&slot->interpreted_steps, memory_order_relaxed);
        summary->translations += atomic_load_explicit(&slot->translations, memory_order_relaxed);
        summary->map_hits += atomic_load_explicit(&slot->map_hits, memory_order_relaxed);
        summary->stw_retries += atomic_load_explicit(&slot->stw_retries, memory_order_relaxed);
        summary->irq_pending += atomic_load_explicit(&slot->irq_pending, memory_order_relaxed);
        for (uint32_t reason = 0; reason < HL_BACKEND_TREE_REASON_COUNT; ++reason)
            summary->reason[reason] += atomic_load_explicit(&slot->reason[reason], memory_order_relaxed);
        summary->reason_other += atomic_load_explicit(&slot->reason_other, memory_order_relaxed);
        for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_T_COUNT; ++kind)
            summary->translated_exit[kind] +=
                atomic_load_explicit(&slot->translated_exit[kind], memory_order_relaxed);
        for (uint32_t reason = 0; reason < HL_BACKEND_FALL_COUNT; ++reason)
            summary->translated_fall_stop[reason] +=
                atomic_load_explicit(&slot->translated_fall_stop[reason], memory_order_relaxed);
        summary->translated_stitch_jmp += atomic_load_explicit(&slot->translated_stitch_jmp, memory_order_relaxed);
        summary->translated_stitch_cond_fall +=
            atomic_load_explicit(&slot->translated_stitch_cond_fall, memory_order_relaxed);
        for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_I_COUNT; ++kind)
            summary->interpreter_entry[kind] +=
                atomic_load_explicit(&slot->interpreter_entry[kind], memory_order_relaxed);
        for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_S_COUNT; ++kind)
            summary->interpreter_stop[kind] +=
                atomic_load_explicit(&slot->interpreter_stop[kind], memory_order_relaxed);
        for (uint32_t family = 0; family < HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT; ++family) {
            summary->direct_edge[family] += atomic_load_explicit(&slot->direct_edge[family], memory_order_relaxed);
            for (uint32_t resolution = 0; resolution < HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT; ++resolution)
                summary->direct_edge_resolution[family][resolution] +=
                    atomic_load_explicit(&slot->direct_edge_resolution[family][resolution], memory_order_relaxed);
            summary->direct_edge_chained[family] +=
                atomic_load_explicit(&slot->direct_edge_chained[family], memory_order_relaxed);
            summary->direct_edge_dispatcher[family] +=
                atomic_load_explicit(&slot->direct_edge_dispatcher[family], memory_order_relaxed);
        }
        summary->jcc_taken_same_page += atomic_load_explicit(&slot->jcc_taken_same_page, memory_order_relaxed);
        summary->jcc_taken_cross_page += atomic_load_explicit(&slot->jcc_taken_cross_page, memory_order_relaxed);
        summary->jcc_taken_target_translated +=
            atomic_load_explicit(&slot->jcc_taken_target_translated, memory_order_relaxed);
        summary->jcc_taken_target_interpreted +=
            atomic_load_explicit(&slot->jcc_taken_target_interpreted, memory_order_relaxed);
        summary->jcc_taken_generation_current +=
            atomic_load_explicit(&slot->jcc_taken_generation_current, memory_order_relaxed);
        summary->jcc_taken_generation_retired +=
            atomic_load_explicit(&slot->jcc_taken_generation_retired, memory_order_relaxed);
        summary->jcc_taken_rel32 += atomic_load_explicit(&slot->jcc_taken_rel32, memory_order_relaxed);
        summary->jcc_taken_rel32_unreachable +=
            atomic_load_explicit(&slot->jcc_taken_rel32_unreachable, memory_order_relaxed);
        summary->jcc_taken_eligible += atomic_load_explicit(&slot->jcc_taken_eligible, memory_order_relaxed);
        summary->jcc_taken_ineligible += atomic_load_explicit(&slot->jcc_taken_ineligible, memory_order_relaxed);
        for (uint32_t family = 0; family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT; ++family)
            for (uint32_t disposition = 0; disposition < HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT; ++disposition)
                summary->would_link[family][disposition] +=
                    atomic_load_explicit(&slot->would_link[family][disposition], memory_order_relaxed);
        summary->family_jmem += atomic_load_explicit(&slot->family_jmem, memory_order_relaxed);
        summary->mixed_sse_executed += atomic_load_explicit(&slot->mixed_sse_executed, memory_order_relaxed);
        summary->mixed_sse_executed_transitions +=
            atomic_load_explicit(&slot->mixed_sse_executed_transitions, memory_order_relaxed);
        summary->mixed_sse_disabled_boundaries +=
            atomic_load_explicit(&slot->mixed_sse_disabled_boundaries, memory_order_relaxed);
        for (uint32_t kind = 0; kind < HL_BACKEND_FAMILY_DIV_KIND_COUNT; ++kind) {
            for (uint32_t outcome = 0; outcome < HL_BACKEND_FAMILY_DIV_OUTCOME_COUNT; ++outcome)
                summary->family_div[kind][outcome] +=
                    atomic_load_explicit(&slot->family_div[kind][outcome], memory_order_relaxed);
            summary->family_div_service64_completed[kind] +=
                atomic_load_explicit(&slot->family_div_service64_completed[kind], memory_order_relaxed);
        }
    }
    uint64_t jcc_links = atomic_load_explicit(&shared->jcc_links, memory_order_relaxed);
    unsigned jcc_family = HL_BACKEND_SHAPE_EDGE_JCC_TAKEN;
    summary->direct_edge[jcc_family] += jcc_links;
    summary->direct_edge_chained[jcc_family] += jcc_links;
    summary->direct_edge_resolution[jcc_family][HL_BACKEND_SHAPE_EDGE_MAPPED] += jcc_links;
    summary->jcc_taken_same_page += jcc_links;
    summary->jcc_taken_target_translated += jcc_links;
    summary->jcc_taken_generation_current += jcc_links;
    summary->jcc_taken_rel32 += jcc_links;
    summary->jcc_taken_eligible += jcc_links;
    summary->claimed += missing_claims;
    summary->missing = summary->claimed - summary->completed - summary->abnormal;
    summary->crossings = summary->translated_entries + summary->interpreted_entries;
    uint64_t reason_total = summary->reason_other;
    for (uint32_t reason = 0; reason < HL_BACKEND_TREE_REASON_COUNT; ++reason)
        reason_total += summary->reason[reason];
    /* A process killed inside run_block has an entry and no returned reason. Preserve exact accounting
       without pretending the interrupted backend supplied a reason code. */
    if (reason_total < summary->crossings) summary->reason_other += summary->crossings - reason_total;
    uint64_t translated_exit_total = 0;
    for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_T_COUNT; ++kind)
        translated_exit_total += summary->translated_exit[kind];
    if (translated_exit_total < summary->translated_entries)
        summary->translated_exit[HL_BACKEND_SHAPE_T_OTHER] += summary->translated_entries - translated_exit_total;
    uint64_t interpreter_entry_total = 0, interpreter_stop_total = 0;
    for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_I_COUNT; ++kind)
        interpreter_entry_total += summary->interpreter_entry[kind];
    for (uint32_t kind = 0; kind < HL_BACKEND_SHAPE_S_COUNT; ++kind)
        interpreter_stop_total += summary->interpreter_stop[kind];
    if (interpreter_entry_total < summary->interpreted_entries)
        summary->interpreter_entry[HL_BACKEND_SHAPE_I_OTHER] +=
            summary->interpreted_entries - interpreter_entry_total;
    if (interpreter_stop_total < summary->interpreted_entries)
        summary->interpreter_stop[HL_BACKEND_SHAPE_S_OTHER] += summary->interpreted_entries - interpreter_stop_total;
    for (uint32_t family = 0; family < HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT; ++family) {
        uint64_t resolutions = 0;
        for (uint32_t resolution = 0; resolution < HL_BACKEND_SHAPE_EDGE_RESOLUTION_COUNT; ++resolution)
            resolutions += summary->direct_edge_resolution[family][resolution];
        if (resolutions < summary->direct_edge[family])
            summary->direct_edge_resolution[family][HL_BACKEND_SHAPE_EDGE_INTERRUPTED] +=
                summary->direct_edge[family] - resolutions;
    }
    summary->fallback_form_total = atomic_load_explicit(&shared->fallback_form_total, memory_order_relaxed);
    summary->fallback_form_unique = atomic_load_explicit(&shared->fallback_form_unique, memory_order_relaxed);
    summary->fallback_form_overflow = atomic_load_explicit(&shared->fallback_form_overflow, memory_order_relaxed);
    summary->stop_form_total = atomic_load_explicit(&shared->stop_form_total, memory_order_relaxed);
    summary->stop_form_unique = atomic_load_explicit(&shared->stop_form_unique, memory_order_relaxed);
    summary->stop_form_overflow = atomic_load_explicit(&shared->stop_form_overflow, memory_order_relaxed);
    hl_backend_shape_top(shared->fallback_forms, summary->fallback_top_key, summary->fallback_top_count);
    hl_backend_shape_top(shared->stop_forms, summary->stop_top_key, summary->stop_top_count);
}

static void hl_backend_tree_summary(struct hl_backend_tree_summary *summary) {
    hl_backend_tree_summary_in(g_backend_tree, summary);
}

static int hl_backend_tree_format(struct hl_backend_tree_shared *shared, char *record, size_t capacity) {
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary_in(shared, &summary);
    return snprintf(
        record, capacity,
        "[diag] backend-tree version=1 root_pid=%llu claimed=%llu completed=%llu abnormal=%llu missing=%llu "
        "duplicate_finalize=%llu crossings=%llu translated_entries=%llu interpreted_entries=%llu "
        "translated_steps=%llu interpreted_steps=%llu translations=%llu map_hits=%llu stw_retries=%llu "
        "irq_pending=%llu reason0=%llu reason1=%llu reason2=%llu reason3=%llu reason4=%llu reason5=%llu "
        "reason6=%llu reason7=%llu reason8=%llu reason9=%llu reason10=%llu reason11=%llu reason12=%llu "
        "reason13=%llu reason14=%llu reason15=%llu reason_other=%llu\n",
        (unsigned long long)summary.root_pid, (unsigned long long)summary.claimed,
        (unsigned long long)summary.completed, (unsigned long long)summary.abnormal,
        (unsigned long long)summary.missing, (unsigned long long)summary.duplicate_finalize,
        (unsigned long long)summary.crossings, (unsigned long long)summary.translated_entries,
        (unsigned long long)summary.interpreted_entries, (unsigned long long)summary.translated_steps,
        (unsigned long long)summary.interpreted_steps, (unsigned long long)summary.translations,
        (unsigned long long)summary.map_hits, (unsigned long long)summary.stw_retries,
        (unsigned long long)summary.irq_pending, (unsigned long long)summary.reason[0],
        (unsigned long long)summary.reason[1], (unsigned long long)summary.reason[2],
        (unsigned long long)summary.reason[3], (unsigned long long)summary.reason[4],
        (unsigned long long)summary.reason[5], (unsigned long long)summary.reason[6],
        (unsigned long long)summary.reason[7], (unsigned long long)summary.reason[8],
        (unsigned long long)summary.reason[9], (unsigned long long)summary.reason[10],
        (unsigned long long)summary.reason[11], (unsigned long long)summary.reason[12],
        (unsigned long long)summary.reason[13], (unsigned long long)summary.reason[14],
        (unsigned long long)summary.reason[15], (unsigned long long)summary.reason_other);
}

static int hl_backend_shape_format(struct hl_backend_tree_shared *shared, char *record, size_t capacity) {
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary_in(shared, &summary);
    uint64_t translated_transfers = summary.translated_entries + summary.translated_stitch_jmp +
                                    summary.translated_stitch_cond_fall;
    for (unsigned family = 0; family < HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT; family++)
        translated_transfers += summary.direct_edge_chained[family];
    uint64_t family_div_total = summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_INLINE] +
                                summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED]
                                                  [HL_BACKEND_FAMILY_DIV_SERVICE64] +
                                summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE];
    uint64_t family_idiv_total = summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE] +
                                 summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED]
                                                   [HL_BACKEND_FAMILY_DIV_SERVICE64] +
                                 summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE];
    uint64_t family_total = summary.family_jmem + family_div_total + family_idiv_total;
    uint64_t fall_stop_total = 0;
    for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT; ++reason)
        fall_stop_total += summary.translated_fall_stop[reason];
    return snprintf(
        record, capacity,
        "[diag] backend-shape version=1 translated_entries=%llu translated_transfers=%llu "
        "t_fallthrough=%llu t_cond_taken=%llu t_cond_not_taken=%llu t_direct_jump=%llu "
        "t_direct_call=%llu t_return=%llu t_indirect_branch=%llu t_indirect_call=%llu t_syscall=%llu "
        "t_irq=%llu t_fault=%llu t_other=%llu fall_total=%llu fall_cap=%llu fall_decode=%llu "
        "fall_normal_to_sse2=%llu fall_sse2_to_normal=%llu fall_normal_to_fs=%llu "
        "fall_fs_to_normal=%llu fall_sse2_to_fs=%llu fall_fs_to_sse2=%llu "
        "fall_tl_no=%llu fall_displaced=%llu fall_fetch=%llu fall_riprel=%llu "
        "fall_fs_transaction=%llu fall_sse_riprel=%llu fall_other=%llu "
        "stitch_jmp=%llu stitch_cond_fall=%llu "
        "e_fall_total=%llu e_fall_mapped=%llu e_fall_unmapped=%llu e_fall_interrupted=%llu "
        "e_fall_chained=%llu e_fall_dispatcher=%llu "
        "e_jt_total=%llu e_jt_mapped=%llu e_jt_unmapped=%llu e_jt_interrupted=%llu "
        "e_jt_chained=%llu e_jt_dispatcher=%llu "
        "e_jn_total=%llu e_jn_mapped=%llu e_jn_unmapped=%llu e_jn_interrupted=%llu "
        "e_jn_chained=%llu e_jn_dispatcher=%llu "
        "e_jmp_total=%llu e_jmp_mapped=%llu e_jmp_unmapped=%llu e_jmp_interrupted=%llu "
        "e_jmp_chained=%llu e_jmp_dispatcher=%llu "
        "e_call_total=%llu e_call_mapped=%llu e_call_unmapped=%llu e_call_interrupted=%llu "
        "e_call_chained=%llu e_call_dispatcher=%llu "
        "jt_same_page=%llu jt_cross_page=%llu jt_target_translated=%llu jt_target_interpreted=%llu "
        "jt_generation_current=%llu jt_generation_retired=%llu jt_rel32=%llu jt_rel32_unreachable=%llu "
        "jt_eligible=%llu jt_ineligible=%llu interpreted_entries=%llu "
        "i_disabled=%llu i_image=%llu i_decode=%llu i_unsupported=%llu i_authority=%llu i_resource=%llu "
        "i_emit=%llu i_runtime_image=%llu i_runtime_bind=%llu i_other=%llu s_fallthrough=%llu "
        "s_cond_taken=%llu s_cond_not_taken=%llu s_direct_jump=%llu s_direct_call=%llu s_return=%llu "
        "s_indirect_branch=%llu s_indirect_call=%llu s_syscall=%llu s_irq=%llu s_fault=%llu "
        "s_service=%llu s_other=%llu fallback_total=%llu fallback_unique=%llu fallback_overflow=%llu "
        "stop_total=%llu stop_unique=%llu stop_overflow=%llu "
        "family_jmem=%llu family_div_total=%llu family_div_inline=%llu family_div_service64=%llu "
        "family_div_service64_completed=%llu family_div_de=%llu family_idiv_total=%llu "
        "family_idiv_inline=%llu family_idiv_service64=%llu family_idiv_service64_completed=%llu "
        "family_idiv_de=%llu family_total=%llu mixed_sse_executed=%llu "
        "mixed_sse_executed_transitions=%llu mixed_sse_disabled_boundaries=%llu "
        "fallback0_key=%llu fallback0_count=%llu fallback1_key=%llu fallback1_count=%llu "
        "fallback2_key=%llu fallback2_count=%llu fallback3_key=%llu fallback3_count=%llu "
        "fallback4_key=%llu fallback4_count=%llu fallback5_key=%llu fallback5_count=%llu "
        "fallback6_key=%llu fallback6_count=%llu fallback7_key=%llu fallback7_count=%llu "
        "stop0_key=%llu stop0_count=%llu stop1_key=%llu stop1_count=%llu "
        "stop2_key=%llu stop2_count=%llu stop3_key=%llu stop3_count=%llu "
        "stop4_key=%llu stop4_count=%llu stop5_key=%llu stop5_count=%llu "
        "stop6_key=%llu stop6_count=%llu stop7_key=%llu stop7_count=%llu\n",
        (unsigned long long)summary.translated_entries, (unsigned long long)translated_transfers,
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_NOT_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_JUMP],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_CALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_RETURN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_SYSCALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_IRQ],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_FAULT],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_OTHER],
        (unsigned long long)fall_stop_total,
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_CAP],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_DECODE],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_NORMAL_TO_SSE2],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_SSE2_TO_NORMAL],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_NORMAL_TO_FS],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_FS_TO_NORMAL],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_SSE2_TO_FS],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_FS_TO_SSE2],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_TL_NO],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_DISPLACED_UNSAFE],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_FETCH],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_RIPREL_LOWER],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_FS_TRANSACTION],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_SSE_RIPREL_LOWER],
        (unsigned long long)summary.translated_fall_stop[HL_BACKEND_FALL_OTHER],
        (unsigned long long)summary.translated_stitch_jmp,
        (unsigned long long)summary.translated_stitch_cond_fall,
        (unsigned long long)summary.direct_edge[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH]
                                                                  [HL_BACKEND_SHAPE_EDGE_MAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH]
                                                                  [HL_BACKEND_SHAPE_EDGE_UNMAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH]
                                                                  [HL_BACKEND_SHAPE_EDGE_INTERRUPTED],
        (unsigned long long)summary.direct_edge_chained[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH],
        (unsigned long long)summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_FALLTHROUGH],
        (unsigned long long)summary.direct_edge[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_MAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_UNMAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_INTERRUPTED],
        (unsigned long long)summary.direct_edge_chained[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN],
        (unsigned long long)summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN],
        (unsigned long long)summary.direct_edge[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_MAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_UNMAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN]
                                                                  [HL_BACKEND_SHAPE_EDGE_INTERRUPTED],
        (unsigned long long)summary.direct_edge_chained[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN],
        (unsigned long long)summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN],
        (unsigned long long)summary.direct_edge[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP]
                                                                  [HL_BACKEND_SHAPE_EDGE_MAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP]
                                                                  [HL_BACKEND_SHAPE_EDGE_UNMAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP]
                                                                  [HL_BACKEND_SHAPE_EDGE_INTERRUPTED],
        (unsigned long long)summary.direct_edge_chained[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP],
        (unsigned long long)summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP],
        (unsigned long long)summary.direct_edge[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL]
                                                                  [HL_BACKEND_SHAPE_EDGE_MAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL]
                                                                  [HL_BACKEND_SHAPE_EDGE_UNMAPPED],
        (unsigned long long)summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL]
                                                                  [HL_BACKEND_SHAPE_EDGE_INTERRUPTED],
        (unsigned long long)summary.direct_edge_chained[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL],
        (unsigned long long)summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_DIRECT_CALL],
        (unsigned long long)summary.jcc_taken_same_page, (unsigned long long)summary.jcc_taken_cross_page,
        (unsigned long long)summary.jcc_taken_target_translated,
        (unsigned long long)summary.jcc_taken_target_interpreted,
        (unsigned long long)summary.jcc_taken_generation_current,
        (unsigned long long)summary.jcc_taken_generation_retired, (unsigned long long)summary.jcc_taken_rel32,
        (unsigned long long)summary.jcc_taken_rel32_unreachable,
        (unsigned long long)summary.jcc_taken_eligible, (unsigned long long)summary.jcc_taken_ineligible,
        (unsigned long long)summary.interpreted_entries,
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_DISABLED],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_IMAGE],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_DECODE],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_UNSUPPORTED],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_AUTHORITY],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_RESOURCE],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_EMIT],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_RUNTIME_IMAGE],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_RUNTIME_BIND],
        (unsigned long long)summary.interpreter_entry[HL_BACKEND_SHAPE_I_OTHER],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_FALLTHROUGH],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_COND_TAKEN],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_COND_NOT_TAKEN],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_DIRECT_JUMP],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_DIRECT_CALL],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_RETURN],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_INDIRECT_BRANCH],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_INDIRECT_CALL],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_SYSCALL],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_IRQ],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_FAULT],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_SERVICE],
        (unsigned long long)summary.interpreter_stop[HL_BACKEND_SHAPE_S_OTHER],
        (unsigned long long)summary.fallback_form_total, (unsigned long long)summary.fallback_form_unique,
        (unsigned long long)summary.fallback_form_overflow, (unsigned long long)summary.stop_form_total,
        (unsigned long long)summary.stop_form_unique, (unsigned long long)summary.stop_form_overflow,
        (unsigned long long)summary.family_jmem, (unsigned long long)family_div_total,
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_INLINE],
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64],
        (unsigned long long)summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_UNSIGNED],
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE],
        (unsigned long long)family_idiv_total,
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE],
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64],
        (unsigned long long)summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_SIGNED],
        (unsigned long long)summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE],
        (unsigned long long)family_total, (unsigned long long)summary.mixed_sse_executed,
        (unsigned long long)summary.mixed_sse_executed_transitions,
        (unsigned long long)summary.mixed_sse_disabled_boundaries,
        (unsigned long long)summary.fallback_top_key[0], (unsigned long long)summary.fallback_top_count[0],
        (unsigned long long)summary.fallback_top_key[1], (unsigned long long)summary.fallback_top_count[1],
        (unsigned long long)summary.fallback_top_key[2], (unsigned long long)summary.fallback_top_count[2],
        (unsigned long long)summary.fallback_top_key[3], (unsigned long long)summary.fallback_top_count[3],
        (unsigned long long)summary.fallback_top_key[4], (unsigned long long)summary.fallback_top_count[4],
        (unsigned long long)summary.fallback_top_key[5], (unsigned long long)summary.fallback_top_count[5],
        (unsigned long long)summary.fallback_top_key[6], (unsigned long long)summary.fallback_top_count[6],
        (unsigned long long)summary.fallback_top_key[7], (unsigned long long)summary.fallback_top_count[7],
        (unsigned long long)summary.stop_top_key[0], (unsigned long long)summary.stop_top_count[0],
        (unsigned long long)summary.stop_top_key[1], (unsigned long long)summary.stop_top_count[1],
        (unsigned long long)summary.stop_top_key[2], (unsigned long long)summary.stop_top_count[2],
        (unsigned long long)summary.stop_top_key[3], (unsigned long long)summary.stop_top_count[3],
        (unsigned long long)summary.stop_top_key[4], (unsigned long long)summary.stop_top_count[4],
        (unsigned long long)summary.stop_top_key[5], (unsigned long long)summary.stop_top_count[5],
        (unsigned long long)summary.stop_top_key[6], (unsigned long long)summary.stop_top_count[6],
        (unsigned long long)summary.stop_top_key[7], (unsigned long long)summary.stop_top_count[7]);
}

static int hl_backend_would_link_format(struct hl_backend_tree_shared *shared, char *record, size_t capacity) {
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary_in(shared, &summary);
    uint64_t candidate[HL_BACKEND_WOULD_LINK_FAMILY_COUNT] = {0};
    for (unsigned family = 0; family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT; ++family)
        for (unsigned disposition = 0; disposition < HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT; ++disposition)
            candidate[family] += summary.would_link[family][disposition];
#define WL_ARGS(family)                                                                                               \
    (unsigned long long)candidate[family],                                                                            \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_ELIGIBLE],                               \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED],                      \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_CROSS_PAGE],                             \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED],                        \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED],                    \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_GENERATION],                             \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_TARGET_PAGE],                            \
        (unsigned long long)summary.would_link[family][HL_BACKEND_WOULD_LINK_REL32]
    int formatted = snprintf(
        record, capacity,
        "[diag] backend-would-link version=1 "
        "fall_candidate=%llu fall_eligible=%llu fall_source_unresolved=%llu fall_cross_page=%llu "
        "fall_target_unmapped=%llu fall_target_untranslated=%llu fall_generation=%llu "
        "fall_target_page=%llu fall_rel32=%llu "
        "jmp_candidate=%llu jmp_eligible=%llu jmp_source_unresolved=%llu jmp_cross_page=%llu "
        "jmp_target_unmapped=%llu jmp_target_untranslated=%llu jmp_generation=%llu "
        "jmp_target_page=%llu jmp_rel32=%llu "
        "call_candidate=%llu call_eligible=%llu call_source_unresolved=%llu call_cross_page=%llu "
        "call_target_unmapped=%llu call_target_untranslated=%llu call_generation=%llu "
        "call_target_page=%llu call_rel32=%llu\n",
        WL_ARGS(HL_BACKEND_WOULD_LINK_FALLTHROUGH), WL_ARGS(HL_BACKEND_WOULD_LINK_DIRECT_JUMP),
        WL_ARGS(HL_BACKEND_WOULD_LINK_DIRECT_CALL));
#undef WL_ARGS
    return formatted;
}

void hl_target_backend_tree_reap_report(void *opaque, size_t shared_size, hl_linux_abi *box) {
    struct hl_backend_tree_shared *shared = opaque;
    if (shared == NULL || shared_size != sizeof *shared || box == NULL) return;
    int root_pid = atomic_load_explicit(&shared->root_pid, memory_order_acquire);
    if (root_pid <= 0 || !hl_backend_tree_parent_barrier(shared, root_pid)) return;
    uint32_t expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&shared->reported, &expected, 1, memory_order_acq_rel,
                                                 memory_order_relaxed))
        return;
    char record[8192];
    int formatted = hl_backend_tree_format(shared, record, sizeof record);
    if (formatted <= 0 || (size_t)formatted >= sizeof record) return;
    int shape = hl_backend_shape_format(shared, record + formatted, sizeof record - (size_t)formatted);
    if (shape <= 0 || (size_t)shape >= sizeof record - (size_t)formatted) return;
    formatted += shape;
    int would_link = hl_backend_would_link_format(shared, record + formatted, sizeof record - (size_t)formatted);
    if (would_link <= 0 || (size_t)would_link >= sizeof record - (size_t)formatted) return;
    formatted += would_link;
    size_t offset = 0;
    while (offset < (size_t)formatted) {
        int64_t written = hl_linux_write(box, STDERR_FILENO, record + offset, (size_t)formatted - offset);
        if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)formatted - offset) return;
        offset += (size_t)written;
    }
}

static _Noreturn void hl_backend_tree_abnormal_exit(int status) {
    (void)hl_backend_tree_finalize(1);
    _exit(status);
}

#if !defined(_WIN32)
static int hl_backend_tree_wait(pid_t child, int reap_as_abnormal) {
    int status = 0;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) return -1;
    }
    if (reap_as_abnormal) hl_backend_tree_reaped((int)child);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 255;
}

static int hl_backend_tree_test_scenario(uint32_t scenario, const hl_host_services *host) {
    hl_backend_tree_begin(1, host);
    if (g_backend_tree_self == NULL) return 10;
    if (scenario == 4) {
        if (!hl_backend_tree_finalize(0) || hl_backend_tree_finalize(0)) return 41;
        struct hl_backend_tree_summary summary;
        hl_backend_tree_summary(&summary);
        return summary.claimed == 1 && summary.completed == 1 && summary.duplicate_finalize == 1 ? 0 : 42;
    }
    if (scenario == 11) {
        for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT; ++reason) {
            hl_backend_tree_run_begin(1, 1);
            hl_backend_tree_translated_exit(HL_BACKEND_SHAPE_T_FALLTHROUGH, 0, 0);
            hl_backend_tree_translated_fall_stop(reason);
            hl_backend_tree_reason(R_BRANCH);
        }
        /* An out-of-range emitted value must remain attributable without creating an unbounded bucket. */
        hl_backend_tree_run_begin(1, 1);
        hl_backend_tree_translated_exit(HL_BACKEND_SHAPE_T_FALLTHROUGH, 0, 0);
        hl_backend_tree_translated_fall_stop(HL_BACKEND_FALL_COUNT + 7);
        hl_backend_tree_reason(R_BRANCH);
        (void)hl_backend_tree_finalize(0);
        struct hl_backend_tree_summary summary;
        hl_backend_tree_summary(&summary);
        uint64_t reasons = 0;
        for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT; ++reason)
            reasons += summary.translated_fall_stop[reason];
        if (summary.claimed != 1 || summary.completed != 1 ||
            summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH] != HL_BACKEND_FALL_COUNT + 1 ||
            reasons != summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH])
            return 43;
        for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT - 1; ++reason)
            if (summary.translated_fall_stop[reason] != 1) return 44;
        return summary.translated_fall_stop[HL_BACKEND_FALL_OTHER] == 2 ? 0 : 45;
    }
    struct hl_backend_tree_slot *child_birth = hl_backend_tree_prepare_fork();
    pid_t child = fork();
    if (!(scenario == 6 && child == 0)) hl_backend_tree_after_fork(child, child_birth);
    if (child < 0) return 11;
    if (child == 0) {
        hl_backend_tree_run_begin(0, 0);
        hl_backend_tree_interpreted_steps(3);
        if (scenario == 8) {
            hl_backend_tree_interpreter_entry(HL_BACKEND_SHAPE_I_UNSUPPORTED, 17);
            hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_SERVICE, 23);
        }
        if (scenario == 7) _exit(0); /* backend entry whose process dies before returning a reason */
        hl_backend_tree_reason(1);
        if (scenario == 10) {
            for (unsigned repeat = 0; repeat < 2; ++repeat) hl_backend_tree_family_jmem();
            for (unsigned repeat = 0; repeat < 2; ++repeat) {
                hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_UNSIGNED,
                                           HL_BACKEND_FAMILY_DIV_SERVICE64);
                hl_backend_tree_family_div_service64_completed(HL_BACKEND_FAMILY_DIV_UNSIGNED);
                hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_SIGNED, HL_BACKEND_FAMILY_DIV_DE);
            }
            for (unsigned form = 0; form < 4; ++form) {
                if (form != 0) hl_backend_tree_run_begin(0, 0);
                hl_backend_tree_interpreter_entry(HL_BACKEND_SHAPE_I_UNSUPPORTED, 100 + form);
                hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_SERVICE, 200 + form);
            }
        }
        if (scenario == 1 || scenario == 8 || scenario == 9 || scenario == 10) {
            struct hl_backend_tree_slot *grandchild_birth = hl_backend_tree_prepare_fork();
            pid_t grandchild = fork();
            hl_backend_tree_after_fork(grandchild, grandchild_birth);
            if (grandchild < 0) hl_backend_tree_abnormal_exit(12);
            if (grandchild == 0) {
                hl_backend_tree_run_begin(1, 5);
                if (scenario == 8) {
                    hl_backend_tree_translated_exit(HL_BACKEND_SHAPE_T_COND_TAKEN, 1, 2);
                    hl_backend_tree_direct_edge(HL_BACKEND_SHAPE_EDGE_JCC_TAKEN, 1);
                    hl_backend_tree_direct_edge_resolution(HL_BACKEND_SHAPE_EDGE_JCC_TAKEN,
                                                           HL_BACKEND_SHAPE_EDGE_MAPPED, 1, 1, 1, 1);
                }
                if (scenario == 9) {
                    hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
                                               HL_BACKEND_WOULD_LINK_ELIGIBLE);
                    hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_CALL,
                                               HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED);
                }
                if (scenario == 10) {
                    for (unsigned repeat = 0; repeat < 3; ++repeat) {
                        hl_backend_tree_family_jmem();
                        hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_UNSIGNED,
                                                   HL_BACKEND_FAMILY_DIV_DE);
                        hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_SIGNED,
                                                   HL_BACKEND_FAMILY_DIV_INLINE);
                    }
                    for (unsigned form = 0; form < 4; ++form) {
                        hl_backend_tree_run_begin(0, 0);
                        hl_backend_tree_interpreter_entry(HL_BACKEND_SHAPE_I_UNSUPPORTED, 104 + form);
                        hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_SERVICE, 204 + form);
                    }
                }
                hl_backend_tree_reason(5);
                (void)hl_backend_tree_finalize(0);
                _exit(0);
            }
            if (hl_backend_tree_wait(grandchild, 1) != 0) hl_backend_tree_abnormal_exit(13);
        }
        if (scenario == 9)
            hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_FALLTHROUGH,
                                       HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED);
        if (scenario == 2 || scenario == 3 || scenario == 6)
            _exit(0); /* exercise parent reaping, missing, and pre-child-execution publication separately */
        if (scenario == 5) hl_backend_tree_abnormal_exit(0);
        (void)hl_backend_tree_finalize(0);
        _exit(0);
    }
    int child_status = hl_backend_tree_wait(child, scenario == 2 || scenario == 5 || scenario == 6 || scenario == 7);
    if (child_status != 0) return 14;
    hl_backend_tree_run_begin(1, 7);
    if (scenario == 8) {
        hl_backend_tree_translated_exit(HL_BACKEND_SHAPE_T_DIRECT_JUMP, 3, 4);
        hl_backend_tree_direct_edge(HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP, 0);
        hl_backend_tree_direct_edge_resolution(HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP,
                                               HL_BACKEND_SHAPE_EDGE_UNMAPPED, 0, 0, 0, 0);
    }
    if (scenario == 9) {
        hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_FALLTHROUGH, HL_BACKEND_WOULD_LINK_CROSS_PAGE);
        hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
                                   HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED);
        hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_CALL, HL_BACKEND_WOULD_LINK_GENERATION);
        hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_CALL, HL_BACKEND_WOULD_LINK_TARGET_PAGE);
        hl_backend_tree_would_link(HL_BACKEND_WOULD_LINK_DIRECT_CALL, HL_BACKEND_WOULD_LINK_REL32);
    }
    if (scenario == 10) {
        hl_backend_tree_family_jmem();
        hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_UNSIGNED, HL_BACKEND_FAMILY_DIV_INLINE);
        hl_backend_tree_family_div(HL_BACKEND_FAMILY_DIV_SIGNED, HL_BACKEND_FAMILY_DIV_SERVICE64);
        hl_backend_tree_family_div_service64_completed(HL_BACKEND_FAMILY_DIV_SIGNED);
        for (unsigned form = 0; form < 4; ++form) {
            hl_backend_tree_run_begin(0, 0);
            hl_backend_tree_interpreter_entry(HL_BACKEND_SHAPE_I_UNSUPPORTED, 108 + form);
            hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_SERVICE, 208 + form);
        }
    }
    hl_backend_tree_reason(0);
    (void)hl_backend_tree_finalize(0);
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary(&summary);
    if (scenario == 0)
        return summary.claimed == 2 && summary.completed == 2 && summary.abnormal == 0 && summary.missing == 0 &&
                       summary.crossings == 2 && summary.translated_entries == 1 && summary.interpreted_entries == 1 &&
                       summary.reason[0] == 1 && summary.reason[1] == 1 && summary.reason_other == 0
                   ? 0
                   : 20;
    if (scenario == 1)
        return summary.claimed == 3 && summary.completed == 3 && summary.abnormal == 0 && summary.missing == 0 &&
                       summary.crossings == 3 && summary.reason[0] == 1 && summary.reason[1] == 1 &&
                       summary.reason[5] == 1 && summary.reason_other == 0
                   ? 0
                   : 30;
    if (scenario == 2)
        return summary.claimed == 2 && summary.completed == 1 && summary.abnormal == 1 && summary.missing == 0 ? 0 : 31;
    if (scenario == 3)
        return summary.claimed == 2 && summary.completed == 1 && summary.abnormal == 0 && summary.missing == 1 ? 0 : 32;
    if (scenario == 5)
        return summary.claimed == 2 && summary.completed == 1 && summary.abnormal == 1 && summary.missing == 0 &&
                       summary.duplicate_finalize == 0
                   ? 0
                   : 33;
    if (scenario == 6)
        return summary.claimed == 2 && summary.completed == 1 && summary.abnormal == 1 && summary.missing == 0 ? 0 : 34;
    if (scenario == 7)
        return summary.claimed == 2 && summary.completed == 1 && summary.abnormal == 1 && summary.missing == 0 &&
                       summary.crossings == 2 && summary.reason[0] == 1 && summary.reason[1] == 0 &&
                       summary.reason_other == 1
                   ? 0
                   : 35;
    if (scenario == 8)
        return summary.claimed == 3 && summary.completed == 3 && summary.translated_entries == 2 &&
                       summary.interpreted_entries == 1 &&
                       summary.translated_exit[HL_BACKEND_SHAPE_T_COND_TAKEN] == 1 &&
                       summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_JUMP] == 1 &&
                       summary.translated_stitch_jmp == 4 && summary.translated_stitch_cond_fall == 6 &&
                       summary.direct_edge[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN] == 1 &&
                       summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN]
                                                             [HL_BACKEND_SHAPE_EDGE_MAPPED] == 1 &&
                       summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_JCC_TAKEN] == 1 &&
                       summary.direct_edge[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP] == 1 &&
                       summary.direct_edge_resolution[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP]
                                                             [HL_BACKEND_SHAPE_EDGE_UNMAPPED] == 1 &&
                       summary.direct_edge_dispatcher[HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP] == 1 &&
                       summary.jcc_taken_same_page == 1 && summary.jcc_taken_target_translated == 1 &&
                       summary.jcc_taken_generation_current == 1 && summary.jcc_taken_rel32 == 1 &&
                       summary.jcc_taken_eligible == 1 && summary.jcc_taken_ineligible == 0 &&
                       summary.interpreter_entry[HL_BACKEND_SHAPE_I_UNSUPPORTED] == 1 &&
                       summary.interpreter_stop[HL_BACKEND_SHAPE_S_SERVICE] == 1 &&
                       summary.fallback_form_total == 1 && summary.fallback_top_key[0] == 17 &&
                       summary.fallback_top_count[0] == 1 && summary.stop_form_total == 1 &&
                       summary.stop_top_key[0] == 23 && summary.stop_top_count[0] == 1
                   ? 0
                   : 36;
    if (scenario == 9) {
        uint64_t candidates[HL_BACKEND_WOULD_LINK_FAMILY_COUNT] = {0};
        for (unsigned family = 0; family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT; ++family)
            for (unsigned disposition = 0; disposition < HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT; ++disposition)
                candidates[family] += summary.would_link[family][disposition];
        return candidates[HL_BACKEND_WOULD_LINK_FALLTHROUGH] == 2 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_FALLTHROUGH]
                                                 [HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED] == 1 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_FALLTHROUGH]
                                                 [HL_BACKEND_WOULD_LINK_CROSS_PAGE] == 1 &&
                       candidates[HL_BACKEND_WOULD_LINK_DIRECT_JUMP] == 2 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_JUMP]
                                                 [HL_BACKEND_WOULD_LINK_ELIGIBLE] == 1 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_JUMP]
                                                 [HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED] == 1 &&
                       candidates[HL_BACKEND_WOULD_LINK_DIRECT_CALL] == 4 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_CALL]
                                                 [HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED] == 1 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_CALL]
                                                 [HL_BACKEND_WOULD_LINK_GENERATION] == 1 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_CALL]
                                                 [HL_BACKEND_WOULD_LINK_TARGET_PAGE] == 1 &&
                       summary.would_link[HL_BACKEND_WOULD_LINK_DIRECT_CALL]
                                                 [HL_BACKEND_WOULD_LINK_REL32] == 1
                   ? 0
                   : 37;
    }
    if (scenario == 10) {
        return summary.family_jmem == 6 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_INLINE] == 1 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] == 2 &&
                       summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_UNSIGNED] == 2 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE] == 3 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE] == 3 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] == 1 &&
                       summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_SIGNED] == 1 &&
                       summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE] == 2 &&
                       summary.fallback_form_total == 12 && summary.fallback_form_unique == 12 &&
                       summary.fallback_top_count[7] == 1 && summary.stop_form_total == 12 &&
                       summary.stop_form_unique == 12 && summary.stop_top_count[7] == 1
                   ? 0
                   : 38;
    }
    return 40;
}
#endif

HL_API int HL_BACKEND_TREE_TEST_NAME(uint32_t scenario) {
#if defined(_WIN32)
    (void)scenario;
    return 4;
#else
    return hl_backend_tree_test_scenario(scenario, effective_host_services());
#endif
}

#else

/* Hook-disabled translit_shape_exit calls compile to no-ops, but retaining the symbolic kinds keeps the
   hook and production control-flow source identical. */
enum {
    HL_BACKEND_SHAPE_T_FALLTHROUGH,
    HL_BACKEND_SHAPE_T_COND_TAKEN,
    HL_BACKEND_SHAPE_T_COND_NOT_TAKEN,
    HL_BACKEND_SHAPE_T_DIRECT_JUMP,
    HL_BACKEND_SHAPE_T_DIRECT_CALL,
    HL_BACKEND_SHAPE_T_RETURN,
    HL_BACKEND_SHAPE_T_INDIRECT_BRANCH,
    HL_BACKEND_SHAPE_T_INDIRECT_CALL,
    HL_BACKEND_SHAPE_T_SYSCALL,
    HL_BACKEND_SHAPE_T_IRQ,
    HL_BACKEND_SHAPE_T_FAULT,
    HL_BACKEND_SHAPE_T_OTHER,
    HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY,
    HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY,
    HL_BACKEND_SHAPE_T_COUNT,
};

enum {
    HL_BACKEND_SHAPE_S_FALLTHROUGH,
    HL_BACKEND_SHAPE_S_COND_TAKEN,
    HL_BACKEND_SHAPE_S_COND_NOT_TAKEN,
    HL_BACKEND_SHAPE_S_DIRECT_JUMP,
    HL_BACKEND_SHAPE_S_DIRECT_CALL,
    HL_BACKEND_SHAPE_S_RETURN,
    HL_BACKEND_SHAPE_S_INDIRECT_BRANCH,
    HL_BACKEND_SHAPE_S_INDIRECT_CALL,
    HL_BACKEND_SHAPE_S_SYSCALL,
    HL_BACKEND_SHAPE_S_IRQ,
    HL_BACKEND_SHAPE_S_FAULT,
    HL_BACKEND_SHAPE_S_SERVICE,
    HL_BACKEND_SHAPE_S_OTHER,
    HL_BACKEND_SHAPE_S_INDIRECT_BRANCH_MEMORY,
    HL_BACKEND_SHAPE_S_INDIRECT_CALL_MEMORY,
    HL_BACKEND_SHAPE_S_COUNT,
};

#define HL_BACKEND_TREE_REASON_COUNT 20u

/* Product diagnostics retain the mixed-builder execution facts and the seven JCC-IBTC facts needed to
   authenticate untimed ON/OFF proofs.  The anonymous mapping is created by the launch lifecycle only when typed
   diagnostics are enabled, before the guest root can fork, and is inherited MAP_SHARED by every guest
   process.  Ordinary launches allocate nothing; their emitted code and completed-terminal path are
   unchanged because no mixed-profile marker exists without diagnostics. */
enum hl_backend_mixed_sse_lifecycle {
    HL_BACKEND_MIXED_SSE_CLAIMED = 1,
    HL_BACKEND_MIXED_SSE_COMPLETED = 2,
    HL_BACKEND_MIXED_SSE_ABNORMAL = 3,
};

#define HL_BACKEND_MIXED_SSE_SLOTS 4096u

struct hl_backend_tree_slot {
    _Atomic int pid;
    _Atomic uint64_t birth_ns;
    _Atomic uint32_t lifecycle;
};

struct hl_backend_mixed_sse_shared {
    _Atomic int root_pid;
    _Atomic uint64_t missing_claims;
    _Atomic uint64_t duplicate_finalize;
    _Atomic uint32_t reported;
    _Atomic uint64_t executed;
    _Atomic uint64_t executed_transitions;
    _Atomic uint64_t disabled_boundaries;
    _Atomic uint64_t translated_entries;
    _Atomic uint64_t interpreted_entries;
    _Atomic uint64_t translated_steps;
    _Atomic uint64_t interpreted_steps;
    _Atomic uint64_t reason[HL_BACKEND_TREE_REASON_COUNT];
    _Atomic uint64_t reason_other;
    _Atomic uint64_t translated_exit[HL_BACKEND_SHAPE_T_COUNT];
    _Atomic uint64_t interpreter_stop[HL_BACKEND_SHAPE_S_COUNT];
    _Atomic uint64_t call_sim_eligible;
    _Atomic uint64_t call_sim_hit;
    _Atomic uint64_t call_sim_miss;
    _Atomic uint64_t call_sim_fill;
    _Atomic uint64_t call_sim_decline_irq;
    _Atomic uint64_t call_sim_decline_stub;
    _Atomic uint64_t call_sim_decline_authority;
    _Atomic uint64_t jcc_ibtc_emitted;
    _Atomic uint64_t jcc_ibtc_hits;
    _Atomic uint64_t jcc_ibtc_misses;
    _Atomic uint64_t jcc_ibtc_irq;
    _Atomic uint64_t jcc_ibtc_fills;
    _Atomic uint64_t jcc_ibtc_suppressed;
    _Atomic uint64_t jcc_ibtc_invalid_refusals;
    _Atomic uint64_t direct_jmp_ibtc_emitted;
    _Atomic uint64_t direct_jmp_ibtc_hits;
    _Atomic uint64_t direct_jmp_ibtc_misses;
    _Atomic uint64_t direct_jmp_ibtc_irq;
    _Atomic uint64_t direct_jmp_ibtc_fills;
    _Atomic uint64_t direct_jmp_ibtc_suppressed;
    _Atomic uint64_t direct_jmp_ibtc_invalid_refusals;
    /* Immutable after root initialization and before any guest fork. */
    uint32_t jcc_ibtc_enabled;
    uint32_t direct_jmp_ibtc_enabled;
    struct hl_backend_tree_slot slots[HL_BACKEND_MIXED_SSE_SLOTS];
};

#if ATOMIC_INT_LOCK_FREE != 2
#error "production mixed-SSE census requires lock-free 32-bit atomics"
#endif
#if (defined(_WIN32) && ATOMIC_LLONG_LOCK_FREE != 2) || (!defined(_WIN32) && ATOMIC_LONG_LOCK_FREE != 2)
#error "production mixed-SSE census requires lock-free 64-bit atomics"
#endif

_Static_assert(sizeof(struct hl_backend_tree_slot) == 24,
               "production mixed-SSE lifecycle slots must remain compact");

static struct hl_backend_mixed_sse_shared *g_backend_mixed_sse;
static struct hl_backend_tree_slot *g_backend_mixed_sse_self;

static struct hl_backend_tree_slot *hl_backend_mixed_sse_reserve(void) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    for (uint32_t index = 0; index < HL_BACKEND_MIXED_SSE_SLOTS; ++index) {
        int expected = 0;
        struct hl_backend_tree_slot *slot = &census->slots[index];
        if (!atomic_compare_exchange_strong_explicit(&slot->pid, &expected, -1, memory_order_acq_rel,
                                                     memory_order_relaxed))
            continue;
        atomic_store_explicit(&slot->lifecycle, HL_BACKEND_MIXED_SSE_CLAIMED, memory_order_relaxed);
        return slot;
    }
    return NULL;
}

static void hl_backend_mixed_sse_publish(struct hl_backend_tree_slot *slot, int pid) {
    uint64_t birth_ns = 0;
    if (slot == NULL || pid <= 0 || !hl_host_process_start_time_ns(pid, &birth_ns)) return;
    atomic_store_explicit(&slot->birth_ns, birth_ns, memory_order_relaxed);
    atomic_store_explicit(&slot->pid, pid, memory_order_release);
}

static struct hl_backend_tree_slot *hl_backend_mixed_sse_claim(int pid) {
    struct hl_backend_tree_slot *slot = hl_backend_mixed_sse_reserve();
    if (slot == NULL) {
        if (g_backend_mixed_sse != NULL)
            atomic_fetch_add_explicit(&g_backend_mixed_sse->missing_claims, 1, memory_order_relaxed);
        return NULL;
    }
    hl_backend_mixed_sse_publish(slot, pid);
    return slot;
}

size_t hl_target_backend_tree_shared_size(int enabled) {
#if defined(_WIN32)
    (void)enabled;
    return 0;
#else
    return enabled ? sizeof(struct hl_backend_mixed_sse_shared) : 0;
#endif
}

void hl_target_backend_tree_child_begin(void *shared, size_t shared_size) {
    g_backend_mixed_sse = shared_size == sizeof(struct hl_backend_mixed_sse_shared) ? shared : NULL;
    g_backend_mixed_sse_self = NULL;
    if (g_backend_mixed_sse == NULL) return;
    /* The anonymous mapping is already zeroed. Publish the root before any guest instruction or fork. */
    int self = (int)getpid();
    atomic_store_explicit(&g_backend_mixed_sse->root_pid, self, memory_order_release);
    g_backend_mixed_sse->jcc_ibtc_enabled = hl_option_get("HL_TRANSLIT_JCC_IBTC_DISABLE") == NULL;
    g_backend_mixed_sse->direct_jmp_ibtc_enabled =
        !hl_option_flag_value("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", 1);
    g_backend_mixed_sse_self = hl_backend_mixed_sse_claim(self);
}

static int hl_backend_mixed_sse_finalize_slot(struct hl_backend_tree_slot *slot, int abnormal) {
    if (slot == NULL) return 0;
    uint32_t expected = HL_BACKEND_MIXED_SSE_CLAIMED;
    uint32_t completed = abnormal ? HL_BACKEND_MIXED_SSE_ABNORMAL : HL_BACKEND_MIXED_SSE_COMPLETED;
    if (atomic_compare_exchange_strong_explicit(&slot->lifecycle, &expected, completed, memory_order_acq_rel,
                                                memory_order_acquire))
        return 1;
    if (g_backend_mixed_sse != NULL)
        atomic_fetch_add_explicit(&g_backend_mixed_sse->duplicate_finalize, 1, memory_order_relaxed);
    return 0;
}

static int hl_backend_tree_finalize(int abnormal) {
    return hl_backend_mixed_sse_finalize_slot(g_backend_mixed_sse_self, abnormal);
}

static int hl_backend_tree_is_finalized(void) {
    return g_backend_mixed_sse_self != NULL &&
           atomic_load_explicit(&g_backend_mixed_sse_self->lifecycle, memory_order_acquire) !=
               HL_BACKEND_MIXED_SSE_CLAIMED;
}

static struct hl_backend_tree_slot *hl_backend_tree_prepare_fork(void) {
    return hl_backend_mixed_sse_reserve();
}

static void hl_backend_tree_after_fork(pid_t result, struct hl_backend_tree_slot *birth) {
    if (g_backend_mixed_sse == NULL) return;
    if (result < 0) {
        if (birth != NULL) {
            atomic_store_explicit(&birth->lifecycle, 0, memory_order_relaxed);
            atomic_store_explicit(&birth->pid, 0, memory_order_release);
        }
        return;
    }
    if (birth == NULL) {
        if (result > 0) atomic_fetch_add_explicit(&g_backend_mixed_sse->missing_claims, 1, memory_order_relaxed);
        if (result == 0) g_backend_mixed_sse_self = NULL;
        return;
    }
    if (result == 0) {
        g_backend_mixed_sse_self = birth;
        hl_backend_mixed_sse_publish(birth, (int)getpid());
    } else
        hl_backend_mixed_sse_publish(birth, (int)result);
}

static void hl_backend_tree_reaped(int pid) {
    if (g_backend_mixed_sse == NULL || pid <= 0) return;
    for (uint32_t index = 0; index < HL_BACKEND_MIXED_SSE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &g_backend_mixed_sse->slots[index];
        if (atomic_load_explicit(&slot->pid, memory_order_acquire) != pid) continue;
        if (atomic_load_explicit(&slot->lifecycle, memory_order_acquire) == HL_BACKEND_MIXED_SSE_CLAIMED)
            (void)hl_backend_mixed_sse_finalize_slot(slot, 1);
        return;
    }
}

#if !defined(_WIN32)
static int hl_backend_mixed_sse_process_can_mutate(const struct hl_backend_tree_slot *slot, int pid) {
    hl_host_process_info process;
    uint64_t expected = atomic_load_explicit(&slot->birth_ns, memory_order_acquire);
    return expected != 0 && hl_host_process_read(pid, &process) && process.start_time_ns == expected &&
           process.state != 'Z' && process.state != 'X';
}

static int hl_backend_mixed_sse_parent_barrier(struct hl_backend_mixed_sse_shared *census, int root_pid) {
    for (unsigned round = 0; round < 2000; ++round) {
        unsigned live = 0;
        for (uint32_t index = 0; index < HL_BACKEND_MIXED_SSE_SLOTS; ++index) {
            struct hl_backend_tree_slot *slot = &census->slots[index];
            int pid = atomic_load_explicit(&slot->pid, memory_order_acquire);
            if (pid == -1) {
                ++live;
                continue;
            }
            if (pid <= 0) continue;
            if (pid == root_pid || !hl_backend_mixed_sse_process_can_mutate(slot, pid)) {
                if (atomic_load_explicit(&slot->lifecycle, memory_order_acquire) == HL_BACKEND_MIXED_SSE_CLAIMED)
                    (void)hl_backend_mixed_sse_finalize_slot(slot, 1);
                continue;
            }
            (void)kill((pid_t)pid, SIGKILL);
            ++live;
        }
        if (live == 0) return 1;
        (void)poll(NULL, 0, 1);
    }
    return 0;
}
#else
#define hl_backend_mixed_sse_parent_barrier(census, root_pid) 0
#endif

static void hl_backend_mixed_sse_report(struct hl_backend_mixed_sse_shared *census, int available,
                                        hl_linux_abi *box) {
    uint32_t expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&census->reported, &expected, 1, memory_order_acq_rel,
                                                 memory_order_relaxed))
        return;
    char record[2048];
    int formatted = snprintf(record, sizeof record,
                             "[diag] backend-shape version=5 available=%d crossings=%llu "
                             "translated_entries=%llu interpreted_entries=%llu translated_steps=%llu "
                             "interpreted_steps=%llu mixed_sse_executed=%llu "
                             "mixed_sse_executed_transitions=%llu mixed_sse_disabled_boundaries=%llu "
                             "jcc_ibtc_enabled=%d jcc_ibtc_emitted=%llu jcc_ibtc_hits=%llu "
                             "jcc_ibtc_misses=%llu jcc_ibtc_irq=%llu jcc_ibtc_fills=%llu "
                             "jcc_ibtc_suppressed=%llu jcc_ibtc_invalid_refusals=%llu "
                             "direct_jmp_ibtc_enabled=%d direct_jmp_ibtc_emitted=%llu "
                             "direct_jmp_ibtc_hits=%llu direct_jmp_ibtc_misses=%llu direct_jmp_ibtc_irq=%llu "
                             "direct_jmp_ibtc_fills=%llu direct_jmp_ibtc_suppressed=%llu "
                             "direct_jmp_ibtc_invalid_refusals=%llu",
                             available,
                             (unsigned long long)(atomic_load_explicit(&census->translated_entries,
                                                                      memory_order_relaxed) +
                                                  atomic_load_explicit(&census->interpreted_entries,
                                                                      memory_order_relaxed)),
                             (unsigned long long)atomic_load_explicit(&census->translated_entries,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->interpreted_entries,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->translated_steps,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->interpreted_steps,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->executed, memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->executed_transitions,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->disabled_boundaries,
                                                                     memory_order_relaxed),
                             (int)census->jcc_ibtc_enabled,
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_emitted,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_hits,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_fills,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_suppressed,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_invalid_refusals,
                                                                     memory_order_relaxed),
                             (int)census->direct_jmp_ibtc_enabled,
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_emitted,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_hits,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_fills,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_suppressed,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_jmp_ibtc_invalid_refusals,
                                                                     memory_order_relaxed));
    if (formatted <= 0 || (size_t)formatted >= sizeof record) return;
    for (unsigned reason = 0; reason < HL_BACKEND_TREE_REASON_COUNT; ++reason) {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " r%u=%llu", reason,
                             (unsigned long long)atomic_load_explicit(&census->reason[reason],
                                                                     memory_order_relaxed));
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) return;
        formatted += added;
    }
    {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " r_other=%llu",
                             (unsigned long long)atomic_load_explicit(&census->reason_other,
                                                                     memory_order_relaxed));
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) return;
        formatted += added;
    }
#define HL_APPEND_CROSSING(name, array, kind)                                                                          \
    do {                                                                                                               \
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " " name "=%llu",              \
                             (unsigned long long)atomic_load_explicit(&(array)[kind], memory_order_relaxed));           \
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) return;                                  \
        formatted += added;                                                                                           \
    } while (0)
    HL_APPEND_CROSSING("t_direct_jmp", census->translated_exit, HL_BACKEND_SHAPE_T_DIRECT_JUMP);
    HL_APPEND_CROSSING("t_direct_call", census->translated_exit, HL_BACKEND_SHAPE_T_DIRECT_CALL);
    HL_APPEND_CROSSING("t_ret", census->translated_exit, HL_BACKEND_SHAPE_T_RETURN);
    HL_APPEND_CROSSING("t_jmp_reg", census->translated_exit, HL_BACKEND_SHAPE_T_INDIRECT_BRANCH);
    HL_APPEND_CROSSING("t_jmp_mem", census->translated_exit, HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY);
    HL_APPEND_CROSSING("t_call_reg", census->translated_exit, HL_BACKEND_SHAPE_T_INDIRECT_CALL);
    HL_APPEND_CROSSING("t_call_mem", census->translated_exit, HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY);
    HL_APPEND_CROSSING("t_syscall", census->translated_exit, HL_BACKEND_SHAPE_T_SYSCALL);
    HL_APPEND_CROSSING("t_irq", census->translated_exit, HL_BACKEND_SHAPE_T_IRQ);
    HL_APPEND_CROSSING("t_fault", census->translated_exit, HL_BACKEND_SHAPE_T_FAULT);
    HL_APPEND_CROSSING("i_direct_jmp", census->interpreter_stop, HL_BACKEND_SHAPE_S_DIRECT_JUMP);
    HL_APPEND_CROSSING("i_direct_call", census->interpreter_stop, HL_BACKEND_SHAPE_S_DIRECT_CALL);
    HL_APPEND_CROSSING("i_ret", census->interpreter_stop, HL_BACKEND_SHAPE_S_RETURN);
    HL_APPEND_CROSSING("i_jmp_reg", census->interpreter_stop, HL_BACKEND_SHAPE_S_INDIRECT_BRANCH);
    HL_APPEND_CROSSING("i_jmp_mem", census->interpreter_stop, HL_BACKEND_SHAPE_S_INDIRECT_BRANCH_MEMORY);
    HL_APPEND_CROSSING("i_call_reg", census->interpreter_stop, HL_BACKEND_SHAPE_S_INDIRECT_CALL);
    HL_APPEND_CROSSING("i_call_mem", census->interpreter_stop, HL_BACKEND_SHAPE_S_INDIRECT_CALL_MEMORY);
    HL_APPEND_CROSSING("i_syscall", census->interpreter_stop, HL_BACKEND_SHAPE_S_SYSCALL);
    HL_APPEND_CROSSING("i_service", census->interpreter_stop, HL_BACKEND_SHAPE_S_SERVICE);
    HL_APPEND_CROSSING("i_irq", census->interpreter_stop, HL_BACKEND_SHAPE_S_IRQ);
    HL_APPEND_CROSSING("i_fault", census->interpreter_stop, HL_BACKEND_SHAPE_S_FAULT);
#define HL_APPEND_CALL_SIM(name, field)                                                                                \
    do {                                                                                                               \
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " " name "=%llu",              \
                             (unsigned long long)atomic_load_explicit(&census->field, memory_order_relaxed));           \
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) return;                                  \
        formatted += added;                                                                                           \
    } while (0)
    HL_APPEND_CALL_SIM("call_sim_eligible", call_sim_eligible);
    HL_APPEND_CALL_SIM("call_sim_hit", call_sim_hit);
    HL_APPEND_CALL_SIM("call_sim_miss", call_sim_miss);
    HL_APPEND_CALL_SIM("call_sim_fill", call_sim_fill);
    HL_APPEND_CALL_SIM("call_sim_decline_irq", call_sim_decline_irq);
    HL_APPEND_CALL_SIM("call_sim_decline_stub", call_sim_decline_stub);
    HL_APPEND_CALL_SIM("call_sim_decline_authority", call_sim_decline_authority);
#undef HL_APPEND_CALL_SIM
#undef HL_APPEND_CROSSING
    if ((size_t)formatted + 1 >= sizeof record) return;
    record[formatted++] = '\n';
    size_t offset = 0;
    while (offset < (size_t)formatted) {
        int64_t written = hl_linux_write(box, STDERR_FILENO, record + offset, (size_t)formatted - offset);
        if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)formatted - offset) return;
        offset += (size_t)written;
    }
}

void hl_target_backend_tree_reap_report(void *shared, size_t shared_size, hl_linux_abi *box) {
    struct hl_backend_mixed_sse_shared *census = shared;
    if (census == NULL || shared_size != sizeof *census || box == NULL) return;
    int root_pid = atomic_load_explicit(&census->root_pid, memory_order_acquire);
    int settled = root_pid > 0 && hl_backend_mixed_sse_parent_barrier(census, root_pid);
    int complete = settled && atomic_load_explicit(&census->missing_claims, memory_order_relaxed) == 0 &&
                   atomic_load_explicit(&census->duplicate_finalize, memory_order_relaxed) == 0;
    hl_backend_mixed_sse_report(census, complete, box);
}

#define hl_backend_tree_begin(enabled, host) ((void)0)
static inline void hl_backend_tree_run_begin(int translated, uint64_t steps) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return;
    if (translated) {
        atomic_fetch_add_explicit(&census->translated_entries, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&census->translated_steps, steps, memory_order_relaxed);
    } else {
        atomic_fetch_add_explicit(&census->interpreted_entries, 1, memory_order_relaxed);
    }
}
static inline int hl_backend_tree_steps_enabled(void) { return g_backend_mixed_sse != NULL; }
static inline void hl_backend_tree_interpreted_steps(uint64_t steps) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census != NULL) atomic_fetch_add_explicit(&census->interpreted_steps, steps, memory_order_relaxed);
}
static inline void hl_backend_tree_reason(unsigned reason) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return;
    if (reason < HL_BACKEND_TREE_REASON_COUNT)
        atomic_fetch_add_explicit(&census->reason[reason], 1, memory_order_relaxed);
    else
        atomic_fetch_add_explicit(&census->reason_other, 1, memory_order_relaxed);
}
static inline void hl_backend_tree_translated_exit_count(unsigned kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census != NULL && kind < HL_BACKEND_SHAPE_T_COUNT)
        atomic_fetch_add_explicit(&census->translated_exit[kind], 1, memory_order_relaxed);
}
#define hl_backend_tree_translated_exit(kind, stitched_jmp, stitched_cond_fall) ((void)0)
static inline void hl_backend_tree_interpreter_stop(unsigned kind, uint64_t form) {
    (void)form;
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census != NULL && kind < HL_BACKEND_SHAPE_S_COUNT)
        atomic_fetch_add_explicit(&census->interpreter_stop[kind], 1, memory_order_relaxed);
}
enum hl_backend_call_sim_counter {
    HL_BACKEND_CALL_SIM_ELIGIBLE,
    HL_BACKEND_CALL_SIM_HIT,
    HL_BACKEND_CALL_SIM_MISS,
    HL_BACKEND_CALL_SIM_FILL,
    HL_BACKEND_CALL_SIM_DECLINE_IRQ,
    HL_BACKEND_CALL_SIM_DECLINE_STUB,
    HL_BACKEND_CALL_SIM_DECLINE_AUTHORITY,
};
static inline void hl_backend_tree_call_sim_count(enum hl_backend_call_sim_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return;
    _Atomic uint64_t *counter = NULL;
    switch (kind) {
    case HL_BACKEND_CALL_SIM_ELIGIBLE: counter = &census->call_sim_eligible; break;
    case HL_BACKEND_CALL_SIM_HIT: counter = &census->call_sim_hit; break;
    case HL_BACKEND_CALL_SIM_MISS: counter = &census->call_sim_miss; break;
    case HL_BACKEND_CALL_SIM_FILL: counter = &census->call_sim_fill; break;
    case HL_BACKEND_CALL_SIM_DECLINE_IRQ: counter = &census->call_sim_decline_irq; break;
    case HL_BACKEND_CALL_SIM_DECLINE_STUB: counter = &census->call_sim_decline_stub; break;
    case HL_BACKEND_CALL_SIM_DECLINE_AUTHORITY: counter = &census->call_sim_decline_authority; break;
    }
    if (counter != NULL) atomic_fetch_add_explicit(counter, 1, memory_order_relaxed);
}
#define hl_backend_tree_translated_fall_stop(reason) ((void)0)
static inline void hl_backend_tree_mixed_sse_completed(uint64_t transitions, int disabled_boundary) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return;
    if (disabled_boundary) {
        if (transitions == 0)
            atomic_fetch_add_explicit(&census->disabled_boundaries, 1, memory_order_relaxed);
        return;
    }
    if (transitions == 0) return;
    atomic_fetch_add_explicit(&census->executed, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&census->executed_transitions, transitions, memory_order_relaxed);
}

enum hl_backend_jcc_ibtc_counter {
    HL_BACKEND_JCC_IBTC_EMITTED,
    HL_BACKEND_JCC_IBTC_HIT,
    HL_BACKEND_JCC_IBTC_MISS,
    HL_BACKEND_JCC_IBTC_IRQ,
    HL_BACKEND_JCC_IBTC_FILL,
    HL_BACKEND_JCC_IBTC_SUPPRESSED,
    HL_BACKEND_JCC_IBTC_INVALID_REFUSAL,
};

static _Atomic uint64_t *hl_backend_tree_jcc_ibtc_counter(enum hl_backend_jcc_ibtc_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_JCC_IBTC_EMITTED: return &census->jcc_ibtc_emitted;
    case HL_BACKEND_JCC_IBTC_HIT: return &census->jcc_ibtc_hits;
    case HL_BACKEND_JCC_IBTC_MISS: return &census->jcc_ibtc_misses;
    case HL_BACKEND_JCC_IBTC_IRQ: return &census->jcc_ibtc_irq;
    case HL_BACKEND_JCC_IBTC_FILL: return &census->jcc_ibtc_fills;
    case HL_BACKEND_JCC_IBTC_SUPPRESSED: return &census->jcc_ibtc_suppressed;
    case HL_BACKEND_JCC_IBTC_INVALID_REFUSAL: return &census->jcc_ibtc_invalid_refusals;
    }
    return NULL;
}

static uintptr_t hl_backend_tree_jcc_ibtc_dynamic_counter_address(enum hl_backend_jcc_ibtc_counter kind) {
    if (kind != HL_BACKEND_JCC_IBTC_HIT && kind != HL_BACKEND_JCC_IBTC_MISS &&
        kind != HL_BACKEND_JCC_IBTC_IRQ)
        return 0;
    return (uintptr_t)hl_backend_tree_jcc_ibtc_counter(kind);
}

static void hl_backend_tree_jcc_ibtc_add(enum hl_backend_jcc_ibtc_counter kind, uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_jcc_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}

enum hl_backend_direct_jmp_ibtc_counter {
    HL_BACKEND_DIRECT_JMP_IBTC_EMITTED,
    HL_BACKEND_DIRECT_JMP_IBTC_HIT,
    HL_BACKEND_DIRECT_JMP_IBTC_MISS,
    HL_BACKEND_DIRECT_JMP_IBTC_IRQ,
    HL_BACKEND_DIRECT_JMP_IBTC_FILL,
    HL_BACKEND_DIRECT_JMP_IBTC_SUPPRESSED,
    HL_BACKEND_DIRECT_JMP_IBTC_INVALID_REFUSAL,
};

static _Atomic uint64_t *hl_backend_tree_direct_jmp_ibtc_counter(
    enum hl_backend_direct_jmp_ibtc_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_DIRECT_JMP_IBTC_EMITTED: return &census->direct_jmp_ibtc_emitted;
    case HL_BACKEND_DIRECT_JMP_IBTC_HIT: return &census->direct_jmp_ibtc_hits;
    case HL_BACKEND_DIRECT_JMP_IBTC_MISS: return &census->direct_jmp_ibtc_misses;
    case HL_BACKEND_DIRECT_JMP_IBTC_IRQ: return &census->direct_jmp_ibtc_irq;
    case HL_BACKEND_DIRECT_JMP_IBTC_FILL: return &census->direct_jmp_ibtc_fills;
    case HL_BACKEND_DIRECT_JMP_IBTC_SUPPRESSED: return &census->direct_jmp_ibtc_suppressed;
    case HL_BACKEND_DIRECT_JMP_IBTC_INVALID_REFUSAL: return &census->direct_jmp_ibtc_invalid_refusals;
    }
    return NULL;
}

static uintptr_t hl_backend_tree_direct_jmp_ibtc_dynamic_counter_address(
    enum hl_backend_direct_jmp_ibtc_counter kind) {
    if (kind != HL_BACKEND_DIRECT_JMP_IBTC_HIT && kind != HL_BACKEND_DIRECT_JMP_IBTC_MISS &&
        kind != HL_BACKEND_DIRECT_JMP_IBTC_IRQ)
        return 0;
    return (uintptr_t)hl_backend_tree_direct_jmp_ibtc_counter(kind);
}

static void hl_backend_tree_direct_jmp_ibtc_add(enum hl_backend_direct_jmp_ibtc_counter kind,
                                                uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_direct_jmp_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}
#define hl_backend_tree_interpreter_entry(kind, fallback_form) ((void)0)
#define hl_backend_tree_interpreter_stop(kind, stop_form) ((void)0)
#define hl_backend_tree_direct_edge(family, same_page) ((void)0)
#define hl_backend_tree_direct_edge_resolution(family, resolution, translated, current, rel32, eligible) ((void)0)
#define hl_backend_tree_would_link(family, disposition) ((void)0)
#define hl_backend_tree_family_jmem() ((void)0)
#define hl_backend_tree_family_div(is_signed, outcome) ((void)0)
#define hl_backend_tree_family_div_service64_completed(is_signed) ((void)0)
#define hl_backend_tree_translation() ((void)0)
#define hl_backend_tree_map_hit() ((void)0)
#define hl_backend_tree_stw_retry() ((void)0)
#define hl_backend_tree_irq_pending() ((void)0)
static _Noreturn void hl_backend_tree_abnormal_exit(int status) {
    (void)hl_backend_tree_finalize(1);
    _exit(status);
}

#endif
