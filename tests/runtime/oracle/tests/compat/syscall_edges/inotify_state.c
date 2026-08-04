// inotify watch bookkeeping the engine must keep itself: re-adding a path returns the SAME
// watch descriptor, IN_MASK_ADD unions the masks, IN_ONESHOT self-removes and emits IN_IGNORED,
// rm_watch twice is EINVAL, and events carry the name for directory watches.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/inotify.h>
#include <unistd.h>

int main(void) {
    char dir[] = "/tmp/hl-inotify-XXXXXX";
    if (!mkdtemp(dir)) return 1;
    int in = inotify_init1(IN_NONBLOCK);
    int w1 = inotify_add_watch(in, dir, IN_CREATE);
    int w2 = inotify_add_watch(in, dir, IN_DELETE);           // replaces mask
    int w3 = inotify_add_watch(in, dir, IN_CREATE | IN_MASK_ADD); // unions
    int same = (w1 == w2) && (w2 == w3);

    char f[512];
    snprintf(f, sizeof f, "%s/probe", dir);
    close(open(f, O_CREAT | O_WRONLY, 0600));
    char buf[4096];
    ssize_t n = read(in, buf, sizeof buf);
    struct inotify_event *ev = (struct inotify_event *)buf;
    int gotcreate = (n > 0) && (ev->mask & IN_CREATE) != 0 && (ev->wd == w1);
    int named = (n > 0) && ev->len > 0 && strcmp(ev->name, "probe") == 0;

    int r1 = inotify_rm_watch(in, w1);
    int r2 = inotify_rm_watch(in, w1);
    int e2 = (r2 == -1) ? errno : 0;
    // the removal itself queues IN_IGNORED
    ssize_t n2 = read(in, buf, sizeof buf);
    struct inotify_event *ig = (struct inotify_event *)buf;
    int ignored = (n2 > 0) && (ig->mask & IN_IGNORED) != 0;

    int bad = inotify_add_watch(in, "/tmp/hl-inotify-does-not-exist", IN_CREATE);
    int ebad = (bad == -1) ? errno : 0;
    int nomask = inotify_add_watch(in, dir, 0);
    int enomask = (nomask == -1) ? errno : 0;
    int badfd = inotify_add_watch(-1, dir, IN_CREATE);
    int ebadfd = (badfd == -1) ? errno : 0;
    // a buffer too small for the next event is EINVAL
    inotify_add_watch(in, dir, IN_DELETE);
    unlink(f);
    char tiny[8];
    ssize_t nt = read(in, tiny, sizeof tiny);
    int ent = (nt == -1) ? errno : 0;
    rmdir(dir);
    printf("same=%d gotcreate=%d named=%d r1=%d r2=%d e2=%d ignored=%d bad=%d ebad=%d nomask=%d enomask=%d badfd=%d ebadfd=%d nt=%zd ent=%d\n",
           same, gotcreate, named, r1, r2, e2, ignored, bad, ebad, nomask, enomask, badfd, ebadfd, nt, ent);
    return 0;
}
