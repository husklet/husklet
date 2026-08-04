// syscall-compat regression: fcntl F_GETFL/F_SETFL/F_GETFD/F_SETFD/F_DUPFD/F_DUPFD_CLOEXEC must behave
// like Linux -- status flags round-trip, FD_CLOEXEC round-trips, F_DUPFD honors the minimum-fd floor and
// a negative floor is EINVAL, and F_GETFL on a bad fd is EBADF. Arch-neutral: only errnos/booleans printed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/null", O_RDONLY);
    int fl = fcntl(fd, F_GETFL);
    // access mode must read back as O_RDONLY(0); O_APPEND not set initially.
    printf("accmode_rdonly=%d append0=%d\n", (fl & O_ACCMODE) == O_RDONLY, (fl & O_APPEND) == 0);
    // Set O_APPEND via F_SETFL, read it back.
    fcntl(fd, F_SETFL, fl | O_APPEND);
    printf("append_set=%d\n", (fcntl(fd, F_GETFL) & O_APPEND) != 0);
    // FD flags: default not cloexec, set FD_CLOEXEC, read back.
    printf("cloexec0=%d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) == 0);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    printf("cloexec_set=%d\n", (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    // F_DUPFD with floor 100 must return a fd >= 100.
    int d = fcntl(fd, F_DUPFD, 100);
    printf("dupfd_floor=%d\n", d >= 100);
    // The plain F_DUPFD dup must NOT inherit FD_CLOEXEC.
    printf("dupfd_noclo=%d\n", (fcntl(d, F_GETFD) & FD_CLOEXEC) == 0);
    // F_DUPFD_CLOEXEC dup MUST have FD_CLOEXEC.
    int dc = fcntl(fd, F_DUPFD_CLOEXEC, 100);
    printf("dupfd_clo=%d\n", (fcntl(dc, F_GETFD) & FD_CLOEXEC) != 0);
    // Negative minimum fd -> EINVAL.
    printf("dupfd_neg_errno=%d\n", fcntl(fd, F_DUPFD, -1) == -1 ? errno : 0);
    // F_GETFL on a closed/bad fd -> EBADF(9).
    printf("getfl_badfd_errno=%d\n", fcntl(4096, F_GETFL) == -1 ? errno : 0);
    return 0;
}
