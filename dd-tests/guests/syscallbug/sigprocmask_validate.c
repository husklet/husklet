// syscall-compat regression: rt_sigprocmask must reject an invalid `how` and a wrong sigsetsize with
// EINVAL, not treat an unknown how as SIG_SETMASK / ignore the size. Raw syscall so the engine's
// validation is exercised (glibc's sigprocmask wrapper pre-checks `how`).
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    unsigned long set = 0;
    // invalid how (99), valid sigsetsize -> EINVAL
    long r1 = syscall(SYS_rt_sigprocmask, 99, &set, (void *)0, 8);
    printf("badhow_errno=%d\n", r1 == -1 ? errno : 0);
    // valid how (SIG_BLOCK=0), wrong sigsetsize (4) -> EINVAL
    long r2 = syscall(SYS_rt_sigprocmask, 0, &set, (void *)0, 4);
    printf("badsize_errno=%d\n", r2 == -1 ? errno : 0);
    return 0;
}
