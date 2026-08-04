// UDP oversized datagram + MSG_TRUNC: send a 32-byte datagram, receive into an
// 8-byte buffer. Without MSG_TRUNC the excess is discarded and only 8 bytes land;
// with MSG_TRUNC recv reports the true datagram length. Deterministic -> oracle.
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
    char payload[32];
    memset(payload, 'Z', sizeof payload);
    sendto(ss, payload, sizeof payload, 0, (struct sockaddr *)&a, sizeof a);
    char big[64];
    memset(big, 'Z', sizeof big);
    sendto(ss, big, sizeof big, 0, (struct sockaddr *)&a, sizeof a);

    char buf[8];
    ssize_t n1 = recv(rs, buf, sizeof buf, 0);          // truncated silently
    ssize_t n2 = recv(rs, buf, sizeof buf, MSG_TRUNC);  // reports real length
    printf("plain=%zd trunc_reports=%zd\n", n1, n2);
    close(ss);
    close(rs);
    return 0;
}
