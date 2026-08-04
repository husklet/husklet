// IPPROTO_IP and IPPROTO_IPV6 integer socket options on a CONNECTED/BOUND loopback socket. Under the engine
// a private 127.0.0.1 / ::1 socket is backed by an AF_UNIX switch socket once bind/connect runs, and the
// host rejects every IPPROTO_IP / IPPROTO_IPV6 getsockopt/setsockopt on an AF_UNIX fd. Native Linux round-
// trips these on a real IP socket -- DNS servers set IP_PKTINFO/IP_RECVTTL, dual-stack servers read
// IPV6_V6ONLY back, QUIC/HTTP3 stacks set IP_TOS/IPV6_TCLASS -- and code sets them *after* bind/connect, so
// the values must survive the switch. Deterministic set-then-get facts -> oracle-checked.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>
#include "net_util.h"

static int geti(int fd, int lvl, int opt) {
    int v = -1;
    socklen_t l = sizeof v;
    if (getsockopt(fd, lvl, opt, &v, &l) < 0) return -errno;
    return v;
}
// set an int option then read it back; print "name set_ok=<0/1> got=<value>"
static void rt(const char *nm, int fd, int lvl, int opt, int val) {
    int s = setsockopt(fd, lvl, opt, &val, sizeof val);
    printf("%s set_ok=%d got=%d\n", nm, s == 0, geti(fd, lvl, opt));
}

static int connfd(void) {
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 4);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);
    int c = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(c, (struct sockaddr *)&a, sizeof a) < 0) return -1;
    return c;
}

int main(void) {
    net_watchdog(15);

    int c = connfd();
    if (c < 0) { printf("connect_failed\n"); return 1; }
    // IP-level options round-trip across the switch on a connected TCP socket.
    rt("IP_TOS", c, IPPROTO_IP, IP_TOS, 0x10);
    rt("IP_TTL", c, IPPROTO_IP, IP_TTL, 42);
    rt("IP_MTU_DISCOVER", c, IPPROTO_IP, IP_MTU_DISCOVER, IP_PMTUDISC_DO);
    rt("IP_PKTINFO", c, IPPROTO_IP, IP_PKTINFO, 1);
    rt("IP_RECVTTL", c, IPPROTO_IP, IP_RECVTTL, 1);
    rt("IP_RECVTOS", c, IPPROTO_IP, IP_RECVTOS, 1);
    rt("IP_RECVERR", c, IPPROTO_IP, IP_RECVERR, 1);
    rt("IP_FREEBIND", c, IPPROTO_IP, IP_FREEBIND, 1);
    close(c);

    int s6 = socket(AF_INET6, SOCK_STREAM, 0);
    if (s6 < 0) { printf("no_ipv6\n"); return 0; }
    int on = 1;
    // Set V6ONLY before bind (unswapped), then bind, then confirm the readback survives the switch and a
    // post-bind change is rejected with EINVAL exactly as native.
    int pre = setsockopt(s6, IPPROTO_IPV6, IPV6_V6ONLY, &on, sizeof on);
    struct sockaddr_in6 a6 = {0};
    a6.sin6_family = AF_INET6;
    a6.sin6_port = 0;
    int b = bind(s6, (struct sockaddr *)&a6, sizeof a6);
    int post = setsockopt(s6, IPPROTO_IPV6, IPV6_V6ONLY, &on, sizeof on);
    int post_e = post < 0 ? errno : 0;
    printf("V6ONLY pre_ok=%d bind_ok=%d post_einval=%d got=%d\n",
           pre == 0, b == 0, post_e == EINVAL, geti(s6, IPPROTO_IPV6, IPV6_V6ONLY));
    rt("IPV6_TCLASS", s6, IPPROTO_IPV6, IPV6_TCLASS, 0x20);
    rt("IPV6_UNICAST_HOPS", s6, IPPROTO_IPV6, IPV6_UNICAST_HOPS, 55);
    rt("IPV6_RECVPKTINFO", s6, IPPROTO_IPV6, IPV6_RECVPKTINFO, 1);
    rt("IPV6_RECVHOPLIMIT", s6, IPPROTO_IPV6, IPV6_RECVHOPLIMIT, 1);
    rt("IPV6_RECVTCLASS", s6, IPPROTO_IPV6, IPV6_RECVTCLASS, 1);
    close(s6);
    return 0;
}
