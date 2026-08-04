// SIGCHLD via SA_SIGINFO exposes how the child ended: si_code CLD_EXITED with si_status == exit
// code, and CLD_KILLED with si_status == terminating signal. Deterministic with one child per case.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t code, status, seen;
static void h(int s, siginfo_t *si, void *u) {
    (void)s; (void)u;
    code = si->si_code;
    status = si->si_status;
    seen++;
}

static void wait_gone(pid_t pid) {
    while (seen == 0) usleep(1000);
    waitpid(pid, NULL, 0);
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGCHLD, &sa, NULL);

    // Case A: normal exit with code 5
    seen = 0;
    pid_t a = fork();
    if (a == 0) _exit(5);
    wait_gone(a);
    int exited = code == CLD_EXITED && status == 5;

    // Case B: killed by SIGKILL
    seen = 0;
    pid_t b = fork();
    if (b == 0) { for (;;) pause(); }
    usleep(30 * 1000);
    kill(b, SIGKILL);
    wait_gone(b);
    int killed = code == CLD_KILLED && status == SIGKILL;

    printf("sigchld_siginfo exited_code=%d killed_sig=%d\n", exited, killed);
    return 0;
}
