// ext_sig/sigspin.c -- #292 regression: an async signal (interval timer) MUST preempt a completely
// syscall-free compute loop. This is the guard for the emitted block-entry irq poll -- and, since
// IRQSLIM, specifically for its cycle-poll invariant (forward direct chains skip the entry poll, so
// every loop must still reach a poll through a backward direct edge or an indirect entry). Two loop
// shapes, each spun until the timer fires (a lost preemption = hang -> harness timeout):
//   (1) a multi-block direct-branch loop: conditional forward branches inside the body + the one
//       backward loop edge (the poll survives on the back-edge);
//   (2) a computed-goto cycle whose only cycle-closing edges are INDIRECT `br` (no backward direct
//       branch at all -- the poll survives on the IBTC-hit entry).
// The handler only sets a flag; each loop exits via that flag, so termination itself proves delivery.
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/time.h>

static volatile sig_atomic_t fired;
static void h(int s) {
    (void)s;
    fired = 1;
}

int main(void) {
    signal(SIGALRM, h);
    struct itimerval it = {{0, 0}, {0, 50000}}; // one-shot, 50ms
    setitimer(ITIMER_REAL, &it, NULL);
    // shape 1: direct-branch loop, forward branches in the body, one backward edge
    uint64_t acc = 1;
    while (!fired) {
        acc = acc * 6364136223846793005ull + 1442695040888963407ull;
        if (acc & 1) acc ^= 0x9E3779B97F4A7C15ull;
        if (acc & 2) acc += 12345;
    }
    int ok1 = fired != 0;
    // shape 2: indirect-only cycle (computed goto); re-arm the timer first
    fired = 0;
    it.it_value.tv_usec = 50000;
    setitimer(ITIMER_REAL, &it, NULL);
    static const void *tab[2];
    tab[0] = &&L0;
    tab[1] = &&L1;
    goto *tab[acc & 1];
L0:
    acc = acc * 3 + 1;
    if (fired) goto done;
    goto *tab[acc & 1];
L1:
    acc = acc * 5 + 2;
    if (fired) goto done;
    goto *tab[acc & 1];
done:
    printf("sigspin loop1=%d loop2=%d\n", ok1, fired != 0);
    return 0;
}
