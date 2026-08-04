// sigprocmask bookkeeping: SIG_BLOCK returns the previous (empty) mask in oldset; a second query
// reflects the newly blocked signal; SIG_SETMASK replaces the whole mask; querying with a NULL set
// reports the current mask without changing it.
#include <signal.h>
#include <stdio.h>

int main(void) {
    sigset_t a, old;
    sigemptyset(&a);
    sigaddset(&a, SIGUSR1);
    sigemptyset(&old);
    sigprocmask(SIG_BLOCK, &a, &old);
    int old_empty = !sigismember(&old, SIGUSR1);

    sigset_t cur;
    sigprocmask(SIG_BLOCK, NULL, &cur); // query only
    int now_blocked = sigismember(&cur, SIGUSR1);

    sigset_t b, old2;
    sigemptyset(&b);
    sigaddset(&b, SIGUSR2);
    sigprocmask(SIG_SETMASK, &b, &old2); // replace: USR1 out, USR2 in
    int replaced_old_had_usr1 = sigismember(&old2, SIGUSR1);
    sigprocmask(SIG_BLOCK, NULL, &cur);
    int replaced = !sigismember(&cur, SIGUSR1) && sigismember(&cur, SIGUSR2);

    printf("sigprocmask_return old_empty=%d now_blocked=%d old2_usr1=%d replaced=%d\n",
           old_empty, now_blocked, replaced_old_had_usr1, replaced);
    return 0;
}
