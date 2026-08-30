#ifndef HL_CORE_PROFILE_H
#define HL_CORE_PROFILE_H

#include <stddef.h>
#include <stdint.h>

typedef struct hl_dispatch_profile {
    int enabled;
    uint64_t crossings;
    uint64_t translations;
    uint64_t translation_ns;
    uint64_t reason[18];
    uint64_t map_hit;
    uint64_t map_miss;
    uint64_t threaded_crossings;
    uint64_t stw_retries;
    uint64_t pending_interrupts;
    uint64_t sampled;
    uint64_t poll_ns;
    uint64_t resolve_ns;
    uint64_t stw_ns;
    uint64_t block_ns;
    uint64_t reason_ns;
} hl_dispatch_profile;

#if defined(HL_NATIVE_TEST_HOOKS)
enum dispatch_redispatch_counter {
    REDISPATCH_ATTEMPTED,
    REDISPATCH_HIT,
    REDISPATCH_THREADED_HIT,
    REDISPATCH_MAP_MISS,
    REDISPATCH_STALE,
    REDISPATCH_THREADED,
    REDISPATCH_IRQ,
    REDISPATCH_SIGNAL,
    REDISPATCH_FATAL,
    REDISPATCH_EXITED,
    REDISPATCH_BUDGET,
    REDISPATCH_COUNTER_COUNT,
};
static _Atomic uint64_t g_dispatch_redispatch[REDISPATCH_COUNTER_COUNT];
#endif


enum {
    /* Native reason values outside the compact stable 0..15 range are deliberately collapsed. */
    HL_DISPATCH_REASON_LIMIT = 16,
    HL_DISPATCH_REASON_STW_RETRY = 16,
    HL_DISPATCH_REASON_OTHER = 17,
};

typedef enum hl_dispatch_profile_phase {
    HL_DISPATCH_PHASE_POLL,
    HL_DISPATCH_PHASE_MAP,
    HL_DISPATCH_PHASE_STW,
    HL_DISPATCH_PHASE_BLOCK,
    HL_DISPATCH_PHASE_REASON,
} hl_dispatch_profile_phase;

typedef void (*hl_dispatch_profile_reporter)(void *context, uint64_t translations, uint64_t translation_ns);

static inline uint64_t hl_dispatch_profile_begin(const hl_dispatch_profile *profile, uint64_t now) {
    return profile->enabled ? now : 0;
}

