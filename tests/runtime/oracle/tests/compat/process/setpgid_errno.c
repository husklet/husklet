// Process-group / session syscall error surfaces fixed by the Linux ABI:
//   - setpgid with a negative pgid is EINVAL.
//   - setpgid targeting an unrelated absent pid is ESRCH.
//   - getpgid/getsid of a definitely-absent pid is ESRCH.
//   - setsid from a process that is already a group leader is EPERM (the harness child is made a
//     group leader first so this is deterministic).
//   - getpgid(0)/getsid(0) succeed and agree with getpgrp(). Only booleans/errnos are printed.
// Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    // Negative pgid -> EINVAL.
    errno = 0;
    printf("setpgid_negpgid_errno=%d\n", setpgid(0, -1) == -1 ? errno : 0);
    // Absent pid -> ESRCH.
    errno = 0;
    printf("setpgid_nopid_errno=%d\n", setpgid(0x3fffffff, 0) == -1 ? errno : 0);
    errno = 0;
    printf("getpgid_nopid_errno=%d\n", getpgid(0x3fffffff) == -1 ? errno : 0);
    errno = 0;
    printf("getsid_nopid_errno=%d\n", getsid(0x3fffffff) == -1 ? errno : 0);
    // Self queries succeed and are internally consistent.
    printf("getpgid0_ok=%d\n", getpgid(0) == getpgrp());
    printf("getsid0_ok=%d\n", getsid(0) > 0);
    // Become a group leader, then setsid -> EPERM.
    setpgid(0, 0);
    errno = 0;
    printf("setsid_leader_errno=%d\n", setsid() == -1 ? errno : 0);
    return 0;
}
