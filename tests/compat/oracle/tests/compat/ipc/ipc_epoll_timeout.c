// epoll_wait timeout contract: 0 returns immediately (0 events), N>0 returns 0 after waiting
// when nothing is ready, and a ready fd returns before the timeout regardless.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    int ep = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN, .data.fd = p[0] };
    epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &r);
    struct epoll_event e;
    int immediate = epoll_wait(ep, &e, 1, 0);     // 0
    int timed = epoll_wait(ep, &e, 1, 20);        // 0 after ~20ms
    write(p[1], "q", 1);
    int hit = epoll_wait(ep, &e, 1, -1);          // 1
    printf("timeout immediate=%d timed=%d ready=%d\n", immediate, timed, hit);
    return 0;
}
