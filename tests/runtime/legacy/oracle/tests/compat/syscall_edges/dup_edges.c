// syscall-compat regression: dup/dup2/dup3 boundary semantics. dup2(fd,fd) with the SAME fd returns fd and
// does NOT close it or clear flags; dup3(fd,fd) with equal fds is EINVAL; dup3 with a bogus flag is EINVAL;
// dup2 to an explicit target closes any previous holder; dup of a bad fd is EBADF; dup3 O_CLOEXEC sets the
// flag on the new fd. Arch-neutral: errnos/booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/null", O_RDONLY);
    // dup2 same fd: returns fd, stays open.
    int r = dup2(fd, fd);
    printf("dup2_same=%d open=%d\n", r == fd, fcntl(fd, F_GETFD) != -1);
    // dup3 with equal oldfd==newfd -> EINVAL (unlike dup2).
    printf("dup3_same_errno=%d\n", dup3(fd, fd, 0) == -1 ? errno : 0);
    // dup3 with an invalid flag (only O_CLOEXEC allowed) -> EINVAL.
    printf("dup3_badflag_errno=%d\n", dup3(fd, 200, O_APPEND) == -1 ? errno : 0);
    // dup3 with O_CLOEXEC sets FD_CLOEXEC on the new descriptor.
    int d = dup3(fd, 201, O_CLOEXEC);
    printf("dup3_cloexec=%d\n", d == 201 && (fcntl(201, F_GETFD) & FD_CLOEXEC) != 0);
    // Plain dup does not set cloexec.
    int p = dup(fd);
    printf("dup_noclo=%d\n", (fcntl(p, F_GETFD) & FD_CLOEXEC) == 0);
    // dup of a bad fd -> EBADF.
    printf("dup_badfd_errno=%d\n", dup(4096) == -1 ? errno : 0);
    // dup2 with a negative newfd -> EBADF.
    printf("dup2_badnew_errno=%d\n", dup2(fd, -1) == -1 ? errno : 0);
    return 0;
}
