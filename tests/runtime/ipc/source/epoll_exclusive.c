// EPOLLEXCLUSIVE adds without error and still delivers readiness to the (single) waiter.
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    int ep = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN | EPOLLEXCLUSIVE, .data.fd = sv[0] };
    errno = 0;
    int add = epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &r);
    int e_add = errno;
    // EPOLLEXCLUSIVE forbids EPOLL_CTL_MOD -> EINVAL
    struct epoll_event m = { .events = EPOLLIN | EPOLLEXCLUSIVE, .data.fd = sv[0] };
    errno = 0;
    int mod = epoll_ctl(ep, EPOLL_CTL_MOD, sv[0], &m);
    int e_mod = errno;
    write(sv[1], "k", 1);
    struct epoll_event e;
    int n = epoll_wait(ep, &e, 1, 100);
    printf("exclusive add=%d add_errno=%s\n", add, add == 0 ? "0" : strerror(e_add));
    printf("mod=%d mod_errno=%s\n", mod, mod == 0 ? "0" : strerror(e_mod));   // -1 EINVAL
    printf("delivered=%d\n", n);   // 1
    return 0;
}
