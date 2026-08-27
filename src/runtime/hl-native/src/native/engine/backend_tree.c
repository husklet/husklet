/*
 * Hook-only whole-process-tree execution census.
 *
 * A guest fork is a host fork, so ordinary globals become COW snapshots and
 * cannot answer how the complete guest tree executed.  This table is created
 * before the first guest instruction, mapped MAP_SHARED by the Linux-memory
 * boundary, and inherited by every guest child.  Each process claims exactly
 * one pid slot; threads in that process update the same atomics.
 *
 * The table is diagnostic state only.  Product builds compile every call to a
 * no-op and allocate no mapping.  No counter influences translation,
 * scheduling, signal delivery, or guest-visible state.
 */

#if defined(HL_NATIVE_TEST_HOOKS)

#define HL_BACKEND_TREE_SLOTS 4096u
#define HL_BACKEND_TREE_REASON_COUNT 16u

enum hl_backend_tree_lifecycle {
    HL_BACKEND_TREE_CLAIMED = 1,
    HL_BACKEND_TREE_COMPLETED = 2,
    HL_BACKEND_TREE_ABNORMAL = 3,
};

struct hl_backend_tree_slot {
    _Atomic int pid; /* 0 free, -1 being filled, positive published last */
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
};

struct hl_backend_tree_shared {
    _Atomic int root_pid;
    _Atomic uint64_t missing_claims;
    _Atomic uint64_t duplicate_finalize;
    _Atomic uint32_t reported;
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
};

static struct hl_backend_tree_shared *g_backend_tree;
static struct hl_backend_tree_slot *g_backend_tree_self;

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
    if (slot != NULL && pid > 0) atomic_store_explicit(&slot->pid, pid, memory_order_release);
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

static void hl_backend_tree_begin(int enabled, const hl_host_services *host) {
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
    g_backend_tree = (struct hl_backend_tree_shared *)mapping;
    int self = (int)getpid();
    atomic_store_explicit(&g_backend_tree->root_pid, self, memory_order_release);
    g_backend_tree_self = hl_backend_tree_claim_pid(self);
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
    int pid = result == 0 ? (int)getpid() : (int)result;
    if (birth == NULL) {
        /* Only the parent records exhaustion: both fork return arms share the mapping, so counting in the child
           too would turn one untracked process into two missing lifecycle rows. */
        if (result > 0) atomic_fetch_add_explicit(&g_backend_tree->missing_claims, 1, memory_order_relaxed);
        if (result == 0) g_backend_tree_self = NULL;
        return;
    }
    hl_backend_tree_publish(birth, pid);
    if (result == 0) g_backend_tree_self = birth;
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

static int hl_backend_tree_finalize_slot(struct hl_backend_tree_slot *slot, int abnormal) {
    if (slot == NULL) return 0;
    uint32_t expected = HL_BACKEND_TREE_CLAIMED;
    uint32_t completed = abnormal ? HL_BACKEND_TREE_ABNORMAL : HL_BACKEND_TREE_COMPLETED;
    if (atomic_compare_exchange_strong_explicit(&slot->lifecycle, &expected, completed, memory_order_acq_rel,
                                                memory_order_acquire))
        return 1;
    if (g_backend_tree != NULL) atomic_fetch_add_explicit(&g_backend_tree->duplicate_finalize, 1, memory_order_relaxed);
    return 0;
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

static int hl_backend_tree_is_root(void) {
    return g_backend_tree != NULL &&
           atomic_load_explicit(&g_backend_tree->root_pid, memory_order_acquire) == (int)getpid();
}

static int hl_backend_tree_is_finalized(void) {
    return g_backend_tree_self != NULL &&
           atomic_load_explicit(&g_backend_tree_self->lifecycle, memory_order_acquire) != HL_BACKEND_TREE_CLAIMED;
}

#if !defined(_WIN32)
/* Root-side process barrier. Normal children have already finalized before becoming reapable. A child killed
   before its finalizer is reaped here when it is ours, or observed gone when it is a deeper descendant; only
   then is its claimed slot closed as abnormal. A bounded failure withholds the record rather than emitting a
   summary while a descendant can still mutate it. */
static int hl_backend_tree_reap_barrier(void) {
    if (!hl_backend_tree_is_root()) return 0;
    int self = (int)getpid();
    for (unsigned round = 0; round < 2000; ++round) {
        unsigned live = 0;
        for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
            struct hl_backend_tree_slot *slot = &g_backend_tree->slots[index];
            int pid = atomic_load_explicit(&slot->pid, memory_order_acquire);
            if (pid <= 0 || pid == self) continue;
            int status = 0;
            pid_t reaped = waitpid((pid_t)pid, &status, WNOHANG);
            if (reaped == (pid_t)pid || (reaped < 0 && errno == ECHILD && kill((pid_t)pid, 0) < 0 && errno == ESRCH)) {
                if (atomic_load_explicit(&slot->lifecycle, memory_order_acquire) == HL_BACKEND_TREE_CLAIMED)
                    (void)hl_backend_tree_finalize_slot(slot, 1);
                continue;
            }
            ++live;
        }
        if (live == 0) return 1;
        (void)poll(NULL, 0, 1);
    }
    return 0;
}
#else
#define hl_backend_tree_reap_barrier() 1
#endif

static void hl_backend_tree_summary(struct hl_backend_tree_summary *summary) {
    memset(summary, 0, sizeof *summary);
    if (g_backend_tree == NULL) return;
    summary->root_pid = (uint64_t)atomic_load_explicit(&g_backend_tree->root_pid, memory_order_acquire);
    summary->duplicate_finalize = atomic_load_explicit(&g_backend_tree->duplicate_finalize, memory_order_relaxed);
    uint64_t missing_claims = atomic_load_explicit(&g_backend_tree->missing_claims, memory_order_relaxed);
    for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &g_backend_tree->slots[index];
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
    }
    summary->claimed += missing_claims;
    summary->missing = summary->claimed - summary->completed - summary->abnormal;
    summary->crossings = summary->translated_entries + summary->interpreted_entries;
    uint64_t reason_total = summary->reason_other;
    for (uint32_t reason = 0; reason < HL_BACKEND_TREE_REASON_COUNT; ++reason)
        reason_total += summary->reason[reason];
    /* A process killed inside run_block has an entry and no returned reason. Preserve exact accounting
       without pretending the interrupted backend supplied a reason code. */
    if (reason_total < summary->crossings) summary->reason_other += summary->crossings - reason_total;
}

