// Writing to a pipe whose read end is closed raises SIGPIPE. With a handler installed, write()
// returns -1/EPIPE after the handler runs. If SIGPIPE is ignored, write still returns EPIPE and
// no handler runs. Fully deterministic, single-threaded.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static volatile sig_atomic_t pipe_sig;
static void h(int s) { (void)s; pipe_sig++; }

int main(void) {
    // Case 1: handler installed
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGPIPE, &sa, NULL);

    int p[2];
    if (pipe(p) != 0) { printf("sigpipe_pipe pipe_fail\n"); return 1; }
    close(p[0]);
    errno = 0;
    ssize_t r = write(p[1], "x", 1);
    int handled = r == -1 && errno == EPIPE && pipe_sig == 1;
    close(p[1]);

    // Case 2: SIGPIPE ignored
    signal(SIGPIPE, SIG_IGN);
    pipe_sig = 0;
    if (pipe(p) != 0) { printf("sigpipe_pipe pipe_fail2\n"); return 1; }
    close(p[0]);
    errno = 0;
    ssize_t r2 = write(p[1], "x", 1);
    int ignored = r2 == -1 && errno == EPIPE && pipe_sig == 0;
    close(p[1]);

    printf("sigpipe_pipe handled_epipe=%d ignored_epipe=%d\n", handled, ignored);
    return 0;
}
