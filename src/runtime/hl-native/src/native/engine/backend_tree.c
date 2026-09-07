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
#ifndef HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE
#define HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE 1
#endif

#define HL_BACKEND_EXECUTED_FORM_SLOTS 4096u
#define HL_BACKEND_EXECUTED_FORM_TOP 16u
#define HL_BACKEND_EXECUTED_STEP_FORM_TOP 64u

/* Reap-time product diagnostics belong to the host worker, after the guest process tree has
 * finished.  A non-NULL box is retained only for hook fixtures that deliberately capture through
 * a synthetic guest descriptor table; production passes NULL so a guest projection of fd 2 cannot
 * redirect or hide the worker's receipt. */
static _Thread_local int hl_backend_report_descriptor = STDERR_FILENO;

static int64_t hl_backend_report_write(hl_linux_abi *box, const char *record, size_t size) {
    if (box != NULL) return hl_linux_write(box, STDERR_FILENO, record, size);
    ssize_t written;
    do {
        written = write(hl_backend_report_descriptor, record, size);
    } while (written < 0 && errno == EINTR);
    return (int64_t)written;
}
enum hl_backend_x86_jcc_route_counter {
    HL_BACKEND_X86_JCC_ROUTE_ATTEMPTS,
    HL_BACKEND_X86_JCC_ROUTE_HIT,
    HL_BACKEND_X86_JCC_ROUTE_EMPTY,
    HL_BACKEND_X86_JCC_ROUTE_COLLISION,
    HL_BACKEND_X86_JCC_ROUTE_STATE_REFUSAL,
    HL_BACKEND_X86_JCC_ROUTE_IRQ,
    HL_BACKEND_X86_JCC_ROUTE_KNOWN_TAKEN,
    HL_BACKEND_X86_JCC_ROUTE_KNOWN_FALLTHROUGH,
    HL_BACKEND_X86_JCC_ROUTE_COUNT,
};

static int hl_backend_x86_jcc_route_format(char *record, size_t capacity, const uint64_t *route) {
    return snprintf(record, capacity,
                    "[diag] x86-jcc-route version=1 attempts=%llu hit=%llu empty=%llu collision=%llu "
                    "state_refusal=%llu irq=%llu known_taken=%llu known_fallthrough=%llu\n",
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_ATTEMPTS],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_HIT],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_EMPTY],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_COLLISION],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_STATE_REFUSAL],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_IRQ],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_KNOWN_TAKEN],
                    (unsigned long long)route[HL_BACKEND_X86_JCC_ROUTE_KNOWN_FALLTHROUGH]);
}

#if defined(HL_NATIVE_TEST_HOOKS)
#define HL_BACKEND_SSE_RIPREL_FORM_SLOTS 512u
#define HL_BACKEND_SSE_RIPREL_FORM_TOP 8u
#endif
#define HL_BACKEND_A64_MAJOR_COUNT 16u

enum hl_backend_finalize_caller {
    HL_BACKEND_FINALIZE_UNKNOWN,
    HL_BACKEND_FINALIZE_PROCESS_EXIT,
    HL_BACKEND_FINALIZE_FATAL_SIGNAL,
    HL_BACKEND_FINALIZE_RUN_EPILOGUE,
    HL_BACKEND_FINALIZE_ABNORMAL_EXIT,
    HL_BACKEND_FINALIZE_REAPER,
    HL_BACKEND_FINALIZE_PARENT_BARRIER,
};

struct hl_backend_executed_form {
    _Atomic uint32_t state;
    uint64_t key;
    _Atomic uint64_t count;
};

static uint64_t hl_backend_executed_form_mix(uint64_t value) {
    value ^= value >> 33;
    value *= UINT64_C(0xff51afd7ed558ccd);
    value ^= value >> 33;
    value *= UINT64_C(0xc4ceb9fe1a85ec53);
    return value ^ (value >> 33);
}

static void hl_backend_executed_form_record(
    struct hl_backend_executed_form forms[HL_BACKEND_EXECUTED_FORM_SLOTS], _Atomic uint64_t *total,
    _Atomic uint64_t *unique, _Atomic uint64_t *overflow, uint64_t key, _Atomic uint32_t *reserved_seen,
    _Atomic uint32_t *pause_ready, _Atomic uint32_t *pause_release) {
    atomic_fetch_add_explicit(total, 1, memory_order_relaxed);
    unsigned start = (unsigned)hl_backend_executed_form_mix(key) & (HL_BACKEND_EXECUTED_FORM_SLOTS - 1u);
    for (unsigned probe = 0; probe < HL_BACKEND_EXECUTED_FORM_SLOTS; ++probe) {
        struct hl_backend_executed_form *form =
            &forms[(start + probe) & (HL_BACKEND_EXECUTED_FORM_SLOTS - 1u)];
    retry_slot:
        uint32_t state = atomic_load_explicit(&form->state, memory_order_acquire);
        if (state == 1) {
            if (reserved_seen != NULL) atomic_store_explicit(reserved_seen, 1, memory_order_release);
            for (unsigned wait = 0; wait < 4096 && state == 1; ++wait) {
                sched_yield();
                state = atomic_load_explicit(&form->state, memory_order_acquire);
            }
            if (state == 1) {
                atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
                return;
            }
        }
        if (state == 2 && form->key == key) {
            atomic_fetch_add_explicit(&form->count, 1, memory_order_relaxed);
            return;
        }
        if (state != 0) continue;
        uint32_t expected = 0;
        if (!atomic_compare_exchange_strong_explicit(&form->state, &expected, 1, memory_order_acquire,
                                                     memory_order_relaxed))
            goto retry_slot;
        if (pause_ready != NULL && pause_release != NULL) {
            atomic_store_explicit(pause_ready, 1, memory_order_release);
            while (!atomic_load_explicit(pause_release, memory_order_acquire)) sched_yield();
        }
        form->key = key;
        atomic_store_explicit(&form->count, 1, memory_order_relaxed);
        atomic_store_explicit(&form->state, 2, memory_order_release);
        atomic_fetch_add_explicit(unique, 1, memory_order_relaxed);
        return;
    }
    atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
}

/* Shared by hook aggregation and the compact product diagnostics census. */
enum hl_backend_would_link_family {
    HL_BACKEND_WOULD_LINK_FALLTHROUGH,
    HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
    HL_BACKEND_WOULD_LINK_DIRECT_CALL,
    HL_BACKEND_WOULD_LINK_FAMILY_COUNT,
};

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

enum hl_backend_jcc_late_reason {
    HL_BACKEND_JCC_LATE_INVALID,
    HL_BACKEND_JCC_LATE_TARGET_ABSENT,
    HL_BACKEND_JCC_LATE_PAGE_GENERATION,
    HL_BACKEND_JCC_LATE_DISPLACEMENT,
    HL_BACKEND_JCC_LATE_OTHER,
    HL_BACKEND_JCC_LATE_ELIGIBLE,
    HL_BACKEND_JCC_LATE_REASON_COUNT,
};

enum hl_backend_jcc_invalid_reason {
    HL_BACKEND_JCC_INVALID_NULL,
    HL_BACKEND_JCC_INVALID_MAGIC,
    HL_BACKEND_JCC_INVALID_GPC,
    HL_BACKEND_JCC_INVALID_BLOCK_GENERATION,
    HL_BACKEND_JCC_INVALID_ENTRY_ZERO,
    HL_BACKEND_JCC_INVALID_LENGTH_ZERO,
    HL_BACKEND_JCC_INVALID_RESOLVE,
    HL_BACKEND_JCC_INVALID_RESOLVED_GENERATION,
    HL_BACKEND_JCC_INVALID_ENTRY_OVERFLOW,
    HL_BACKEND_JCC_INVALID_REASON_COUNT,
};

#define HL_BACKEND_JCC_INVALID_SITES 4096u
_Static_assert((HL_BACKEND_JCC_INVALID_SITES & (HL_BACKEND_JCC_INVALID_SITES - 1)) == 0,
               "JCC invalid-site table must be a power of two");
struct hl_backend_jcc_invalid_site {
    _Atomic uint32_t state;
    uint32_t reason;
    uint64_t source;
    uint64_t target;
    uint64_t source_mapping_start;
    uint64_t source_mapping_offset;
    uint64_t source_mapping_device;
    uint64_t source_mapping_inode;
    uint64_t target_mapping_start;
    uint64_t target_mapping_offset;
    uint64_t target_mapping_device;
    uint64_t target_mapping_inode;
    uint64_t target_bytes_lo;
    uint64_t target_bytes_hi;
    uint8_t target_bytes_len;
    uint8_t build_failure_reason;
    uint8_t build_failure_index;
    uint8_t build_failure_bytes_len;
    uint8_t build_failure_transient;
    uint64_t build_failure_pc;
    uint64_t build_failure_form;
    uint64_t build_failure_bytes_lo;
    uint64_t build_failure_bytes_hi;
    _Atomic uint64_t count;
};

static void hl_backend_jcc_fill_record(_Atomic uint64_t *empty, _Atomic uint64_t *collision,
                                       _Atomic uint64_t *irq, _Atomic uint64_t *same_key,
                                       uint64_t target, uint64_t previous_target,
                                       int interrupt_consumed) {
    if (interrupt_consumed)
        atomic_fetch_add_explicit(irq, 1, memory_order_relaxed);
    else if (previous_target == 0)
        atomic_fetch_add_explicit(empty, 1, memory_order_relaxed);
    else if (previous_target != target)
        atomic_fetch_add_explicit(collision, 1, memory_order_relaxed);
    else
        atomic_fetch_add_explicit(same_key, 1, memory_order_relaxed);
}

#define HL_BACKEND_JCC_LATE_SITES 524288u
#define HL_BACKEND_JCC_LATE_MAX_PROBES 8u
struct hl_backend_jcc_late_site {
    _Atomic int owner; /* 0 empty, negative host pid reserving, 1 published */
    _Atomic uint64_t owner_birth_ns;
    int process;
    uint64_t process_birth_ns;
    uint64_t cache_generation;
    uint64_t source;
    uint64_t target;
    uintptr_t body;
    uint64_t target_generation;
    _Atomic uint64_t appearances;
};

static void hl_backend_jcc_late_record(
    struct hl_backend_jcc_late_site *sites, uint32_t site_count, _Atomic uint64_t *first,
    _Atomic uint64_t *repeated, _Atomic uint64_t *stable, _Atomic uint64_t *changed,
    _Atomic uint64_t *current, _Atomic uint64_t *retired, _Atomic uint64_t *unique,
    _Atomic uint64_t *overflow, _Atomic uint64_t *abandoned, _Atomic uint64_t *maximum,
    int process, uint64_t process_birth_ns, uint64_t cache_generation, uint64_t source,
    uint64_t target, uintptr_t body, uint64_t target_generation, int generation_current,
    _Atomic uint32_t *pause_ready, _Atomic uint32_t *pause_release,
    _Atomic uint32_t *reserved_seen) {
    if (process <= 0 || process_birth_ns == 0) {
        atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
        return;
    }
    uint64_t hash = hl_backend_executed_form_mix(source ^ process_birth_ns ^ cache_generation);
    uint32_t probe_count = site_count < HL_BACKEND_JCC_LATE_MAX_PROBES ? site_count : HL_BACKEND_JCC_LATE_MAX_PROBES;
    for (uint32_t probe = 0; probe < probe_count; ++probe) {
        struct hl_backend_jcc_late_site *site = &sites[(uint32_t)(hash + probe) & (site_count - 1)];
    retry:
        int owner = atomic_load_explicit(&site->owner, memory_order_acquire);
        if (owner < 0) {
            if (reserved_seen != NULL) {
                atomic_store_explicit(reserved_seen, 1, memory_order_release);
                while (pause_release != NULL && !atomic_load_explicit(pause_release, memory_order_acquire))
                    sched_yield();
            }
            int reserved_by = -owner;
            uint64_t reserved_birth = atomic_load_explicit(&site->owner_birth_ns, memory_order_acquire);
            uint64_t live_birth = 0;
            int live = hl_host_process_start_time_ns(reserved_by, &live_birth) &&
                       (reserved_birth == 0 || live_birth == reserved_birth);
            if (!live) {
                int expected = owner;
                if (atomic_compare_exchange_strong_explicit(&site->owner, &expected, 0,
                                                            memory_order_acq_rel, memory_order_acquire)) {
                    atomic_fetch_add_explicit(abandoned, 1, memory_order_relaxed);
                    goto retry;
                }
            }
            for (unsigned wait = 0; wait < 4096 && owner < 0; ++wait) {
                sched_yield();
                owner = atomic_load_explicit(&site->owner, memory_order_acquire);
            }
            if (owner < 0) {
                atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
                return;
            }
            goto retry;
        }
        uint64_t appearances;
        if (owner == 1 && site->process == process && site->process_birth_ns == process_birth_ns &&
            site->cache_generation == cache_generation && site->source == source) {
            appearances = atomic_fetch_add_explicit(&site->appearances, 1, memory_order_relaxed) + 1;
            atomic_fetch_add_explicit(repeated, 1, memory_order_relaxed);
            if (site->target == target && site->body == body && site->target_generation == target_generation)
                atomic_fetch_add_explicit(stable, 1, memory_order_relaxed);
            else
                atomic_fetch_add_explicit(changed, 1, memory_order_relaxed);
        } else {
            if (owner != 0) continue;
            int expected = 0;
            if (!atomic_compare_exchange_strong_explicit(&site->owner, &expected, -process,
                                                         memory_order_acq_rel, memory_order_acquire)) goto retry;
            atomic_store_explicit(&site->owner_birth_ns, process_birth_ns, memory_order_release);
            if (pause_ready != NULL && pause_release != NULL) {
                atomic_store_explicit(pause_ready, 1, memory_order_release);
                while (!atomic_load_explicit(pause_release, memory_order_acquire)) sched_yield();
            }
            site->process = process;
            site->process_birth_ns = process_birth_ns;
            site->cache_generation = cache_generation;
            site->source = source;
            site->target = target;
            site->body = body;
            site->target_generation = target_generation;
            atomic_store_explicit(&site->appearances, 1, memory_order_relaxed);
            atomic_store_explicit(&site->owner, 1, memory_order_release);
            atomic_fetch_add_explicit(first, 1, memory_order_relaxed);
            atomic_fetch_add_explicit(unique, 1, memory_order_relaxed);
            appearances = 1;
        }
        atomic_fetch_add_explicit(generation_current ? current : retired, 1, memory_order_relaxed);
        uint64_t held = atomic_load_explicit(maximum, memory_order_relaxed);
        while (held < appearances &&
               !atomic_compare_exchange_weak_explicit(maximum, &held, appearances, memory_order_relaxed,
                                                      memory_order_relaxed)) {}
        return;
    }
    atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
}

enum hl_backend_translit_failure_reason {
    HL_BACKEND_TRANSLIT_FAILURE_NONE,
    HL_BACKEND_TRANSLIT_FAILURE_IMAGE,
    HL_BACKEND_TRANSLIT_FAILURE_AUTHORITY,
    HL_BACKEND_TRANSLIT_FAILURE_OWNER_RESERVE,
    HL_BACKEND_TRANSLIT_FAILURE_ARENA,
    HL_BACKEND_TRANSLIT_FAILURE_FIRST_DECODE,
    HL_BACKEND_TRANSLIT_FAILURE_FIRST_UNSUPPORTED,
    HL_BACKEND_TRANSLIT_FAILURE_FIRST_DISPLACED,
    HL_BACKEND_TRANSLIT_FAILURE_FIRST_CAPACITY,
    HL_BACKEND_TRANSLIT_FAILURE_FIRST_EMIT,
    HL_BACKEND_TRANSLIT_FAILURE_TRANSACTION,
    HL_BACKEND_TRANSLIT_FAILURE_OWNER_PUBLISH,
};

#define HL_BACKEND_TRANSLIT_FAILURE_SITES 4096u
struct hl_backend_translit_failure {
    _Atomic uint32_t state;
    uint64_t gpc, pc, form, bytes_lo, bytes_hi;
    uint8_t reason, index, bytes_len, transient;
};
static struct hl_backend_translit_failure g_backend_translit_failures[HL_BACKEND_TRANSLIT_FAILURE_SITES];

