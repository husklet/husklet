// syscall-compat regression: bad-flag / bad-fd arg validation must return the Linux errno, not a
// blanket success. Each line prints the raw errno so the native oracle self-defines the expectation.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>
#ifndef __NR_close_range
#define __NR_close_range 436
#endif
#ifndef __NR_pidfd_open
#define __NR_pidfd_open 434
#endif

static int ec(long nr, long a, long b, long c, long d, long e) {
    long r = syscall(nr, a, b, c, d, e);
    return r == -1 ? errno : 0;
}

int main(void) {
    // close_range: only CLOSE_RANGE_UNSHARE(2)/CLOSE_RANGE_CLOEXEC(4) are valid; bit 8 -> EINVAL, fd stays open.
    int fd = open("/dev/null", O_RDONLY);
    long r = syscall(__NR_close_range, (unsigned)fd, (unsigned)fd, 8);
    printf("close_range_badflag_errno=%d open=%d\n", r == -1 ? errno : 0, fcntl(fd, F_GETFD) != -1);

    // pidfd_open: only PIDFD_NONBLOCK(0x800); flag 1 -> EINVAL.
    printf("pidfd_open_badflag_errno=%d\n", ec(__NR_pidfd_open, getpid(), 1, 0, 0, 0));

    // unshare: bit 0x1 is not a valid unshare flag -> EINVAL.
    printf("unshare_badflag_errno=%d\n", ec(SYS_unshare, 0x1, 0, 0, 0, 0));

    // setns: a negative fd -> EBADF.
    printf("setns_badfd_errno=%d\n", ec(SYS_setns, -1, 0, 0, 0, 0));

    // inotify_init1: only IN_NONBLOCK(0x800)/IN_CLOEXEC(0x80000); bit 0x4 -> EINVAL.
    printf("inotify_init1_badflag_errno=%d\n", ec(SYS_inotify_init1, 0x4, 0, 0, 0, 0));

    // epoll_pwait: maxevents <= 0 -> EINVAL (must not clamp to a poll).
    int ep = syscall(SYS_epoll_create1, 0);
    char evbuf[64];
    long z = syscall(SYS_epoll_pwait, ep, (long)evbuf, 0, 0, (long)0, 8);
    long n = syscall(SYS_epoll_pwait, ep, (long)evbuf, -1, 0, (long)0, 8);
    printf("epoll_pwait_zero_errno=%d neg_errno=%d\n", z == -1 ? errno : 0, n == -1 ? errno : 0);

    // signalfd4: sizemask must equal sizeof(sigset_t)=8; sizemask 0 -> EINVAL.
    unsigned long mask = 0;
    printf("signalfd4_badsize_errno=%d\n", ec(SYS_signalfd4, -1, (long)&mask, 0, 0, 0));
    return 0;
}
