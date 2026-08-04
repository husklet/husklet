// pidfd_send_signal(2): open a pidfd for a child and deliver SIGTERM through it; the child dies by
// SIGTERM. Also verify sending signal 0 is a liveness probe (returns 0) before exit.
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_pidfd_open
#define SYS_pidfd_open 434
#endif
#ifndef SYS_pidfd_send_signal
#define SYS_pidfd_send_signal 424
#endif

int main(void) {
    pid_t pid = fork();
    if (pid == 0) {
        signal(SIGTERM, SIG_DFL);
        for (;;) pause();
        _exit(0);
    }
    usleep(50 * 1000);
    int pidfd = (int)syscall(SYS_pidfd_open, pid, 0);
    if (pidfd < 0) { printf("pidfd_send_signal open_fail\n"); return 1; }

    int probe = (int)syscall(SYS_pidfd_send_signal, pidfd, 0, (void *)0, 0);
    int alive = probe == 0;

    int r = (int)syscall(SYS_pidfd_send_signal, pidfd, SIGTERM, (void *)0, 0);
    int sent = r == 0;

    int st = 0;
    waitpid(pid, &st, 0);
    int killed = WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM;

    printf("pidfd_send_signal alive_probe=%d sent=%d killed=%d\n", alive, sent, killed);
    return 0;
}
