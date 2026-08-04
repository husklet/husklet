// sigqueue delivers a value with si_code == SI_QUEUE and correct si_pid; a plain kill() yields
// SI_USER. SA_SIGINFO exposes both. Arch-neutral: only normalized verdicts printed.
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static volatile sig_atomic_t q_code, q_val, u_code, u_pid_ok;
static pid_t me;

static void h(int s, siginfo_t *si, void *u) {
    (void)s; (void)u;
    if (si->si_code == SI_QUEUE) { q_code = 1; q_val = si->si_value.sival_int; }
    else if (si->si_code == SI_USER) { u_code = 1; if (si->si_pid == me) u_pid_ok = 1; }
}

int main(void) {
    me = getpid();
    struct sigaction sa = {0};
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    union sigval v; v.sival_int = 12345;
    sigqueue(me, SIGUSR1, v);
    kill(me, SIGUSR1);

    printf("sigqueue si_queue=%d val_ok=%d si_user=%d pid_ok=%d\n",
           q_code, q_val == 12345, u_code, u_pid_ok);
    return 0;
}
