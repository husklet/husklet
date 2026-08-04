/* landlock_create_ruleset robustness + ABI-version query. The version query
   (rule_attr=NULL, size=0, flags=LANDLOCK_CREATE_RULESET_VERSION) returns the supported ABI version
   (a small positive integer) on a landlock-capable kernel, or fails ENOSYS when the LSM is absent. A
   correct engine must do one of those, never crash or return a bogus value. Verdict is a derived
   boolean (arch-neutral, host-independent). */
#include "compat.h"
#include <stdio.h>
#ifndef __NR_landlock_create_ruleset
#define __NR_landlock_create_ruleset 444
#endif
#ifndef LANDLOCK_CREATE_RULESET_VERSION
#define LANDLOCK_CREATE_RULESET_VERSION (1U << 0)
#endif

int main(void) {
    long v = syscall(__NR_landlock_create_ruleset, (void *)0, (size_t)0,
                     (unsigned)LANDLOCK_CREATE_RULESET_VERSION);
    int handled = (v >= 1) || (v < 0 && (errno == ENOSYS || errno == EOPNOTSUPP || errno == EPERM));

    /* a bad flags value must be rejected, not silently accepted */
    long bad = syscall(__NR_landlock_create_ruleset, (void *)0, (size_t)0, 0xFFFFFFFFu);
    int bad_rejected = (bad < 0) && (errno == EINVAL || errno == ENOSYS || errno == EOPNOTSUPP);

    printf("landlock version_handled=%d bad_flags_rejected=%d\n", handled, bad_rejected);
    return 0;
}
