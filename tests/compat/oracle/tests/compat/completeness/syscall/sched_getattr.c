/* sched_getattr / sched_setattr (the extended, sched_attr-struct scheduler interface, distinct from
   the legacy sched_getparam probe). getattr on self must return the task's policy and nice/priority
   in a well-formed sched_attr; a setattr that re-applies the SAME normal-policy attributes must be
   accepted. A correct engine round-trips these or returns ENOSYS. Derived booleans, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#ifndef __NR_sched_getattr
#if defined(__aarch64__)
#define __NR_sched_getattr 275
#define __NR_sched_setattr 274
#else
#define __NR_sched_getattr 315
#define __NR_sched_setattr 314
#endif
#endif

struct sched_attr {
    uint32_t size, sched_policy;
    uint64_t sched_flags;
    int32_t  sched_nice;
    uint32_t sched_priority;
    uint64_t sched_runtime, sched_deadline, sched_period;
};

int main(void) {
    struct sched_attr a;
    memset(&a, 0, sizeof a);
    a.size = sizeof a;
    long g = syscall(__NR_sched_getattr, 0, &a, (unsigned)sizeof a, 0u);
    int get_ok = (g == 0) || (errno == ENOSYS);
    int policy_normal = (g != 0) ? 1 : (a.sched_policy <= 6);
    int size_set = (g != 0) ? 1 : (a.size >= 48);

    long s = -1;
    if (g == 0 && a.sched_policy <= 2) {   /* only for normal/batch/idle */
        a.size = sizeof a;
        s = syscall(__NR_sched_setattr, 0, &a, 0u);
    }
    int set_ok = (g != 0) || (a.sched_policy > 2) || (s == 0) ||
                 (errno == ENOSYS || errno == EPERM);

    printf("sched_getattr get_ok=%d policy_normal=%d size_set=%d set_ok=%d\n",
           get_ok, policy_normal, size_set, set_ok);
    return 0;
}
