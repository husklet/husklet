// sigsuspend() must restore the EXACT caller mask on return (LTP sigsuspend01). With SIGALRM blocked,
// sigsuspend(empty) unblocks everything, an alarm(1)/SIGALRM is delivered and its handler runs, then
// sigsuspend returns -1/EINTR AND the pre-suspend mask (SIGALRM blocked) is restored. A regression that
// leaves the awaited signal unblocked in the restored mask is caught here. Portable POSIX -> golden verdict.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static volatile sig_atomic_t caught;
static void h(int s) { caught = s; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);

    // Pre-suspend mask blocks SIGALRM.
    sigset_t block, prev;
    sigemptyset(&block);
    sigaddset(&block, SIGALRM);
    sigprocmask(SIG_BLOCK, &block, &prev);

    sigset_t empty;
    sigemptyset(&empty);

    caught = 0;
    alarm(1);
    errno = 0;
    int rc = sigsuspend(&empty);      // unblocks SIGALRM, waits; handler runs; returns -1/EINTR
    alarm(0);
    int eintr = rc == -1 && errno == EINTR;
    int ran = caught == SIGALRM;

    // The mask in effect after sigsuspend must equal the pre-suspend mask (SIGALRM still blocked).
    sigset_t now;
    sigemptyset(&now);
    sigprocmask(SIG_BLOCK, NULL, &now);
    int restored = sigismember(&now, SIGALRM) == 1;

    printf("sigsuspend eintr=%d ran=%d restored=%d\n", eintr, ran, restored);
    return 0;
}
