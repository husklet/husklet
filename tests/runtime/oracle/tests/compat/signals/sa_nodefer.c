// SA_NODEFER: the delivered signal is NOT auto-masked inside its own handler, so a re-raise
// from within nests immediately. Without SA_NODEFER the same signal is blocked during the handler.
// Deterministic single-thread verdicts, arch-neutral.
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t depth, max_depth, defer_blocked;

static void nodefer_h(int s) {
    depth++;
    if (depth > max_depth) max_depth = depth;
    if (depth < 3) raise(s); // re-enter immediately because not masked
    depth--;
}

static volatile sig_atomic_t defer_ran;
static void defer_h(int s) {
    defer_ran++;
    // inside a default (masked) handler the signal is blocked
    sigset_t cur;
    sigprocmask(SIG_BLOCK, NULL, &cur);
    if (sigismember(&cur, s)) defer_blocked = 1;
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = nodefer_h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_NODEFER;
    sigaction(SIGUSR1, &sa, NULL);
    raise(SIGUSR1);
    int nested = max_depth == 3;

    struct sigaction sb = {0};
    sb.sa_handler = defer_h;
    sigemptyset(&sb.sa_mask);
    sb.sa_flags = 0; // default: signal masked in handler
    sigaction(SIGUSR2, &sb, NULL);
    raise(SIGUSR2);

    printf("sa_nodefer nested=%d ran_once=%d defer_masked=%d\n",
           nested, defer_ran == 1, defer_blocked);
    return 0;
}
