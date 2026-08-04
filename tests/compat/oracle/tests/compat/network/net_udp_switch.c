// UDP datagram semantics over the private-loopback AF_INET<->AF_UNIX switch. Locks the stable facts the
// switch must preserve: getsockname reports the guest AF_INET addr (not the private AF_UNIX backing path),
// recvfrom yields the guest AF_INET sender addr, queued datagrams keep boundaries + order, MSG_PEEK leaves
// the datagram queued, a zero-length datagram is a valid send+receive, MSG_TRUNC reports the true datagram
// length, MSG_DONTWAIT gives EAGAIN on an empty queue, an oversized (> 65507) UDP send fails EMSGSIZE, and
// an unconnected send to a loopback port with no receiver succeeds (fire-and-forget). Deterministic -> oracle.
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
    // getsockname must expose the guest AF_INET loopback addr, never the AF_UNIX switch path.
    printf("bound fam=%d ip=%08x hasport=%d\n", a.sin_family, ntohl(a.sin_addr.s_addr), ntohs(a.sin_port) != 0);

    int ss = socket(AF_INET, SOCK_DGRAM, 0);
    sendto(ss, "AAA", 3, 0, (struct sockaddr *)&a, sizeof a);
    sendto(ss, "BB", 2, 0, (struct sockaddr *)&a, sizeof a);
    sendto(ss, "", 0, 0, (struct sockaddr *)&a, sizeof a); // zero-length datagram

    char b[64];
    struct sockaddr_in from = {0};
    socklen_t fl = sizeof from;
    ssize_t r0 = recvfrom(rs, b, sizeof b, 0, (struct sockaddr *)&from, &fl);
    printf("dg0=%zd/%.*s fromfam=%d fromip=%08x\n", r0, (int)(r0 < 0 ? 0 : r0), b, from.sin_family,
           ntohl(from.sin_addr.s_addr));

    ssize_t rp = recv(rs, b, sizeof b, MSG_PEEK);
    ssize_t r1 = recv(rs, b, sizeof b, 0);
    printf("peek=%zd/%.*s next=%zd/%.*s\n", rp, (int)(rp < 0 ? 0 : rp), b, r1, (int)(r1 < 0 ? 0 : r1), b);

    ssize_t r2 = recv(rs, b, sizeof b, 0); // zero-length datagram
    printf("zerolen=%zd\n", r2);

    errno = 0;
    ssize_t re = recv(rs, b, sizeof b, MSG_DONTWAIT); // empty queue
    printf("empty=%zd/%s\n", re, err_name(errno));

    sendto(ss, "HELLOWORLD", 10, 0, (struct sockaddr *)&a, sizeof a);
    ssize_t rt = recv(rs, b, 4, MSG_TRUNC); // true datagram length, not truncated
    printf("trunc=%zd\n", rt);

    // Oversized datagram -> EMSGSIZE (payload > 65507).
    static char big[70000];
    memset(big, 'A', sizeof big);
    errno = 0;
    ssize_t ro = sendto(ss, big, sizeof big, 0, (struct sockaddr *)&a, sizeof a);
    printf("oversize=%zd/%s\n", ro, err_name(errno));

    // Unconnected send to a loopback port with no receiver: success (datagram dropped).
    struct sockaddr_in dead = {0};
    dead.sin_family = AF_INET;
    dead.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    dead.sin_port = htons(9);
    errno = 0;
    ssize_t rd = sendto(ss, "z", 1, 0, (struct sockaddr *)&dead, sizeof dead);
    printf("noreceiver=%zd/%s\n", rd, err_name(errno));

    close(ss);
    close(rs);
    return 0;
}
