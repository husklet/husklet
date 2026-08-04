// EPOLLONESHOT disables the fd after one delivery; it must be re-armed with EPOLL_CTL_MOD.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
static int ready(int ep){ struct epoll_event e; return epoll_wait(ep, &e, 1, 0); }
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    write(p[1], "a", 1);
    int ep = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN | EPOLLONESHOT, .data.fd = p[0] };
    epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &r);
    int first = ready(ep);          // 1
    int second = ready(ep);         // 0 (disarmed, data still present)
    struct epoll_event m = { .events = EPOLLIN | EPOLLONESHOT, .data.fd = p[0] };
    epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &m);
    int rearmed = ready(ep);        // 1
    printf("oneshot first=%d second=%d rearmed=%d\n", first, second, rearmed);
    return 0;
}