static void hl_backend_tree_translit_failure(unsigned reason, uint64_t gpc, uint64_t pc, unsigned index,
                                             uint64_t form, const uint8_t *bytes, unsigned bytes_len,
                                             int transient) {
    if (!g_prof || reason == HL_BACKEND_TRANSLIT_FAILURE_NONE) return;
    uint32_t start = (uint32_t)(hl_backend_executed_form_mix(gpc) &
                                (HL_BACKEND_TRANSLIT_FAILURE_SITES - 1));
    struct hl_backend_translit_failure *failure = NULL;
    for (uint32_t probe = 0; probe < HL_BACKEND_TRANSLIT_FAILURE_SITES; ++probe) {
        struct hl_backend_translit_failure *candidate =
            &g_backend_translit_failures[(start + probe) & (HL_BACKEND_TRANSLIT_FAILURE_SITES - 1)];
        uint32_t state = atomic_load_explicit(&candidate->state, memory_order_acquire);
        if (state == 2 && candidate->gpc == gpc) return; /* Preserve the original refusal. */
        if (state != 0) continue;
        uint32_t empty = 0;
        if (!atomic_compare_exchange_strong_explicit(&candidate->state, &empty, 1, memory_order_acq_rel,
                                                     memory_order_acquire))
            continue;
        failure = candidate;
        break;
    }
    if (failure == NULL) return;
    failure->gpc = gpc;
    failure->pc = pc;
    failure->form = form;
    failure->reason = (uint8_t)reason;
    failure->index = (uint8_t)index;
    failure->bytes_len = (uint8_t)(bytes_len > 16 ? 16 : bytes_len);
    failure->transient = transient != 0;
    if (bytes != NULL) {
        memcpy(&failure->bytes_lo, bytes, failure->bytes_len < 8 ? failure->bytes_len : 8);
        if (failure->bytes_len > 8) memcpy(&failure->bytes_hi, bytes + 8, failure->bytes_len - 8);
    }
    atomic_store_explicit(&failure->state, 2, memory_order_release);
}

static const struct hl_backend_translit_failure *hl_backend_tree_translit_failure_find(uint64_t gpc) {
    uint32_t start = (uint32_t)(hl_backend_executed_form_mix(gpc) &
                                (HL_BACKEND_TRANSLIT_FAILURE_SITES - 1));
    for (uint32_t probe = 0; probe < HL_BACKEND_TRANSLIT_FAILURE_SITES; ++probe) {
        const struct hl_backend_translit_failure *failure =
            &g_backend_translit_failures[(start + probe) & (HL_BACKEND_TRANSLIT_FAILURE_SITES - 1)];
        uint32_t state = atomic_load_explicit(&failure->state, memory_order_acquire);
        if (state == 0) return NULL;
        if (state == 2 && failure->gpc == gpc) return failure;
    }
    return NULL;
}

static void hl_backend_jcc_invalid_mapping_evidence(uint64_t address, uint64_t *start_out,
                                                    uint64_t *offset_out, uint64_t *device_out,
                                                    uint64_t *inode_out) {
#if defined(__linux__)
    FILE *maps = fopen("/proc/self/maps", "r");
    char line[1024];
    if (maps == NULL) return;
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long start, end, offset, inode;
        unsigned major, minor;
        char protection[5] = {0};
        if (sscanf(line, "%llx-%llx %4s %llx %x:%x %llu", &start, &end, protection, &offset,
                   &major, &minor, &inode) != 7 || address < start || address >= end)
            continue;
        *start_out = start;
        *offset_out = offset;
        *device_out = ((uint64_t)major << 32) | minor;
        *inode_out = inode;
        break;
    }
    fclose(maps);
#else
    (void)address;
    (void)start_out;
    (void)offset_out;
    (void)device_out;
    (void)inode_out;
#endif
}

static void hl_backend_jcc_invalid_site_evidence(struct hl_backend_jcc_invalid_site *site) {
    hl_backend_jcc_invalid_mapping_evidence(site->source, &site->source_mapping_start,
                                            &site->source_mapping_offset, &site->source_mapping_device,
                                            &site->source_mapping_inode);
    hl_backend_jcc_invalid_mapping_evidence(site->target, &site->target_mapping_start,
                                            &site->target_mapping_offset, &site->target_mapping_device,
                                            &site->target_mapping_inode);
    if (site->target_mapping_start == 0) return;
    uint64_t page_end = (site->target & ~UINT64_C(0xfff)) + UINT64_C(0x1000);
    size_t available = (size_t)(page_end - site->target);
    site->target_bytes_len = available < 16 ? (uint8_t)available : 16;
    memcpy(&site->target_bytes_lo, (const void *)(uintptr_t)site->target,
           site->target_bytes_len < 8 ? site->target_bytes_len : 8);
    if (site->target_bytes_len > 8)
        memcpy(&site->target_bytes_hi, (const void *)(uintptr_t)(site->target + 8), site->target_bytes_len - 8);
    const struct hl_backend_translit_failure *failure = hl_backend_tree_translit_failure_find(site->target);
    if (failure != NULL) {
        site->build_failure_reason = failure->reason;
        site->build_failure_index = failure->index;
        site->build_failure_bytes_len = failure->bytes_len;
        site->build_failure_transient = failure->transient;
        site->build_failure_pc = failure->pc;
        site->build_failure_form = failure->form;
        site->build_failure_bytes_lo = failure->bytes_lo;
        site->build_failure_bytes_hi = failure->bytes_hi;
    }
}

static int hl_backend_jcc_invalid_site_format(char *record, size_t capacity,
                                              const struct hl_backend_jcc_invalid_site *site) {
    return snprintf(record, capacity,
                    "[diag] jcc-invalid-site version=2 source=%llu target=%llu reason=%u count=%llu "
                    "source_mapping_start=%llu source_mapping_offset=%llu source_mapping_device=%llu "
                    "source_mapping_inode=%llu target_mapping_start=%llu target_mapping_offset=%llu "
                    "target_mapping_device=%llu target_mapping_inode=%llu "
                    "target_bytes_len=%u target_bytes_lo=%016llx target_bytes_hi=%016llx "
                    "build_failure_reason=%u build_failure_index=%u build_failure_transient=%u "
                    "build_failure_pc=%llu build_failure_form=%llu build_failure_bytes_len=%u "
                    "build_failure_bytes_lo=%016llx build_failure_bytes_hi=%016llx\n",
                    (unsigned long long)site->source, (unsigned long long)site->target, site->reason,
                    (unsigned long long)atomic_load_explicit(&site->count, memory_order_relaxed),
                    (unsigned long long)site->source_mapping_start,
                    (unsigned long long)site->source_mapping_offset,
                    (unsigned long long)site->source_mapping_device,
                    (unsigned long long)site->source_mapping_inode,
                    (unsigned long long)site->target_mapping_start,
                    (unsigned long long)site->target_mapping_offset,
                    (unsigned long long)site->target_mapping_device,
                    (unsigned long long)site->target_mapping_inode, site->target_bytes_len,
                    (unsigned long long)site->target_bytes_lo, (unsigned long long)site->target_bytes_hi,
                    site->build_failure_reason, site->build_failure_index, site->build_failure_transient,
                    (unsigned long long)site->build_failure_pc,
                    (unsigned long long)site->build_failure_form, site->build_failure_bytes_len,
                    (unsigned long long)site->build_failure_bytes_lo,
                    (unsigned long long)site->build_failure_bytes_hi);
}

static void hl_backend_jcc_invalid_site_record(
    struct hl_backend_jcc_invalid_site sites[HL_BACKEND_JCC_INVALID_SITES], _Atomic uint64_t *unique,
    _Atomic uint64_t *overflow, unsigned reason, uint64_t source, uint64_t target) {
    uint64_t hash = hl_backend_executed_form_mix(source ^ (target << 1) ^ ((uint64_t)reason << 57));
    for (uint32_t probe = 0; probe < HL_BACKEND_JCC_INVALID_SITES; ++probe) {
        struct hl_backend_jcc_invalid_site *site =
            &sites[(uint32_t)(hash + probe) & (HL_BACKEND_JCC_INVALID_SITES - 1)];
        uint32_t state = atomic_load_explicit(&site->state, memory_order_acquire);
        if (state == 2 && site->reason == reason && site->source == source && site->target == target) {
            atomic_fetch_add_explicit(&site->count, 1, memory_order_relaxed);
            return;
        }
        if (state != 0) continue;
        uint32_t expected = 0;
        if (!atomic_compare_exchange_strong_explicit(&site->state, &expected, 1, memory_order_acq_rel,
                                                     memory_order_acquire))
            continue;
        site->reason = reason;
        site->source = source;
        site->target = target;
        if (reason == HL_BACKEND_JCC_INVALID_ENTRY_ZERO) hl_backend_jcc_invalid_site_evidence(site);
        atomic_store_explicit(&site->count, 1, memory_order_relaxed);
        atomic_store_explicit(&site->state, 2, memory_order_release);
        atomic_fetch_add_explicit(unique, 1, memory_order_relaxed);
        return;
    }
    atomic_fetch_add_explicit(overflow, 1, memory_order_relaxed);
}

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
    HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY,
    HL_BACKEND_SHAPE_T_INDIRECT_CALL,
    HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY,
    HL_BACKEND_SHAPE_T_SYSCALL,
    HL_BACKEND_SHAPE_T_IRQ,
    HL_BACKEND_SHAPE_T_FAULT,
    HL_BACKEND_SHAPE_T_OTHER,
    HL_BACKEND_SHAPE_T_COUNT,
};

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
    _Atomic uint32_t first_finalize_caller;
    _Atomic int first_finalize_actor;
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
    _Atomic uint64_t fallthrough_ibtc_fs_transaction_hits;
    _Atomic uint64_t fallthrough_ibtc_normal_to_fs_hits;
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
    _Atomic uint64_t jcc_late[HL_BACKEND_JCC_LATE_REASON_COUNT];
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
    _Atomic uint32_t first_finalize_caller;
    _Atomic uint32_t duplicate_finalize_caller;
    _Atomic uint32_t duplicate_slot_first_caller;
    _Atomic int duplicate_slot_first_actor;
    _Atomic uint32_t reported;
#if defined(HL_BACKEND_A64_OPCODE_CENSUS)
    _Atomic uint64_t a64_major[HL_BACKEND_A64_MAJOR_COUNT];
#endif
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
    _Atomic uint64_t direct_call_ibtc_emitted;
    _Atomic uint64_t direct_call_ibtc_hits;
    _Atomic uint64_t direct_call_ibtc_misses;
    _Atomic uint64_t direct_call_ibtc_irq;
    _Atomic uint64_t direct_call_ibtc_fills;
    _Atomic uint64_t direct_call_ibtc_invalid_refusals;
    _Atomic uint64_t direct_call_ibtc_fast_redispatch;
    _Atomic uint64_t direct_call_guard_candidate_enabled;
    _Atomic uint64_t direct_call_guard_attempts;
    _Atomic uint64_t direct_call_guard_fast_hits;
    _Atomic uint64_t direct_call_guard_key_misses;
    _Atomic uint64_t direct_call_guard_null_misses;
    _Atomic uint64_t direct_call_guard_irq;
    _Atomic uint64_t direct_call_guard_slow_entries;
    _Atomic uint64_t sse_riprel_form_keyed;
    _Atomic uint64_t sse_riprel_form_overflow;
    _Atomic uint64_t sse_riprel_form_unique;
    _Atomic uint64_t sse_riprel_form_collisions;
    struct hl_backend_executed_form sse_riprel_forms[HL_BACKEND_SSE_RIPREL_FORM_SLOTS];
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
    uint64_t jcc_late[HL_BACKEND_JCC_LATE_REASON_COUNT];
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

