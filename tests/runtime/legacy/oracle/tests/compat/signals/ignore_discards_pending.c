// Setting a signal's disposition to SIG_IGN while it is blocked-and-pending discards the pending
// instance: after installing SIG_IGN and unblocking, no handler runs and nothing is pending.
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    signal(SIGUSR1, h);
    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigprocmask(SIG_BLOCK, &block, &old);

    raise(SIGUSR1); // pending
    sigset_t pend;
    sigpending(&pend);
    int was_pending = sigismember(&pend, SIGUSR1);

    signal(SIGUSR1, SIG_IGN); // discards the pending instance
    sigpending(&pend);
    int cleared = !sigismember(&pend, SIGUSR1);

    sigprocmask(SIG_SETMASK, &old, NULL);
    int no_handler = ran == 0;

    printf("ignore_discards was_pending=%d cleared=%d no_handler=%d\n",
           was_pending, cleared, no_handler);
    return 0;
}
