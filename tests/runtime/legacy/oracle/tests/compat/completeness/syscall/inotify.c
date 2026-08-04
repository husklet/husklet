/* inotify filesystem-watch lifecycle. inotify_init1(IN_NONBLOCK|IN_CLOEXEC) must yield a fd; adding a
   watch on an existing directory returns a non-negative watch descriptor; removing it succeeds. A
   correct engine round-trips these, or reports ENOSYS if the whole subsystem is absent. Derived
   booleans (never the raw wd/fd numbers), arch-neutral and host-independent. */
#include "compat.h"
#include <stdio.h>
#include <sys/inotify.h>

int main(void) {
    int fd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (fd < 0 && errno == ENOSYS) { printf("inotify unsupported=1\n"); return 0; }
    int init_ok = fd >= 0;

    int wd = init_ok ? inotify_add_watch(fd, "/tmp", IN_CREATE | IN_DELETE | IN_MODIFY) : -1;
    int add_ok = wd >= 0;

    int rm_ok = (init_ok && add_ok) ? (inotify_rm_watch(fd, wd) == 0) : 0;

    /* removing a bogus watch descriptor must fail EINVAL, not succeed */
    int bad_rejected = init_ok ? (inotify_rm_watch(fd, 999999) == -1 && errno == EINVAL) : 0;

    if (fd >= 0) close(fd);
    printf("inotify init_ok=%d add_ok=%d rm_ok=%d bad_rejected=%d\n",
           init_ok, add_ok, rm_ok, bad_rejected);
    return 0;
}
