// sigsuspend always returns -1/EINTR after running a handler, and it atomically installs then
// restores the previous signal mask. We block SIGUSR1, arm an itimer for SIGALRM, and suspend
// with a mask that unblocks SIGALRM only; after return SIGUSR1 must still be blocked.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/time.h>

static volatile sig_atomic_t alrm, usr1;
static void ha(int s) { (void)s; alrm++; }
static void hu(int s) { (void)s; usr1++; }

int main(void) {
    struct sigaction sa = {0};
    sigemptyset(&sa.sa_mask);
    sa.sa_handler = ha; sigaction(SIGALRM, &sa, NULL);
    sa.sa_handler = hu; sigaction(SIGUSR1, &sa, NULL);

    sigset_t block;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigaddset(&block, SIGALRM);
    sigprocmask(SIG_BLOCK, &block, NULL);

    struct itimerval it = {{0, 0}, {0, 50 * 1000}};
    setitimer(ITIMER_REAL, &it, NULL);

    sigset_t suspmask; // everything blocked except SIGALRM
    sigfillset(&suspmask);
    sigdelset(&suspmask, SIGALRM);
    errno = 0;
    int r = sigsuspend(&suspmask);
    int eintr = r == -1 && errno == EINTR;

    sigset_t cur;
    sigprocmask(SIG_BLOCK, NULL, &cur);
    int mask_restored = sigismember(&cur, SIGUSR1) && sigismember(&cur, SIGALRM);
    int only_alrm = alrm == 1 && usr1 == 0;

    printf("eintr_sigsuspend eintr=%d only_alrm=%d mask_restored=%d\n",
           eintr, only_alrm, mask_restored);
    return 0;
}
