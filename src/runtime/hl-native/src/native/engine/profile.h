#ifndef HL_CORE_PROFILE_H
#define HL_CORE_PROFILE_H

#include <stddef.h>
#include <stdint.h>

typedef struct hl_dispatch_profile {
    int enabled;
    uint64_t crossings;
    uint64_t translations;
    uint64_t translation_ns;
} hl_dispatch_profile;

typedef void (*hl_dispatch_profile_reporter)(void *context, uint64_t translations, uint64_t translation_ns);

static inline uint64_t hl_dispatch_profile_begin(const hl_dispatch_profile *profile, uint64_t now) {
    return profile->enabled ? now : 0;
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

static inline void hl_dispatch_profile_report(const hl_dispatch_profile *profile, void *context,
                                              hl_dispatch_profile_reporter reporter) {
    if (reporter != NULL) reporter(context, profile->translations, profile->translation_ns);
}

#endif
