// A single standard signal blocked and raised is delivered exactly once the moment it is
// unblocked, and not before. Verify no handler runs while blocked, then exactly one on unblock.
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    signal(SIGUSR1, h);
    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigprocmask(SIG_BLOCK, &block, &old);

    raise(SIGUSR1);
    int while_blocked = ran; // must be 0

    sigprocmask(SIG_SETMASK, &old, NULL);
    int after_unblock = ran; // must be 1

    printf("unblock_single while_blocked=%d after_unblock=%d\n", while_blocked, after_unblock);
    return 0;
}
