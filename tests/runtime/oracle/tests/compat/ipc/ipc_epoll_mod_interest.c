// EPOLL_CTL_MOD swaps the interest set: start on EPOLLOUT (writable), switch to EPOLLIN.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
static int wait0(int ep, struct epoll_event *e){ return epoll_wait(ep, e, 1, 0); }
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    int ep = epoll_create1(0);
    // watch write end for OUT
    struct epoll_event ro = { .events = EPOLLOUT, .data.fd = p[1] };
    epoll_ctl(ep, EPOLL_CTL_ADD, p[1], &ro);
    struct epoll_event e;
    int outn = wait0(ep, &e);
    int isout = outn == 1 && (e.events & EPOLLOUT);
    // now write end has no IN interest; switch to IN-only -> not readable, so no report
    struct epoll_event ri = { .events = EPOLLIN, .data.fd = p[1] };
    epoll_ctl(ep, EPOLL_CTL_MOD, p[1], &ri);
    int inn = wait0(ep, &e);
    printf("mod out_ready=%d in_ready=%d\n", isout, inn);   // 1 0
    return 0;
}