static void hl_backend_tree_report(void) {
    if (!hl_backend_tree_is_root() || !hl_backend_tree_is_finalized()) return;
    if (!hl_backend_tree_reap_barrier()) return;
    uint32_t expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&g_backend_tree->reported, &expected, 1, memory_order_acq_rel,
                                                 memory_order_relaxed))
        return;
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary(&summary);
    char record[2048];
    int size =
        snprintf(record, sizeof record,
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
    if (size > 0 && (size_t)size < sizeof record)
        (void)hl_linux_write(g_linux_box, STDERR_FILENO, record, (size_t)size);
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
    struct hl_backend_tree_slot *child_birth = hl_backend_tree_prepare_fork();
    pid_t child = fork();
    if (!(scenario == 6 && child == 0)) hl_backend_tree_after_fork(child, child_birth);
    if (child < 0) return 11;
    if (child == 0) {
        hl_backend_tree_run_begin(0, 0);
        hl_backend_tree_interpreted_steps(3);
        if (scenario == 7) _exit(0); /* backend entry whose process dies before returning a reason */
        hl_backend_tree_reason(1);
        if (scenario == 1) {
            struct hl_backend_tree_slot *grandchild_birth = hl_backend_tree_prepare_fork();
            pid_t grandchild = fork();
            hl_backend_tree_after_fork(grandchild, grandchild_birth);
            if (grandchild < 0) hl_backend_tree_abnormal_exit(12);
            if (grandchild == 0) {
                hl_backend_tree_run_begin(1, 5);
                hl_backend_tree_reason(5);
                (void)hl_backend_tree_finalize(0);
                _exit(0);
            }
            if (hl_backend_tree_wait(grandchild, 1) != 0) hl_backend_tree_abnormal_exit(13);
        }
        if (scenario == 2 || scenario == 3 || scenario == 6)
            _exit(0); /* exercise parent reaping, missing, and pre-child-execution publication separately */
        if (scenario == 5) hl_backend_tree_abnormal_exit(0);
        (void)hl_backend_tree_finalize(0);
        _exit(0);
    }
    int child_status = hl_backend_tree_wait(child, scenario == 2 || scenario == 5 || scenario == 6 || scenario == 7);
    if (child_status != 0) return 14;
    hl_backend_tree_run_begin(1, 7);
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

#define hl_backend_tree_begin(enabled, host) ((void)0)
#define hl_backend_tree_prepare_fork() NULL
#define hl_backend_tree_after_fork(result, birth) ((void)(result), (void)(birth))
#define hl_backend_tree_run_begin(translated, steps) ((void)0)
#define hl_backend_tree_interpreted_steps(steps) ((void)0)
#define hl_backend_tree_reason(reason) ((void)0)
#define hl_backend_tree_translation() ((void)0)
#define hl_backend_tree_map_hit() ((void)0)
#define hl_backend_tree_stw_retry() ((void)0)
#define hl_backend_tree_irq_pending() ((void)0)
#define hl_backend_tree_finalize(abnormal) 0
#define hl_backend_tree_reaped(pid) ((void)0)
#define hl_backend_tree_is_root() 0
#define hl_backend_tree_is_finalized() 0
#define hl_backend_tree_report() ((void)0)
#define hl_backend_tree_abnormal_exit(status) _exit(status)

#endif