static void hl_backend_tree_sse_riprel_form(uint64_t key) {
    struct hl_backend_tree_shared *tree = g_backend_tree;
    // Like translated_fall_stop, this event belongs to the shared tree even before a fork child
    // has claimed its per-process lifecycle slot.
    if (tree == NULL) return;
    // A restored or legacy descriptor can carry no hook key. It still belongs in the exact total;
    // classify it as overflow rather than silently making the form census smaller than the fall census.
    if (key == 0) {
        atomic_fetch_add_explicit(&tree->sse_riprel_form_overflow, 1, memory_order_relaxed);
        return;
    }
    unsigned start = (unsigned)hl_backend_executed_form_mix(key) &
                     (HL_BACKEND_SSE_RIPREL_FORM_SLOTS - 1u);
    for (unsigned probe = 0; probe < HL_BACKEND_SSE_RIPREL_FORM_SLOTS; ++probe) {
        struct hl_backend_executed_form *form =
            &tree->sse_riprel_forms[(start + probe) & (HL_BACKEND_SSE_RIPREL_FORM_SLOTS - 1u)];
        uint32_t state = atomic_load_explicit(&form->state, memory_order_acquire);
        if (state == 2 && form->key == key) {
            atomic_fetch_add_explicit(&form->count, 1, memory_order_relaxed);
            atomic_fetch_add_explicit(&tree->sse_riprel_form_keyed, 1, memory_order_relaxed);
            return;
        }
        /* A fork child can die after reserving this MAP_SHARED slot. Never wait for another
           process's transient publication: preserve the exact total as overflow and let every
           surviving recorder continue. */
        if (state == 1) {
            atomic_fetch_add_explicit(&tree->sse_riprel_form_overflow, 1, memory_order_relaxed);
            return;
        }
        if (state == 0) {
            uint32_t expected = 0;
            if (!atomic_compare_exchange_strong_explicit(&form->state, &expected, 1,
                                                         memory_order_acquire, memory_order_relaxed)) {
                atomic_fetch_add_explicit(&tree->sse_riprel_form_overflow, 1, memory_order_relaxed);
                return;
            }
            form->key = key;
            atomic_store_explicit(&form->count, 1, memory_order_relaxed);
            atomic_store_explicit(&form->state, 2, memory_order_release);
            atomic_fetch_add_explicit(&tree->sse_riprel_form_unique, 1, memory_order_relaxed);
            atomic_fetch_add_explicit(&tree->sse_riprel_form_keyed, 1, memory_order_relaxed);
            return;
        }
        atomic_fetch_add_explicit(&tree->sse_riprel_form_collisions, 1, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(&tree->sse_riprel_form_overflow, 1, memory_order_relaxed);
}

static void hl_backend_tree_sse_riprel_snapshot(uint64_t keys[HL_BACKEND_SSE_RIPREL_FORM_TOP],
                                                 uint64_t counts[HL_BACKEND_SSE_RIPREL_FORM_TOP],
                                                 uint64_t *keyed, uint64_t *overflow,
                                                 uint64_t *unique, uint64_t *collisions) {
    struct hl_backend_tree_shared *tree = g_backend_tree;
    if (tree == NULL) return;
    *keyed = atomic_load_explicit(&tree->sse_riprel_form_keyed, memory_order_relaxed);
    *overflow = atomic_load_explicit(&tree->sse_riprel_form_overflow, memory_order_relaxed);
    *unique = atomic_load_explicit(&tree->sse_riprel_form_unique, memory_order_relaxed);
    *collisions = atomic_load_explicit(&tree->sse_riprel_form_collisions, memory_order_relaxed);
    for (unsigned slot = 0; slot < HL_BACKEND_SSE_RIPREL_FORM_SLOTS; ++slot) {
        struct hl_backend_executed_form *form = &tree->sse_riprel_forms[slot];
        if (atomic_load_explicit(&form->state, memory_order_acquire) != 2) continue;
        uint64_t key = form->key, count = atomic_load_explicit(&form->count, memory_order_relaxed);
        unsigned rank = 0;
        while (rank < HL_BACKEND_SSE_RIPREL_FORM_TOP &&
               (counts[rank] > count || (counts[rank] == count && keys[rank] <= key))) ++rank;
        if (rank == HL_BACKEND_SSE_RIPREL_FORM_TOP) continue;
        for (unsigned move = HL_BACKEND_SSE_RIPREL_FORM_TOP - 1; move > rank; --move) {
            keys[move] = keys[move - 1]; counts[move] = counts[move - 1];
        }
        keys[rank] = key; counts[rank] = count;
    }
}

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
    /* The process option store is bound before this lifecycle entry. Snapshot
       the immutable launch/test authority; all outcome observation remains ungated. */
    atomic_store_explicit(&g_backend_tree->direct_call_guard_candidate_enabled,
                          (uint64_t)(hl_option_flag_value("HL_TRANSLIT_DIRECT_CALL_PRE_SPILL", 0) ||
                                     hl_option_flag_value("HL_TRANSLIT_DIRECT_CALL_PRE_SPILL_TEST", 0)),
                          memory_order_relaxed);
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
static inline void hl_backend_tree_a64_body_retired(unsigned major) {
#if defined(HL_BACKEND_A64_OPCODE_CENSUS)
    if (g_backend_tree != NULL && major < HL_BACKEND_A64_MAJOR_COUNT)
        atomic_fetch_add_explicit(&g_backend_tree->a64_major[major], 1, memory_order_relaxed);
#else
    (void)major;
#endif
}
static inline void hl_backend_tree_a64_unsupported(uint32_t instruction) { (void)instruction; }

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

static _Atomic uint64_t hl_backend_x86_jcc_route_test[HL_BACKEND_X86_JCC_ROUTE_COUNT];
static int hl_backend_x86_jcc_route_test_enabled;
static inline uintptr_t hl_backend_tree_x86_jcc_route_counter_address(unsigned kind) {
    return hl_backend_x86_jcc_route_test_enabled && kind < HL_BACKEND_X86_JCC_ROUTE_COUNT
               ? (uintptr_t)&hl_backend_x86_jcc_route_test[kind]
               : 0;
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

static inline void hl_backend_tree_jcc_late(unsigned reason) {
    struct hl_backend_tree_slot *slot = g_backend_tree_self;
    if (slot != NULL && reason < HL_BACKEND_JCC_LATE_REASON_COUNT)
        atomic_fetch_add_explicit(&slot->jcc_late[reason], 1, memory_order_relaxed);
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
static inline void hl_backend_tree_map_miss(void) {}

static inline void hl_backend_tree_stw_retry(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->stw_retries, 1, memory_order_relaxed);
}

static inline void hl_backend_tree_irq_pending(void) {
    if (g_backend_tree_self != NULL)
        atomic_fetch_add_explicit(&g_backend_tree_self->irq_pending, 1, memory_order_relaxed);
}

static int hl_backend_tree_finalize_slot_in(struct hl_backend_tree_shared *shared, struct hl_backend_tree_slot *slot,
                                            int abnormal, unsigned caller) {
    if (slot == NULL) return 0;
    uint32_t expected = HL_BACKEND_TREE_CLAIMED;
    uint32_t completed = abnormal ? HL_BACKEND_TREE_ABNORMAL : HL_BACKEND_TREE_COMPLETED;
    if (atomic_compare_exchange_strong_explicit(&slot->lifecycle, &expected, completed, memory_order_acq_rel,
                                                memory_order_acquire)) {
        atomic_store_explicit(&slot->first_finalize_caller, caller, memory_order_relaxed);
        atomic_store_explicit(&slot->first_finalize_actor, (int)getpid(), memory_order_release);
        uint32_t unset = HL_BACKEND_FINALIZE_UNKNOWN;
        if (shared != NULL)
            (void)atomic_compare_exchange_strong_explicit(&shared->first_finalize_caller, &unset, caller,
                                                          memory_order_acq_rel, memory_order_relaxed);
        return 1;
    }
    if (shared != NULL) {
        atomic_fetch_add_explicit(&shared->duplicate_finalize, 1, memory_order_relaxed);
        uint32_t unset = HL_BACKEND_FINALIZE_UNKNOWN;
        if (atomic_compare_exchange_strong_explicit(&shared->duplicate_finalize_caller, &unset, caller,
                                                    memory_order_acq_rel, memory_order_relaxed)) {
            atomic_store_explicit(&shared->duplicate_slot_first_caller,
                                  atomic_load_explicit(&slot->first_finalize_caller, memory_order_acquire),
                                  memory_order_relaxed);
            atomic_store_explicit(&shared->duplicate_slot_first_actor,
                                  atomic_load_explicit(&slot->first_finalize_actor, memory_order_acquire),
                                  memory_order_release);
        }
    }
    return 0;
}

static int hl_backend_tree_finalize_slot(struct hl_backend_tree_slot *slot, int abnormal) {
    return hl_backend_tree_finalize_slot_in(g_backend_tree, slot, abnormal, HL_BACKEND_FINALIZE_UNKNOWN);
}

static int hl_backend_tree_finalize(int abnormal) {
    return hl_backend_tree_finalize_slot(g_backend_tree_self, abnormal);
}

static int hl_backend_tree_finalize_from(int abnormal, unsigned caller) {
    return hl_backend_tree_finalize_slot_in(g_backend_tree, g_backend_tree_self, abnormal, caller);
}

/* A reaper closes a child that could not execute its own finalizer (SIGKILL, host fault). */
static void hl_backend_tree_reaped(int pid) {
    if (g_backend_tree == NULL || pid <= 0) return;
    for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &g_backend_tree->slots[index];
        if (atomic_load_explicit(&slot->pid, memory_order_acquire) != pid) continue;
        uint32_t lifecycle = atomic_load_explicit(&slot->lifecycle, memory_order_acquire);
        if (lifecycle != HL_BACKEND_TREE_CLAIMED) continue;
        (void)hl_backend_tree_finalize_slot_in(g_backend_tree, slot, 1, HL_BACKEND_FINALIZE_REAPER);
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
                    (void)hl_backend_tree_finalize_slot_in(shared, slot, 1, HL_BACKEND_FINALIZE_PARENT_BARRIER);
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
        for (uint32_t reason = 0; reason < HL_BACKEND_JCC_LATE_REASON_COUNT; ++reason)
            summary->jcc_late[reason] += atomic_load_explicit(&slot->jcc_late[reason], memory_order_relaxed);
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
        "[diag] backend-shape-detail version=2 translated_entries=%llu translated_transfers=%llu "
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
        "stop6_key=%llu stop6_count=%llu stop7_key=%llu stop7_count=%llu "
        "direct_call_ibtc_emitted=%llu direct_call_ibtc_hits=%llu direct_call_ibtc_misses=%llu "
        "direct_call_ibtc_irq=%llu direct_call_ibtc_fills=%llu direct_call_ibtc_invalid_refusals=%llu "
        "direct_call_ibtc_fast_redispatch=%llu direct_call_guard_candidate_enabled=%llu "
        "direct_call_guard_attempts=%llu direct_call_guard_fast_hits=%llu direct_call_guard_key_misses=%llu "
        "direct_call_guard_null_misses=%llu direct_call_guard_irq=%llu direct_call_guard_slow_entries=%llu\n",
        (unsigned long long)summary.translated_entries, (unsigned long long)translated_transfers,
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_NOT_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_JUMP],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_CALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_RETURN],
        (unsigned long long)(summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH] +
                             summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY]),
        (unsigned long long)(summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL] +
                             summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY]),
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
        (unsigned long long)summary.stop_top_key[7], (unsigned long long)summary.stop_top_count[7],
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_emitted, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_hits, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_misses, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_irq, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_fills, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_invalid_refusals,
                                                memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_ibtc_fast_redispatch,
                                                memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_candidate_enabled,
                                                memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_attempts, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_fast_hits, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_key_misses, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_null_misses, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_irq, memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&shared->direct_call_guard_slow_entries, memory_order_relaxed));
}

static int hl_backend_exit_family_format(struct hl_backend_tree_shared *shared, char *record, size_t capacity) {
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary_in(shared, &summary);
    uint64_t total = 0;
    for (unsigned shape = 0; shape < HL_BACKEND_SHAPE_T_COUNT; ++shape)
        total += summary.translated_exit[shape];
    return snprintf(
        record, capacity,
        "[diag] x86-exit-family version=1 translated_entries=%llu total=%llu "
        "t_fallthrough=%llu t_jcc_taken=%llu t_jcc_fall=%llu t_direct_jmp=%llu t_direct_call=%llu "
        "t_ret=%llu t_jmp_reg=%llu t_jmp_mem=%llu t_call_reg=%llu t_call_mem=%llu "
        "t_syscall=%llu t_irq=%llu t_fault=%llu t_other=%llu\n",
        (unsigned long long)summary.translated_entries, (unsigned long long)total,
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_COND_NOT_TAKEN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_JUMP],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_DIRECT_CALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_RETURN],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_SYSCALL],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_IRQ],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_FAULT],
        (unsigned long long)summary.translated_exit[HL_BACKEND_SHAPE_T_OTHER]);
}

static int hl_backend_would_link_format(struct hl_backend_tree_shared *shared, char *record, size_t capacity) {
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary_in(shared, &summary);
    uint64_t candidate[HL_BACKEND_WOULD_LINK_FAMILY_COUNT] = {0};
    for (unsigned family = 0; family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT; ++family)
        for (unsigned disposition = 0; disposition < HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT; ++disposition)
            candidate[family] += summary.would_link[family][disposition];
    uint64_t jcc_late_candidate = 0;
    for (unsigned reason = 0; reason < HL_BACKEND_JCC_LATE_REASON_COUNT; ++reason)
        jcc_late_candidate += summary.jcc_late[reason];
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
        "call_target_page=%llu call_rel32=%llu "
        "jcc_late_candidate=%llu jcc_late_eligible=%llu jcc_late_invalid=%llu "
        "jcc_late_target_absent=%llu jcc_late_page_generation=%llu "
        "jcc_late_displacement=%llu jcc_late_other=%llu\n",
        WL_ARGS(HL_BACKEND_WOULD_LINK_FALLTHROUGH), WL_ARGS(HL_BACKEND_WOULD_LINK_DIRECT_JUMP),
        WL_ARGS(HL_BACKEND_WOULD_LINK_DIRECT_CALL), (unsigned long long)jcc_late_candidate,
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_ELIGIBLE],
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_INVALID],
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_TARGET_ABSENT],
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_PAGE_GENERATION],
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_DISPLACEMENT],
        (unsigned long long)summary.jcc_late[HL_BACKEND_JCC_LATE_OTHER]);
#undef WL_ARGS
    return formatted;
}

void hl_target_backend_tree_reap_report(void *opaque, size_t shared_size, hl_linux_abi *box, int diagnostic_port) {
    hl_backend_report_descriptor = diagnostic_port >= 0 ? diagnostic_port : STDERR_FILENO;
    struct hl_backend_tree_shared *shared = opaque;
    if (shared == NULL || shared_size != sizeof *shared) return;
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
    int exit_family = hl_backend_exit_family_format(shared, record + formatted, sizeof record - (size_t)formatted);
    if (exit_family <= 0 || (size_t)exit_family >= sizeof record - (size_t)formatted) return;
    formatted += exit_family;
    int would_link = hl_backend_would_link_format(shared, record + formatted, sizeof record - (size_t)formatted);
    if (would_link <= 0 || (size_t)would_link >= sizeof record - (size_t)formatted) return;
    formatted += would_link;
#if defined(HL_BACKEND_A64_OPCODE_CENSUS)
    uint64_t major[HL_BACKEND_A64_MAJOR_COUNT];
    uint64_t family[6] = {0};
    uint64_t body_retired = 0;
    static const unsigned char family_for_major[HL_BACKEND_A64_MAJOR_COUNT] = {
        0, 0, 0, 0, 1, 2, 1, 5, 3, 3, 4, 4, 1, 2, 1, 5,
    };
    for (unsigned i = 0; i < HL_BACKEND_A64_MAJOR_COUNT; ++i) {
        major[i] = atomic_load_explicit(&shared->a64_major[i], memory_order_relaxed);
        body_retired += major[i];
        family[family_for_major[i]] += major[i];
    }
    int a64 = snprintf(record + formatted, sizeof record - (size_t)formatted,
                       "[diag] aarch64-opcode version=1 available=1 body_retired=%llu "
                       "major0=%llu major1=%llu major2=%llu major3=%llu major4=%llu major5=%llu "
                       "major6=%llu major7=%llu major8=%llu major9=%llu major10=%llu major11=%llu "
                       "major12=%llu major13=%llu major14=%llu major15=%llu "
                       "reserved=%llu load_store=%llu dp_register=%llu dp_immediate=%llu "
                       "branch_system=%llu simd_fp=%llu",
                       (unsigned long long)body_retired,
                       (unsigned long long)major[0], (unsigned long long)major[1],
                       (unsigned long long)major[2], (unsigned long long)major[3],
                       (unsigned long long)major[4], (unsigned long long)major[5],
                       (unsigned long long)major[6], (unsigned long long)major[7],
                       (unsigned long long)major[8], (unsigned long long)major[9],
                       (unsigned long long)major[10], (unsigned long long)major[11],
                       (unsigned long long)major[12], (unsigned long long)major[13],
                       (unsigned long long)major[14], (unsigned long long)major[15],
                       (unsigned long long)family[0], (unsigned long long)family[1],
                       (unsigned long long)family[2], (unsigned long long)family[3],
                       (unsigned long long)family[4], (unsigned long long)family[5]);
    if (a64 <= 0 || (size_t)a64 >= sizeof record - (size_t)formatted) return;
    formatted += a64;
#endif
    size_t offset = 0;
    while (offset < (size_t)formatted) {
        int64_t written = hl_backend_report_write(box, record + offset, (size_t)formatted - offset);
        if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)formatted - offset) return;
        offset += (size_t)written;
    }
}

