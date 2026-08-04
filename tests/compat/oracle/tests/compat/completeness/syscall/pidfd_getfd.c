/* pidfd_getfd: duplicate a file descriptor out of a target process referenced by a pidfd. We open a
   pidfd to OURSELVES and pull one of our own fds through it. On a capable kernel this either succeeds
   (returning a fresh fd onto the same open file) or fails EPERM under yama/ptrace restrictions; where
   unimplemented it is ENOSYS. A correct engine gives one of these — never a crash or bogus errno.
   Derived boolean verdict, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#include <fcntl.h>
#ifndef __NR_pidfd_open
#define __NR_pidfd_open 434
#endif
#ifndef __NR_pidfd_getfd
#define __NR_pidfd_getfd 438
#endif

int main(void) {
    int target = open("/dev/null", O_RDONLY);
    long pidfd = syscall(__NR_pidfd_open, getpid(), 0u);
    int open_ok = (pidfd >= 0) || (errno == ENOSYS);

    long dup = -1;
    int getfd_handled = 1;
    if (pidfd >= 0 && target >= 0) {
        dup = syscall(__NR_pidfd_getfd, (int)pidfd, target, 0u);
        getfd_handled = (dup >= 0) || (errno == EPERM || errno == ENOSYS || errno == EACCES);
    }
    if (dup >= 0) close((int)dup);
    if (pidfd >= 0) close((int)pidfd);
    if (target >= 0) close(target);

    printf("pidfd_open_handled=%d pidfd_getfd_handled=%d\n", open_ok, getfd_handled);
    return 0;
}
