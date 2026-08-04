// syscall-compat regression: rt_sigaction must refuse to install a handler for SIGKILL/SIGSTOP with EINVAL,
// reject an out-of-range signal number with EINVAL. Raw syscall so
// the engine's validation is exercised (glibc pre-checks these). Arch-neutral: errnos only.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = SIG_IGN;
    // sigsetsize must be 8 (kernel sigset size on both ISAs).
    printf("kill_errno=%d\n", syscall(SYS_rt_sigaction, SIGKILL, &sa, (void *)0, 8) == -1 ? errno : 0);
    printf("stop_errno=%d\n", syscall(SYS_rt_sigaction, SIGSTOP, &sa, (void *)0, 8) == -1 ? errno : 0);
    printf("badsig_errno=%d\n", syscall(SYS_rt_sigaction, 12345, &sa, (void *)0, 8) == -1 ? errno : 0);
    return 0;
}
