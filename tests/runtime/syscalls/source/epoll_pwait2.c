// epoll_pwait2: like epoll_pwait but the timeout is a struct timespec (nanosecond
// resolution) instead of an int millisecond count, plus an atomically-applied sigmask.
// Exercised via the raw syscall so the result does not depend on a libc wrapper being
// present. Verifies: timeout honored (return 0 on a finite timespec with no readiness),
// readiness reported (return 1 with round-tripped data), NULL-timeout non-block when the
// fd is already ready, and maxevents<=0 -> EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef __NR_epoll_pwait2
#define __NR_epoll_pwait2 441
#endif

static int ep_pwait2(int epfd, struct epoll_event *ev, int maxev, const struct timespec *to,
                     const sigset_t *sm) {
    return (int)syscall(__NR_epoll_pwait2, epfd, ev, maxev, to, sm, _NSIG / 8);
}

int main(void) {
    int fds[2];
    if (pipe(fds) != 0) return 1;
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0xdeadbeefcafef00dULL};
    epoll_ctl(ep, EPOLL_CTL_ADD, fds[0], &ev);
    struct epoll_event out[4];
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);

    // (1) finite timespec, nothing ready -> return 0 (timeout honored).
    struct timespec t = {.tv_sec = 0, .tv_nsec = 30 * 1000 * 1000};
    int timed_out = ep_pwait2(ep, out, 4, &t, &mask) == 0;

    // (2) data arrives -> exactly one event, data round-trips.
    if (write(fds[1], "z", 1) != 1) return 1;
    int r = ep_pwait2(ep, out, 4, &t, &mask);
    int ready = (r == 1) && (out[0].events & EPOLLIN) && out[0].data.u64 == 0xdeadbeefcafef00dULL;

    // (3) already ready + NULL timeout must return immediately (not block) with the event.
    int nullto = ep_pwait2(ep, out, 4, NULL, &mask) == 1;

    // (4) maxevents <= 0 -> EINVAL.
    errno = 0;
    int einval = (ep_pwait2(ep, out, 0, &t, &mask) == -1) && (errno == EINVAL);

    close(ep);
    close(fds[0]);
    close(fds[1]);
    printf("epoll_pwait2 timeout=%d ready=%d nullto=%d einval=%d\n", timed_out, ready, nullto, einval);
    return 0;
}
