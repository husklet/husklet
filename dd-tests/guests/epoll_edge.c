// #396 epoll surface (Linux-only; kqueue-emulated on macOS) diffed against a native oracle. Covers
// epoll_create1/epoll_create flag+size validation, EPOLLIN/EPOLLOUT readiness, 0/finite timeouts, the
// EPOLLONESHOT disarm+re-arm cycle (#390 overlap), and the epoll_ctl error return values kqueue does
// not enforce on its own (EEXIST/ENOENT/EINVAL/EPERM). Every line is a deterministic boolean.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

int main(void) {
    // epoll_create1: bad flags -> EINVAL; valid + EPOLL_CLOEXEC -> ok. epoll_create: size<=0 -> EINVAL.
    errno = 0;
    int badflag = epoll_create1(0x12345) < 0 && errno == EINVAL;
    int ep = epoll_create1(0);
    int cloexec = epoll_create1(EPOLL_CLOEXEC);
    int create_ok = ep >= 0 && cloexec >= 0;
    if (cloexec >= 0) close(cloexec);
    errno = 0;
    int badsize = epoll_create(0) < 0 && errno == EINVAL;
    printf("create badflag=%d ok=%d badsize=%d\n", badflag, create_ok, badsize);

    int a[2], b[2];
    if (pipe(a) < 0 || pipe(b) < 0) return 1;
    if (write(a[1], "x", 1) < 0) return 1; // a[0] readable

    // readiness: EPOLLIN on the readable end, EPOLLOUT on a writable end.
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = a[0]};
    epoll_ctl(ep, EPOLL_CTL_ADD, a[0], &ev);
    struct epoll_event out[8];
    int nr = epoll_wait(ep, out, 8, 1000);
    int rd = (nr == 1) && (out[0].events & EPOLLIN) && out[0].data.fd == a[0];
    ev.events = EPOLLOUT; ev.data.fd = b[1];
    epoll_ctl(ep, EPOLL_CTL_ADD, b[1], &ev);
    // now both a[0] (readable) and b[1] (writable) are armed
    nr = epoll_wait(ep, out, 8, 1000);
    int both = (nr == 2);
    // 0-timeout on a not-ready-only set: remove the ready ones, keep b[0] (never readable)
    epoll_ctl(ep, EPOLL_CTL_DEL, a[0], &ev);
    epoll_ctl(ep, EPOLL_CTL_DEL, b[1], &ev);
    ev.events = EPOLLIN; ev.data.fd = b[0];
    epoll_ctl(ep, EPOLL_CTL_ADD, b[0], &ev);
    int to0 = (epoll_wait(ep, out, 8, 0) == 0);
    int to = (epoll_wait(ep, out, 8, 30) == 0);
    printf("ready rd=%d both=%d to0=%d to=%d\n", rd, both, to0, to);

    // EPOLLONESHOT: fires once, then disarms until re-armed via MOD.
    int ep2 = epoll_create1(0);
    struct epoll_event oe = {.events = EPOLLIN | EPOLLONESHOT, .data.fd = a[0]};
    epoll_ctl(ep2, EPOLL_CTL_ADD, a[0], &oe);
    int os1 = epoll_wait(ep2, out, 8, 500); // a[0] still readable -> 1
    int os2 = epoll_wait(ep2, out, 8, 100); // disarmed by oneshot -> 0 despite still readable
    oe.events = EPOLLIN | EPOLLONESHOT;
    epoll_ctl(ep2, EPOLL_CTL_MOD, a[0], &oe);
    int os3 = epoll_wait(ep2, out, 8, 500); // re-armed -> 1
    printf("oneshot os1=%d os2=%d os3=%d\n", os1, os2, os3);
    close(ep2);

    // epoll_ctl error surface.
    int ep3 = epoll_create1(0);
    struct epoll_event e2 = {.events = EPOLLIN, .data.fd = a[0]};
    int add1 = epoll_ctl(ep3, EPOLL_CTL_ADD, a[0], &e2) == 0;
    errno = 0;
    int exist = epoll_ctl(ep3, EPOLL_CTL_ADD, a[0], &e2) < 0 && errno == EEXIST;
    errno = 0;
    int noent = epoll_ctl(ep3, EPOLL_CTL_MOD, a[1], &e2) < 0 && errno == ENOENT; // a[1] not registered
    errno = 0;
    int self = epoll_ctl(ep3, EPOLL_CTL_ADD, ep3, &e2) < 0 && errno == EINVAL;   // can't watch itself
    char tmpl[] = "/tmp/ddep_XXXXXX";
    int rf = mkstemp(tmpl);
    errno = 0;
    int perm = epoll_ctl(ep3, EPOLL_CTL_ADD, rf, &e2) < 0 && errno == EPERM;      // regular file -> EPERM
    if (rf >= 0) { close(rf); unlink(tmpl); }
    int delok = epoll_ctl(ep3, EPOLL_CTL_DEL, a[0], &e2) == 0;
    errno = 0;
    int delno = epoll_ctl(ep3, EPOLL_CTL_DEL, a[0], &e2) < 0 && errno == ENOENT;  // now gone -> ENOENT
    printf("ctl add=%d exist=%d noent=%d self=%d perm=%d del=%d delno=%d\n",
           add1, exist, noent, self, perm, delok, delno);
    close(ep3); close(ep);
    close(a[0]); close(a[1]); close(b[0]); close(b[1]);
    return 0;
}
