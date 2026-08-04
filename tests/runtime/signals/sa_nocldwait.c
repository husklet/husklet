// SA_NOCLDWAIT on SIGCHLD: children are auto-reaped, never becoming zombies, so a subsequent
// wait() returns -1/ECHILD once they are all gone.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = SIG_DFL;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_NOCLDWAIT;
    sigaction(SIGCHLD, &sa, NULL);

    for (int i = 0; i < 3; i++) {
        pid_t p = fork();
        if (p == 0) { _exit(0); }
    }
    // give kernel time to auto-reap
    usleep(200 * 1000);
    errno = 0;
    pid_t r = wait(NULL);
    int echild = r == -1 && errno == ECHILD;
    printf("sa_nocldwait auto_reaped_echild=%d\n", echild);
    return 0;
}
