// SO_RCVTIMEO / SO_SNDTIMEO: set both, read them back, then recv on an idle
// connected socket and confirm it returns EAGAIN after the receive timeout rather
// than blocking forever. Asserts errno, not wall-clock -> deterministic oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 1);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    struct timeval tv = {0, 100000}; // 100ms
    setsockopt(cs, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    setsockopt(cs, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    struct timeval got = {0};
    socklen_t gl = sizeof got;
    getsockopt(cs, SOL_SOCKET, SO_RCVTIMEO, &got, &gl);
    connect(cs, (struct sockaddr *)&a, sizeof a);
    int as = accept(ls, NULL, NULL); // connected, but nothing sent

    char buf[16];
    errno = 0;
    ssize_t n = recv(cs, buf, sizeof buf, 0);
    printf("rcvtimeo_set=%d recv=%zd errno=%s\n", got.tv_usec == 100000, n, err_name(errno));
    close(as);
    close(cs);
    close(ls);
    return 0;
}
