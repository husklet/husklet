// getpeername on an unconnected TCP socket fails with ENOTCONN, and getsockname on an
// unbound socket succeeds reporting the AF_INET family with port 0. Error-path and
// name-introspection checks with no traffic. Deterministic errno -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    socklen_t al = sizeof a;
    errno = 0;
    int pr = getpeername(s, (struct sockaddr *)&a, &al);
    int peer_errno = errno;

    struct sockaddr_in b;
    memset(&b, 0, sizeof b);
    socklen_t bl = sizeof b;
    int sr = getsockname(s, (struct sockaddr *)&b, &bl);
    printf("getpeername=%d errno=%s getsockname=%d fam_inet=%d port0=%d\n", pr, err_name(peer_errno),
           sr, b.sin_family == AF_INET, ntohs(b.sin_port) == 0);
    close(s);
    return 0;
}
