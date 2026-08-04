// SIGCHLD with several children exiting: a handler drains all reapable children via
// waitpid(WNOHANG) in a loop. Because standard signals coalesce, the handler may fire fewer times
// than there are children, so the loop is what guarantees every child is reaped exactly once.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#define KIDS 5
static volatile sig_atomic_t reaped, handler_calls;

static void chld(int s) {
    (void)s;
    handler_calls++;
    int st;
    pid_t p;
    while ((p = waitpid(-1, &st, WNOHANG)) > 0) reaped++;
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = chld;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGCHLD, &sa, NULL);

    for (int i = 0; i < KIDS; i++) {
        pid_t p = fork();
        if (p == 0) { usleep(10000 * (i + 1)); _exit(0); }
    }
    // wait until all reaped
    while (reaped < KIDS) pause();
    int calls_bounded = handler_calls >= 1 && handler_calls <= KIDS;
    printf("sigchld_multi reaped=%d calls_ok=%d\n", (int)reaped, calls_bounded);
    return 0;
}
