// FIONREAD reports bytes available to read on a connected TCP loopback socket: zero
// when idle, the queued count after the peer writes, and the decremented remainder
// after a partial read. ioctl-driven, content-checked -> deterministic oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

static int avail(int fd) {
    int n = -1;
    ioctl(fd, FIONREAD, &n);
    return n;
}

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
    connect(cs, (struct sockaddr *)&a, sizeof a);
    int as = accept(ls, NULL, NULL);

    int empty = avail(cs);
    write(as, "0123456789", 10);
    // block until the data is queued so the count is deterministic
    char pk[1];
    recv(cs, pk, 1, MSG_PEEK);
    int queued = avail(cs);
    char buf[4];
    read(cs, buf, 4);
    int remaining = avail(cs);
    printf("empty=%d queued=%d remaining=%d\n", empty, queued, remaining);
    close(cs);
    close(as);
    close(ls);
    return 0;
}
