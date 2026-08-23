// sigreturn's mask restore is the only thing that unblocks a pending signal.
//   * SIGUSR1's handler runs with SIGUSR2 added to the mask via sa_mask.
//   * The handler raises SIGUSR2, which must stay pending and blocked for the rest of the handler.
//   * Returning from the handler restores the entry mask, which unblocks SIGUSR2; it must be
//     delivered before the statement after raise(SIGUSR1) observes the counter.
// Every printed field is a normalized 0/1 verdict, byte-identical to the oracle on both Linux engines.
#include <signal.h>
#include <stdio.h>
#include <string.h>

static volatile sig_atomic_t got1, got2, got2_inside;

static void h2(int s) {
    (void)s;
    got2++;
}

static void h1(int s) {
    (void)s;
    got1++;
    raise(SIGUSR2);
    // Blocked by this handler's sa_mask, so it must not have run yet.
    got2_inside = got2;
}

int main(void) {
    struct sigaction a2;
    memset(&a2, 0, sizeof a2);
    a2.sa_handler = h2;
    sigemptyset(&a2.sa_mask);
    sigaction(SIGUSR2, &a2, NULL);

    struct sigaction a1;
    memset(&a1, 0, sizeof a1);
    a1.sa_handler = h1;
    sigemptyset(&a1.sa_mask);
    sigaddset(&a1.sa_mask, SIGUSR2);
    sigaction(SIGUSR1, &a1, NULL);

    raise(SIGUSR1);
    // sigreturn has already restored the entry mask, so SIGUSR2 must be delivered by now.
    int delivered = got2 == 1;

    // The pending SIGUSR2 must be gone, not merely late.
    sigset_t pend;
    sigemptyset(&pend);
    sigpending(&pend);
    int drained = !sigismember(&pend, SIGUSR2);

    printf("sigreturn-unblock handler=%d blocked_inside=%d delivered=%d drained=%d\n", got1 == 1, got2_inside == 0,
           delivered, drained);
    return 0;
}
