// signal() returns the previous disposition: initially SIG_DFL, then the first handler when
// replaced. Installing a handler for SIGKILL fails with SIG_ERR. Restoring SIG_DFL works.
#include <errno.h>
#include <signal.h>
#include <stdio.h>

static void h1(int s) { (void)s; }
static void h2(int s) { (void)s; }

int main(void) {
    void (*p0)(int) = signal(SIGUSR1, h1);
    int was_dfl = p0 == SIG_DFL;
    void (*p1)(int) = signal(SIGUSR1, h2);
    int returned_h1 = p1 == h1;
    void (*p2)(int) = signal(SIGUSR1, SIG_DFL);
    int returned_h2 = p2 == h2;

    errno = 0;
    void (*pk)(int) = signal(SIGKILL, h1);
    int kill_err = pk == SIG_ERR && errno == EINVAL;

    printf("signal_returns_prev was_dfl=%d ret_h1=%d ret_h2=%d kill_err=%d\n",
           was_dfl, returned_h1, returned_h2, kill_err);
    return 0;
}
