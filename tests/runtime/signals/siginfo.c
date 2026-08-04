// SA_SIGINFO: the handler receives a siginfo_t whose si_signo/si_code/si_pid identify the sender.
// A child kill()s the parent, so si_code==SI_USER and si_pid==child. Portable POSIX -> golden verdict.
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t signo = 0, code_user = 0, pid_match = 0;
static volatile pid_t sender = 0;

static void h(int s, siginfo_t *si, void *uc) {
    (void)uc;
    signo = (si->si_signo == s);
    code_user = (si->si_code == SI_USER);
    pid_match = (si->si_pid == sender);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigprocmask(SIG_BLOCK, &block, &old);

    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) { usleep(50000); kill(parent, SIGUSR1); _exit(0); }
    sender = pid;
    sigprocmask(SIG_SETMASK, &old, NULL);
    while (!signo) usleep(1000);
    waitpid(pid, NULL, 0);
    printf("siginfo signo=%d code_user=%d pid_match=%d\n", (int)signo, (int)code_user, (int)pid_match);
    return 0;
}