static _Noreturn void hl_backend_tree_abnormal_exit(int status) {
    (void)hl_backend_tree_finalize_from(1, HL_BACKEND_FINALIZE_ABNORMAL_EXIT);
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

static int hl_backend_tree_jcc_late_eligible_test(void);

static int hl_backend_tree_test_scenario(uint32_t scenario, const hl_host_services *host) {
    if (scenario == 21) return hl_backend_tree_jcc_late_eligible_test() ? 0 : 119;
    hl_backend_tree_begin(1, host);
    if (g_backend_tree_self == NULL) return 10;
    if (scenario == 22) {
        atomic_store_explicit(&g_backend_tree_self->translated_entries, 2, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree_self->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY], 1,
                              memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree_self->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY], 1,
                              memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_candidate_enabled, 1, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_attempts, 4, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_fast_hits, 1, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_key_misses, 1, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_null_misses, 1, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_irq, 1, memory_order_relaxed);
        atomic_store_explicit(&g_backend_tree->direct_call_guard_slow_entries, 3, memory_order_relaxed);
        char record[8192];
        int formatted = hl_backend_shape_format(g_backend_tree, record, sizeof record);
        return formatted > 0 && (size_t)formatted < sizeof record &&
                       strstr(record, "backend-shape-detail version=2 ") != NULL &&
                       strstr(record, "t_indirect_branch=1 t_indirect_call=1 ") != NULL &&
                       strstr(record, "direct_call_guard_candidate_enabled=1 ") != NULL &&
                       strstr(record, "direct_call_guard_attempts=4 ") != NULL &&
                       strstr(record, "direct_call_guard_fast_hits=1 ") != NULL &&
                       strstr(record, "direct_call_guard_key_misses=1 ") != NULL &&
                       strstr(record, "direct_call_guard_null_misses=1 ") != NULL &&
                       strstr(record, "direct_call_guard_irq=1 ") != NULL &&
                       strstr(record, "direct_call_guard_slow_entries=3\n") != NULL &&
                       atomic_load_explicit(&g_backend_tree->direct_call_guard_attempts,
                                            memory_order_relaxed) ==
                           atomic_load_explicit(&g_backend_tree->direct_call_guard_fast_hits,
                                                memory_order_relaxed) +
                               atomic_load_explicit(&g_backend_tree->direct_call_guard_key_misses,
                                                    memory_order_relaxed) +
                               atomic_load_explicit(&g_backend_tree->direct_call_guard_null_misses,
                                                    memory_order_relaxed) +
                               atomic_load_explicit(&g_backend_tree->direct_call_guard_irq,
                                                    memory_order_relaxed)
                   ? 0
                   : 120;
    }
    if (scenario == 17) return HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE == 0 ? 0 : 110;
    if (scenario == 18) return HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE == 1 ? 0 : 111;
    if (scenario == 19) {
        hl_backend_tree_sse_riprel_form(UINT64_C(0x111));
        pid_t child = fork();
        if (child < 0) return 112;
        if (child == 0) {
            hl_backend_tree_sse_riprel_form(UINT64_C(0x222));
            _exit(0);
        }
        if (hl_backend_tree_wait(child, 0) != 0) return 113;
        uint64_t keys[HL_BACKEND_SSE_RIPREL_FORM_TOP] = {0};
        uint64_t counts[HL_BACKEND_SSE_RIPREL_FORM_TOP] = {0};
        uint64_t keyed = 0, overflow = 0, unique = 0, collisions = 0;
        hl_backend_tree_sse_riprel_snapshot(keys, counts, &keyed, &overflow, &unique, &collisions);
        if (keyed != 2 || overflow != 0 || unique != 2 || counts[0] != 1 || counts[1] != 1) return 114;
        hl_backend_tree_begin(1, host);
        memset(keys, 0, sizeof keys);
        memset(counts, 0, sizeof counts);
        keyed = overflow = unique = collisions = 0;
        hl_backend_tree_sse_riprel_snapshot(keys, counts, &keyed, &overflow, &unique, &collisions);
        return keyed == 0 && overflow == 0 && unique == 0 && collisions == 0 ? 0 : 115;
    }
    if (scenario == 20) {
        uint64_t key = UINT64_C(0x333);
        unsigned start = (unsigned)hl_backend_executed_form_mix(key) &
                         (HL_BACKEND_SSE_RIPREL_FORM_SLOTS - 1u);
        pid_t child = fork();
        if (child < 0) return 116;
        if (child == 0) {
            atomic_store_explicit(&g_backend_tree->sse_riprel_forms[start].state, 1,
                                  memory_order_release);
            _exit(0);
        }
        if (hl_backend_tree_wait(child, 0) != 0) return 117;
        hl_backend_tree_sse_riprel_form(key);
        uint64_t keys[HL_BACKEND_SSE_RIPREL_FORM_TOP] = {0};
        uint64_t counts[HL_BACKEND_SSE_RIPREL_FORM_TOP] = {0};
        uint64_t keyed = 0, overflow = 0, unique = 0, collisions = 0;
        hl_backend_tree_sse_riprel_snapshot(keys, counts, &keyed, &overflow, &unique, &collisions);
        return keyed == 0 && overflow == 1 && unique == 0 ? 0 : 118;
    }
    if (scenario == 16) {
        struct hl_backend_tree_slot *birth = hl_backend_tree_prepare_fork();
        if (birth == NULL) return 104;
        pid_t child = fork();
        hl_backend_tree_after_fork(child, birth);
        if (child < 0) return 105;
        if (child == 0) _exit(hl_backend_tree_finalize(0) ? 0 : 106);
        if (hl_backend_tree_wait(child, 0) != 0) return 107;
        if (!hl_backend_tree_finalize(0)) return 108;
        return atomic_load_explicit(&g_backend_tree->duplicate_finalize, memory_order_relaxed) == 0 &&
                       atomic_load_explicit(&birth->pid, memory_order_acquire) == child &&
                       atomic_load_explicit(&birth->lifecycle, memory_order_acquire) ==
                           HL_BACKEND_TREE_COMPLETED
                   ? 0
                   : 109;
    }
    if (scenario == 15) {
        if (!hl_backend_tree_finalize_from(0, HL_BACKEND_FINALIZE_PROCESS_EXIT))
            return 100;
        struct hl_backend_tree_slot *second = hl_backend_tree_reserve();
        if (second == NULL) return 102;
        hl_backend_tree_publish(second, (int)getpid());
        if (!hl_backend_tree_finalize_slot_in(g_backend_tree, second, 0, HL_BACKEND_FINALIZE_FATAL_SIGNAL) ||
            hl_backend_tree_finalize_slot_in(g_backend_tree, second, 1, HL_BACKEND_FINALIZE_REAPER))
            return 103;
        return atomic_load_explicit(&g_backend_tree->first_finalize_caller, memory_order_acquire) ==
                           HL_BACKEND_FINALIZE_PROCESS_EXIT &&
                       atomic_load_explicit(&g_backend_tree->duplicate_finalize_caller, memory_order_acquire) ==
                           HL_BACKEND_FINALIZE_REAPER &&
                       atomic_load_explicit(&g_backend_tree->duplicate_slot_first_caller, memory_order_acquire) ==
                           HL_BACKEND_FINALIZE_FATAL_SIGNAL &&
                       atomic_load_explicit(&g_backend_tree->duplicate_slot_first_actor, memory_order_acquire) ==
                           (int)getpid() &&
                       atomic_load_explicit(&g_backend_tree->duplicate_finalize, memory_order_relaxed) == 1
                   ? 0
                   : 101;
    }
    if (scenario == 14) {
        struct hl_backend_tree_slot *paused = hl_backend_tree_prepare_fork();
        if (paused == NULL || atomic_load_explicit(&paused->pid, memory_order_acquire) != -1 ||
            atomic_load_explicit(&paused->lifecycle, memory_order_acquire) != HL_BACKEND_TREE_CLAIMED)
            return 97;
        uint64_t reserved = 0;
        for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index)
            if (atomic_load_explicit(&g_backend_tree->slots[index].pid, memory_order_acquire) == -1) ++reserved;
        if (reserved != 1) return 98;
        hl_backend_tree_after_fork(-1, paused);
        reserved = 0;
        for (uint32_t index = 0; index < HL_BACKEND_TREE_SLOTS; ++index)
            if (atomic_load_explicit(&g_backend_tree->slots[index].pid, memory_order_acquire) == -1) ++reserved;
        return reserved == 0 && atomic_load_explicit(&paused->pid, memory_order_acquire) == 0 &&
                       atomic_load_explicit(&paused->lifecycle, memory_order_acquire) == 0
                   ? 0
                   : 99;
    }
    if (scenario == 4) {
        if (!hl_backend_tree_finalize(0) || hl_backend_tree_finalize(0)) return 41;
        struct hl_backend_tree_summary summary;
        hl_backend_tree_summary(&summary);
        return summary.claimed == 1 && summary.completed == 1 && summary.duplicate_finalize == 1 ? 0 : 42;
    }
    if (scenario == 13) {
        struct invalid_fixture {
            _Atomic uint64_t unique, overflow;
            struct hl_backend_jcc_invalid_site sites[HL_BACKEND_JCC_INVALID_SITES];
        } *fixture = mmap(NULL, sizeof *fixture, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (fixture == MAP_FAILED) return 95;
        for (uint64_t index = 0; index < HL_BACKEND_JCC_INVALID_SITES; ++index)
            hl_backend_jcc_invalid_site_record(fixture->sites, &fixture->unique, &fixture->overflow,
                                               HL_BACKEND_JCC_INVALID_MAGIC, 0x400000 + index,
                                               0x800000 + index);
        hl_backend_jcc_invalid_site_record(fixture->sites, &fixture->unique, &fixture->overflow,
                                           HL_BACKEND_JCC_INVALID_MAGIC, 0x400000, 0x800000);
        hl_backend_jcc_invalid_site_record(fixture->sites, &fixture->unique, &fixture->overflow,
                                           HL_BACKEND_JCC_INVALID_GPC, 0xdead, 0xbeef);
        struct invalid_fixture *evidence = mmap(NULL, sizeof *evidence, PROT_READ | PROT_WRITE,
                                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (evidence == MAP_FAILED) {
            (void)munmap(fixture, sizeof *fixture);
            return 97;
        }
        uint8_t target_bytes[16];
        for (unsigned index = 0; index < sizeof target_bytes; ++index) target_bytes[index] = (uint8_t)(index + 1);
        int saved_prof = g_prof;
        g_prof = 1;
        int failures_ok = 1;
        for (unsigned reason = HL_BACKEND_TRANSLIT_FAILURE_IMAGE;
             reason <= HL_BACKEND_TRANSLIT_FAILURE_OWNER_PUBLISH; ++reason) {
            uint64_t key = UINT64_C(0x70000000) + reason * UINT64_C(0x1000);
            hl_backend_tree_translit_failure(reason, key, key + 7, reason, UINT64_C(0xabc000) + reason,
                                             target_bytes, sizeof target_bytes,
                                             reason == HL_BACKEND_TRANSLIT_FAILURE_TRANSACTION);
            const struct hl_backend_translit_failure *failure = hl_backend_tree_translit_failure_find(key);
            failures_ok &= failure != NULL && failure->reason == reason && failure->pc == key + 7 &&
                           failure->index == reason && failure->form == UINT64_C(0xabc000) + reason &&
                           failure->bytes_len == 16 &&
                           failure->bytes_lo == UINT64_C(0x0807060504030201) &&
                           failure->transient == (reason == HL_BACKEND_TRANSLIT_FAILURE_TRANSACTION);
        }
        hl_backend_tree_translit_failure(HL_BACKEND_TRANSLIT_FAILURE_TRANSACTION,
                                         (uint64_t)(uintptr_t)target_bytes,
                                         (uint64_t)(uintptr_t)target_bytes + 3, 2, UINT64_C(0x123456),
                                         target_bytes, sizeof target_bytes, 1);
        hl_backend_jcc_invalid_site_record(evidence->sites, &evidence->unique, &evidence->overflow,
                                           HL_BACKEND_JCC_INVALID_ENTRY_ZERO, 0x1234,
                                           (uint64_t)(uintptr_t)target_bytes);
        g_prof = saved_prof;
        uint64_t duplicate_count = 0;
        int evidence_ok = 0;
        for (uint32_t slot = 0; slot < HL_BACKEND_JCC_INVALID_SITES; ++slot)
            if (atomic_load_explicit(&fixture->sites[slot].state, memory_order_acquire) == 2 &&
                fixture->sites[slot].source == 0x400000 && fixture->sites[slot].target == 0x800000)
                duplicate_count = atomic_load_explicit(&fixture->sites[slot].count, memory_order_relaxed);
        for (uint32_t slot = 0; slot < HL_BACKEND_JCC_INVALID_SITES; ++slot) {
            struct hl_backend_jcc_invalid_site *site = &evidence->sites[slot];
            if (atomic_load_explicit(&site->state, memory_order_acquire) == 2) {
                char record[896];
                int formatted = hl_backend_jcc_invalid_site_format(record, sizeof record, site);
                evidence_ok = site->target_mapping_start != 0 && site->target_bytes_len == 16 &&
                              site->target_bytes_lo == UINT64_C(0x0807060504030201) &&
                              site->target_bytes_hi == UINT64_C(0x100f0e0d0c0b0a09) && formatted > 0 &&
                              (size_t)formatted < sizeof record &&
                              strstr(record, "jcc-invalid-site version=2 ") != NULL &&
                              strstr(record, "target_bytes_len=16 target_bytes_lo=0807060504030201 ") != NULL &&
                              site->build_failure_reason == HL_BACKEND_TRANSLIT_FAILURE_TRANSACTION &&
                              site->build_failure_index == 2 && site->build_failure_transient == 1 &&
                              site->build_failure_pc == (uint64_t)(uintptr_t)target_bytes + 3 &&
                              site->build_failure_form == UINT64_C(0x123456);
            }
        }
        int ok = atomic_load_explicit(&fixture->unique, memory_order_relaxed) ==
                     HL_BACKEND_JCC_INVALID_SITES &&
                 atomic_load_explicit(&fixture->overflow, memory_order_relaxed) == 1 &&
                 duplicate_count == 2 && evidence_ok && failures_ok;
        (void)munmap(evidence, sizeof *evidence);
        (void)munmap(fixture, sizeof *fixture);
        return ok ? 0 : 96;
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
        for (unsigned reason = 0; reason < HL_BACKEND_JCC_LATE_REASON_COUNT; ++reason)
            hl_backend_tree_jcc_late(reason);
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
        char record[2048];
        int formatted = hl_backend_would_link_format(g_backend_tree, record, sizeof record);
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
                                                 [HL_BACKEND_WOULD_LINK_REL32] == 1 &&
                       summary.jcc_late[HL_BACKEND_JCC_LATE_ELIGIBLE] == 1 &&
                       formatted > 0 && (size_t)formatted < sizeof record &&
                       strstr(record, "jcc_late_candidate=6 jcc_late_eligible=1 jcc_late_invalid=1 ") != NULL &&
                       strstr(record, "jcc_late_page_generation=1 jcc_late_displacement=1 jcc_late_other=1") != NULL
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
    if (scenario == 12) {
        struct concurrency_fixture {
            _Atomic uint64_t total, unique, overflow;
            _Atomic uint32_t pause_ready, pause_release, start, waiter_ready[2], reserved_seen[2];
            struct hl_backend_executed_form forms[HL_BACKEND_EXECUTED_FORM_SLOTS];
        } *fixture = mmap(NULL, sizeof *fixture, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        if (fixture == MAP_FAILED) return 91;
        pid_t winner = fork();
        if (winner == 0) {
            hl_backend_executed_form_record(fixture->forms, &fixture->total, &fixture->unique,
                                            &fixture->overflow, 2307, NULL, &fixture->pause_ready,
                                            &fixture->pause_release);
            _exit(0);
        }
        while (!atomic_load_explicit(&fixture->pause_ready, memory_order_acquire)) sched_yield();
        pid_t waiters[2];
        const uint64_t keys[2] = {2307, 12547}; /* Same initial bucket, same and different keys. */
        for (unsigned index = 0; index < 2; ++index) {
            waiters[index] = fork();
            if (waiters[index] == 0) {
                atomic_store_explicit(&fixture->waiter_ready[index], 1, memory_order_release);
                while (!atomic_load_explicit(&fixture->start, memory_order_acquire)) sched_yield();
                hl_backend_executed_form_record(fixture->forms, &fixture->total, &fixture->unique,
                                                &fixture->overflow, keys[index], &fixture->reserved_seen[index],
                                                NULL, NULL);
                _exit(0);
            }
        }
        while (!atomic_load_explicit(&fixture->waiter_ready[0], memory_order_acquire) ||
               !atomic_load_explicit(&fixture->waiter_ready[1], memory_order_acquire))
            sched_yield();
        atomic_store_explicit(&fixture->start, 1, memory_order_release);
        while (!atomic_load_explicit(&fixture->reserved_seen[0], memory_order_acquire) ||
               !atomic_load_explicit(&fixture->reserved_seen[1], memory_order_acquire))
            sched_yield();
        atomic_store_explicit(&fixture->pause_release, 1, memory_order_release);
        int status = 0;
        if (waitpid(winner, &status, 0) != winner || status != 0) return 92;
        for (unsigned index = 0; index < 2; ++index)
            if (waitpid(waiters[index], &status, 0) != waiters[index] || status != 0) return 93;
        uint64_t first = 0, second = 0;
        for (unsigned slot = 0; slot < HL_BACKEND_EXECUTED_FORM_SLOTS; ++slot) {
            if (atomic_load_explicit(&fixture->forms[slot].state, memory_order_acquire) != 2) continue;
            if (fixture->forms[slot].key == 2307)
                first = atomic_load_explicit(&fixture->forms[slot].count, memory_order_relaxed);
            if (fixture->forms[slot].key == 12547)
                second = atomic_load_explicit(&fixture->forms[slot].count, memory_order_relaxed);
        }
        int ok = atomic_load_explicit(&fixture->total, memory_order_relaxed) == 3 &&
                 atomic_load_explicit(&fixture->unique, memory_order_relaxed) == 2 &&
                 atomic_load_explicit(&fixture->overflow, memory_order_relaxed) == 0 && first == 2 && second == 1;
        (void)munmap(fixture, sizeof *fixture);
        return ok ? 0 : 94;
    }
    return 40;
}
#endif

enum hl_backend_direct_call_ibtc_counter {
    HL_BACKEND_DIRECT_CALL_IBTC_EMITTED,
    HL_BACKEND_DIRECT_CALL_IBTC_HIT,
    HL_BACKEND_DIRECT_CALL_IBTC_MISS,
    HL_BACKEND_DIRECT_CALL_IBTC_IRQ,
    HL_BACKEND_DIRECT_CALL_IBTC_FILL,
    HL_BACKEND_DIRECT_CALL_IBTC_INVALID_REFUSAL,
    HL_BACKEND_DIRECT_CALL_IBTC_FAST_REDISPATCH,
    HL_BACKEND_DIRECT_CALL_GUARD_ENABLED,
    HL_BACKEND_DIRECT_CALL_GUARD_ATTEMPT,
    HL_BACKEND_DIRECT_CALL_GUARD_HIT,
    HL_BACKEND_DIRECT_CALL_GUARD_KEY_MISS,
    HL_BACKEND_DIRECT_CALL_GUARD_NULL_MISS,
    HL_BACKEND_DIRECT_CALL_GUARD_IRQ,
    HL_BACKEND_DIRECT_CALL_GUARD_SLOW,
};
static _Atomic uint64_t *hl_backend_tree_direct_call_ibtc_counter(
    enum hl_backend_direct_call_ibtc_counter kind) {
    struct hl_backend_tree_shared *tree = g_backend_tree;
    if (tree == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_DIRECT_CALL_IBTC_EMITTED: return &tree->direct_call_ibtc_emitted;
    case HL_BACKEND_DIRECT_CALL_IBTC_HIT: return &tree->direct_call_ibtc_hits;
    case HL_BACKEND_DIRECT_CALL_IBTC_MISS: return &tree->direct_call_ibtc_misses;
    case HL_BACKEND_DIRECT_CALL_IBTC_IRQ: return &tree->direct_call_ibtc_irq;
    case HL_BACKEND_DIRECT_CALL_IBTC_FILL: return &tree->direct_call_ibtc_fills;
    case HL_BACKEND_DIRECT_CALL_IBTC_INVALID_REFUSAL: return &tree->direct_call_ibtc_invalid_refusals;
    case HL_BACKEND_DIRECT_CALL_IBTC_FAST_REDISPATCH: return &tree->direct_call_ibtc_fast_redispatch;
    case HL_BACKEND_DIRECT_CALL_GUARD_ENABLED: return &tree->direct_call_guard_candidate_enabled;
    case HL_BACKEND_DIRECT_CALL_GUARD_ATTEMPT: return &tree->direct_call_guard_attempts;
    case HL_BACKEND_DIRECT_CALL_GUARD_HIT: return &tree->direct_call_guard_fast_hits;
    case HL_BACKEND_DIRECT_CALL_GUARD_KEY_MISS: return &tree->direct_call_guard_key_misses;
    case HL_BACKEND_DIRECT_CALL_GUARD_NULL_MISS: return &tree->direct_call_guard_null_misses;
    case HL_BACKEND_DIRECT_CALL_GUARD_IRQ: return &tree->direct_call_guard_irq;
    case HL_BACKEND_DIRECT_CALL_GUARD_SLOW: return &tree->direct_call_guard_slow_entries;
    }
    return NULL;
}
static uintptr_t hl_backend_tree_direct_call_ibtc_dynamic_counter_address(
    enum hl_backend_direct_call_ibtc_counter kind) {
    return (uintptr_t)hl_backend_tree_direct_call_ibtc_counter(kind);
}
static void hl_backend_tree_direct_call_ibtc_add(enum hl_backend_direct_call_ibtc_counter kind,
                                                 uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_direct_call_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}

HL_API int HL_BACKEND_TREE_TEST_NAME(uint32_t scenario) {
#if defined(_WIN32)
    (void)scenario;
    return 4;
#else
    return hl_backend_tree_test_scenario(scenario, effective_host_services());
#endif
}

static _Atomic uint64_t hl_backend_jcc_fill_test_empty;
static _Atomic uint64_t hl_backend_jcc_fill_test_collision;
static _Atomic uint64_t hl_backend_jcc_fill_test_irq;
static _Atomic uint64_t hl_backend_jcc_fill_test_same_key;
static int hl_backend_jcc_fill_test_enabled;

static void hl_backend_tree_jcc_ibtc_fill_cause(uint64_t source, uint64_t target,
                                                uint64_t previous_target, int interrupt_consumed) {
    (void)source;
    if (!hl_backend_jcc_fill_test_enabled) return;
    hl_backend_jcc_fill_record(&hl_backend_jcc_fill_test_empty, &hl_backend_jcc_fill_test_collision,
                               &hl_backend_jcc_fill_test_irq, &hl_backend_jcc_fill_test_same_key,
                               target, previous_target, interrupt_consumed);
}

static void hl_backend_tree_jcc_ibtc_fill_cause_test_begin(void) {
    atomic_store_explicit(&hl_backend_jcc_fill_test_empty, 0, memory_order_relaxed);
    atomic_store_explicit(&hl_backend_jcc_fill_test_collision, 0, memory_order_relaxed);
    atomic_store_explicit(&hl_backend_jcc_fill_test_irq, 0, memory_order_relaxed);
    atomic_store_explicit(&hl_backend_jcc_fill_test_same_key, 0, memory_order_relaxed);
    hl_backend_jcc_fill_test_enabled = 1;
}

static int hl_backend_tree_jcc_ibtc_fill_cause_test_end(void) {
    hl_backend_jcc_fill_test_enabled = 0;
    return atomic_load_explicit(&hl_backend_jcc_fill_test_empty, memory_order_relaxed) == 1 &&
           atomic_load_explicit(&hl_backend_jcc_fill_test_collision, memory_order_relaxed) == 1 &&
           atomic_load_explicit(&hl_backend_jcc_fill_test_irq, memory_order_relaxed) == 1 &&
           atomic_load_explicit(&hl_backend_jcc_fill_test_same_key, memory_order_relaxed) == 1;
}

static void hl_backend_tree_jcc_late_eligible(uint64_t cache_generation, uint64_t source,
                                              uint64_t target, uintptr_t body,
                                              uint64_t generation, int generation_current) {
    (void)cache_generation; (void)source; (void)target; (void)body; (void)generation;
    (void)generation_current;
}

struct hl_backend_jcc_late_test_shared {
    struct hl_backend_jcc_late_site sites[16];
    _Atomic uint64_t first, repeated, stable, changed, current, retired, unique, overflow, abandoned, maximum;
    _Atomic uint32_t pause_ready, pause_release, reserved_seen;
};

struct hl_backend_jcc_late_test_call {
    struct hl_backend_jcc_late_test_shared *shared;
    uint32_t site_count;
    int process;
    uint64_t birth;
    _Atomic uint32_t *pause_ready;
    _Atomic uint32_t *pause_release;
    _Atomic uint32_t *reserved_seen;
};

static void *hl_backend_jcc_late_test_worker(void *opaque) {
    struct hl_backend_jcc_late_test_call *call = opaque;
    struct hl_backend_jcc_late_test_shared *s = call->shared;
    hl_backend_jcc_late_record(s->sites, call->site_count, &s->first, &s->repeated, &s->stable, &s->changed,
                               &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                               &s->maximum, call->process, call->birth, 7, UINT64_C(0x1000),
                               UINT64_C(0x2000), UINT64_C(0x3000), 7, 1, call->pause_ready,
                               call->pause_release, call->reserved_seen);
    return NULL;
}

static int hl_backend_tree_jcc_late_eligible_test(void) {
    struct hl_backend_jcc_late_test_shared *s = mmap(NULL, sizeof *s, PROT_READ | PROT_WRITE,
                                                      MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (s == MAP_FAILED) return 0;
    memset(s, 0, sizeof *s);
    int self = (int)getpid();
    uint64_t birth = 0;
    if (!hl_host_process_start_time_ns(self, &birth)) { munmap(s, sizeof *s); return 0; }
    struct hl_backend_jcc_late_test_call first = {s, 2, self, birth, &s->pause_ready, &s->pause_release, NULL};
    struct hl_backend_jcc_late_test_call second = {s, 2, self, birth, NULL, &s->pause_release, &s->reserved_seen};
    pthread_t a, b;
    if (pthread_create(&a, NULL, hl_backend_jcc_late_test_worker, &first) != 0) goto fail;
    while (!atomic_load_explicit(&s->pause_ready, memory_order_acquire)) sched_yield();
    if (pthread_create(&b, NULL, hl_backend_jcc_late_test_worker, &second) != 0) goto release_a;
    while (!atomic_load_explicit(&s->reserved_seen, memory_order_acquire)) sched_yield();
    atomic_store_explicit(&s->pause_release, 1, memory_order_release);
    (void)pthread_join(a, NULL); (void)pthread_join(b, NULL);
    hl_backend_jcc_late_record(s->sites, 2, &s->first, &s->repeated, &s->stable, &s->changed,
                               &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                               &s->maximum, self, birth, 7, UINT64_C(0x1000), UINT64_C(0x2000),
                               UINT64_C(0x4000), 8, 1, NULL, NULL, NULL);
    int exact = atomic_load_explicit(&s->first, memory_order_relaxed) == 1 &&
                atomic_load_explicit(&s->repeated, memory_order_relaxed) == 2 &&
                atomic_load_explicit(&s->stable, memory_order_relaxed) == 1 &&
                atomic_load_explicit(&s->changed, memory_order_relaxed) == 1 &&
                atomic_load_explicit(&s->current, memory_order_relaxed) == 3 &&
                atomic_load_explicit(&s->unique, memory_order_relaxed) == 1 &&
                atomic_load_explicit(&s->overflow, memory_order_relaxed) == 0 &&
                atomic_load_explicit(&s->maximum, memory_order_relaxed) == 3;
    if (!exact) goto fail;

    memset(s, 0, sizeof *s);
    pid_t child = fork();
    if (child < 0) goto fail;
    if (child == 0) {
        struct hl_backend_jcc_late_test_call dead = {s, 1, (int)getpid(), 0,
                                                     &s->pause_ready, &s->pause_release, NULL};
        if (!hl_host_process_start_time_ns(dead.process, &dead.birth)) _exit(2);
        (void)hl_backend_jcc_late_test_worker(&dead);
        _exit(3);
    }
    while (!atomic_load_explicit(&s->pause_ready, memory_order_acquire)) sched_yield();
    (void)kill(child, SIGKILL);
    (void)waitpid(child, NULL, 0);
    struct hl_backend_jcc_late_test_call reclaim = {s, 1, self, birth, NULL, NULL, NULL};
    (void)hl_backend_jcc_late_test_worker(&reclaim);
    exact = atomic_load_explicit(&s->first, memory_order_relaxed) == 1 &&
            atomic_load_explicit(&s->abandoned, memory_order_relaxed) == 1 &&
            atomic_load_explicit(&s->overflow, memory_order_relaxed) == 0;
    if (!exact) goto fail;

    memset(s, 0, sizeof *s);
    /* A fork can enter diagnostics before claiming its compact process slot.
       The global eligible count already includes that event, so an unbound
       identity must reconcile as overflow rather than disappearing. */
    hl_backend_jcc_late_record(s->sites, 2, &s->first, &s->repeated, &s->stable, &s->changed,
                               &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                               &s->maximum, 0, 0, 7, UINT64_C(0x7000), UINT64_C(0x8000),
                               UINT64_C(0x9000), 7, 1, NULL, NULL, NULL);
    for (uint64_t source = 1; source <= 3; ++source)
        hl_backend_jcc_late_record(s->sites, 2, &s->first, &s->repeated, &s->stable, &s->changed,
                                   &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                                   &s->maximum, self, birth, 7, source, source + 10, source + 20, 7, 1,
                                   NULL, NULL, NULL);
    exact = atomic_load_explicit(&s->first, memory_order_relaxed) == 2 &&
            atomic_load_explicit(&s->unique, memory_order_relaxed) == 2 &&
            atomic_load_explicit(&s->overflow, memory_order_relaxed) == 2 &&
            atomic_load_explicit(&s->first, memory_order_relaxed) +
                atomic_load_explicit(&s->repeated, memory_order_relaxed) +
                atomic_load_explicit(&s->overflow, memory_order_relaxed) == 4;
    if (!exact) goto fail;

    munmap(s, sizeof *s);
    size_t bounded_size = sizeof *s;
    s = mmap(NULL, bounded_size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (s == MAP_FAILED) return 0;
    memset(s, 0, bounded_size);
    const uint32_t expected_probe_cap = 8;
    uint64_t bounded_source = UINT64_C(0x1000);
    uint64_t hash = hl_backend_executed_form_mix(bounded_source ^ birth ^ UINT64_C(7));
    for (uint32_t probe = 0; probe < expected_probe_cap; ++probe) {
        struct hl_backend_jcc_late_site *site = &s->sites[(uint32_t)(hash + probe) & 15u];
        atomic_store_explicit(&site->owner, 1, memory_order_relaxed);
        site->process = self;
        site->process_birth_ns = birth;
        site->cache_generation = 7;
        site->source = bounded_source + probe + 1;
    }
    struct hl_backend_jcc_late_site *last =
        &s->sites[(uint32_t)(hash + expected_probe_cap - 1) & 15u];
    last->source = bounded_source;
    last->target = UINT64_C(0x2000);
    last->body = UINT64_C(0x3000);
    last->target_generation = 7;
    atomic_store_explicit(&last->appearances, 1, memory_order_relaxed);
    hl_backend_jcc_late_record(s->sites, 16, &s->first, &s->repeated, &s->stable, &s->changed,
                               &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                               &s->maximum, self, birth, 7, bounded_source, UINT64_C(0x2000),
                               UINT64_C(0x3000), 7, 1, NULL, NULL, NULL);
    memset(s->sites, 0, sizeof s->sites);
    uint64_t beyond_source = UINT64_C(0x5000);
    hash = hl_backend_executed_form_mix(beyond_source ^ birth ^ UINT64_C(7));
    for (uint32_t probe = 0; probe < expected_probe_cap; ++probe) {
        struct hl_backend_jcc_late_site *site = &s->sites[(uint32_t)(hash + probe) & 15u];
        atomic_store_explicit(&site->owner, 1, memory_order_relaxed);
        site->process = self;
        site->process_birth_ns = birth;
        site->cache_generation = 7;
        site->source = beyond_source + probe + 1;
    }
    hl_backend_jcc_late_record(s->sites, 16, &s->first, &s->repeated, &s->stable, &s->changed,
                               &s->current, &s->retired, &s->unique, &s->overflow, &s->abandoned,
                               &s->maximum, self, birth, 7, beyond_source, UINT64_C(0x2000),
                               UINT64_C(0x3000), 7, 1, NULL, NULL, NULL);
    exact = atomic_load_explicit(&s->first, memory_order_relaxed) == 0 &&
            atomic_load_explicit(&s->repeated, memory_order_relaxed) == 1 &&
            atomic_load_explicit(&s->stable, memory_order_relaxed) == 1 &&
            atomic_load_explicit(&s->current, memory_order_relaxed) == 1 &&
            atomic_load_explicit(&s->overflow, memory_order_relaxed) == 1;
    munmap(s, bounded_size);
    return exact;
release_a:
    atomic_store_explicit(&s->pause_release, 1, memory_order_release); (void)pthread_join(a, NULL);
fail:
    munmap(s, sizeof *s); return 0;
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
_Static_assert(HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY != HL_BACKEND_SHAPE_T_INDIRECT_BRANCH,
               "register and memory indirect jumps need separate production counters");
_Static_assert(HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY != HL_BACKEND_SHAPE_T_INDIRECT_CALL,
               "register and memory indirect calls need separate production counters");

enum {
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
#define HL_BACKEND_A64_UNSUPPORTED_FORM_COUNT 2048u
#define HL_BACKEND_A64_UNSUPPORTED_TOP 16u

struct hl_backend_tree_slot {
    _Atomic int pid;
    _Atomic uint64_t birth_ns;
    _Atomic uint32_t lifecycle;
    _Atomic uint32_t first_finalize_caller;
    _Atomic int first_finalize_actor;
};

struct hl_backend_mixed_sse_shared {
    _Atomic int root_pid;
    _Atomic uint64_t missing_claims;
    _Atomic uint64_t duplicate_finalize;
    _Atomic uint32_t first_finalize_caller;
    _Atomic int first_finalize_actor;
    _Atomic int first_finalize_slot_pid;
    _Atomic uint32_t duplicate_finalize_caller;
    _Atomic int duplicate_finalize_actor;
    _Atomic int duplicate_finalize_slot_pid;
    _Atomic uint32_t duplicate_slot_first_caller;
    _Atomic int duplicate_slot_first_actor;
    _Atomic uint32_t reported;
    _Atomic uint64_t executed;
    _Atomic uint64_t executed_transitions;
    _Atomic uint64_t disabled_boundaries;
    _Atomic uint64_t translated_entries;
    _Atomic uint64_t interpreted_entries;
    _Atomic uint64_t translated_steps;
    _Atomic uint64_t interpreted_steps;
    _Atomic uint64_t a64_major[HL_BACKEND_A64_MAJOR_COUNT];
    _Atomic uint64_t a64_unsupported_total;
    _Atomic uint64_t a64_unsupported_form[HL_BACKEND_A64_UNSUPPORTED_FORM_COUNT];
    _Atomic uint64_t map_misses;
    _Atomic uint64_t executed_form_total;
    _Atomic uint64_t executed_form_unique;
    _Atomic uint64_t executed_form_overflow;
    struct hl_backend_executed_form executed_forms[HL_BACKEND_EXECUTED_FORM_SLOTS];
    _Atomic uint64_t executed_step_form_total;
    _Atomic uint64_t executed_step_form_unique;
    _Atomic uint64_t executed_step_form_overflow;
    struct hl_backend_executed_form executed_step_forms[HL_BACKEND_EXECUTED_FORM_SLOTS];
    _Atomic uint64_t reason[HL_BACKEND_TREE_REASON_COUNT];
    _Atomic uint64_t reason_other;
    _Atomic uint64_t translated_exit[HL_BACKEND_SHAPE_T_COUNT];
    _Atomic uint64_t translated_fall_stop[HL_BACKEND_FALL_COUNT];
    _Atomic uint64_t fallthrough_ibtc_fs_transaction_hits;
    _Atomic uint64_t fallthrough_ibtc_normal_to_fs_hits;
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
    _Atomic uint64_t jcc_ibtc_fill_empty;
    _Atomic uint64_t jcc_ibtc_fill_collision;
    _Atomic uint64_t jcc_ibtc_fill_irq;
    _Atomic uint64_t jcc_ibtc_fill_same_key;
    _Atomic uint64_t x86_jcc_route[HL_BACKEND_X86_JCC_ROUTE_COUNT];
    _Atomic uint64_t jcc_late[HL_BACKEND_JCC_LATE_REASON_COUNT];
    _Atomic uint64_t jcc_late_site_first;
    _Atomic uint64_t jcc_late_site_repeated;
    _Atomic uint64_t jcc_late_site_stable;
    _Atomic uint64_t jcc_late_site_changed;
    _Atomic uint64_t jcc_late_site_current;
    _Atomic uint64_t jcc_late_site_retired;
    _Atomic uint64_t jcc_late_site_unique;
    _Atomic uint64_t jcc_late_site_overflow;
    _Atomic uint64_t jcc_late_site_abandoned;
    _Atomic uint64_t jcc_late_site_max;
    struct hl_backend_jcc_late_site jcc_late_sites[HL_BACKEND_JCC_LATE_SITES];
    _Atomic uint64_t indirect_ibtc_misses;
    _Atomic uint64_t jcc_invalid_reason[HL_BACKEND_JCC_INVALID_REASON_COUNT];
    _Atomic uint64_t jcc_invalid_site_unique;
    _Atomic uint64_t jcc_invalid_site_overflow;
    struct hl_backend_jcc_invalid_site jcc_invalid_sites[HL_BACKEND_JCC_INVALID_SITES];
    _Atomic uint64_t direct_jmp_ibtc_emitted;
    _Atomic uint64_t direct_jmp_ibtc_hits;
    _Atomic uint64_t direct_jmp_ibtc_misses;
    _Atomic uint64_t direct_jmp_ibtc_irq;
    _Atomic uint64_t direct_jmp_ibtc_fills;
    _Atomic uint64_t direct_jmp_ibtc_suppressed;
    _Atomic uint64_t direct_jmp_ibtc_invalid_refusals;
    _Atomic uint64_t direct_call_ibtc_emitted;
    _Atomic uint64_t direct_call_ibtc_hits;
    _Atomic uint64_t direct_call_ibtc_misses;
    _Atomic uint64_t direct_call_ibtc_irq;
    _Atomic uint64_t direct_call_ibtc_fills;
    _Atomic uint64_t direct_call_ibtc_invalid_refusals;
    _Atomic uint64_t ret_ibtc_attempts;
    _Atomic uint64_t ret_ibtc_hits;
    _Atomic uint64_t ret_ibtc_key_misses;
    _Atomic uint64_t ret_ibtc_null_misses;
    _Atomic uint64_t ret_ibtc_irq;
    _Atomic uint64_t ret_ibtc_fills;
    _Atomic uint64_t ret_ibtc_collisions;
    _Atomic uint64_t ret_ibtc_unmapped;
    _Atomic uint64_t ret_ibtc_invalid_refusals;
    _Atomic uint64_t ret_fast_ibtc_hits;
    _Atomic uint64_t ret_fast_ibtc_misses;
    _Atomic uint64_t ret_fast_ibtc_irq;
    _Atomic uint64_t ret_fast_ibtc_fills;
    _Atomic uint64_t ret_fast_ibtc_invalid_refusals;
    /* Immutable after root initialization and before any guest fork. */
    uint32_t jcc_ibtc_enabled;
    uint32_t direct_jmp_ibtc_enabled;
    uint32_t x86_jcc_route_enabled;
    struct hl_backend_tree_slot slots[HL_BACKEND_MIXED_SSE_SLOTS];
};

#if ATOMIC_INT_LOCK_FREE != 2
#error "production mixed-SSE census requires lock-free 32-bit atomics"
#endif
#if (defined(_WIN32) && ATOMIC_LLONG_LOCK_FREE != 2) || (!defined(_WIN32) && ATOMIC_LONG_LOCK_FREE != 2)
#error "production mixed-SSE census requires lock-free 64-bit atomics"
#endif

_Static_assert(sizeof(struct hl_backend_tree_slot) == 32,
               "production mixed-SSE lifecycle slots must remain compact");
_Static_assert((HL_BACKEND_JCC_LATE_SITES & (HL_BACKEND_JCC_LATE_SITES - 1u)) == 0,
               "late-JCC census table must remain a power of two");
_Static_assert(sizeof(struct hl_backend_mixed_sse_shared) <= 64u * 1024u * 1024u,
               "diagnostics mapping must remain bounded");

static struct hl_backend_mixed_sse_shared *g_backend_mixed_sse;
static struct hl_backend_tree_slot *g_backend_mixed_sse_self;

static inline uintptr_t hl_backend_tree_x86_jcc_route_counter_address(unsigned kind) {
    return g_backend_mixed_sse != NULL && g_backend_mixed_sse->x86_jcc_route_enabled &&
                   kind < HL_BACKEND_X86_JCC_ROUTE_COUNT
               ? (uintptr_t)&g_backend_mixed_sse->x86_jcc_route[kind]
               : 0;
}

static inline void hl_backend_tree_executed_form(uint64_t key) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL || g_backend_mixed_sse_self == NULL) return;
    hl_backend_executed_form_record(census->executed_forms, &census->executed_form_total,
                                    &census->executed_form_unique, &census->executed_form_overflow, key, NULL, NULL,
                                    NULL);
}

static inline void hl_backend_tree_executed_step_form(uint64_t key) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL || g_backend_mixed_sse_self == NULL) return;
    hl_backend_executed_form_record(census->executed_step_forms, &census->executed_step_form_total,
                                    &census->executed_step_form_unique, &census->executed_step_form_overflow, key,
                                    NULL, NULL, NULL);
}

static void hl_backend_executed_form_top(struct hl_backend_executed_form forms[HL_BACKEND_EXECUTED_FORM_SLOTS],
                                         uint64_t *keys, uint64_t *counts, unsigned top) {
    for (unsigned slot = 0; slot < HL_BACKEND_EXECUTED_FORM_SLOTS; ++slot) {
        struct hl_backend_executed_form *form = &forms[slot];
        if (atomic_load_explicit(&form->state, memory_order_acquire) != 2) continue;
        uint64_t key = form->key;
        uint64_t count = atomic_load_explicit(&form->count, memory_order_relaxed);
        unsigned rank = 0;
        while (rank < top &&
               (counts[rank] > count || (counts[rank] == count && keys[rank] <= key)))
            ++rank;
        if (rank == top) continue;
        for (unsigned move = top - 1; move > rank; --move) {
            keys[move] = keys[move - 1];
            counts[move] = counts[move - 1];
        }
        keys[rank] = key;
        counts[rank] = count;
    }
}

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
    g_backend_mixed_sse->jcc_ibtc_enabled = !hl_option_flag_value("HL_TRANSLIT_JCC_IBTC_DISABLE", 0);
    g_backend_mixed_sse->direct_jmp_ibtc_enabled =
        !hl_option_flag_value("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", 0);
    g_backend_mixed_sse->x86_jcc_route_enabled = hl_option_flag_value("HL_PCACHE_OBSERVE", 0);
    g_backend_mixed_sse_self = hl_backend_mixed_sse_claim(self);
}

static int hl_backend_mixed_sse_finalize_slot(struct hl_backend_tree_slot *slot, int abnormal, unsigned caller) {
    if (slot == NULL) return 0;
    uint32_t expected = HL_BACKEND_MIXED_SSE_CLAIMED;
    uint32_t completed = abnormal ? HL_BACKEND_MIXED_SSE_ABNORMAL : HL_BACKEND_MIXED_SSE_COMPLETED;
    if (atomic_compare_exchange_strong_explicit(&slot->lifecycle, &expected, completed, memory_order_acq_rel,
                                                memory_order_acquire)) {
        atomic_store_explicit(&slot->first_finalize_caller, caller, memory_order_relaxed);
        atomic_store_explicit(&slot->first_finalize_actor, (int)getpid(), memory_order_release);
        uint32_t unset = HL_BACKEND_FINALIZE_UNKNOWN;
        if (g_backend_mixed_sse != NULL &&
            atomic_compare_exchange_strong_explicit(&g_backend_mixed_sse->first_finalize_caller, &unset, caller,
                                                    memory_order_acq_rel, memory_order_relaxed)) {
            atomic_store_explicit(&g_backend_mixed_sse->first_finalize_actor, (int)getpid(), memory_order_relaxed);
            atomic_store_explicit(&g_backend_mixed_sse->first_finalize_slot_pid,
                                  atomic_load_explicit(&slot->pid, memory_order_acquire), memory_order_release);
        }
        return 1;
    }
    if (g_backend_mixed_sse != NULL) {
        atomic_fetch_add_explicit(&g_backend_mixed_sse->duplicate_finalize, 1, memory_order_relaxed);
        uint32_t unset = HL_BACKEND_FINALIZE_UNKNOWN;
        if (atomic_compare_exchange_strong_explicit(&g_backend_mixed_sse->duplicate_finalize_caller, &unset,
                                                    caller, memory_order_acq_rel, memory_order_relaxed)) {
            atomic_store_explicit(&g_backend_mixed_sse->duplicate_slot_first_caller,
                                  atomic_load_explicit(&slot->first_finalize_caller, memory_order_acquire),
                                  memory_order_relaxed);
            atomic_store_explicit(&g_backend_mixed_sse->duplicate_slot_first_actor,
                                  atomic_load_explicit(&slot->first_finalize_actor, memory_order_acquire),
                                  memory_order_relaxed);
            atomic_store_explicit(&g_backend_mixed_sse->duplicate_finalize_actor, (int)getpid(),
                                  memory_order_relaxed);
            atomic_store_explicit(&g_backend_mixed_sse->duplicate_finalize_slot_pid,
                                  atomic_load_explicit(&slot->pid, memory_order_acquire), memory_order_release);
        }
    }
    return 0;
}

static int hl_backend_tree_finalize_from(int abnormal, unsigned caller) {
    return hl_backend_mixed_sse_finalize_slot(g_backend_mixed_sse_self, abnormal, caller);
}
#define hl_backend_tree_finalize(abnormal) hl_backend_tree_finalize_from((abnormal), HL_BACKEND_FINALIZE_UNKNOWN)

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
            (void)hl_backend_mixed_sse_finalize_slot(slot, 1, HL_BACKEND_FINALIZE_REAPER);
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
                    (void)hl_backend_mixed_sse_finalize_slot(slot, 1, HL_BACKEND_FINALIZE_PARENT_BARRIER);
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

struct hl_backend_mixed_sse_lifecycle_summary {
    uint64_t reserved;
    uint64_t live;
    uint64_t claimed;
};

static struct hl_backend_mixed_sse_lifecycle_summary hl_backend_mixed_sse_lifecycle_summary(
    struct hl_backend_mixed_sse_shared *census, int root_pid) {
    struct hl_backend_mixed_sse_lifecycle_summary summary = {0};
    for (uint32_t index = 0; index < HL_BACKEND_MIXED_SSE_SLOTS; ++index) {
        struct hl_backend_tree_slot *slot = &census->slots[index];
        int pid = atomic_load_explicit(&slot->pid, memory_order_acquire);
        if (pid == -1) {
            ++summary.reserved;
            continue;
        }
        if (pid <= 0 ||
            atomic_load_explicit(&slot->lifecycle, memory_order_acquire) != HL_BACKEND_MIXED_SSE_CLAIMED)
            continue;
        ++summary.claimed;
#if !defined(_WIN32)
        if (pid != root_pid && hl_backend_mixed_sse_process_can_mutate(slot, pid)) ++summary.live;
#else
        (void)root_pid;
#endif
    }
    return summary;
}

#define HL_BACKEND_PRODUCT_RECORD_CAPACITY 32768u
#define HL_BACKEND_PRODUCT_FORMAT_FAIL(box)                                                                           \
    do {                                                                                                               \
        static const char failure[] = "[diag] backend-shape-error version=1 reason=record-overflow\n";                \
        (void)hl_backend_report_write((box), failure, sizeof failure - 1);                                             \
        return;                                                                                                        \
    } while (0)

static void hl_backend_mixed_sse_report(struct hl_backend_mixed_sse_shared *census, int available,
                                        int settled, struct hl_backend_mixed_sse_lifecycle_summary lifecycle,
                                        hl_linux_abi *box) {
    uint32_t expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&census->reported, &expected, 1, memory_order_acq_rel,
                                                 memory_order_relaxed))
        return;
    char record[HL_BACKEND_PRODUCT_RECORD_CAPACITY];
    uint64_t jcc_late_candidate = 0;
    for (unsigned reason = 0; reason < HL_BACKEND_JCC_LATE_REASON_COUNT; ++reason)
        jcc_late_candidate += atomic_load_explicit(&census->jcc_late[reason], memory_order_relaxed);
    int formatted = snprintf(record, sizeof record,
                             "[diag] backend-shape version=13 available=%d translation_codegen_available=%d lifecycle_settled=%d "
                             "missing_claims=%llu duplicate_finalize=%llu reserved=%llu live=%llu claimed=%llu "
                             "first_finalize_caller=%u first_finalize_actor=%d first_finalize_slot_pid=%d "
                             "duplicate_finalize_caller=%u duplicate_finalize_actor=%d "
                             "duplicate_finalize_slot_pid=%d duplicate_slot_first_caller=%u "
                             "duplicate_slot_first_actor=%d "
                             "crossings=%llu "
                             "translated_entries=%llu interpreted_entries=%llu translated_steps=%llu "
                             "interpreted_steps=%llu mixed_sse_executed=%llu "
                             "mixed_sse_executed_transitions=%llu mixed_sse_disabled_boundaries=%llu "
                             "jcc_ibtc_enabled=%d jcc_ibtc_emitted=%llu jcc_ibtc_hits=%llu "
                             "jcc_ibtc_misses=%llu jcc_ibtc_irq=%llu jcc_ibtc_fills=%llu "
                             "jcc_ibtc_suppressed=%llu jcc_ibtc_invalid_refusals=%llu "
                             "jcc_ibtc_fill_empty=%llu jcc_ibtc_fill_collision=%llu jcc_ibtc_fill_irq_cause=%llu "
                             "jcc_ibtc_fill_same_key=%llu "
                             "jcc_taken_ibtc_misses=%llu indirect_ibtc_misses=%llu "
                             "jcc_late_candidate=%llu jcc_late_eligible=%llu jcc_late_invalid=%llu "
                             "jcc_late_target_absent=%llu jcc_late_page_generation=%llu "
                             "jcc_late_displacement=%llu jcc_late_other=%llu "
                             "jcc_late_site_first=%llu jcc_late_site_repeated=%llu "
                             "jcc_late_site_stable=%llu jcc_late_site_changed=%llu "
                             "jcc_late_site_current=%llu jcc_late_site_retired=%llu "
                             "jcc_late_site_unique=%llu jcc_late_site_overflow=%llu "
                             "jcc_late_site_abandoned=%llu jcc_late_site_max=%llu "
                             "jcc_invalid_null=%llu jcc_invalid_magic=%llu jcc_invalid_gpc=%llu "
                             "jcc_invalid_block_generation=%llu jcc_invalid_entry_zero=%llu "
                             "jcc_invalid_length_zero=%llu jcc_invalid_resolve=%llu "
                             "jcc_invalid_resolved_generation=%llu jcc_invalid_entry_overflow=%llu "
                             "jcc_invalid_site_unique=%llu jcc_invalid_site_overflow=%llu "
                             "direct_jmp_ibtc_enabled=%d direct_jmp_ibtc_emitted=%llu "
                             "direct_jmp_ibtc_hits=%llu direct_jmp_ibtc_misses=%llu direct_jmp_ibtc_irq=%llu "
                             "direct_jmp_ibtc_fills=%llu direct_jmp_ibtc_suppressed=%llu "
                             "direct_jmp_ibtc_invalid_refusals=%llu direct_call_ibtc_emitted=%llu "
                             "direct_call_ibtc_hits=%llu direct_call_ibtc_misses=%llu "
                             "direct_call_ibtc_irq=%llu direct_call_ibtc_fills=%llu "
                             "direct_call_ibtc_invalid_refusals=%llu ret_ibtc_attempts=%llu "
                             "ret_ibtc_hits=%llu ret_ibtc_key_misses=%llu ret_ibtc_null_misses=%llu "
                             "ret_ibtc_irq=%llu ret_ibtc_fills=%llu ret_ibtc_collisions=%llu "
                             "ret_ibtc_unmapped=%llu ret_ibtc_invalid_refusals=%llu "
                             "ret_fast_ibtc_hits=%llu ret_fast_ibtc_misses=%llu ret_fast_ibtc_irq=%llu "
                             "ret_fast_ibtc_fills=%llu ret_fast_ibtc_invalid_refusals=%llu\n",
                             available, HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE, settled,
                             (unsigned long long)atomic_load_explicit(&census->missing_claims,
                                                                      memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->duplicate_finalize,
                                                                      memory_order_relaxed),
                             (unsigned long long)lifecycle.reserved, (unsigned long long)lifecycle.live,
                             (unsigned long long)lifecycle.claimed,
                             atomic_load_explicit(&census->first_finalize_caller, memory_order_acquire),
                             atomic_load_explicit(&census->first_finalize_actor, memory_order_acquire),
                             atomic_load_explicit(&census->first_finalize_slot_pid, memory_order_acquire),
                             atomic_load_explicit(&census->duplicate_finalize_caller, memory_order_acquire),
                             atomic_load_explicit(&census->duplicate_finalize_actor, memory_order_acquire),
                             atomic_load_explicit(&census->duplicate_finalize_slot_pid, memory_order_acquire),
                             atomic_load_explicit(&census->duplicate_slot_first_caller, memory_order_acquire),
                             atomic_load_explicit(&census->duplicate_slot_first_actor, memory_order_acquire),
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
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_fill_empty,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_fill_collision,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_fill_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_fill_same_key,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->indirect_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)jcc_late_candidate,
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_ELIGIBLE], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_INVALID], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_TARGET_ABSENT], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_PAGE_GENERATION], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_DISPLACEMENT], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_late[HL_BACKEND_JCC_LATE_OTHER], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_first,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_repeated,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_stable,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_changed,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_current,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_retired,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_unique,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_overflow,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_abandoned,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_late_site_max,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_NULL], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_MAGIC], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_GPC], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_BLOCK_GENERATION],
                                 memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_ENTRY_ZERO], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_LENGTH_ZERO], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_RESOLVE], memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_RESOLVED_GENERATION],
                                 memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(
                                 &census->jcc_invalid_reason[HL_BACKEND_JCC_INVALID_ENTRY_OVERFLOW],
                                 memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_invalid_site_unique,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->jcc_invalid_site_overflow,
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
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_emitted,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_hits,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_fills,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->direct_call_ibtc_invalid_refusals,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_attempts,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_hits,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_key_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_null_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_fills,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_collisions,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_unmapped,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_ibtc_invalid_refusals,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_fast_ibtc_hits,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_fast_ibtc_misses,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_fast_ibtc_irq,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_fast_ibtc_fills,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->ret_fast_ibtc_invalid_refusals,
                                                                     memory_order_relaxed));
    if (formatted <= 0 || (size_t)formatted >= sizeof record) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    --formatted;
    uint64_t form_keys[HL_BACKEND_EXECUTED_FORM_TOP] = {0};
    uint64_t form_counts[HL_BACKEND_EXECUTED_FORM_TOP] = {0};
    hl_backend_executed_form_top(census->executed_forms, form_keys, form_counts, HL_BACKEND_EXECUTED_FORM_TOP);
    {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                             " executed_form_total=%llu executed_form_unique=%llu executed_form_overflow=%llu",
                             (unsigned long long)atomic_load_explicit(&census->executed_form_total,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->executed_form_unique,
                                                                     memory_order_relaxed),
                             (unsigned long long)atomic_load_explicit(&census->executed_form_overflow,
                                                                     memory_order_relaxed));
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        formatted += added;
    }
    for (unsigned rank = 0; rank < HL_BACKEND_EXECUTED_FORM_TOP; ++rank) {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                             " executed_form%u_key=%llu executed_form%u_count=%llu", rank,
                             (unsigned long long)form_keys[rank], rank,
                             (unsigned long long)form_counts[rank]);
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        formatted += added;
    }
    uint64_t translated_return_total = 0;
    for (unsigned kind = 0; kind < HL_BACKEND_SHAPE_T_COUNT; ++kind)
        translated_return_total += atomic_load_explicit(&census->translated_exit[kind], memory_order_relaxed);
    uint64_t translated_entries = atomic_load_explicit(&census->translated_entries, memory_order_relaxed);
    /* The product receipt is one record.  Splitting its reason and exit fields onto a bare
       continuation line makes the strict consumer reject a successful long-running workload,
       and leaves that unprefixed tail indistinguishable from unrelated diagnostics. */
    for (unsigned reason = 0; reason < HL_BACKEND_TREE_REASON_COUNT; ++reason) {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " r%u=%llu", reason,
                             (unsigned long long)atomic_load_explicit(&census->reason[reason],
                                                                     memory_order_relaxed));
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        formatted += added;
    }
    {
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " r_other=%llu",
                             (unsigned long long)atomic_load_explicit(&census->reason_other,
                                                                     memory_order_relaxed));
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        formatted += added;
    }
