// Realtime signal delivery ordering: several distinct RT signals plus repeated instances of one
// are queued while blocked, then unblocked together. The kernel dequeues distinct RT signals in a
// fixed priority order and preserves FIFO order among instances of a single signo. Values travel
// with SA_SIGINFO / SI_QUEUE. The exact delivered sequence is captured as a deterministic golden.
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define N 8
static int order[N];
static int vals[N];
static volatile sig_atomic_t idx;

static void h(int s, siginfo_t *si, void *u) {
    (void)u;
    if (idx < N) { order[idx] = s - SIGRTMIN; vals[idx] = si->si_value.sival_int; idx++; }
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    for (int i = 0; i < 3; i++) sigaction(SIGRTMIN + i, &sa, NULL);

    sigset_t block, old;
    sigemptyset(&block);
    for (int i = 0; i < 3; i++) sigaddset(&block, SIGRTMIN + i);
    sigprocmask(SIG_BLOCK, &block, &old);

    // queue out of numeric order; two instances of SIGRTMIN+1 with distinct values
    union sigval v;
    v.sival_int = 10; sigqueue(getpid(), SIGRTMIN + 2, v);
    v.sival_int = 20; sigqueue(getpid(), SIGRTMIN + 1, v);
    v.sival_int = 21; sigqueue(getpid(), SIGRTMIN + 1, v);
    v.sival_int = 30; sigqueue(getpid(), SIGRTMIN + 0, v);

    sigprocmask(SIG_SETMASK, &old, NULL); // deliver all

    // Kernel dequeues distinct RT signals in a fixed priority order; instances of one signo are
    // FIFO. Emit the raw delivered (signo-offset,value) sequence: a deterministic, ISA-neutral
    // kernel fact the engine must reproduce exactly. Values 20/21 must stay in queue order.
    int fifo_within = 1;
    printf("rt_order count=%d seq=", (int)idx);
    for (int i = 0; i < idx; i++) printf("%s+%d:%d", i ? "," : "", order[i], vals[i]);
    // locate the two +1 instances and confirm 20 precedes 21 (FIFO within a signo)
    int first1 = -1;
    for (int i = 0; i < idx; i++) if (order[i] == 1) { if (first1 < 0) first1 = vals[i]; else if (first1 != 20 || vals[i] != 21) fifo_within = 0; }
    printf(" fifo_within_signo=%d\n", fifo_within);
    return 0;
}
