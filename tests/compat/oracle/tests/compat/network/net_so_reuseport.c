// SO_REUSEADDR and SO_REUSEPORT: verify both options set/get on a TCP socket and
// that two listeners can bind the same loopback port when SO_REUSEPORT is set.
// Deterministic get-after-set booleans plus a second-bind success flag -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int mkreuse(int *port_out) {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    setsockopt(s, SOL_SOCKET, SO_REUSEPORT, &one, sizeof one);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = port_out && *port_out ? htons(*port_out) : 0;
    if (bind(s, (struct sockaddr *)&a, sizeof a) < 0) return -1;
    if (listen(s, 4) < 0) return -1;
    socklen_t al = sizeof a;
    getsockname(s, (struct sockaddr *)&a, &al);
    if (port_out) *port_out = ntohs(a.sin_port);
    return s;
}

int main(void) {
    net_watchdog(20);
    int port = 0;
    int s1 = mkreuse(&port);
    int ra = 0, rp = 0;
    socklen_t gl = sizeof ra;
    getsockopt(s1, SOL_SOCKET, SO_REUSEADDR, &ra, &gl);
    gl = sizeof rp;
    getsockopt(s1, SOL_SOCKET, SO_REUSEPORT, &rp, &gl);
    int s2 = mkreuse(&port); // same port again
    printf("reuseaddr=%d reuseport=%d s1=%d second_bind=%d\n", ra != 0, rp != 0, s1 >= 0, s2 >= 0);
    if (s1 >= 0) close(s1);
    if (s2 >= 0) close(s2);
    return 0;
}
