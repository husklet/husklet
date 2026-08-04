// sigsetjmp(buf,1) saves the signal mask so siglongjmp restores it. A handler runs with the
// triggering signal masked; longjmp-ing out of the handler restores the pre-handler mask (SIGUSR1
// unblocked). Verify the mask is back to unblocked after returning via siglongjmp.
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>

static sigjmp_buf jb;
static volatile sig_atomic_t blocked_in_handler;

static void h(int s) {
    (void)s;
    sigset_t cur;
    sigprocmask(SIG_BLOCK, NULL, &cur);
    if (sigismember(&cur, SIGUSR1)) blocked_in_handler = 1; // auto-masked during handler
    siglongjmp(jb, 1); // restores saved mask (SIGUSR1 unblocked)
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    int done = 0;
    if (sigsetjmp(jb, 1) == 0) {
        raise(SIGUSR1);
    } else {
        done = 1;
    }
    sigset_t after;
    sigprocmask(SIG_BLOCK, NULL, &after);
    int restored = !sigismember(&after, SIGUSR1);
    printf("sigsetjmp_savemask blocked_in_handler=%d restored=%d done=%d\n",
           blocked_in_handler, restored, done);
    return 0;
}
