// clone3(CLONE_PIDFD): the kernel installs a pidfd referring to the child. The pidfd becomes readable
// (POLLIN) when the child dies and can be reaped with waitid(P_PIDFD). Exercises the modern pidfd
// child-death path end to end. Deterministic derived booleans.
#define _GNU_SOURCE
#include <linux/sched.h>
#include <poll.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int pidfd = -1;
    struct clone_args args = {0};
    args.flags = CLONE_PIDFD;
    args.pidfd = (uint64_t)(uintptr_t)&pidfd;
    args.exit_signal = SIGCHLD;

    long pid = syscall(SYS_clone3, &args, sizeof args);
    if (pid == 0) {
        usleep(40 * 1000);
        _exit(19);
    }
    if (pid < 0) { printf("clone3 fail\n"); return 1; }
    int got_fd = pidfd >= 0;

    // Poll the pidfd: must transition to readable when the child exits.
    struct pollfd pfd = { .fd = pidfd, .events = POLLIN };
    int pr = poll(&pfd, 1, 5000);
    int became_ready = pr == 1 && (pfd.revents & POLLIN);

    // Reap via the pidfd.
    siginfo_t si;
    si.si_pid = 0;
    int wr = waitid(P_PIDFD, (id_t)pidfd, &si, WEXITED);
    int reaped = wr == 0 && si.si_code == CLD_EXITED && si.si_status == 19;

    close(pidfd);
    printf("clone3_pidfd got_fd=%d became_ready=%d reaped=%d\n", got_fd, became_ready, reaped);
    return 0;
}
