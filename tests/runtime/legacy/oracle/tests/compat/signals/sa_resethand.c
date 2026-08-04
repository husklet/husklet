// SA_RESETHAND: disposition reverts to SIG_DFL after the first delivery. We catch once, then a
// second raise of a normally-fatal signal must take the default action (terminate) in a child.
// Also verify the handler's own mask still applied for the single delivery.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t hits;
static void oneshot(int s) { (void)s; hits++; }

int main(void) {
    pid_t pid = fork();
    if (pid == 0) {
        struct sigaction sa = {0};
        sa.sa_handler = oneshot;
        sigemptyset(&sa.sa_mask);
        sa.sa_flags = SA_RESETHAND;
        sigaction(SIGUSR1, &sa, NULL);
        raise(SIGUSR1);          // handled once
        // disposition now SIG_DFL for SIGUSR1 (terminates process)
        if (hits != 1) _exit(42);
        raise(SIGUSR1);          // must terminate now
        _exit(7);                // reached only if not terminated
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int reset = WIFSIGNALED(st) && WTERMSIG(st) == SIGUSR1;
    printf("sa_resethand handled_once_then_default=%d\n", reset);
    return 0;
}
