// Standard signals do not queue: raising a blocked standard signal N times delivers it once. RT
// signals queue: sigqueue-ing a blocked RT signal N times delivers it N times. Verify both while
// blocked, using sigpending to confirm the standard signal shows pending only once.
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static volatile sig_atomic_t std_count, rt_count;
static void hs(int s) { (void)s; std_count++; }
static void hr(int s, siginfo_t *si, void *u) { (void)s; (void)si; (void)u; rt_count++; }

int main(void) {
    struct sigaction ss = {0};
    ss.sa_handler = hs; sigemptyset(&ss.sa_mask);
    sigaction(SIGUSR1, &ss, NULL);

    struct sigaction sr = {0};
    sr.sa_sigaction = hr; sr.sa_flags = SA_SIGINFO; sigemptyset(&sr.sa_mask);
    sigaction(SIGRTMIN, &sr, NULL);

    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigaddset(&block, SIGRTMIN);
    sigprocmask(SIG_BLOCK, &block, &old);

    for (int i = 0; i < 5; i++) raise(SIGUSR1);          // coalesces
    union sigval v; v.sival_int = 0;
    for (int i = 0; i < 5; i++) sigqueue(getpid(), SIGRTMIN, v); // queues

    sigset_t pend;
    sigpending(&pend);
    int both_pending = sigismember(&pend, SIGUSR1) && sigismember(&pend, SIGRTMIN);

    sigprocmask(SIG_SETMASK, &old, NULL); // deliver

    printf("rt_vs_standard std_once=%d rt_all=%d pending_both=%d\n",
           std_count == 1, rt_count == 5, both_pending);
    return 0;
}
