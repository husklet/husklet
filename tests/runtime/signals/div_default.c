// signals-compat regression: an x86 integer divide fault (#DE) with NO SIGFPE handler installed must
// terminate the process by SIGFPE (WIFSIGNALED, WTERMSIG==SIGFPE), exactly as Linux does. The x86 engine
// used to encode the fault as a plain exit status 136 (WIFEXITED, 128+SIGFPE) instead of a real
// signal-death, because the dispatcher's #DE default-disposition path set an exit code rather than
// recording death-by-signal / raising the host signal like the SIGBUS and aarch64 fault paths do.
//
// Coverage (each fault in its own child so the raw wait status is deterministic):
//   - divide-by-zero, default disposition -> terminated by SIGFPE,
//   - INT_MIN / -1 overflow, default disposition -> terminated by SIGFPE,
//   - divide-by-zero with a handler -> observed as SIGFPE with si_code FPE_INTDIV,
//   - INT_MIN / -1 overflow with a handler -> observed as SIGFPE with si_code FPE_INTDIV.
// (Linux/x86 reports FPE_INTDIV for the #DE trap in BOTH cases -- it does not distinguish the overflow
// sub-cause -- matching the qemu-x86_64 oracle.) Arch-neutral output.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    // 110 = SIGFPE + FPE_INTDIV (correct), 111 otherwise.
    _exit((sig == SIGFPE && si->si_code == FPE_INTDIV) ? 110 : 111);
}

// Defeat constant folding: the operands come through volatiles the compiler cannot prove.
static int div_by(int num, int den) {
    volatile int a = num, b = den;
    return a / b;
}

static int run_fault(int num, int den, int with_handler) {
    pid_t p = fork();
    if (p == 0) {
        if (with_handler) {
            struct sigaction sa = {0};
            sa.sa_sigaction = handler;
            sa.sa_flags = SA_SIGINFO;
            sigaction(SIGFPE, &sa, NULL);
        } else {
            signal(SIGFPE, SIG_DFL);
        }
        volatile int r = div_by(num, den);
        (void)r;
        _exit(3); // reached only if no fault fired
    }
    int st = 0;
    waitpid(p, &st, 0);
    return st;
}

int main(void) {
    int imin = -2147483647 - 1;

    int s1 = run_fault(1, 0, 0);
    printf("div0_default: is_sigfpe=%d\n", WIFSIGNALED(s1) && WTERMSIG(s1) == SIGFPE);

    int s2 = run_fault(imin, -1, 0);
    printf("ovf_default: is_sigfpe=%d\n", WIFSIGNALED(s2) && WTERMSIG(s2) == SIGFPE);

    int s3 = run_fault(1, 0, 1);
    printf("div0_handler: is_intdiv=%d\n", WIFEXITED(s3) && WEXITSTATUS(s3) == 110);

    int s4 = run_fault(imin, -1, 1);
    printf("ovf_handler: is_intdiv=%d\n", WIFEXITED(s4) && WEXITSTATUS(s4) == 110);
    return 0;
}
