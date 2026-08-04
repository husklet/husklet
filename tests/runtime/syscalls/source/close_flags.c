// syscall-compat coverage: close_range(2). Closing a contiguous range closes every fd in it; the
// CLOSE_RANGE_CLOEXEC flag marks a range cloexec instead of closing it; first > last -> EINVAL; an
// unknown flag -> EINVAL. Arch-neutral: errnos/booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>
#ifndef __NR_close_range
#define __NR_close_range 436
#endif
#ifndef CLOSE_RANGE_CLOEXEC
#define CLOSE_RANGE_CLOEXEC 4
#endif

int main(void) {
    int a = dup2(open("/dev/null", O_RDONLY), 300);
    int b = dup2(open("/dev/null", O_RDONLY), 301);
    (void)a; (void)b;
    // Mark [302,303] cloexec (open two there first).
    dup2(open("/dev/null", O_RDONLY), 302);
    dup2(open("/dev/null", O_RDONLY), 303);
    long fc = syscall(__NR_close_range, 302u, 303u, CLOSE_RANGE_CLOEXEC);
    printf("cloexec_ok=%d flagged=%d\n", fc == 0, (fcntl(302, F_GETFD) & FD_CLOEXEC) != 0);

    // Close [300,301].
    long cc = syscall(__NR_close_range, 300u, 301u, 0);
    printf("closed_ok=%d gone=%d\n", cc == 0, fcntl(300, F_GETFD) == -1);

    // first > last -> EINVAL.
    printf("inverted_errno=%d\n", syscall(__NR_close_range, 400u, 300u, 0) == -1 ? errno : 0);
    // unknown flag -> EINVAL.
    printf("badflag_errno=%d\n", syscall(__NR_close_range, 302u, 303u, 8) == -1 ? errno : 0);
    return 0;
}
