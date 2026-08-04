// SCM_RIGHTS passes a LIVE epoll fd across an AF_UNIX socketpair; the receiver epoll_waits on the
// received fd and must observe readiness of the interest set the sender registered. An epoll instance is
// an engine-emulated object (host kqueue + per-fd interest keyed by fd number); the received fd carries
// none of that state, so epoll_wait on it currently fails (excluded-known-bug: emulated-fd identity not
// transferred across SCM_RIGHTS -- same root class as ipc_scm_timerfd, which IS fixed).
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/epoll.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <sys/wait.h>
int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    pid_t child = fork();
    if (child < 0) return 1;
    if (child == 0) {
        close(sv[0]);
        int ep = epoll_create1(0);
        int pp[2];
        if (ep < 0 || pipe(pp)) _exit(2);
        struct epoll_event ev = {.events = EPOLLIN, .data.u32 = 0x5151};
        if (epoll_ctl(ep, EPOLL_CTL_ADD, pp[0], &ev)) _exit(3);

        char cbuf[CMSG_SPACE(sizeof(int))];
        memset(cbuf, 0, sizeof cbuf);
        char data = 'E';
        struct iovec io = {&data, 1};
        struct msghdr mh = {0};
        mh.msg_iov = &io;
        mh.msg_iovlen = 1;
        mh.msg_control = cbuf;
        mh.msg_controllen = sizeof cbuf;
        struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &ep, sizeof(int));
        if (sendmsg(sv[1], &mh, 0) != 1) _exit(4);
        char acknowledged = 0;
        if (read(sv[1], &acknowledged, 1) != 1 || acknowledged != 'A' || write(pp[1], "z", 1) != 1) _exit(5);
        close(pp[0]);
        close(pp[1]);
        close(ep);
        close(sv[1]);
        _exit(0);
    }
    close(sv[1]);

    char rdata;
    struct iovec rio = {&rdata, 1};
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmh = {0};
    rmh.msg_iov = &rio;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof rcbuf;
    if (recvmsg(sv[0], &rmh, 0) != 1) return 1;
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    int rep = -1;
    if (rc && rc->cmsg_type == SCM_RIGHTS) memcpy(&rep, CMSG_DATA(rc), sizeof(int));
    if (write(sv[0], "A", 1) != 1) return 1;

    struct epoll_event out[4];
    int n = (rep >= 0) ? epoll_wait(rep, out, 4, 2000) : -1;
    if (rep >= 0) close(rep);
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 1;
    printf("n=%d data=%x\n", n, n > 0 ? out[0].data.u32 : 0); // 1 5151
    return n == 1 && out[0].data.u32 == 0x5151 ? 0 : 1;
}
