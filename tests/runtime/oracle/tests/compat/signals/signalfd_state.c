// signalfd per-fd state: a blocked pending signal is readable, siginfo fields survive the
// transfer, updating the mask with signalfd(fd,...) retargets the same descriptor, a signal
// consumed through signalfd is no longer pending, and standard signals do not queue.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/signalfd.h>
#include <unistd.h>

int main(void) {
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int fd = signalfd(-1, &m, SFD_NONBLOCK);
    struct signalfd_siginfo si;
    ssize_t r0 = read(fd, &si, sizeof si);
    int e0 = (r0 == -1) ? errno : 0;

    raise(SIGUSR1);
    raise(SIGUSR1); // standard signals collapse
    ssize_t r1 = read(fd, &si, sizeof si);
    int signo = (int)si.ssi_signo;
    int code = si.ssi_code;
    ssize_t r2 = read(fd, &si, sizeof si);
    int e2 = (r2 == -1) ? errno : 0;

    sigset_t pend;
    sigpending(&pend);
    int stillpend = sigismember(&pend, SIGUSR1);

    // retarget the same fd to SIGUSR2 only
    sigset_t m2;
    sigemptyset(&m2);
    sigaddset(&m2, SIGUSR2);
    sigprocmask(SIG_BLOCK, &m2, NULL);
    int fd2 = signalfd(fd, &m2, 0);
    raise(SIGUSR1);
    raise(SIGUSR2);
    ssize_t r3 = read(fd, &si, sizeof si);
    int signo3 = (int)si.ssi_signo;
    sigpending(&pend);
    int usr1pend = sigismember(&pend, SIGUSR1);
    // short read of less than one record is EINVAL
    char small[4];
    ssize_t r4 = read(fd, small, sizeof small);
    int e4 = (r4 == -1) ? errno : 0;
    printf("r0=%zd e0=%d r1=%zd signo=%d code=%d r2=%zd e2=%d stillpend=%d same=%d r3=%zd signo3=%d usr1pend=%d r4=%zd e4=%d\n",
           r0, e0, r1, signo, code, r2, e2, stillpend, fd2 == fd, r3, signo3, usr1pend, r4, e4);
    return 0;
}