static inline void hl_dispatch_profile_translation(hl_dispatch_profile *profile) {
    /* This count is also the backend-executed proof, so it exists independently of diagnostics. */
    __atomic_fetch_add(&profile->translations, 1, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_translation_end(hl_dispatch_profile *profile, uint64_t start, uint64_t now) {
    if (profile->enabled) __atomic_fetch_add(&profile->translation_ns, now - start, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_crossing(hl_dispatch_profile *profile) {
    if (profile->enabled) __atomic_fetch_add(&profile->crossings, 1, __ATOMIC_RELAXED);
}

static inline int hl_dispatch_profile_sample(const hl_dispatch_profile *profile) {
    return profile->enabled && (__atomic_load_n(&profile->crossings, __ATOMIC_RELAXED) & UINT64_C(63)) == 0;
}

static inline uint64_t hl_dispatch_profile_load(const uint64_t *value) {
    return __atomic_load_n(value, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_map(hl_dispatch_profile *profile, int hit) {
    if (!profile->enabled) return;
    if (hit)
        __atomic_fetch_add(&profile->map_hit, 1, __ATOMIC_RELAXED);
    else
        __atomic_fetch_add(&profile->map_miss, 1, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_pending(hl_dispatch_profile *profile) {
    if (profile->enabled) __atomic_fetch_add(&profile->pending_interrupts, 1, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_threaded(hl_dispatch_profile *profile) {
    if (profile->enabled) __atomic_fetch_add(&profile->threaded_crossings, 1, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_reason(hl_dispatch_profile *profile, uint64_t reason, int stw_retry) {
    if (!profile->enabled) return;
    size_t bucket = stw_retry ? HL_DISPATCH_REASON_STW_RETRY
                              : reason < HL_DISPATCH_REASON_LIMIT ? (size_t)reason : HL_DISPATCH_REASON_OTHER;
    __atomic_fetch_add(&profile->reason[bucket], 1, __ATOMIC_RELAXED);
    if (stw_retry) __atomic_fetch_add(&profile->stw_retries, 1, __ATOMIC_RELAXED);
}

static inline void hl_dispatch_profile_delta(hl_dispatch_profile *profile, hl_dispatch_profile_phase phase,
                                             uint64_t start, uint64_t end) {
    if (!profile->enabled || end < start) return;
    uint64_t delta = end - start;
    switch (phase) {
    case HL_DISPATCH_PHASE_POLL: __atomic_fetch_add(&profile->poll_ns, delta, __ATOMIC_RELAXED); break;
    case HL_DISPATCH_PHASE_MAP: __atomic_fetch_add(&profile->resolve_ns, delta, __ATOMIC_RELAXED); break;
    case HL_DISPATCH_PHASE_STW: __atomic_fetch_add(&profile->stw_ns, delta, __ATOMIC_RELAXED); break;
    case HL_DISPATCH_PHASE_BLOCK: __atomic_fetch_add(&profile->block_ns, delta, __ATOMIC_RELAXED); break;
    case HL_DISPATCH_PHASE_REASON: __atomic_fetch_add(&profile->reason_ns, delta, __ATOMIC_RELAXED); break;
    default: break;
    }
}

static inline uint64_t hl_dispatch_profile_reason_total(const hl_dispatch_profile *profile) {
    uint64_t total = 0;
    for (size_t index = 0; index < sizeof profile->reason / sizeof profile->reason[0]; ++index)
        total += __atomic_load_n(&profile->reason[index], __ATOMIC_RELAXED);
    return total;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static inline int hl_dispatch_profile_accumulator_test(void) {
    hl_dispatch_profile profile = { .enabled = 1 };
    if (!hl_dispatch_profile_sample(&profile)) return 1;
    hl_dispatch_profile_crossing(&profile);
    if (hl_dispatch_profile_sample(&profile)) return 2;
    profile.crossings = 64;
    if (!hl_dispatch_profile_sample(&profile)) return 3;
    hl_dispatch_profile_map(&profile, 1);
    hl_dispatch_profile_map(&profile, 0);
    hl_dispatch_profile_threaded(&profile);
    hl_dispatch_profile_pending(&profile);
    hl_dispatch_profile_reason(&profile, 3, 0);
    hl_dispatch_profile_reason(&profile, 99, 0);
    hl_dispatch_profile_reason(&profile, 0, 1);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_POLL, 10, 17);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_MAP, 20, 31);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_STW, 40, 53);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_BLOCK, 60, 77);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_REASON, 90, 109);
    hl_dispatch_profile_delta(&profile, HL_DISPATCH_PHASE_REASON, 10, 9); /* a broken clock adds nothing */
    hl_dispatch_profile_delta(&profile, (hl_dispatch_profile_phase)99, 1, 1000); /* an invalid phase adds nothing */
    profile.crossings = 3;
    if (profile.map_hit != 1 || profile.map_miss != 1 || profile.threaded_crossings != 1 ||
        profile.pending_interrupts != 1 || profile.stw_retries != 1)
        return 4;
    if (profile.reason[3] != 1 || profile.reason[HL_DISPATCH_REASON_OTHER] != 1 ||
        profile.reason[HL_DISPATCH_REASON_STW_RETRY] != 1 || hl_dispatch_profile_reason_total(&profile) != 3)
        return 5;
    if (profile.poll_ns != 7 || profile.resolve_ns != 11 || profile.stw_ns != 13 || profile.block_ns != 17 ||
        profile.reason_ns != 19)
        return 6;
    if (hl_dispatch_profile_reason_total(&profile) != profile.crossings) return 8;
    profile.enabled = 0;
    hl_dispatch_profile_map(&profile, 1);
    hl_dispatch_profile_reason(&profile, 4, 0);
    if (profile.map_hit != 1 || hl_dispatch_profile_reason_total(&profile) != 3) return 7;
    return 0;
}
#endif

static inline void hl_dispatch_profile_report(const hl_dispatch_profile *profile, void *context,
                                              hl_dispatch_profile_reporter reporter) {
    if (reporter != NULL) reporter(context, profile->translations, profile->translation_ns);
}

#endif
