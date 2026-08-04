// A SCM_RIGHTS control message carrying more fds than the receive control buffer can hold is delivered
// PARTIALLY on Linux: the fds that fit are installed, MSG_CTRUNC is flagged, and the fds that did not fit
// are closed by the kernel (never leaked). The engine previously dropped the whole record (zero fds).
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdint.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) { printf("socketpair fail\n"); return 1; }
    int p0[2], p1[2], p2[2];
    if (pipe(p0) || pipe(p1) || pipe(p2)) return 1;
    write(p0[1], "A", 1); write(p1[1], "B", 1); write(p2[1], "C", 1);
    int send[3] = { p0[0], p1[0], p2[0] };
    char cbuf[CMSG_SPACE(sizeof(int) * 3)]; memset(cbuf, 0, sizeof cbuf);
    char d = 'D'; struct iovec io = { &d, 1 };
    struct msghdr mh = {0}; mh.msg_iov = &io; mh.msg_iovlen = 1;
    mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int) * 3);
    memcpy(CMSG_DATA(c), send, sizeof(int) * 3);
    sendmsg(sv[1], &mh, 0);

    // Receive control buffer sized for exactly two fds.
    char rd; struct iovec rio = { &rd, 1 };
    char rcbuf[CMSG_LEN(0) + 2 * sizeof(int)]; memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmh = {0}; rmh.msg_iov = &rio; rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf; rmh.msg_controllen = sizeof rcbuf;
    recvmsg(sv[0], &rmh, 0);
    int ctrunc = (rmh.msg_flags & MSG_CTRUNC) != 0;
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    int nfds = rc ? (int)((rc->cmsg_len - CMSG_LEN(0)) / sizeof(int)) : 0;
    int readable = 0;
    for (int i = 0; i < nfds; i++) {
        int fd; memcpy(&fd, CMSG_DATA(rc) + i * sizeof(int), sizeof(int));
        char b; if (read(fd, &b, 1) == 1) readable++;
    }
    // Detect an fd leak: the next fd we allocate should be a small number, not pushed up by leaked fds.
    int probe = dup(0);
    printf("ctrunc=%d nfds=%d readable=%d no_leak=%d\n", ctrunc, nfds, readable, probe < 20);
    return 0;
}
