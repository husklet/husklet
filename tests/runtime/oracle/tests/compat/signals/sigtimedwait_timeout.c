// sigtimedwait returns -1/EAGAIN when no signal arrives within the timeout, and returns the
// pending signal immediately when one is already queued (timeout not consulted).
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, &old);

    struct timespec ts = {0, 50 * 1000 * 1000}; // 50ms
    errno = 0;
    int r = sigtimedwait(&set, NULL, &ts);
    int timed_out = r == -1 && errno == EAGAIN;

    raise(SIGUSR1);
    siginfo_t si;
    int r2 = sigtimedwait(&set, &si, &ts);
    int got = r2 == SIGUSR1;

    printf("sigtimedwait eagain=%d immediate=%d\n", timed_out, got);
    return 0;
}