#define HL_APPEND_CROSSING(name, array, kind)                                                                          \
    do {                                                                                                               \
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " " name "=%llu",              \
                             (unsigned long long)atomic_load_explicit(&(array)[kind], memory_order_relaxed));           \
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);     \
        formatted += added;                                                                                           \
    } while (0)
    uint64_t interpreted_return_total = 0;
    for (unsigned kind = 0; kind < HL_BACKEND_SHAPE_S_COUNT; ++kind)
        interpreted_return_total += atomic_load_explicit(&census->interpreter_stop[kind], memory_order_relaxed);
    uint64_t interpreted_entries = atomic_load_explicit(&census->interpreted_entries, memory_order_relaxed);
    int added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                         " dispatch_translation_miss=%llu dispatch_interpreted=%llu"
                         " dispatch_translated_return_total=%llu dispatch_translated_return_mismatch=%lld"
                         " dispatch_interpreted_return_total=%llu dispatch_interpreted_return_mismatch=%lld",
                         (unsigned long long)atomic_load_explicit(&census->map_misses, memory_order_relaxed),
                         (unsigned long long)interpreted_entries, (unsigned long long)translated_return_total,
                         translated_entries >= translated_return_total
                             ? (long long)(translated_entries - translated_return_total)
                             : -(long long)(translated_return_total - translated_entries),
                         (unsigned long long)interpreted_return_total,
                         interpreted_entries >= interpreted_return_total
                             ? (long long)(interpreted_entries - interpreted_return_total)
                             : -(long long)(interpreted_return_total - interpreted_entries));
    if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    formatted += added;
    HL_APPEND_CROSSING("t_fallthrough", census->translated_exit, HL_BACKEND_SHAPE_T_FALLTHROUGH);
    HL_APPEND_CROSSING("t_jcc_taken", census->translated_exit, HL_BACKEND_SHAPE_T_COND_TAKEN);
    HL_APPEND_CROSSING("t_jcc_fall", census->translated_exit, HL_BACKEND_SHAPE_T_COND_NOT_TAKEN);
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
    HL_APPEND_CROSSING("t_other", census->translated_exit, HL_BACKEND_SHAPE_T_OTHER);
    added = snprintf(
        record + formatted, sizeof record - (size_t)formatted,
        "\n[diag] x86-exit-family version=1 translated_entries=%llu total=%llu"
        " t_fallthrough=%llu t_jcc_taken=%llu t_jcc_fall=%llu t_direct_jmp=%llu t_direct_call=%llu"
        " t_ret=%llu t_jmp_reg=%llu t_jmp_mem=%llu t_call_reg=%llu t_call_mem=%llu"
        " t_syscall=%llu t_irq=%llu t_fault=%llu t_other=%llu",
        (unsigned long long)translated_entries, (unsigned long long)translated_return_total,
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_COND_TAKEN], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_COND_NOT_TAKEN], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_DIRECT_JUMP], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_DIRECT_CALL], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_RETURN],
                                                 memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_BRANCH_MEMORY], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(
            &census->translated_exit[HL_BACKEND_SHAPE_T_INDIRECT_CALL_MEMORY], memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_SYSCALL],
                                                 memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_IRQ],
                                                 memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_FAULT],
                                                 memory_order_relaxed),
        (unsigned long long)atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_OTHER],
                                                 memory_order_relaxed));
    if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    formatted += added;
    if ((size_t)formatted + 1 >= sizeof record) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    record[formatted++] = '\n';
    uint64_t fall_total = 0;
    for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT; ++reason)
        fall_total += atomic_load_explicit(&census->translated_fall_stop[reason], memory_order_relaxed);
    added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                     " fall_total=%llu fall_mismatch=%lld",
                     (unsigned long long)fall_total,
                     fall_total <= atomic_load_explicit(&census->translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
                                                        memory_order_relaxed)
                         ? (long long)(atomic_load_explicit(
                                           &census->translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
                                           memory_order_relaxed) -
                                       fall_total)
                         : -(long long)(fall_total - atomic_load_explicit(
                                                        &census->translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH],
                                                        memory_order_relaxed)));
    if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    formatted += added;
