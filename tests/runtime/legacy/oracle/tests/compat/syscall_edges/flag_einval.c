// syscall-compat regression: bad-flag / bad-fd arg validation must return the Linux errno, not a
// blanket success. Each line prints the raw errno so the native oracle self-defines the expectation.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
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

    // accept4: only SOCK_CLOEXEC(0x80000)/SOCK_NONBLOCK(0x800) are valid flags. A junk bit (0x1) -> EINVAL,
    // and Linux checks it BEFORE consulting the listen fd -- so it wins even on a nonblocking listener whose
    // accept would otherwise EAGAIN. A valid flag on that same nonblocking listener yields EAGAIN.
    int ls = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    struct sockaddr_in sa;
    __builtin_memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    (void)bind(ls, (struct sockaddr *)&sa, sizeof sa);
    (void)listen(ls, 1);
    printf("accept4_badflag_errno=%d\n", ec(SYS_accept4, ls, 0, 0, 0x1, 0));
    printf("accept4_goodflag_errno=%d\n", ec(SYS_accept4, ls, 0, 0, 0x800, 0));

    // socket/socketpair: a junk bit outside SOCK_TYPE_MASK(0xf)|SOCK_CLOEXEC(0x80000)|SOCK_NONBLOCK(0x800)
    // is EINVAL BEFORE the family is consulted -- the type must not silently mask down to SOCK_STREAM.
    int spair[2] = {-1, -1};
    long spr = syscall(SYS_socketpair, AF_UNIX, SOCK_STREAM | 0x10, 0, (long)spair);
    printf("socketpair_badtype_errno=%d socket_badtype_errno=%d\n",
           spr == -1 ? errno : 0, ec(SYS_socket, AF_UNIX, SOCK_STREAM | 0x10, 0, 0, 0));

    // process_vm_readv/writev: the 6th arg `flags` must be 0 -- no flags are defined, so any non-zero value is
    // EINVAL, and the kernel checks it before importing the iovecs (so even valid iovecs still fail). A valid
    // flags==0 call round-trips the bytes.
    char pvm_src[8] = "abcd", pvm_dst[8] = {0};
    struct iovec lv = {pvm_dst, 4}, rv = {pvm_src, 4};
    long pg = syscall(SYS_process_vm_readv, getpid(), (long)&lv, 1L, (long)&rv, 1L, 0L);
    long pb = syscall(SYS_process_vm_readv, getpid(), (long)&lv, 1L, (long)&rv, 1L, 1L);
    long pw = syscall(SYS_process_vm_writev, getpid(), (long)&lv, 1L, (long)&rv, 1L, 7L);
    printf("pvm_readv_ok=%d pvm_readv_badflag_errno=%d pvm_writev_badflag_errno=%d\n",
           pg == 4 && __builtin_memcmp(pvm_dst, "abcd", 4) == 0, pb == -1 ? errno : 0, pw == -1 ? errno : 0);
    return 0;
}
