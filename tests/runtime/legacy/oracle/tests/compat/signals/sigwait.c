// sigwait(): synchronously consume a blocked signal (no handler runs). A child raises SIGUSR2 in
// the parent; sigwait returns that signo. Portable POSIX -> golden verdict on every engine.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);

    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) {
        usleep(100000);
        kill(parent, SIGUSR2);
        _exit(0);
    }
    int sig = 0;
    int rc = sigwait(&set, &sig);
    int ok = rc == 0 && sig == SIGUSR2;
    // the signal was consumed synchronously => nothing pending afterwards
    sigset_t pend;
    sigpending(&pend);
    int clear = !sigismember(&pend, SIGUSR2);
    waitpid(pid, NULL, 0);
    printf("sigwait ok=%d clear=%d\n", ok, clear);
    return 0;
}
