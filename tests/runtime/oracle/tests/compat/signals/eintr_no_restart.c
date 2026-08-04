// A handler installed WITHOUT SA_RESTART interrupts a blocking read() on an empty pipe: the read
// returns -1/EINTR after the handler runs. An itimer fires the signal deterministically.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    int p[2];
    if (pipe(p) != 0) { printf("eintr_no_restart pipe_fail\n"); return 1; }

    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0; // no SA_RESTART
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it = {{0, 0}, {0, 50 * 1000}}; // 50ms one-shot
    setitimer(ITIMER_REAL, &it, NULL);

    char buf[1];
    errno = 0;
    ssize_t r = read(p[0], buf, 1); // blocks; interrupted by SIGALRM
    int eintr = r == -1 && errno == EINTR;

    printf("eintr_no_restart handler_ran=%d eintr=%d\n", ran == 1, eintr);
    return 0;
}
