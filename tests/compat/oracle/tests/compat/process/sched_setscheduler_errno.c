// sched_setscheduler / sched_setparam argument validation is a fixed Linux ABI error surface,
// checked BEFORE any privilege decision:
//   - an unknown policy is EINVAL.
//   - a NULL sched_param is rejected (EINVAL).
//   - a real-time priority outside [1,99] for SCHED_FIFO is EINVAL.
//   - a non-zero priority for SCHED_OTHER is EINVAL.
//   - sched_getscheduler/sched_getparam for an absent pid is ESRCH.
// Only errnos are printed, so the golden is host-invariant. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>

int main(void) {
    struct sched_param pr = {.sched_priority = 50};
    // Unknown policy -> EINVAL.
    errno = 0;
    printf("badpolicy_errno=%d\n", sched_setscheduler(0, 0x999, &pr) == -1 ? errno : 0);
    // NULL param -> EFAULT.
    errno = 0;
    printf("nullparam_errno=%d\n", sched_setscheduler(0, SCHED_FIFO, NULL) == -1 ? errno : 0);
    // RT priority out of range for FIFO -> EINVAL.
    struct sched_param hi = {.sched_priority = 1000};
    errno = 0;
    printf("fifo_hiprio_errno=%d\n", sched_setscheduler(0, SCHED_FIFO, &hi) == -1 ? errno : 0);
    // Non-zero priority for SCHED_OTHER -> EINVAL.
    struct sched_param nz = {.sched_priority = 5};
    errno = 0;
    printf("other_nonzero_errno=%d\n", sched_setscheduler(0, SCHED_OTHER, &nz) == -1 ? errno : 0);
    // Queries for an absent pid -> ESRCH.
    errno = 0;
    printf("getsched_nopid_errno=%d\n", sched_getscheduler(0x3fffffff) == -1 ? errno : 0);
    struct sched_param out;
    errno = 0;
    printf("getparam_nopid_errno=%d\n", sched_getparam(0x3fffffff, &out) == -1 ? errno : 0);
    return 0;
}
