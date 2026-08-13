#ifndef HL_CORE_PROFILE_H
#define HL_CORE_PROFILE_H

#include <stddef.h>
#include <stdint.h>

typedef struct hl_dispatch_profile {
    int enabled;
    uint64_t crossings;
    uint64_t translations;
    uint64_t translation_ns;
    uint64_t exit_softmiss;
    uint64_t exit_softspan;
    uint64_t exit_branch;
    uint64_t exit_syscall;
    uint64_t exit_other;
    uint64_t ibtc_miss;
    uint64_t branch_after_translation;
    uint64_t branch_cached;
    uint64_t threaded_transitions;
    int threaded_final;
} hl_dispatch_profile;

typedef enum hl_dispatch_exit_category {
    HL_DISPATCH_EXIT_SOFTMISS,
    HL_DISPATCH_EXIT_SOFTSPAN,
    HL_DISPATCH_EXIT_BRANCH,
    HL_DISPATCH_EXIT_SYSCALL,
    HL_DISPATCH_EXIT_OTHER,
} hl_dispatch_exit_category;

typedef void (*hl_dispatch_profile_reporter)(void *context, uint64_t translations, uint64_t translation_ns);

static inline uint64_t hl_dispatch_profile_begin(const hl_dispatch_profile *profile, uint64_t now) {
    return profile->enabled ? now : 0;
}

static inline void hl_dispatch_profile_threaded(hl_dispatch_profile *profile, int threaded) {
    if (!profile->enabled) return;
    threaded = threaded != 0;
    if (threaded != profile->threaded_final) profile->threaded_transitions++;
    profile->threaded_final = threaded;
}

static inline void hl_dispatch_profile_translation(hl_dispatch_profile *profile) {
    profile->translations++;
}

static inline void hl_dispatch_profile_translation_end(hl_dispatch_profile *profile, uint64_t start, uint64_t now) {
    if (profile->enabled) profile->translation_ns += now - start;
}

static inline void hl_dispatch_profile_crossing(hl_dispatch_profile *profile) {
    if (profile->enabled) profile->crossings++;
}

static inline void hl_dispatch_profile_exit(hl_dispatch_profile *profile, hl_dispatch_exit_category category,
                                            int ibtc_miss, int translated) {
    if (!profile->enabled) return;
    switch (category) {
        case HL_DISPATCH_EXIT_SOFTMISS: profile->exit_softmiss++; break;
        case HL_DISPATCH_EXIT_SOFTSPAN: profile->exit_softspan++; break;
        case HL_DISPATCH_EXIT_BRANCH: profile->exit_branch++; break;
        case HL_DISPATCH_EXIT_SYSCALL: profile->exit_syscall++; break;
        case HL_DISPATCH_EXIT_OTHER: profile->exit_other++; break;
    }
    if (ibtc_miss) profile->ibtc_miss++;
    if (category == HL_DISPATCH_EXIT_BRANCH && !ibtc_miss) {
        if (translated)
            profile->branch_after_translation++;
        else
            profile->branch_cached++;
    }
}

static inline void hl_dispatch_profile_report(const hl_dispatch_profile *profile, void *context,
                                              hl_dispatch_profile_reporter reporter) {
    if (reporter != NULL) reporter(context, profile->translations, profile->translation_ns);
}

#endif
