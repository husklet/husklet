// SA_RESTART semantics: a blocking read() interrupted by a signal is auto-restarted when the handler
// was installed with SA_RESTART, but returns EINTR when it was not. A child delivers the data ~250ms
// after the ~80ms timer signal, so the two outcomes are unambiguous.
// Portable POSIX -> golden verdict on every engine.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t hits = 0;
static void h(int s) { (void)s; hits++; }

// One trial: read 1 byte from a pipe a child fills at +250ms, with a +80ms SIGALRM in between.
// Returns the read()'s result (1 on restart-and-complete, -1 on EINTR).
static int trial(int restart, int *saw_eintr) {
    int p[2];
    pipe(p);
    pid_t pid = fork();
    if (pid == 0) {
        close(p[0]);
        usleep(250000);
        write(p[1], "z", 1);
        close(p[1]);
        _exit(0);
    }
    close(p[1]);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = restart ? SA_RESTART : 0;
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 80000;      // 80 ms
    setitimer(ITIMER_REAL, &it, NULL);

    char c = 0;
    errno = 0;
    int rc = read(p[0], &c, 1);
    *saw_eintr = (rc == -1 && errno == EINTR);
    if (rc == -1) { /* drain the late byte so the child's write doesn't linger */ read(p[0], &c, 1); }
    close(p[0]);
    waitpid(pid, NULL, 0);
    return rc;
}

int main(void) {
    int e1 = 0, e2 = 0;
    hits = 0;
    int r_restart = trial(1, &e1);          // SA_RESTART: should complete with 1 byte
    int r_eintr = trial(0, &e2);            // no SA_RESTART: should be EINTR
    int restarted = r_restart == 1 && !e1;
    int eintr = r_eintr == -1 && e2;
    int handler_ran = hits == 2;
    printf("sarestart restarted=%d eintr=%d handler=%d\n", restarted, eintr, handler_ran);
    return 0;
}
