// syscall-compat regression: RLIMIT_NOFILE setrlimit/getrlimit round-trip and privilege check. A lowered
// soft limit reads back exactly; raising the soft limit above the hard limit -> EPERM (unprivileged). The
// test sets its own limit so the outcome is independent of the host/container default. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    struct rlimit rl;
    getrlimit(RLIMIT_NOFILE, &rl);
    rlim_t hard = rl.rlim_max;

    // Pin the soft limit to 64 open fds.
    rl.rlim_cur = 64;
    setrlimit(RLIMIT_NOFILE, &rl);

    struct rlimit back;
    getrlimit(RLIMIT_NOFILE, &back);
    printf("soft_set=%d\n", back.rlim_cur == 64);

    // Raising the soft limit above the hard limit -> EPERM (only privileged processes may).
    struct rlimit up;
    up.rlim_cur = hard + 1;
    up.rlim_max = hard + 1;
    printf("raise_hard_errno=%d\n", setrlimit(RLIMIT_NOFILE, &up) == -1 ? errno : 0);
    return 0;
}
