// sched_setattr/sched_getattr conformance + cross-entry-point consistency for the root container.
// The scheduling profile has ONE source of truth, so every query path must agree AND round-trip:
//   - sched_getattr's sched_policy equals sched_getscheduler(2) after any policy change (the old getattr
//     hardcoded SCHED_OTHER, so it disagreed with getscheduler -> RED);
//   - sched_setattr RECORDS the policy, so a following sched_getscheduler/sched_getattr reflect it (the old
//     setattr ignored its argument entirely -> getscheduler stayed SCHED_OTHER -> RED);
//   - sched_setattr rejects the real-time classes with EPERM exactly as sched_setscheduler does, instead of
//     the old blanket success that claimed RT scheduling was installed -> RED;
//   - sched_getattr's sched_nice tracks the live process nice (setpriority), not a frozen 0 -> RED;
//   - argument validation (NULL attr, unknown policy) matches the kernel (EINVAL).
// Reported as a single boolean (ok=1), validated by running THROUGH the engine.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <unistd.h>

struct hl_sched_attr {
    uint32_t size;
    uint32_t sched_policy;
    uint64_t sched_flags;
    int32_t sched_nice;
    uint32_t sched_priority;
    uint64_t sched_runtime;
    uint64_t sched_deadline;
    uint64_t sched_period;
};

static int getattr(struct hl_sched_attr *a) {
    memset(a, 0, sizeof *a);
    return (int)syscall(SYS_sched_getattr, 0, a, (unsigned)sizeof *a, 0u);
}

static int setattr(uint32_t policy, int32_t nice, uint32_t prio) {
    struct hl_sched_attr a;
    memset(&a, 0, sizeof a);
    a.size = sizeof a;
    a.sched_policy = policy;
    a.sched_nice = nice;
    a.sched_priority = prio;
    errno = 0;
    return (int)syscall(SYS_sched_setattr, 0, &a, 0u);
}

int main(void) {
    int ok = 1;
    struct sched_param z;
    memset(&z, 0, sizeof z);
    struct hl_sched_attr a;

    // default policy is SCHED_OTHER and getattr agrees with getscheduler
    if (sched_getscheduler(0) != SCHED_OTHER) ok = 0;
    if (getattr(&a) != 0 || a.size != sizeof a || a.sched_policy != SCHED_OTHER || a.sched_priority != 0) ok = 0;

    // sched_setscheduler(BATCH) -> getattr policy must follow (the hardcoded-OTHER bug)
    if (sched_setscheduler(0, SCHED_BATCH, &z) != 0) ok = 0;
    if (getattr(&a) != 0 || a.sched_policy != (uint32_t)SCHED_BATCH) ok = 0;
    if (sched_getscheduler(0) != SCHED_BATCH) ok = 0;

    // sched_setscheduler(IDLE) -> getattr policy follows
    if (sched_setscheduler(0, SCHED_IDLE, &z) != 0) ok = 0;
    if (getattr(&a) != 0 || a.sched_policy != (uint32_t)SCHED_IDLE) ok = 0;

    // reset via sched_setattr(BATCH) -> RECORDED, so getscheduler + getattr both report BATCH
    if (setattr(SCHED_BATCH, 0, 0) != 0) ok = 0;
    if (sched_getscheduler(0) != SCHED_BATCH) ok = 0;
    if (getattr(&a) != 0 || a.sched_policy != (uint32_t)SCHED_BATCH) ok = 0;

    // sched_setattr(OTHER) round-trips back to SCHED_OTHER
    if (setattr(SCHED_OTHER, 0, 0) != 0) ok = 0;
    if (sched_getscheduler(0) != SCHED_OTHER) ok = 0;

    // real-time class via setattr rejected EPERM, consistent with sched_setscheduler
    errno = 0;
    if (setattr(SCHED_FIFO, 0, 50) != -1 || errno != EPERM) ok = 0;
    errno = 0;
    if (setattr(SCHED_RR, 0, 50) != -1 || errno != EPERM) ok = 0;
    // policy unchanged by the rejected RT requests
    if (sched_getscheduler(0) != SCHED_OTHER) ok = 0;

    // argument validation: NULL attr and unknown policy -> EINVAL
    errno = 0;
    if (syscall(SYS_sched_setattr, 0, (void *)0, 0u) != -1 || errno != EINVAL) ok = 0;
    errno = 0;
    if (setattr(99u, 0, 0) != -1 || errno != EINVAL) ok = 0;

    // sched_getattr's sched_nice tracks the live process nice, not a frozen 0
    setpriority(PRIO_PROCESS, 0, 5);
    if (getattr(&a) != 0 || a.sched_nice != 5) ok = 0;
    setpriority(PRIO_PROCESS, 0, 0);

    // priority bands per policy match the kernel
    if (sched_get_priority_max(SCHED_FIFO) != 99 || sched_get_priority_min(SCHED_FIFO) != 1) ok = 0;
    if (sched_get_priority_max(SCHED_OTHER) != 0 || sched_get_priority_min(SCHED_OTHER) != 0) ok = 0;

    printf("sched_attr ok=%d\n", ok);
    return 0;
}