#define HL_APPEND_FALL(name, reason) HL_APPEND_CROSSING(name, census->translated_fall_stop, reason)
    HL_APPEND_FALL("fall_cap", HL_BACKEND_FALL_CAP);
    HL_APPEND_FALL("fall_decode", HL_BACKEND_FALL_DECODE);
    HL_APPEND_FALL("fall_normal_to_sse2", HL_BACKEND_FALL_NORMAL_TO_SSE2);
    HL_APPEND_FALL("fall_sse2_to_normal", HL_BACKEND_FALL_SSE2_TO_NORMAL);
    HL_APPEND_FALL("fall_normal_to_fs", HL_BACKEND_FALL_NORMAL_TO_FS);
    HL_APPEND_FALL("fall_fs_to_normal", HL_BACKEND_FALL_FS_TO_NORMAL);
    HL_APPEND_FALL("fall_sse2_to_fs", HL_BACKEND_FALL_SSE2_TO_FS);
    HL_APPEND_FALL("fall_fs_to_sse2", HL_BACKEND_FALL_FS_TO_SSE2);
    HL_APPEND_FALL("fall_tl_no", HL_BACKEND_FALL_TL_NO);
    HL_APPEND_FALL("fall_displaced", HL_BACKEND_FALL_DISPLACED_UNSAFE);
    HL_APPEND_FALL("fall_fetch", HL_BACKEND_FALL_FETCH);
    HL_APPEND_FALL("fall_riprel", HL_BACKEND_FALL_RIPREL_LOWER);
    HL_APPEND_FALL("fall_fs_transaction", HL_BACKEND_FALL_FS_TRANSACTION);
    HL_APPEND_FALL("fall_sse_riprel", HL_BACKEND_FALL_SSE_RIPREL_LOWER);
    HL_APPEND_FALL("fall_other", HL_BACKEND_FALL_OTHER);
