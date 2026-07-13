// sigaction/rt_sigaction edge semantics — all portable POSIX, golden on every engine.
//   * SIGKILL / SIGSTOP can never have their disposition changed: sigaction() fails EINVAL and, crucially,
//     a "handler" installed for SIGKILL must NOT prevent the process being killed (LTP signal01/signal02).
//   * An out-of-range signo (>= _NSIG) fails EINVAL.
//   * SA_RESETHAND resets the disposition to SIG_DFL after the first delivery (the 2nd raise terminates).
//   * SA_NODEFER lets a signal recurse into its own handler (not auto-blocked during delivery).
//   * oldact reports the previously installed handler.
// Every printed field is a normalized 0/1 verdict, so the output is byte-identical across arches/engines.
#include <errno.h>
#include <signal.h>
#ifndef _NSIG
#define _NSIG 65   /* macOS libc omits _NSIG; Linux value for the guest range check */
#endif
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t hits, depth, max_depth;

static void h(int s) { (void)s; hits++; }

static void nodefer_h(int s) {
    depth++;
    if (depth > max_depth) max_depth = depth;
    if (depth < 3) raise(s); // SA_NODEFER: this re-entry must nest, not deadlock/block
    depth--;
}

// SIGKILL cannot be caught: a child that "installs" a SIGKILL handler and pauses must still die BY SIGKILL.
static int sigkill_uncatchable(void) {
    pid_t pid = fork();
    if (pid == 0) {
        signal(SIGKILL, h); // must fail; even if it "succeeded" the kill below must still terminate us
        pause();
        _exit(7); // must never be reached
    }
    usleep(120000);
    kill(pid, SIGKILL);
    int st;
    waitpid(pid, &st, 0);
    return WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL;
}

int main(void) {
    // 1) EINVAL for the uncatchable signals + an out-of-range signo.
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    errno = 0;
    int kill_einval = sigaction(SIGKILL, &sa, NULL) == -1 && errno == EINVAL;
    errno = 0;
    int stop_einval = sigaction(SIGSTOP, &sa, NULL) == -1 && errno == EINVAL;
    errno = 0;
    int range_einval = sigaction(_NSIG + 1, &sa, NULL) == -1 && errno == EINVAL;

    // 2) SIGKILL stays uncatchable regardless of the failed install.
    int kill_kills = sigkill_uncatchable();

    // 3) oldact reports the handler we just installed for SIGUSR1.
    struct sigaction old;
    sigaction(SIGUSR1, &sa, NULL);
    sigaction(SIGUSR1, NULL, &old);
    int old_reports = old.sa_handler == h;

    // 4) SA_RESETHAND: after one delivery the disposition reverts to SIG_DFL.
    struct sigaction rr;
    memset(&rr, 0, sizeof rr);
    rr.sa_handler = h;
    rr.sa_flags = SA_RESETHAND;
    sigemptyset(&rr.sa_mask);
    sigaction(SIGUSR2, &rr, NULL);
    hits = 0;
    raise(SIGUSR2);            // handler runs once, then disposition -> SIG_DFL
    sigaction(SIGUSR2, NULL, &old);
    int reset_ran = hits == 1;
    int reset_to_dfl = old.sa_handler == SIG_DFL;

    // 5) SA_NODEFER: the signal is deliverable inside its own handler (recursion nests to depth 3).
    struct sigaction nd;
    memset(&nd, 0, sizeof nd);
    nd.sa_handler = nodefer_h;
    nd.sa_flags = SA_NODEFER;
    sigemptyset(&nd.sa_mask);
    sigaction(SIGUSR1, &nd, NULL);
    depth = 0;
    max_depth = 0;
    raise(SIGUSR1);
    int nodefer_nested = max_depth == 3;

    printf("sigact kill_einval=%d stop_einval=%d range_einval=%d kill_kills=%d old_reports=%d "
           "reset_ran=%d reset_dfl=%d nodefer_nested=%d\n",
           kill_einval, stop_einval, range_einval, kill_kills, old_reports, reset_ran, reset_to_dfl,
           nodefer_nested);
    return 0;
}
