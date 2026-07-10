// syscall-compat regression: a read() of a signalfd shorter than one struct signalfd_siginfo (128 bytes)
// must return EINVAL and leave the pending signal queued -- not consume it.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/signalfd.h>
#include <unistd.h>

int main(void) {
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int sfd = signalfd(-1, &m, 0);
    raise(SIGUSR1); // pending -> signalfd readable
    char small[64];
    ssize_t sr = read(sfd, small, sizeof small); // < 128 -> EINVAL, signal preserved
    int se = (sr == -1) ? errno : 0;
    char buf[128];
    ssize_t fr = read(sfd, buf, sizeof buf);
    unsigned signo = (fr >= 128) ? *(unsigned *)buf : 0;
    printf("signalfd short=%zd serr=%d full=%zd signo=%u\n", sr, se, fr, signo);
    return 0;
}
