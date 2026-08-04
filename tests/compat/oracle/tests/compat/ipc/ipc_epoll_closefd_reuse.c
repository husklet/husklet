// Closing a watched fd auto-removes it from the epoll set (Linux). A later ADD of a NEW fd that
// reuses the same number must succeed (no stale EEXIST) and its readiness must be reported.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <unistd.h>

int main(void) {
    int ep = epoll_create1(0);
    int p1[2];
    if (pipe(p1)) return 1;
    struct epoll_event r = {.events = EPOLLIN, .data.fd = p1[0]};
    epoll_ctl(ep, EPOLL_CTL_ADD, p1[0], &r);
    int watched = p1[0];
    // close both ends WITHOUT EPOLL_CTL_DEL -> kernel auto-removes the registration.
    close(p1[0]);
    close(p1[1]);
    // reuse the number: a fresh pipe whose read end grabs the just-freed fd.
    int p2[2];
    if (pipe(p2)) return 1;
    int reused = (p2[0] == watched);
    // ADD the reused number: must NOT return EEXIST (stale membership would wrongly report it).
    struct epoll_event r2 = {.events = EPOLLIN, .data.fd = p2[0]};
    errno = 0;
    int add = epoll_ctl(ep, EPOLL_CTL_ADD, p2[0], &r2);
    int add_eexist = (add == -1 && errno == EEXIST);
    if (write(p2[1], "x", 1) != 1) return 1;
    struct epoll_event ev;
    int n = epoll_wait(ep, &ev, 1, 200);
    printf("closefd reused=%d add_ok=%d add_eexist=%d n=%d in=%d\n", reused, add == 0, add_eexist, n,
           (ev.events & EPOLLIN) != 0);
    close(ep);
    close(p2[0]);
    close(p2[1]);
    return 0;
}
