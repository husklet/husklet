// wait-family argument validation is a fixed Linux ABI error surface. waitid requires at least one of
// WEXITED/WSTOPPED/WCONTINUED (else EINVAL), rejects unknown option bits (EINVAL), rejects an unknown
// idtype (EINVAL), and reports ECHILD when there are no children. wait4 rejects unknown option bits
// (EINVAL). Only errnos are printed, so the golden is host-invariant. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int ec(long nr, long a, long b, long c, long d, long e) {
    long r = syscall(nr, a, b, c, d, e);
    return r == -1 ? errno : 0;
}

int main(void) {
    siginfo_t si;
    // waitid with options==0 (no WEXITED/WSTOPPED/WCONTINUED) -> EINVAL.
    printf("waitid_noflags_errno=%d\n", ec(SYS_waitid, P_ALL, 0, (long)&si, 0, 0));
    // waitid with an unknown option bit (0x40 is not a defined wait flag) -> EINVAL.
    printf("waitid_badopt_errno=%d\n", ec(SYS_waitid, P_ALL, 0, (long)&si, WEXITED | 0x40, 0));
    // waitid with an unknown idtype (99) -> EINVAL.
    printf("waitid_badidtype_errno=%d\n", ec(SYS_waitid, 99, 0, (long)&si, WEXITED, 0));
    // waitid with WEXITED and no children -> ECHILD(10).
    printf("waitid_nochild_errno=%d\n", ec(SYS_waitid, P_ALL, 0, (long)&si, WEXITED, 0));
    // wait4 with an unknown option bit -> EINVAL.
    int status;
    printf("wait4_badopt_errno=%d\n", ec(SYS_wait4, -1, (long)&status, 0x1000000, 0, 0));
    return 0;
}
