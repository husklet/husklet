// pidfd_getfd(2): duplicate a descriptor out of the process a pidfd refers to. A container manager or
// debugger pulls a live fd (e.g. a listening socket) out of a child this way. Here we pidfd_open our own
// pid and pull one of our own fds through it: on this kernel (same-user, permissive ptrace scope) that
// succeeds and yields a fresh fd onto the SAME open file, which we prove by reading back a byte written
// through the original. flags must be 0 (any other value -> EINVAL) and a non-pidfd argument -> EBADF.
// Derived booleans, deterministic, arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_pidfd_open
#define SYS_pidfd_open 434
#endif
#ifndef SYS_pidfd_getfd
#define SYS_pidfd_getfd 438
#endif

int main(void) {
    // A pipe gives us a real fd whose duplicate we can prove points at the same open file description.
    int pp[2];
    if (pipe(pp) != 0) { printf("pidfd_getfd pipe_fail\n"); return 1; }

    long pidfd = syscall(SYS_pidfd_open, getpid(), 0u);
    int opened = pidfd >= 0;

    // Bad flags -> EINVAL, before any duplication.
    long bad = syscall(SYS_pidfd_getfd, (int)pidfd, pp[0], 1u);
    int badflags_einval = (bad < 0 && errno == EINVAL);

    // A descriptor that is not one of our pidfds -> EBADF.
    long ebadf = syscall(SYS_pidfd_getfd, pp[1], pp[0], 0u);
    int nonpidfd_ebadf = (ebadf < 0 && errno == EBADF);

    // Duplicate the read end of the pipe out of "ourselves".
    long dup = syscall(SYS_pidfd_getfd, (int)pidfd, pp[0], 0u);
    int got = dup >= 0;

    // The duplicate must share the pipe: a byte written to pp[1] is readable from the duplicate.
    int same_file = 0;
    if (got) {
        char w = 'Z', r = 0;
        if (write(pp[1], &w, 1) == 1 && read((int)dup, &r, 1) == 1 && r == 'Z') same_file = 1;
        close((int)dup);
    }

    if (pidfd >= 0) close((int)pidfd);
    close(pp[0]);
    close(pp[1]);

    printf("pidfd_getfd opened=%d badflags_einval=%d nonpidfd_ebadf=%d got=%d same_file=%d\n",
           opened, badflags_einval, nonpidfd_ebadf, got, same_file);
    return 0;
}
