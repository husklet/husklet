// Demonstrating test for a deferred architectural divergence: EPOLLHUP is reported too early on a TCP
// socket whose peer has fully closed while the local write half is still open. On native Linux, a peer
// close (FIN) sets EPOLLIN (readable EOF) + EPOLLRDHUP but NOT EPOLLHUP, because the connection is only
// half torn down -- the local side can still send until it shuts down or receives an RST. Under the engine
// a private 127.0.0.1 stream socket is backed by an AF_UNIX switch socket, which has no half-open-write
// state: a peer close collapses the whole socket, so poll/epoll report EPOLLHUP immediately. This is the
// same architectural limitation as the deferred SO_LINGER RST case (linger-reset) -- the AF_UNIX switch
// cannot represent TCP's asymmetric FIN/half-open teardown -- and is left excluded pending a poll/epoll
// result-rewriting layer that tracks per-fd local shutdown state. Deterministic -> oracle-checked.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/epoll.h>
#include <unistd.h>
#include "net_util.h"

static int mkpair(int *c, int *s) {
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 4);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);
    int cc = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(cc, (struct sockaddr *)&a, sizeof a) < 0) return -1;
    int ss = accept(ls, NULL, NULL);
    close(ls);
    *c = cc;
    *s = ss;
    return 0;
}

int main(void) {
    net_watchdog(10);
    int c, s;
    if (mkpair(&c, &s)) {
        printf("pairfail\n");
        return 1;
    }
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN | EPOLLRDHUP, .data.fd = s};
    epoll_ctl(ep, EPOLL_CTL_ADD, s, &ev);
    // Peer fully closes; s never shut its own write side, so this is a half-open teardown.
    close(c);
    struct epoll_event out[4];
    int ne = epoll_wait(ep, out, 4, 1000);
    printf("n=%d in=%d rdhup=%d hup=%d\n", ne, (out[0].events & EPOLLIN) != 0,
           (out[0].events & EPOLLRDHUP) != 0, (out[0].events & EPOLLHUP) != 0);
    close(s);
    close(ep);
    return 0;
}
