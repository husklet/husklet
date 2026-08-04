// syscall-compat regression: prlimit64 must validate its target pid and resource like Linux -- a dead
// pid is ESRCH, an out-of-range resource is EINVAL, and invalid user buffers
// are EFAULT rather than faults in the emulator process.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    struct rlimit old;
    // self pid, bogus resource number -> EINVAL
    long r1 = syscall(SYS_prlimit64, 0, 99999, (void *)0, &old);
    printf("badres_errno=%d\n", r1 == -1 ? errno : 0);
    // resource valid, but a pid far above PID_MAX (no such task) -> ESRCH
    long r2 = syscall(SYS_prlimit64, 0x40000000, RLIMIT_NOFILE, (void *)0, &old);
    printf("badpid_errno=%d\n", r2 == -1 ? errno : 0);
    long r3 = syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, (void *)0, (void *)-1);
    printf("badold_errno=%d\n", r3 == -1 ? errno : 0);
    long r4 = syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, (void *)-1, (void *)0);
    printf("badnew_errno=%d\n", r4 == -1 ? errno : 0);
    return 0;
}
