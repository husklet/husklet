// syscall-compat regression: SIGKILL (and SIGSTOP) can never be blocked via rt_sigprocmask. A child
// that blocks SIGKILL and then self-signals must still die from it -- not survive with it "pending".
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t pid = fork();
    if (pid == 0) {
        sigset_t s;
        sigemptyset(&s);
        sigaddset(&s, SIGKILL);
        sigaddset(&s, SIGSTOP);
        sigprocmask(SIG_BLOCK, &s, NULL); // Linux silently refuses to block these
        kill(getpid(), SIGKILL);          // must be fatal despite the block above
        printf("survived=1\n");           // only reached if SIGKILL was wrongly held pending
        fflush(stdout);
        _exit(7);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    printf("killed_by=%d\n", WIFSIGNALED(st) ? WTERMSIG(st) : 0);
    return 0;
}
