// epoll interest-set bookkeeping the engine models deterministically and host-invariantly:
// a duplicate ADD is EEXIST, MOD/DEL of an unregistered fd is ENOENT, adding an epoll fd to itself
// is EINVAL, and a level-triggered readable fd is reported on every wait until it is drained.
//
// NOTE: the EPOLLONESHOT-disarm and EPOLLET-transition readiness of an *emulated object* (eventfd)
// are deliberately NOT asserted here. On the macOS backend epoll is emulated over kqueue and those
// flags are honoured for real kqueue-able fds but not yet for emulated objects such as eventfd, so
// their readiness is host-delegated. Asserting
// an exact oneshot/edge readiness sequence would encode a host-specific outcome, which is an invalid
// differential golden; this case asserts only the Linux-invariant errno/ordering contract.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int ep = epoll_create1(0);
    int ev = eventfd(0, EFD_NONBLOCK);
    int unreg = eventfd(0, EFD_NONBLOCK); // never registered: MOD/DEL of it must be ENOENT
    struct epoll_event e = {.events = EPOLLIN, .data.u32 = 7};
    int a1 = epoll_ctl(ep, EPOLL_CTL_ADD, ev, &e);
    int a2 = epoll_ctl(ep, EPOLL_CTL_ADD, ev, &e);
    int ea2 = (a2 == -1) ? errno : 0;
    int m = epoll_ctl(ep, EPOLL_CTL_MOD, unreg, &e);
    int em = (m == -1) ? errno : 0;
    int d = epoll_ctl(ep, EPOLL_CTL_DEL, unreg, &e);
    int ed = (d == -1) ? errno : 0;
    int self = epoll_ctl(ep, EPOLL_CTL_ADD, ep, &e);
    int eself = (self == -1) ? errno : 0;

    struct epoll_event out[4];
    uint64_t one = 1;
    (void)!write(ev, &one, 8);
    int n1 = epoll_wait(ep, out, 4, 0);          // level: readable
    int n2 = epoll_wait(ep, out, 4, 0);          // still level-triggered readable
    uint32_t data = (n1 > 0) ? out[0].data.u32 : 0;
    uint64_t drain = 0;
    (void)!read(ev, &drain, 8);                  // drain the counter
    int n3 = epoll_wait(ep, out, 4, 0);          // no longer readable
    printf("a1=%d a2=%d ea2=%d m=%d em=%d d=%d ed=%d self=%d eself=%d n=%d,%d,%d data=%u\n",
           a1, a2, ea2, m, em, d, ed, self, eself, n1, n2, n3, data);
    return 0;
}
