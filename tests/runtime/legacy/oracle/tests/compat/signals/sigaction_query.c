// sigaction bookkeeping: querying with a NULL act returns the current disposition in oldact;
// installing a handler then querying returns that handler; SIGKILL and SIGSTOP cannot be caught or
// ignored (EINVAL); the default disposition starts as SIG_DFL.
#include <errno.h>
#include <signal.h>
#include <stdio.h>

static void h(int s) { (void)s; }

int main(void) {
    struct sigaction old = {0};
    sigaction(SIGUSR1, NULL, &old);
    int starts_dfl = old.sa_handler == SIG_DFL;

    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);
    struct sigaction q = {0};
    sigaction(SIGUSR1, NULL, &q);
    int handler_reported = q.sa_handler == h;

    struct sigaction ign = {0};
    ign.sa_handler = SIG_IGN;
    sigemptyset(&ign.sa_mask);
    errno = 0;
    int k = sigaction(SIGKILL, &ign, NULL);
    int kill_einval = k == -1 && errno == EINVAL;
    errno = 0;
    int s = sigaction(SIGSTOP, &ign, NULL);
    int stop_einval = s == -1 && errno == EINVAL;

    printf("sigaction_query starts_dfl=%d handler_reported=%d kill_einval=%d stop_einval=%d\n",
           starts_dfl, handler_reported, kill_einval, stop_einval);
    return 0;
}
