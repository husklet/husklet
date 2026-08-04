// sigqueue carries an accompanying value; sigwaitinfo retrieves signal + si_value + si_code SI_QUEUE.
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGRTMIN);
    sigprocmask(SIG_BLOCK, &set, NULL);

    union sigval sv;
    sv.sival_int = 0x5a5a;
    sigqueue(getpid(), SIGRTMIN, sv);

    siginfo_t info;
    int sig = sigwaitinfo(&set, &info);
    int ok = sig == SIGRTMIN &&
             info.si_signo == SIGRTMIN &&
             info.si_value.sival_int == 0x5a5a &&
             info.si_code == SI_QUEUE;

    // Second queued signal retrieved via sigtimedwait.
    sv.sival_int = 7;
    sigqueue(getpid(), SIGRTMIN, sv);
    struct timespec to = {2, 0};
    int sig2 = sigtimedwait(&set, &info, &to);
    int ok2 = sig2 == SIGRTMIN && info.si_value.sival_int == 7;

    printf("sigqueuewait first=%d second=%d\n", ok, ok2);
    return 0;
}
