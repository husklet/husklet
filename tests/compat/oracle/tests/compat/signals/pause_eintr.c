// pause() always returns -1/EINTR once a caught signal's handler has run. Deterministic via a
// one-shot itimer delivering SIGALRM.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it = {{0, 0}, {0, 50 * 1000}};
    setitimer(ITIMER_REAL, &it, NULL);

    errno = 0;
    int r = pause();
    int eintr = r == -1 && errno == EINTR;
    printf("pause_eintr handler=%d eintr=%d\n", ran == 1, eintr);
    return 0;
}
