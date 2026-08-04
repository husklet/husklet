// poll() is never restarted by SA_RESTART: an interrupting signal returns -1/EINTR even with the
// flag set. Deterministic via a one-shot itimer.
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    int p[2];
    if (pipe(p) != 0) { printf("eintr_poll pipe_fail\n"); return 1; }

    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART; // ignored by poll
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it = {{0, 0}, {0, 50 * 1000}};
    setitimer(ITIMER_REAL, &it, NULL);

    struct pollfd pfd = { .fd = p[0], .events = POLLIN };
    errno = 0;
    int r = poll(&pfd, 1, 5000); // blocks 5s; interrupted at 50ms
    int eintr = r == -1 && errno == EINTR;
    printf("eintr_poll handler=%d eintr=%d\n", ran == 1, eintr);
    return 0;
}
