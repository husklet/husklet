// Connected UDP over loopback: both datagram sockets connect() to each other's
// ephemeral address, then send/recv without addresses. A connected UDP socket rejects
// datagrams from other peers implicitly; here it round-trips a bidirectional exchange.
// Deterministic content echoes -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int bind_lo(struct sockaddr_in *out) {
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(s, (struct sockaddr *)&a, sizeof a);
    socklen_t al = sizeof a;
    getsockname(s, (struct sockaddr *)out, &al);
    return s;
}

int main(void) {
    net_watchdog(20);
    struct sockaddr_in aa, ba;
    int a = bind_lo(&aa);
    int b = bind_lo(&ba);
    connect(a, (struct sockaddr *)&ba, sizeof ba);
    connect(b, (struct sockaddr *)&aa, sizeof aa);

    send(a, "ping", 4, 0);
    char rb[16] = {0};
    ssize_t n1 = recv(b, rb, sizeof rb - 1, 0);
    rb[n1 > 0 ? n1 : 0] = 0;
    send(b, "pong!", 5, 0);
    char ra[16] = {0};
    ssize_t n2 = recv(a, ra, sizeof ra - 1, 0);
    ra[n2 > 0 ? n2 : 0] = 0;
    printf("b_got=%s a_got=%s\n", rb, ra);
    close(a);
    close(b);
    return 0;
}
