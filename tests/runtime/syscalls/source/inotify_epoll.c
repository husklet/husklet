// Regression: an armed inotify fd must be epollable. The watch is a typed box
// object with no host descriptor, so epoll_ctl(EPOLL_CTL_ADD) used to fail -1
// (the epoll provider subscribed on the object's INVALID host_handle). epoll on
// an inotify fd is THE dominant file-watcher pattern (vite/webpack/cargo-watch/
// language servers), so this must work like native Linux: ADD succeeds, and a
// bounded epoll_wait returns the fd with EPOLLIN when an event is queued, with
// the registered epoll_data round-tripped. select()/poll() must agree.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/inotify.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128], created[192];
    snprintf(dir, sizeof dir, "/tmp/hl_iep_dir_%d", (int)getpid());
    mkdir(dir, 0700);

    int in = inotify_init1(IN_NONBLOCK);
    int wd = inotify_add_watch(in, dir, IN_CREATE | IN_MODIFY);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (in < 0 || wd < 0 || ep < 0) return 10;

    // Arm epoll on the inotify fd. The ADD itself is the primary regression.
    struct epoll_event ev = {0};
    ev.events = EPOLLIN;
    ev.data.u64 = 0xfeedface1234abcdULL; // must round-trip out of epoll_wait
    int add_ok = (epoll_ctl(ep, EPOLL_CTL_ADD, in, &ev) == 0);

    // Nothing has happened yet: a non-blocking epoll_wait must report no events.
    struct epoll_event out[4];
    int idle = epoll_wait(ep, out, 4, 0);

    // Generate a real event inside the watched directory.
    snprintf(created, sizeof created, "%s/a", dir);
    int cf = open(created, O_CREAT | O_WRONLY, 0644);
    if (cf >= 0) {
        if (write(cf, "z", 1) != 1) { /* ignore */ }
        close(cf);
    }

    // select/poll/epoll must all agree the inotify fd is now readable. None of
    // these consume the queued events, so sample all three before reading.
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(in, &rfds);
    struct timeval tv = {2, 0};
    int sel_ready = (select(in + 1, &rfds, NULL, NULL, &tv) == 1 && FD_ISSET(in, &rfds));

    struct pollfd pfd = {in, POLLIN, 0};
    int poll_ready = (poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN));

    // Bounded epoll_wait (2000ms) -> deterministic, no sleep race.
    int nready = epoll_wait(ep, out, 4, 2000);
    int epoll_ready = 0, data_ok = 0;
    for (int i = 0; i < nready; i++)
        if (out[i].events & EPOLLIN) {
            epoll_ready = 1;
            if (out[i].data.u64 == 0xfeedface1234abcdULL) data_ok = 1;
        }

    // Read the event back to confirm the readiness was real (IN_CREATE for "a").
    char buf[4096];
    int saw_create = 0;
    int n = read(in, buf, sizeof buf);
    for (int o = 0; o + (int)sizeof(struct inotify_event) <= n;) {
        struct inotify_event *e = (struct inotify_event *)(buf + o);
        if (e->mask & IN_CREATE) saw_create = 1;
        o += sizeof(struct inotify_event) + e->len;
    }

    // After draining, a non-blocking epoll_wait reports no more events.
    int drained = (epoll_wait(ep, out, 4, 0) == 0);

    // EPOLL_CTL_DEL removes it cleanly; a second DEL is ENOENT.
    int del_ok = (epoll_ctl(ep, EPOLL_CTL_DEL, in, NULL) == 0);
    int del_again = (epoll_ctl(ep, EPOLL_CTL_DEL, in, NULL) == -1 && errno == ENOENT);

    inotify_rm_watch(in, wd);
    close(ep);
    close(in);
    unlink(created);
    rmdir(dir);

    printf("inotify_epoll add=%d idle=%d sel=%d poll=%d epoll=%d data=%d create=%d drained=%d del=%d del2=%d\n",
           add_ok, idle, sel_ready, poll_ready, epoll_ready, data_ok, saw_create, drained, del_ok, del_again);
    return 0;
}
