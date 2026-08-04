// connect/bind edge semantics that container apps and net utilities rely on.
// All facts are stack-level and container-invariant: the loopback 127.0.0.1
// stack behavior does not depend on the synthesized interface set, so native
// oracle and engine must agree byte-for-byte.
#include "net_util.h"
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/socket.h>

static struct sockaddr_in lo(int port) {
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = htons(port);
    return a;
}

int main(void) {
    net_watchdog(10);

    // connect(AF_UNSPEC) dissolves a UDP association; getpeername then ENOTCONN.
    {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        struct sockaddr_in a = lo(9);
        connect(fd, (struct sockaddr *)&a, sizeof a);
        struct sockaddr_in un;
        memset(&un, 0, sizeof un);
        un.sin_family = AF_UNSPEC;
        int r = connect(fd, (struct sockaddr *)&un, sizeof un);
        printf("udp_disconnect_unspec=%s\n", r < 0 ? err_name(errno) : "OK");
        struct sockaddr_in pn;
        socklen_t pl = sizeof pn;
        r = getpeername(fd, (struct sockaddr *)&pn, &pl);
        printf("getpeername_after_disconnect=%s\n", r < 0 ? err_name(errno) : "OK");
        close(fd);
    }

    // double-connect a UDP socket re-points it (no error).
    {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        struct sockaddr_in a = lo(1111);
        connect(fd, (struct sockaddr *)&a, sizeof a);
        struct sockaddr_in b = lo(2222);
        int r = connect(fd, (struct sockaddr *)&b, sizeof b);
        struct sockaddr_in pn;
        socklen_t pl = sizeof pn;
        int r2 = getpeername(fd, (struct sockaddr *)&pn, &pl);
        printf("udp_reconnect=%s repointed=%d\n", r < 0 ? err_name(errno) : "OK",
               r2 == 0 && ntohs(pn.sin_port) == 2222);
        close(fd);
    }

    // getpeername on an unconnected stream socket -> ENOTCONN;
    // getsockname on an unbound socket -> wildcard port 0.
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in pn;
        socklen_t pl = sizeof pn;
        int r = getpeername(fd, (struct sockaddr *)&pn, &pl);
        printf("getpeername_unconn=%s\n", r < 0 ? err_name(errno) : "OK");
        struct sockaddr_in sn;
        socklen_t sl = sizeof sn;
        r = getsockname(fd, (struct sockaddr *)&sn, &sl);
        printf("getsockname_unbound=%s port0=%d\n", r < 0 ? err_name(errno) : "OK",
               r == 0 && sn.sin_port == 0);
        close(fd);
    }

    // bind an already-bound socket -> EINVAL.
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in a = lo(23456);
        bind(fd, (struct sockaddr *)&a, sizeof a);
        struct sockaddr_in b = lo(23457);
        int r = bind(fd, (struct sockaddr *)&b, sizeof b);
        printf("double_bind=%s\n", r < 0 ? err_name(errno) : "OK");
        close(fd);
    }

    // bind to a non-local address -> EADDRNOTAVAIL.
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in a;
        memset(&a, 0, sizeof a);
        a.sin_family = AF_INET;
        inet_pton(AF_INET, "1.2.3.4", &a.sin_addr);
        a.sin_port = htons(23458);
        int r = bind(fd, (struct sockaddr *)&a, sizeof a);
        printf("bind_nonlocal=%s\n", r < 0 ? err_name(errno) : "OK");
        close(fd);
    }

    // bind to an in-use address -> EADDRINUSE.
    {
        int a1 = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in a = lo(23459);
        bind(a1, (struct sockaddr *)&a, sizeof a);
        listen(a1, 4);
        int a2 = socket(AF_INET, SOCK_STREAM, 0);
        int r = bind(a2, (struct sockaddr *)&a, sizeof a);
        printf("bind_inuse=%s\n", r < 0 ? err_name(errno) : "OK");
        close(a1);
        close(a2);
    }

    // connect on a listening socket -> EISCONN.
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in a = lo(23460);
        bind(fd, (struct sockaddr *)&a, sizeof a);
        listen(fd, 4);
        struct sockaddr_in d = lo(23461);
        int r = connect(fd, (struct sockaddr *)&d, sizeof d);
        printf("connect_listening=%s\n", r < 0 ? err_name(errno) : "OK");
        close(fd);
    }

    return 0;
}
