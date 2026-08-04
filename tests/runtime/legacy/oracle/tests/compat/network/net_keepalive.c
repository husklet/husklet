// SO_KEEPALIVE and SO_OOBINLINE get-after-set on a TCP socket: toggle each option on
// and off and confirm getsockopt tracks the last write. Pure option-plumbing with no
// traffic. Deterministic booleans -> oracle.
#include "net_util.h"
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int geti(int fd, int opt) {
    int v = -1;
    socklen_t l = sizeof v;
    getsockopt(fd, SOL_SOCKET, opt, &v, &l);
    return v != 0;
}

int main(void) {
    net_watchdog(20);
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int on = 1, off = 0;
    setsockopt(s, SOL_SOCKET, SO_KEEPALIVE, &on, sizeof on);
    int ka_on = geti(s, SO_KEEPALIVE);
    setsockopt(s, SOL_SOCKET, SO_KEEPALIVE, &off, sizeof off);
    int ka_off = geti(s, SO_KEEPALIVE);
    setsockopt(s, SOL_SOCKET, SO_OOBINLINE, &on, sizeof on);
    int oob_on = geti(s, SO_OOBINLINE);
    printf("keepalive_on=%d keepalive_off=%d oobinline_on=%d\n", ka_on, ka_off, oob_on);
    close(s);
    return 0;
}
