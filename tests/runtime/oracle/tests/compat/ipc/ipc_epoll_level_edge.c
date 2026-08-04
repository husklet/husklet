// epoll level-triggered vs edge-triggered (EPOLLET) delivery semantics.
// Level: a still-readable fd is reported on every wait. Edge: reported once until new data.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
#include <string.h>
static int ready(int ep){ struct epoll_event e; return epoll_wait(ep, &e, 1, 0); }
int main(void){
    int lv[2], ed[2];
    if (pipe(lv) || pipe(ed)) { perror("pipe"); return 1; }
    write(lv[1], "xx", 2);
    write(ed[1], "xx", 2);
    int el = epoll_create1(0), ee = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN, .data.fd = lv[0] };
    epoll_ctl(el, EPOLL_CTL_ADD, lv[0], &r);
    struct epoll_event t = { .events = EPOLLIN | EPOLLET, .data.fd = ed[0] };
    epoll_ctl(ee, EPOLL_CTL_ADD, ed[0], &t);
    // level: two consecutive waits both report (no read in between)
    int l1 = ready(el), l2 = ready(el);
    // edge: first wait reports, second (no new data) does not
    int e1 = ready(ee), e2 = ready(ee);
    printf("level first=%d second=%d\n", l1, l2);      // 1 1
    printf("edge first=%d second=%d\n", e1, e2);        // 1 0
    // edge re-arms on new write
    write(ed[1], "y", 1);
    int e3 = ready(ee);
    printf("edge after_write=%d\n", e3);                // 1
    return 0;
}