#undef HL_APPEND_FALL
    added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                     " fall_chain_fs_transaction_hits=%llu fall_chain_normal_to_fs_hits=%llu",
                     (unsigned long long)atomic_load_explicit(
                         &census->fallthrough_ibtc_fs_transaction_hits, memory_order_relaxed),
                     (unsigned long long)atomic_load_explicit(
                         &census->fallthrough_ibtc_normal_to_fs_hits, memory_order_relaxed));
    if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    formatted += added;
    HL_APPEND_CROSSING("i_fallthrough", census->interpreter_stop, HL_BACKEND_SHAPE_S_FALLTHROUGH);
    HL_APPEND_CROSSING("i_jcc_taken", census->interpreter_stop, HL_BACKEND_SHAPE_S_COND_TAKEN);
    HL_APPEND_CROSSING("i_jcc_fall", census->interpreter_stop, HL_BACKEND_SHAPE_S_COND_NOT_TAKEN);
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
    HL_APPEND_CROSSING("i_other", census->interpreter_stop, HL_BACKEND_SHAPE_S_OTHER);
#define HL_APPEND_CALL_SIM(name, field)                                                                                \
    do {                                                                                                               \
        int added = snprintf(record + formatted, sizeof record - (size_t)formatted, " " name "=%llu",              \
                             (unsigned long long)atomic_load_explicit(&census->field, memory_order_relaxed));           \
        if (added <= 0 || (size_t)added >= sizeof record - (size_t)formatted) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);     \
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
#if defined(HL_BACKEND_A64_OPCODE_CENSUS)
    uint64_t major[HL_BACKEND_A64_MAJOR_COUNT];
    uint64_t family[6] = {0};
    uint64_t body_retired = 0;
    for (unsigned i = 0; i < HL_BACKEND_A64_MAJOR_COUNT; ++i) {
        major[i] = atomic_load_explicit(&census->a64_major[i], memory_order_relaxed);
        body_retired += major[i];
    }
    static const unsigned char family_for_major[HL_BACKEND_A64_MAJOR_COUNT] = {
        0, 0, 0, 0, 1, 2, 1, 5, 3, 3, 4, 4, 1, 2, 1, 5,
    };
    for (unsigned i = 0; i < HL_BACKEND_A64_MAJOR_COUNT; ++i)
        family[family_for_major[i]] += major[i];
    int a64_added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                             "\n[diag] aarch64-opcode version=1 available=%d body_retired=%llu "
                             "major0=%llu major1=%llu major2=%llu major3=%llu major4=%llu major5=%llu "
                             "major6=%llu major7=%llu major8=%llu major9=%llu major10=%llu major11=%llu "
                             "major12=%llu major13=%llu major14=%llu major15=%llu "
                             "reserved=%llu load_store=%llu dp_register=%llu dp_immediate=%llu "
                             "branch_system=%llu simd_fp=%llu",
                             available, (unsigned long long)body_retired,
                             (unsigned long long)major[0], (unsigned long long)major[1],
                             (unsigned long long)major[2], (unsigned long long)major[3],
                             (unsigned long long)major[4], (unsigned long long)major[5],
                             (unsigned long long)major[6], (unsigned long long)major[7],
                             (unsigned long long)major[8], (unsigned long long)major[9],
                             (unsigned long long)major[10], (unsigned long long)major[11],
                             (unsigned long long)major[12], (unsigned long long)major[13],
                             (unsigned long long)major[14], (unsigned long long)major[15],
                             (unsigned long long)family[0], (unsigned long long)family[1],
                             (unsigned long long)family[2], (unsigned long long)family[3],
                             (unsigned long long)family[4], (unsigned long long)family[5]);
    if (a64_added <= 0 || (size_t)a64_added >= sizeof record - (size_t)formatted)
        HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    formatted += a64_added;
    if (census->x86_jcc_route_enabled) {
        uint64_t top_count[HL_BACKEND_A64_UNSUPPORTED_TOP] = {0};
        unsigned top_form[HL_BACKEND_A64_UNSUPPORTED_TOP] = {0};
        uint64_t selected = 0;
        for (unsigned form = 0; form < HL_BACKEND_A64_UNSUPPORTED_FORM_COUNT; ++form) {
            uint64_t count = atomic_load_explicit(&census->a64_unsupported_form[form], memory_order_relaxed);
            for (unsigned rank = 0; count != 0 && rank < HL_BACKEND_A64_UNSUPPORTED_TOP; ++rank) {
                if (count > top_count[rank] || (count == top_count[rank] && form < top_form[rank])) {
                    for (unsigned move = HL_BACKEND_A64_UNSUPPORTED_TOP - 1; move > rank; --move) {
                        top_count[move] = top_count[move - 1];
                        top_form[move] = top_form[move - 1];
                    }
                    top_count[rank] = count;
                    top_form[rank] = form;
                    break;
                }
            }
        }
        for (unsigned rank = 0; rank < HL_BACKEND_A64_UNSUPPORTED_TOP; ++rank) selected += top_count[rank];
        uint64_t total = atomic_load_explicit(&census->a64_unsupported_total, memory_order_relaxed);
        a64_added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                             "\n[diag] aarch64-x86-unsupported version=1 total=%llu selected=%llu other=%llu",
                             (unsigned long long)total, (unsigned long long)selected,
                             (unsigned long long)(total - selected));
        if (a64_added <= 0 || (size_t)a64_added >= sizeof record - (size_t)formatted)
            HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        formatted += a64_added;
        for (unsigned rank = 0; rank < HL_BACKEND_A64_UNSUPPORTED_TOP; ++rank) {
            a64_added = snprintf(record + formatted, sizeof record - (size_t)formatted,
                                 " form%u_key=%03x form%u_count=%llu", rank, top_form[rank], rank,
                                 (unsigned long long)top_count[rank]);
            if (a64_added <= 0 || (size_t)a64_added >= sizeof record - (size_t)formatted)
                HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
            formatted += a64_added;
        }
    }
