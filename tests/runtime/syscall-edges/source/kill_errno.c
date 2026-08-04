// syscall-compat regression: kill/tgkill signal-number and target validation. kill with an out-of-range
// signal -> EINVAL; kill(0-signal) to a non-existent high pid -> ESRCH; tgkill with a bad signal -> EINVAL;
// tgkill to a non-existent tgid -> ESRCH; kill of our own pid with signal 0 (existence probe) -> 0.
// Arch-neutral: errnos only.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    printf("kill_badsig_errno=%d\n", kill(getpid(), 12345) == -1 ? errno : 0);
    printf("kill_nopid_errno=%d\n", kill(0x40000000, 0) == -1 ? errno : 0);
    printf("self_probe_ok=%d\n", kill(getpid(), 0) == 0);
    printf("tgkill_badsig_errno=%d\n", syscall(SYS_tgkill, getpid(), getpid(), 12345) == -1 ? errno : 0);
    printf("tgkill_nopid_errno=%d\n", syscall(SYS_tgkill, 0x40000000, 0x40000000, 0) == -1 ? errno : 0);
    return 0;
}
