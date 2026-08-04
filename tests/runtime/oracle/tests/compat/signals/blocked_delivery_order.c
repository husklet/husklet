// When several standard signals are pending simultaneously, Linux delivers them in a fixed
// kernel-determined order upon unblocking. Block a spread of signals, raise each twice, then
// unblock and record the raw order handlers run (a deterministic, ISA-neutral golden). Also
// confirm standard signals coalesce (each raised twice -> delivered once). Signal numbers
// (SIGUSR1=10, SIGUSR2=12, SIGTERM=15) are identical on aarch64 and x86_64.
#include <signal.h>
#include <stdio.h>

static int order[8];
static volatile sig_atomic_t idx;
static volatile sig_atomic_t counts[64];

static void h(int s) {
    if (idx < 8) order[idx++] = s;
    if (s < 64) counts[s]++;
}

int main(void) {
    int sigs[3] = {SIGUSR2, SIGUSR1, SIGTERM}; // note SIGUSR1 < SIGUSR2 < SIGTERM by number
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    for (int i = 0; i < 3; i++) sigaction(sigs[i], &sa, NULL);

    sigset_t block, old;
    sigemptyset(&block);
    for (int i = 0; i < 3; i++) sigaddset(&block, sigs[i]);
    sigprocmask(SIG_BLOCK, &block, &old);

    for (int i = 0; i < 3; i++) { raise(sigs[i]); raise(sigs[i]); } // twice each -> coalesce

    sigprocmask(SIG_SETMASK, &old, NULL);

    int coalesced = counts[SIGUSR1] == 1 && counts[SIGUSR2] == 1 && counts[SIGTERM] == 1;
    printf("blocked_order count=%d seq=", (int)idx);
    for (int i = 0; i < idx; i++) printf("%s%d", i ? "," : "", order[i]);
    printf(" coalesced=%d\n", coalesced);
    return 0;
}
