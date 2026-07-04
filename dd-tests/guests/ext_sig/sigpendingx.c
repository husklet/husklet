// sigpending / rt_sigpending fidelity (LTP sigpending02) — Linux syscalls, diffed vs the native oracle.
//   * With SIGUSR1+SIGUSR2 blocked and raised, sigpending reports EXACTLY those two, and no handler runs
//     until they are unblocked (then each runs once).
//   * A bad set pointer (a PROT_NONE guard page) is -1/EFAULT.
//   * The raw rt_sigpending syscall agrees with the libc wrapper.
// Every printed field is a normalized 0/1 verdict, byte-identical to the oracle on both Linux engines.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile sig_atomic_t got1, got2;
static void h1(int s) { (void)s; got1++; }
static void h2(int s) { (void)s; got2++; }

int main(void) {
    signal(SIGUSR1, h1);
    signal(SIGUSR2, h2);

    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigaddset(&block, SIGUSR2);
    sigprocmask(SIG_BLOCK, &block, &old);

    // Nothing pending yet.
    sigset_t pend;
    sigemptyset(&pend);
    sigpending(&pend);
    int empty0 = !sigismember(&pend, SIGUSR1) && !sigismember(&pend, SIGUSR2);

    raise(SIGUSR1);
    int no_handler_yet1 = got1 == 0;
    sigemptyset(&pend);
    sigpending(&pend);
    int only_usr1 = sigismember(&pend, SIGUSR1) && !sigismember(&pend, SIGUSR2);

    raise(SIGUSR2);
    sigemptyset(&pend);
    sigpending(&pend);
    int both = sigismember(&pend, SIGUSR1) && sigismember(&pend, SIGUSR2);

    // Raw rt_sigpending agrees.
    sigset_t raw;
    sigemptyset(&raw);
    syscall(SYS_rt_sigpending, &raw, (size_t)(_NSIG / 8));
    int raw_both = sigismember(&raw, SIGUSR1) && sigismember(&raw, SIGUSR2);

    // EFAULT on a bad set pointer.
    void *bad = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    errno = 0;
    long r = syscall(SYS_rt_sigpending, bad, (size_t)(_NSIG / 8));
    int efault = r == -1 && errno == EFAULT;

    // Unblock -> both handlers run exactly once.
    sigprocmask(SIG_SETMASK, &old, NULL);
    int delivered = got1 == 1 && got2 == 1;

    printf("sigpending empty0=%d no_handler=%d only_usr1=%d both=%d raw_both=%d efault=%d delivered=%d\n",
           empty0, no_handler_yet1, only_usr1, both, raw_both, efault, delivered);
    return 0;
}
