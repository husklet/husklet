// recvmsg sets MSG_TRUNC in msg_flags when a UDP datagram is larger than the supplied
// iovec: the returned byte count is the truncated buffer size, but msg_flags reports
// the overflow so callers can detect loss. Deterministic flag check -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int rs = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(rs, (struct sockaddr *)&a, sizeof a);
    socklen_t al = sizeof a;
    getsockname(rs, (struct sockaddr *)&a, &al);

    int ss = socket(AF_INET, SOCK_DGRAM, 0);
    char payload[40];
    memset(payload, 'Q', sizeof payload);
    sendto(ss, payload, sizeof payload, 0, (struct sockaddr *)&a, sizeof a);

    char small[10];
    struct iovec iov = {small, sizeof small};
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    ssize_t n = recvmsg(rs, &msg, 0);
    printf("recv_len=%zd msg_trunc=%d\n", n, (msg.msg_flags & MSG_TRUNC) != 0);
    close(ss);
    close(rs);
    return 0;
}
