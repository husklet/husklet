// epoll_ctl error contract: ADD an existing fd -> EEXIST; MOD/DEL an unknown fd -> ENOENT.
#include <sys/epoll.h>
#include <unistd.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
static const char *en(int e){ return strerror(e); }
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    int ep = epoll_create1(0);
    struct epoll_event r = { .events = EPOLLIN, .data.fd = p[0] };
    int add1 = epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &r);
    errno = 0;
    int add2 = epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &r);
    int e_add = errno;
    errno = 0;
    int modx = epoll_ctl(ep, EPOLL_CTL_MOD, p[1], &r);
    int e_mod = errno;
    errno = 0;
    int delx = epoll_ctl(ep, EPOLL_CTL_DEL, p[1], NULL);
    int e_del = errno;
    int delok = epoll_ctl(ep, EPOLL_CTL_DEL, p[0], NULL);
    printf("add1=%d add2=%d errno=%s\n", add1, add2, en(e_add));   // 0 -1 EEXIST
    printf("mod=%d errno=%s\n", modx, en(e_mod));                  // -1 ENOENT
    printf("del=%d errno=%s delok=%d\n", delx, en(e_del), delok);  // -1 ENOENT 0
    return 0;
}
