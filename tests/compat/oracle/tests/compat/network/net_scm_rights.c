// SCM_RIGHTS fd passing: send one end of a pre-filled pipe across a socketpair via
// sendmsg/recvmsg ancillary data, then read the original pipe contents through the
// received descriptor. Proves the passed fd is live in the receiver. -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);

    int pfd[2];
    pipe(pfd);
    write(pfd[1], "through-fd", 10);
    close(pfd[1]);

    struct msghdr msg = {0};
    char iobuf[1] = {'x'};
    struct iovec iov = {iobuf, 1};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    msg.msg_control = cbuf;
    msg.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &pfd[0], sizeof(int));
    ssize_t sn = sendmsg(sv[0], &msg, 0);

    struct msghdr rmsg = {0};
    char riobuf[1] = {0};
    struct iovec riov = {riobuf, 1};
    rmsg.msg_iov = &riov;
    rmsg.msg_iovlen = 1;
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof rcbuf);
    rmsg.msg_control = rcbuf;
    rmsg.msg_controllen = sizeof rcbuf;
    ssize_t rn = recvmsg(sv[1], &rmsg, 0);
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmsg);
    int got_fd = -1;
    int is_rights = rc && rc->cmsg_level == SOL_SOCKET && rc->cmsg_type == SCM_RIGHTS;
    if (is_rights) memcpy(&got_fd, CMSG_DATA(rc), sizeof(int));

    char rbuf[16] = {0};
    ssize_t pn = got_fd >= 0 ? read(got_fd, rbuf, sizeof rbuf - 1) : -1;
    rbuf[pn > 0 ? pn : 0] = 0;
    printf("sent=%zd recv=%zd scm_rights=%d passed_fd_content=%s\n", sn, rn, is_rights, rbuf);
    return 0;
}
