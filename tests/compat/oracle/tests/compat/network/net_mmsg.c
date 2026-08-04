// sendmmsg / recvmmsg over a connected UDP loopback pair: send three datagrams in a
// single sendmmsg call and gather them with one recvmmsg call, checking the reported
// message count and each per-message length. Deterministic -> oracle.
#define _GNU_SOURCE
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
    connect(ss, (struct sockaddr *)&a, sizeof a);

    const char *msgs[3] = {"one", "twoo", "three"};
    struct mmsghdr out[3] = {0};
    struct iovec oiov[3];
    for (int i = 0; i < 3; i++) {
        oiov[i].iov_base = (void *)msgs[i];
        oiov[i].iov_len = strlen(msgs[i]);
        out[i].msg_hdr.msg_iov = &oiov[i];
        out[i].msg_hdr.msg_iovlen = 1;
    }
    int sent = sendmmsg(ss, out, 3, 0);

    struct mmsghdr in[3] = {0};
    struct iovec iiov[3];
    char bufs[3][16];
    for (int i = 0; i < 3; i++) {
        iiov[i].iov_base = bufs[i];
        iiov[i].iov_len = sizeof bufs[i];
        in[i].msg_hdr.msg_iov = &iiov[i];
        in[i].msg_hdr.msg_iovlen = 1;
    }
    int got = recvmmsg(rs, in, 3, MSG_WAITALL, NULL);
    printf("sent=%d got=%d len0=%u len1=%u len2=%u\n", sent, got, in[0].msg_len, in[1].msg_len,
           in[2].msg_len);
    close(ss);
    close(rs);
    return 0;
}
