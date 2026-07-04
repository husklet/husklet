// EPOLLONESHOT: the fd reports once, then is disabled until re-armed with EPOLL_CTL_MOD.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/epoll.h>
#include <unistd.h>

int main(void) {
    int fds[2];
    pipe(fds);
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN | EPOLLONESHOT, .data.fd = fds[0]};
    epoll_ctl(ep, EPOLL_CTL_ADD, fds[0], &ev);
    struct epoll_event out[4];
    write(fds[1], "ab", 2);
    int first = epoll_wait(ep, out, 4, 100) == 1;
    // still readable (didn't drain) but ONESHOT disabled it -> no report
    int disabled = epoll_wait(ep, out, 4, 100) == 0;
    // re-arm
    ev.events = EPOLLIN | EPOLLONESHOT;
    epoll_ctl(ep, EPOLL_CTL_MOD, fds[0], &ev);
    int rearmed = epoll_wait(ep, out, 4, 100) == 1;
    close(ep); close(fds[0]); close(fds[1]);
    printf("epoll_oneshot first=%d disabled=%d rearmed=%d\n", first, disabled, rearmed);
    // #390: this case was flaky under host load with a *fully empty* stdout (rc=0, never a wrong value)
    // — a `mac`-bridge teardown race that dropped the guest's final buffered stdout write. The former
    // per-guest workaround (fflush + a 50ms drain gap before exit) is now REDUNDANT: the dd-tests runner
    // captures guest stdout via a durable file on the shared repo tree instead of the bridge pipe (see
    // `run()` in dd-tests/src/lib.rs), so the final line can no longer be lost in the stream teardown.
    // Verified 0 flakes / 50 runs under a mac-side CPU flood with no guest-side flush/sleep.
    return 0;
}
