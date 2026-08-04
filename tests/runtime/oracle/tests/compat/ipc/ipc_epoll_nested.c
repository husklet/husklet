// A nested epoll fd is itself pollable: readiness of an inner fd makes the outer epoll report it.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    int inner = epoll_create1(0), outer = epoll_create1(0);
    struct epoll_event ri = { .events = EPOLLIN, .data.fd = p[0] };
    epoll_ctl(inner, EPOLL_CTL_ADD, p[0], &ri);
    struct epoll_event ro = { .events = EPOLLIN, .data.fd = inner };
    epoll_ctl(outer, EPOLL_CTL_ADD, inner, &ro);
    struct epoll_event e;
    int before = epoll_wait(outer, &e, 1, 0);   // 0: nothing ready
    write(p[1], "z", 1);
    int after = epoll_wait(outer, &e, 1, 100);   // 1: inner became readable
    int isinner = after == 1 && e.data.fd == inner;
    printf("nested before=%d after=%d inner_reported=%d\n", before, after, isinner);
    return 0;
}
