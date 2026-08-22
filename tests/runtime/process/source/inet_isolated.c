#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/* An isolated network namespace is loopback-only, not socket-less: Linux
   creates inet sockets and refuses the route instead. */
static int created(int family, int type) {
    int descriptor = socket(family, type, 0);
    if (descriptor < 0) return 0;
    close(descriptor);
    return 1;
}

static int connect_errno(int family, int type, const void *address, unsigned length) {
    int descriptor = socket(family, type, 0);
    if (descriptor < 0) return 0;
    errno = 0;
    int result = connect(descriptor, (const struct sockaddr *)address, length);
    int captured = result < 0 ? errno : 0;
    close(descriptor);
    return captured;
}

int main(void) {
    int stream4 = created(AF_INET, SOCK_STREAM);
    int datagram4 = created(AF_INET, SOCK_DGRAM);
    int stream6 = created(AF_INET6, SOCK_STREAM);

    struct sockaddr_in external4;
    memset(&external4, 0, sizeof external4);
    external4.sin_family = AF_INET;
    external4.sin_port = htons(53);
    external4.sin_addr.s_addr = htonl(0x08080808);

    struct sockaddr_in6 external6;
    memset(&external6, 0, sizeof external6);
    external6.sin6_family = AF_INET6;
    external6.sin6_port = htons(53);
    external6.sin6_addr.s6_addr[0] = 0x20;
    external6.sin6_addr.s6_addr[1] = 0x01;
    external6.sin6_addr.s6_addr[15] = 0x01;

    /* Datagram connect is a pure route lookup, so it cannot block on a
       reachable destination and still proves the route is refused. */
    int routed4 = connect_errno(AF_INET, SOCK_DGRAM, &external4, sizeof external4) == ENETUNREACH;
    int routed6 = connect_errno(AF_INET6, SOCK_DGRAM, &external6, sizeof external6) == ENETUNREACH;

    struct sockaddr_in loopback;
    memset(&loopback, 0, sizeof loopback);
    loopback.sin_family = AF_INET;
    loopback.sin_port = htons(9);
    loopback.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int local = connect_errno(AF_INET, SOCK_DGRAM, &loopback, sizeof loopback) == 0;

    printf("inet isolated stream4=%d datagram4=%d stream6=%d external4=%d external6=%d loopback=%d\n", stream4,
           datagram4, stream6, routed4, routed6, local);
    return !(stream4 && datagram4 && stream6 && routed4 && routed6 && local);
}
