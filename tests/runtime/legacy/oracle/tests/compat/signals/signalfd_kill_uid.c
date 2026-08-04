// A SI_USER signal (plain kill/raise) delivered to a signalfd -- and dequeued via sigwaitinfo -- carries
// the sender's credentials: ssi_code == SI_USER(0), ssi_pid == the sending process, and ssi_uid == the
// sender's REAL uid (getuid()). The engine previously stamped ssi_uid/si_uid as a hardcoded 0, so an
// event loop / sigwaitinfo consumer saw uid 0 where the kernel reports the real sender uid. Assertions
// are uid-portable (compared against getuid()) so the golden is stable across hosts.
#include <signal.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/signalfd.h>
#include <unistd.h>

int main(void) {
    uint32_t me = (uint32_t)getuid();
    pid_t self = getpid();

    // ---- signalfd path: block SIGUSR1, kill self, read the struct signalfd_siginfo ----
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, 0);
    kill(self, SIGUSR1);
    struct signalfd_siginfo si;
    memset(&si, 0, sizeof si);
    ssize_t n = read(fd, &si, sizeof si);
    int sfd_ok = n == (ssize_t)sizeof si && si.ssi_signo == (uint32_t)SIGUSR1 && si.ssi_code == 0 &&
                 si.ssi_pid == (uint32_t)self && si.ssi_uid == me;

    // ---- sigwaitinfo path: block SIGUSR2, raise, dequeue with siginfo ----
    sigset_t m2;
    sigemptyset(&m2);
    sigaddset(&m2, SIGUSR2);
    sigprocmask(SIG_BLOCK, &m2, NULL);
    raise(SIGUSR2);
    siginfo_t wi;
    memset(&wi, 0, sizeof wi);
    int w = sigwaitinfo(&m2, &wi);
    int wait_ok = w == SIGUSR2 && wi.si_code == SI_USER && wi.si_pid == self && wi.si_uid == (uid_t)me;

    printf("signalfd_kill_uid sfd_ok=%d wait_ok=%d\n", sfd_ok, wait_ok);
    return 0;
}
