// epoll readiness on a UNIX socketpair: an empty endpoint reports no readiness with a
// zero timeout; after the peer writes, epoll_wait returns exactly one fd with EPOLLIN.
// Bounded (zero/short timeouts) and content-checked -> deterministic oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    int ep = epoll_create1(0);
    struct epoll_event ev = {0};
    ev.events = EPOLLIN;
    ev.data.fd = sv[1];
    epoll_ctl(ep, EPOLL_CTL_ADD, sv[1], &ev);

    struct epoll_event out[4];
    int before = epoll_wait(ep, out, 4, 0); // nothing pending
    write(sv[0], "go", 2);
    int after = epoll_wait(ep, out, 4, 1000);
    int in_flag = after == 1 && (out[0].events & EPOLLIN) && out[0].data.fd == sv[1];
    printf("before=%d after=%d epollin=%d\n", before, after, in_flag);
    close(ep);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
