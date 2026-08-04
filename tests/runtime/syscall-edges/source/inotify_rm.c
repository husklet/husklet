// syscall-compat regression: inotify_rm_watch with a watch descriptor that is not a real watch must
// return EINVAL and leave unrelated fds open -- never close a victim fd whose number matches the wd.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/inotify.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    int ino = inotify_init1(0);
    int victim = dup(1); // an ordinary fd, NOT a watch of `ino`
    long r = syscall(SYS_inotify_rm_watch, ino, victim);
    printf("inotify_rm_errno=%d victim_open=%d\n", r == -1 ? errno : 0, fcntl(victim, F_GETFD) != -1);
    return 0;
}
