// Regression: an inotify descriptor must reserve its guest fd number in the host
// fd space too. The watch object lives only in the typed box table, so if the
// number is not held by a real host descriptor the next ordinary (absolute-path)
// open is handed the identical number by the kernel and clobbers the watch --
// read/poll/select then fail with EBADF. A real file watcher always opens other
// files after arming its watches, so this must stay a distinct, live descriptor.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128], other[128], created[192];
    snprintf(dir, sizeof dir, "/tmp/hl_ifr_dir_%d", (int)getpid());
    snprintf(other, sizeof other, "/tmp/hl_ifr_other_%d", (int)getpid());
    mkdir(dir, 0700);

    int in = inotify_init1(IN_NONBLOCK);
    int wd = inotify_add_watch(in, dir, IN_CREATE | IN_MODIFY);
    if (in < 0 || wd < 0) return 10;

    // Open an unrelated file by absolute path (the non-bound open that used to
    // reuse the inotify fd number). On a correct kernel this is a distinct fd.
    int other_fd = open(other, O_CREAT | O_RDWR, 0644);
    int distinct = (other_fd >= 0 && other_fd != in);
    if (other_fd >= 0) close(other_fd);
    unlink(other);

    // Now generate a real event inside the watched directory and read it back.
    snprintf(created, sizeof created, "%s/a", dir);
    int cf = open(created, O_CREAT | O_WRONLY, 0644);
    if (cf >= 0) {
        if (write(cf, "z", 1) != 1) { /* ignore */ }
        close(cf);
    }

    struct pollfd p = {in, POLLIN, 0};
    int saw_create = 0, event_ok = 0;
    char buf[4096];
    errno = 0;
    if (poll(&p, 1, 1000) == 1 && (p.revents & POLLIN)) {
        int n = read(in, buf, sizeof buf);
        // The regression is EBADF here (errno 9) when the fd was clobbered.
        event_ok = (n > 0);
        for (int o = 0; o + (int)sizeof(struct inotify_event) <= n;) {
            struct inotify_event *e = (struct inotify_event *)(buf + o);
            if (e->mask & IN_CREATE) saw_create = 1;
            o += sizeof(struct inotify_event) + e->len;
        }
    }

    inotify_rm_watch(in, wd);
    close(in);
    unlink(created);
    rmdir(dir);
    printf("inotify_fdreserve distinct=%d event_ok=%d create=%d\n", distinct, event_ok, saw_create);
    return 0;
}