#endif
    if ((size_t)formatted + 1 >= sizeof record) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
    record[formatted++] = '\n';
    size_t offset = 0;
    while (offset < (size_t)formatted) {
        int64_t written = hl_backend_report_write(box, record + offset, (size_t)formatted - offset);
        if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)formatted - offset) return;
        offset += (size_t)written;
    }
    if (census->x86_jcc_route_enabled) {
        uint64_t route[HL_BACKEND_X86_JCC_ROUTE_COUNT];
        for (unsigned kind = 0; kind < HL_BACKEND_X86_JCC_ROUTE_COUNT; ++kind)
            route[kind] = atomic_load_explicit(&census->x86_jcc_route[kind], memory_order_relaxed);
        formatted = hl_backend_x86_jcc_route_format(record, sizeof record, route);
        if (formatted <= 0 || (size_t)formatted >= sizeof record) HL_BACKEND_PRODUCT_FORMAT_FAIL(box);
        offset = 0;
        while (offset < (size_t)formatted) {
            int64_t written = hl_backend_report_write(box, record + offset, (size_t)formatted - offset);
            if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)formatted - offset) return;
            offset += (size_t)written;
        }
    }
    if (atomic_load_explicit(&census->executed_step_form_total, memory_order_relaxed) != 0) {
        uint64_t step_keys[HL_BACKEND_EXECUTED_STEP_FORM_TOP] = {0};
        uint64_t step_counts[HL_BACKEND_EXECUTED_STEP_FORM_TOP] = {0};
        uint64_t keyed = 0, top_cumulative = 0;
        for (unsigned slot = 0; slot < HL_BACKEND_EXECUTED_FORM_SLOTS; ++slot) {
            struct hl_backend_executed_form *form = &census->executed_step_forms[slot];
            if (atomic_load_explicit(&form->state, memory_order_acquire) == 2)
                keyed += atomic_load_explicit(&form->count, memory_order_relaxed);
        }
        hl_backend_executed_form_top(census->executed_step_forms, step_keys, step_counts,
                                     HL_BACKEND_EXECUTED_STEP_FORM_TOP);
        char detail[8192];
        uint64_t total = atomic_load_explicit(&census->executed_step_form_total, memory_order_relaxed);
        uint64_t unique = atomic_load_explicit(&census->executed_step_form_unique, memory_order_relaxed);
        uint64_t overflow = atomic_load_explicit(&census->executed_step_form_overflow, memory_order_relaxed);
        uint64_t interpreted = atomic_load_explicit(&census->interpreted_steps, memory_order_relaxed);
        int detail_len = snprintf(
            detail, sizeof detail,
            "[diag] x86-executed-step-form version=2 tree_complete=%d total=%llu keyed=%llu overflow=%llu"
            " unique=%llu interpreted_steps=%llu reconcile=%u interpreted_reconcile=%u top_n=%u",
            available, (unsigned long long)total, (unsigned long long)keyed, (unsigned long long)overflow,
            (unsigned long long)unique, (unsigned long long)interpreted, keyed + overflow == total,
            total == interpreted, HL_BACKEND_EXECUTED_STEP_FORM_TOP);
        if (detail_len <= 0 || (size_t)detail_len >= sizeof detail) return;
        for (unsigned rank = 0; rank < HL_BACKEND_EXECUTED_STEP_FORM_TOP; ++rank) {
            top_cumulative += step_counts[rank];
            int added = snprintf(detail + detail_len, sizeof detail - (size_t)detail_len,
                                 " top%u_key=%llu top%u_count=%llu", rank,
                                 (unsigned long long)step_keys[rank], rank,
                                 (unsigned long long)step_counts[rank]);
            if (added <= 0 || (size_t)added >= sizeof detail - (size_t)detail_len) return;
            detail_len += added;
        }
        int added = snprintf(detail + detail_len, sizeof detail - (size_t)detail_len,
                             " top_cumulative=%llu top_reconcile=%u\n", (unsigned long long)top_cumulative,
                             top_cumulative <= keyed);
        if (added <= 0 || (size_t)added >= sizeof detail - (size_t)detail_len) return;
        detail_len += added;
        size_t detail_offset = 0;
        while (detail_offset < (size_t)detail_len) {
            int64_t written = hl_backend_report_write(box, detail + detail_offset,
                                                      (size_t)detail_len - detail_offset);
            if (written <= 0 || (uint64_t)written > (uint64_t)detail_len - detail_offset) return;
            detail_offset += (size_t)written;
        }
    }
    for (uint32_t slot = 0; slot < HL_BACKEND_JCC_INVALID_SITES; ++slot) {
        struct hl_backend_jcc_invalid_site *site = &census->jcc_invalid_sites[slot];
        if (atomic_load_explicit(&site->state, memory_order_acquire) != 2) continue;
        char site_record[896];
        int site_len = hl_backend_jcc_invalid_site_format(site_record, sizeof site_record, site);
        if (site_len <= 0 || (size_t)site_len >= sizeof site_record) return;
        size_t site_offset = 0;
        while (site_offset < (size_t)site_len) {
            int64_t written = hl_backend_report_write(box, site_record + site_offset,
                                                      (size_t)site_len - site_offset);
            if (written <= 0 || (uint64_t)written > (uint64_t)(size_t)site_len - site_offset) return;
            site_offset += (size_t)written;
        }
    }
}

void hl_target_backend_tree_reap_report(void *shared, size_t shared_size, hl_linux_abi *box, int diagnostic_port) {
    hl_backend_report_descriptor = diagnostic_port >= 0 ? diagnostic_port : STDERR_FILENO;
    struct hl_backend_mixed_sse_shared *census = shared;
    if (census == NULL || shared_size != sizeof *census) return;
    int root_pid = atomic_load_explicit(&census->root_pid, memory_order_acquire);
    int settled = root_pid > 0 && hl_backend_mixed_sse_parent_barrier(census, root_pid);
    int complete = settled && atomic_load_explicit(&census->missing_claims, memory_order_relaxed) == 0 &&
                   atomic_load_explicit(&census->duplicate_finalize, memory_order_relaxed) == 0;
    struct hl_backend_mixed_sse_lifecycle_summary lifecycle =
        hl_backend_mixed_sse_lifecycle_summary(census, root_pid);
    hl_backend_mixed_sse_report(census, complete, settled, lifecycle, box);
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
static inline void hl_backend_tree_a64_body_retired(unsigned major) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL || major >= HL_BACKEND_A64_MAJOR_COUNT) return;
    atomic_fetch_add_explicit(&census->a64_major[major], 1, memory_order_relaxed);
}
/* The protocol's stable mask is 0xffe00000: it preserves the decoder-facing
   prefix and discards the low 21 bits. The fixed array is exhaustive. */
static inline void hl_backend_tree_a64_unsupported(uint32_t instruction) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL || !census->x86_jcc_route_enabled) return;
    unsigned form = instruction >> 21;
    atomic_fetch_add_explicit(&census->a64_unsupported_total, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&census->a64_unsupported_form[form], 1, memory_order_relaxed);
}
static inline void hl_backend_tree_map_miss(void) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census != NULL) atomic_fetch_add_explicit(&census->map_misses, 1, memory_order_relaxed);
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
static inline void hl_backend_tree_translated_fall_stop(unsigned reason) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census != NULL && reason < HL_BACKEND_FALL_COUNT)
        atomic_fetch_add_explicit(&census->translated_fall_stop[reason], 1, memory_order_relaxed);
}
static uintptr_t hl_backend_tree_fallthrough_ibtc_hit_counter_address(unsigned reason) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return 0;
    if (reason == HL_BACKEND_FALL_FS_TRANSACTION)
        return (uintptr_t)&census->fallthrough_ibtc_fs_transaction_hits;
    if (reason == HL_BACKEND_FALL_NORMAL_TO_FS)
        return (uintptr_t)&census->fallthrough_ibtc_normal_to_fs_hits;
    return 0;
}
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

/* Diagnostic shadow only. No emitted branch reads this table and none of its
 * counters selects policy, so the census cannot change the misses it counts. */
static void hl_backend_tree_jcc_ibtc_fill_cause(uint64_t source, uint64_t target,
                                                uint64_t previous_target, int interrupt_consumed) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return;
    (void)source;
    hl_backend_jcc_fill_record(&census->jcc_ibtc_fill_empty, &census->jcc_ibtc_fill_collision,
                               &census->jcc_ibtc_fill_irq, &census->jcc_ibtc_fill_same_key,
                               target, previous_target, interrupt_consumed);
}

static void hl_backend_tree_jcc_late_eligible(uint64_t cache_generation, uint64_t source,
                                              uint64_t target, uintptr_t body,
                                              uint64_t target_generation, int generation_current) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    struct hl_backend_tree_slot *process_slot = g_backend_mixed_sse_self;
    if (census == NULL) return;
    int process = process_slot == NULL ? 0 : atomic_load_explicit(&process_slot->pid, memory_order_acquire);
    uint64_t birth = process_slot == NULL ? 0 : atomic_load_explicit(&process_slot->birth_ns, memory_order_acquire);
    hl_backend_jcc_late_record(census->jcc_late_sites, HL_BACKEND_JCC_LATE_SITES,
                               &census->jcc_late_site_first, &census->jcc_late_site_repeated,
                               &census->jcc_late_site_stable, &census->jcc_late_site_changed,
                               &census->jcc_late_site_current, &census->jcc_late_site_retired,
                               &census->jcc_late_site_unique, &census->jcc_late_site_overflow,
                               &census->jcc_late_site_abandoned, &census->jcc_late_site_max,
                               process, birth, cache_generation, source, target, body, target_generation,
                               generation_current, NULL, NULL, NULL);
}

static uintptr_t hl_backend_tree_indirect_ibtc_miss_counter_address(void) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return 0;
    return (uintptr_t)&census->indirect_ibtc_misses;
}

static void hl_backend_tree_jcc_invalid(unsigned reason, uint64_t source, uint64_t target) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL || reason >= HL_BACKEND_JCC_INVALID_REASON_COUNT) return;
    atomic_fetch_add_explicit(&census->jcc_invalid_reason[reason], 1, memory_order_relaxed);
    hl_backend_jcc_invalid_site_record(census->jcc_invalid_sites, &census->jcc_invalid_site_unique,
                                       &census->jcc_invalid_site_overflow, reason, source, target);
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
enum hl_backend_direct_call_ibtc_counter {
    HL_BACKEND_DIRECT_CALL_IBTC_EMITTED,
    HL_BACKEND_DIRECT_CALL_IBTC_HIT,
    HL_BACKEND_DIRECT_CALL_IBTC_MISS,
    HL_BACKEND_DIRECT_CALL_IBTC_IRQ,
    HL_BACKEND_DIRECT_CALL_IBTC_FILL,
    HL_BACKEND_DIRECT_CALL_IBTC_INVALID_REFUSAL,
};
static _Atomic uint64_t *hl_backend_tree_direct_call_ibtc_counter(
    enum hl_backend_direct_call_ibtc_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_DIRECT_CALL_IBTC_EMITTED: return &census->direct_call_ibtc_emitted;
    case HL_BACKEND_DIRECT_CALL_IBTC_HIT: return &census->direct_call_ibtc_hits;
    case HL_BACKEND_DIRECT_CALL_IBTC_MISS: return &census->direct_call_ibtc_misses;
    case HL_BACKEND_DIRECT_CALL_IBTC_IRQ: return &census->direct_call_ibtc_irq;
    case HL_BACKEND_DIRECT_CALL_IBTC_FILL: return &census->direct_call_ibtc_fills;
    case HL_BACKEND_DIRECT_CALL_IBTC_INVALID_REFUSAL: return &census->direct_call_ibtc_invalid_refusals;
    }
    return NULL;
}
static uintptr_t hl_backend_tree_direct_call_ibtc_dynamic_counter_address(
    enum hl_backend_direct_call_ibtc_counter kind) {
    if (kind != HL_BACKEND_DIRECT_CALL_IBTC_HIT && kind != HL_BACKEND_DIRECT_CALL_IBTC_MISS &&
        kind != HL_BACKEND_DIRECT_CALL_IBTC_IRQ)
        return 0;
    return (uintptr_t)hl_backend_tree_direct_call_ibtc_counter(kind);
}
static void hl_backend_tree_direct_call_ibtc_add(enum hl_backend_direct_call_ibtc_counter kind,
                                                 uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_direct_call_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}

enum hl_backend_ret_ibtc_counter {
    HL_BACKEND_RET_IBTC_ATTEMPT,
    HL_BACKEND_RET_IBTC_HIT,
    HL_BACKEND_RET_IBTC_KEY_MISS,
    HL_BACKEND_RET_IBTC_NULL_MISS,
    HL_BACKEND_RET_IBTC_IRQ,
    HL_BACKEND_RET_IBTC_FILL,
    HL_BACKEND_RET_IBTC_COLLISION,
    HL_BACKEND_RET_IBTC_UNMAPPED,
    HL_BACKEND_RET_IBTC_INVALID_REFUSAL,
};

static _Atomic uint64_t *hl_backend_tree_ret_ibtc_counter(enum hl_backend_ret_ibtc_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_RET_IBTC_ATTEMPT: return &census->ret_ibtc_attempts;
    case HL_BACKEND_RET_IBTC_HIT: return &census->ret_ibtc_hits;
    case HL_BACKEND_RET_IBTC_KEY_MISS: return &census->ret_ibtc_key_misses;
    case HL_BACKEND_RET_IBTC_NULL_MISS: return &census->ret_ibtc_null_misses;
    case HL_BACKEND_RET_IBTC_IRQ: return &census->ret_ibtc_irq;
    case HL_BACKEND_RET_IBTC_FILL: return &census->ret_ibtc_fills;
    case HL_BACKEND_RET_IBTC_COLLISION: return &census->ret_ibtc_collisions;
    case HL_BACKEND_RET_IBTC_UNMAPPED: return &census->ret_ibtc_unmapped;
    case HL_BACKEND_RET_IBTC_INVALID_REFUSAL: return &census->ret_ibtc_invalid_refusals;
    }
    return NULL;
}

static int hl_backend_tree_ret_ibtc_enabled(void) {
    return g_backend_mixed_sse != NULL;
}

static void hl_backend_tree_ret_ibtc_add(enum hl_backend_ret_ibtc_counter kind, uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_ret_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}

enum hl_backend_ret_fast_ibtc_counter {
    HL_BACKEND_RET_FAST_IBTC_HIT,
    HL_BACKEND_RET_FAST_IBTC_MISS,
    HL_BACKEND_RET_FAST_IBTC_IRQ,
    HL_BACKEND_RET_FAST_IBTC_FILL,
    HL_BACKEND_RET_FAST_IBTC_INVALID_REFUSAL,
};

static _Atomic uint64_t *hl_backend_tree_ret_fast_ibtc_counter(enum hl_backend_ret_fast_ibtc_counter kind) {
    struct hl_backend_mixed_sse_shared *census = g_backend_mixed_sse;
    if (census == NULL) return NULL;
    switch (kind) {
    case HL_BACKEND_RET_FAST_IBTC_HIT: return &census->ret_fast_ibtc_hits;
    case HL_BACKEND_RET_FAST_IBTC_MISS: return &census->ret_fast_ibtc_misses;
    case HL_BACKEND_RET_FAST_IBTC_IRQ: return &census->ret_fast_ibtc_irq;
    case HL_BACKEND_RET_FAST_IBTC_FILL: return &census->ret_fast_ibtc_fills;
    case HL_BACKEND_RET_FAST_IBTC_INVALID_REFUSAL: return &census->ret_fast_ibtc_invalid_refusals;
    }
    return NULL;
}

static uintptr_t hl_backend_tree_ret_fast_ibtc_dynamic_counter_address(
    enum hl_backend_ret_fast_ibtc_counter kind) {
    return (uintptr_t)hl_backend_tree_ret_fast_ibtc_counter(kind);
}

static void hl_backend_tree_ret_fast_ibtc_add(enum hl_backend_ret_fast_ibtc_counter kind, uint64_t count) {
    _Atomic uint64_t *counter = hl_backend_tree_ret_fast_ibtc_counter(kind);
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
}
#define hl_backend_tree_interpreter_entry(kind, fallback_form) ((void)0)
#define hl_backend_tree_interpreter_stop(kind, stop_form) ((void)0)
#define hl_backend_tree_direct_edge(family, same_page) ((void)0)
#define hl_backend_tree_direct_edge_resolution(family, resolution, translated, current, rel32, eligible) ((void)0)
#define hl_backend_tree_would_link(family, disposition) ((void)0)
static inline void hl_backend_tree_jcc_late(unsigned reason) {
    if (g_backend_mixed_sse != NULL && reason < HL_BACKEND_JCC_LATE_REASON_COUNT)
        atomic_fetch_add_explicit(&g_backend_mixed_sse->jcc_late[reason], 1, memory_order_relaxed);
}
#define hl_backend_tree_family_jmem() ((void)0)
#define hl_backend_tree_family_div(is_signed, outcome) ((void)0)
#define hl_backend_tree_family_div_service64_completed(is_signed) ((void)0)
#define hl_backend_tree_translation() ((void)0)
#define hl_backend_tree_map_hit() ((void)0)
#define hl_backend_tree_map_miss() ((void)0)
#define hl_backend_tree_stw_retry() ((void)0)
#define hl_backend_tree_irq_pending() ((void)0)
static _Noreturn void hl_backend_tree_abnormal_exit(int status) {
    (void)hl_backend_tree_finalize_from(1, HL_BACKEND_FINALIZE_ABNORMAL_EXIT);
    _exit(status);
}

#endif
