// IPPROTO_TCP socket options and SIOCOUTQ on a CONNECTED TCP loopback socket. Under the engine a private
// 127.0.0.1 stream socket is backed by an AF_UNIX switch socket once connect() runs, and the host rejects
// every IPPROTO_TCP getsockopt/setsockopt (and the SIOCOUTQ ioctl) on an AF_UNIX fd with ENOPROTOOPT/ENOTTY.
// Linux round-trips these options on a real TCP socket, and applications (Redis, gRPC, Postgres) set
// TCP_NODELAY *after* connect(), so the values must survive the switch. Deterministic -> oracle-checked.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <errno.h>
#include "net_util.h"

static int mkpair(int *c, int *s) {
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 4);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);
    int cc = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(cc, (struct sockaddr *)&a, sizeof a) < 0) return -1;
    int ss = accept(ls, NULL, NULL);
    close(ls);
    *c = cc;
    *s = ss;
    return 0;
}

int main(void) {
    net_watchdog(10);
    int c, s;
    if (mkpair(&c, &s)) {
        printf("pairfail\n");
        return 1;
    }
    int v;
    socklen_t l;
    // TCP_NODELAY set-after-connect round-trips (the headline case).
    v = 1;
    int set1 = setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &v, sizeof v);
    v = -1;
    l = sizeof v;
    getsockopt(c, IPPROTO_TCP, TCP_NODELAY, &v, &l);
    printf("nodelay set=%d get=%d\n", set1, v);
    v = 0;
    setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &v, sizeof v);
    v = -1;
    l = sizeof v;
    getsockopt(c, IPPROTO_TCP, TCP_NODELAY, &v, &l);
    printf("nodelay_off get=%d\n", v);
    // TCP_CORK round-trips.
    v = 1;
    setsockopt(c, IPPROTO_TCP, TCP_CORK, &v, sizeof v);
    v = -1;
    l = sizeof v;
    getsockopt(c, IPPROTO_TCP, TCP_CORK, &v, &l);
    printf("cork get=%d\n", v);
    // TCP keepalive tunables round-trip on the connected fd.
    v = 55;
    setsockopt(c, IPPROTO_TCP, TCP_KEEPIDLE, &v, sizeof v);
    v = -1;
    l = sizeof v;
    getsockopt(c, IPPROTO_TCP, TCP_KEEPIDLE, &v, &l);
    printf("keepidle get=%d\n", v);
    v = 7;
    setsockopt(c, IPPROTO_TCP, TCP_KEEPCNT, &v, sizeof v);
    v = -1;
    l = sizeof v;
    getsockopt(c, IPPROTO_TCP, TCP_KEEPCNT, &v, &l);
    printf("keepcnt get=%d\n", v);
    // TCP_MAXSEG is readable and nonzero (exact value host-variable -> only assert nonzero).
    v = -1;
    l = sizeof v;
    int msr = getsockopt(c, IPPROTO_TCP, TCP_MAXSEG, &v, &l);
    printf("maxseg ok=%d nonzero=%d\n", msr == 0, v > 0);
    // The switch must still report the guest's INET identity, not the AF_UNIX backing.
    v = -1;
    l = sizeof v;
    getsockopt(c, SOL_SOCKET, SO_TYPE, &v, &l);
    printf("sotype=%d\n", v);
    v = -1;
    l = sizeof v;
    getsockopt(c, SOL_SOCKET, SO_DOMAIN, &v, &l);
    printf("sodomain=%d\n", v);
    v = -1;
    l = sizeof v;
    getsockopt(c, SOL_SOCKET, SO_PROTOCOL, &v, &l);
    printf("soproto=%d\n", v);
    // TCP_INFO reports success with tcpi_state == TCP_ESTABLISHED for a connected socket.
    struct tcp_info ti;
    memset(&ti, 0, sizeof ti);
    l = sizeof ti;
    int rti = getsockopt(c, IPPROTO_TCP, TCP_INFO, &ti, &l);
    printf("tcp_info ok=%d established=%d\n", rti == 0, ti.tcpi_state == TCP_ESTABLISHED);
    // SIOCOUTQ answers without error on the connected stream socket.
    int oq = -1;
    int ro = ioctl(c, TIOCOUTQ, &oq);
    printf("outq_ok=%d\n", ro == 0);
    close(c);
    close(s);
    return 0;
}
