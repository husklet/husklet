// During a handler, the triggering signal plus any sa_mask signals are added to the thread's
// signal mask, and the previous mask is restored on return. Verify both signals appear blocked
// inside the handler and neither remains blocked afterward.
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t self_blocked, extra_blocked;

static void h(int s) {
    (void)s;
    sigset_t cur;
    sigprocmask(SIG_BLOCK, NULL, &cur);
    if (sigismember(&cur, SIGUSR1)) self_blocked = 1;    // triggering signal auto-masked
    if (sigismember(&cur, SIGUSR2)) extra_blocked = 1;   // sa_mask member
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaddset(&sa.sa_mask, SIGUSR2); // additionally mask SIGUSR2 during handler
    sa.sa_flags = 0;
    sigaction(SIGUSR1, &sa, NULL);

    raise(SIGUSR1);

    sigset_t after;
    sigprocmask(SIG_BLOCK, NULL, &after);
    int restored = !sigismember(&after, SIGUSR1) && !sigismember(&after, SIGUSR2);

    printf("mask_in_handler self_blocked=%d extra_blocked=%d restored=%d\n",
           self_blocked, extra_blocked, restored);
    return 0;
}
