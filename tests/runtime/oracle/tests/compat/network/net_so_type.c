// SO_TYPE / SO_DOMAIN / SO_PROTOCOL / SO_ACCEPTCONN queried on a fresh, bound, and
// listening TCP socket plus a UDP socket. Verifies the kernel reports the socket's
// own construction parameters and flips SO_ACCEPTCONN only after listen(). -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int geti(int fd, int opt) {
    int v = -1;
    socklen_t l = sizeof v;
    getsockopt(fd, SOL_SOCKET, opt, &v, &l);
    return v;
}

int main(void) {
    net_watchdog(20);
    int t = socket(AF_INET, SOCK_STREAM, 0);
    int acc_before = geti(t, SO_ACCEPTCONN);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(t, (struct sockaddr *)&a, sizeof a);
    listen(t, 4);
    int acc_after = geti(t, SO_ACCEPTCONN);
    printf("tcp type=%d domain=%d proto_stream=%d accept_before=%d accept_after=%d\n",
           geti(t, SO_TYPE) == SOCK_STREAM, geti(t, SO_DOMAIN) == AF_INET,
           geti(t, SO_PROTOCOL) == IPPROTO_TCP, acc_before, acc_after);

    int u = socket(AF_INET, SOCK_DGRAM, 0);
    printf("udp type=%d proto_udp=%d accept=%d\n", geti(u, SO_TYPE) == SOCK_DGRAM,
           geti(u, SO_PROTOCOL) == IPPROTO_UDP, geti(u, SO_ACCEPTCONN));
    close(t);
    close(u);
    return 0;
}
