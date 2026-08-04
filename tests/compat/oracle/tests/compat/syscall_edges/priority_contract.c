// getpriority/setpriority contract that is fixed by the Linux ABI regardless of host:
//   - setpriority clamps a nice request above the maximum down to 19 (the ceiling), and getpriority
//     then reads back exactly 19 -- an ABSOLUTE value independent of the starting nice, so host-invariant.
//   - an unknown `which` is EINVAL.
//   - PRIO_PROCESS for a definitely-absent pid is ESRCH.
// Raising niceness is always permitted unprivileged, so this needs no capabilities. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/resource.h>

int main(void) {
    // Request nice=100; the kernel clamps to the ceiling 19.
    setpriority(PRIO_PROCESS, 0, 100);
    errno = 0;
    int p = getpriority(PRIO_PROCESS, 0);
    printf("clamped_nice=%d\n", p);
    // Unknown `which` (99) -> EINVAL.
    errno = 0;
    printf("badwhich_errno=%d\n", getpriority(99, 0) == -1 ? errno : 0);
    errno = 0;
    printf("setbadwhich_errno=%d\n", setpriority(99, 0, 0) == -1 ? errno : 0);
    // PRIO_PROCESS for an absent pid -> ESRCH(3).
    errno = 0;
    printf("nopid_errno=%d\n", getpriority(PRIO_PROCESS, 0x3fffffff) == -1 ? errno : 0);
    return 0;
}
