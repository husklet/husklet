// sendmsg gathers multiple iovecs into one stream write; the peer reads the segments
// concatenated in order. Verifies scatter/gather output ordering and total length over
// a loopback pair. Deterministic content -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    struct iovec iov[3];
    iov[0].iov_base = "seg1-";
    iov[0].iov_len = 5;
    iov[1].iov_base = "seg2-";
    iov[1].iov_len = 5;
    iov[2].iov_base = "end";
    iov[2].iov_len = 3;
    struct msghdr msg = {0};
    msg.msg_iov = iov;
    msg.msg_iovlen = 3;
    ssize_t sn = sendmsg(sv[0], &msg, 0);

    char buf[32] = {0};
    ssize_t rn = read(sv[1], buf, sizeof buf - 1);
    buf[rn > 0 ? rn : 0] = 0;
    printf("sent=%zd recv=%zd data=%s\n", sn, rn, buf);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
