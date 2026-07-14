// Two INDEPENDENT signalfds: one for SIGUSR1, one for SIGUSR2. Linux gives each its own mask + delivery
// queue, so raising SIGUSR1 must be readable on the USR1 fd only and the USR2 fd stays empty (EAGAIN).
// hl's old single-shared-pipe model aliased them (same fd, ORed masks) and failed this. Deterministic ->
// oracle-checked against native Linux.
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <sys/signalfd.h>
#include <unistd.h>

int main(void) {
    sigset_t m1, m2, both;
    sigemptyset(&m1);
    sigaddset(&m1, SIGUSR1);
    sigemptyset(&m2);
    sigaddset(&m2, SIGUSR2);
    sigemptyset(&both);
    sigaddset(&both, SIGUSR1);
    sigaddset(&both, SIGUSR2);
    sigprocmask(SIG_BLOCK, &both, NULL);

    int f1 = signalfd(-1, &m1, SFD_NONBLOCK);
    int f2 = signalfd(-1, &m2, SFD_NONBLOCK);
    if (f1 < 0 || f2 < 0) { perror("signalfd"); return 1; }

    raise(SIGUSR1); // only the USR1 signalfd should become readable

    struct signalfd_siginfo si;
    ssize_t n1 = read(f1, &si, sizeof si);
    int s1_usr1 = (n1 == (ssize_t)sizeof si && si.ssi_signo == SIGUSR1);

    struct signalfd_siginfo si2;
    ssize_t n2 = read(f2, &si2, sizeof si2); // USR2 not raised -> EAGAIN
    int s2_eagain = (n2 < 0);

    // A second, independent read of the USR1 fd now yields EAGAIN (the one queued signal was consumed).
    ssize_t n1b = read(f1, &si, sizeof si);
    int s1_drained = (n1b < 0);

    printf("distinct=%d s1_usr1=%d s2_eagain=%d s1_drained=%d\n", f1 != f2, s1_usr1, s2_eagain, s1_drained);
    close(f1);
    close(f2);
    return 0;
}
