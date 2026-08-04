// EPOLLRDHUP fires when a stream peer half-closes (shutdown SHUT_WR); EPOLLHUP on full close.
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    int ep = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN | EPOLLRDHUP, .data.fd = sv[0] };
    epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &r);
    shutdown(sv[1], SHUT_WR);
    struct epoll_event ev;
    int n = epoll_wait(ep, &ev, 1, 100);
    printf("rdhup n=%d rdhup=%d in=%d\n", n, (ev.events & EPOLLRDHUP) != 0, (ev.events & EPOLLIN) != 0);
    close(sv[1]);
    struct epoll_event ev2;
    int n2 = epoll_wait(ep, &ev2, 1, 100);
    printf("hup n=%d hup=%d\n", n2, (ev2.events & EPOLLHUP) != 0);
    return 0;
}
