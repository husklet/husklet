// syscall-compat regression: a read() of an inotify fd too small for one struct inotify_event (16-byte
// header) must return EINVAL and leave the queued event -- not consume it and return 0.
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/inotify.h>
#include <unistd.h>

int main(void) {
    int ino = inotify_init1(0);
    char path[] = "/tmp/sc_ino_XXXXXX";
    int tf = mkstemp(path);
    inotify_add_watch(ino, path, IN_MODIFY);
    if (write(tf, "x", 1) < 0) { /* trigger IN_MODIFY */
    }
    struct pollfd pf = {ino, POLLIN, 0};
    poll(&pf, 1, 1000);
    char small[8];
    ssize_t sr = read(ino, small, sizeof small); // < 16 -> EINVAL, event preserved
    int se = (sr == -1) ? errno : 0;
    char buf[256];
    ssize_t fr = read(ino, buf, sizeof buf);
    printf("inotify short=%zd serr=%d full=%zd\n", sr, se, fr >= 16 ? 16 : fr);
    unlink(path);
    return 0;
}
