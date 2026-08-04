#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int listener(int family, int port) {
    int fd = socket(family, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (family == AF_INET6 && setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof one) != 0)
        return -1;
    if (family == AF_INET6) {
        struct sockaddr_in6 address = {
            .sin6_family = AF_INET6,
            .sin6_port = htons((uint16_t)port),
            .sin6_addr = IN6ADDR_ANY_INIT,
        };
        if (bind(fd, (struct sockaddr *)&address, sizeof address) != 0) return -1;
    } else {
        struct sockaddr_in address = {
            .sin_family = AF_INET,
            .sin_port = htons((uint16_t)port),
            .sin_addr.s_addr = htonl(INADDR_ANY),
        };
        if (bind(fd, (struct sockaddr *)&address, sizeof address) != 0) return -1;
    }
    return listen(fd, 4) == 0 ? fd : -1;
}

int main(void) {
    int port = 16000 + (getpid() % 40000);
    int v6 = listener(AF_INET6, port);
    int v4 = listener(AF_INET, port);
    printf("v6=%d v4=%d\n", v6 >= 0, v4 >= 0);
    if (v6 >= 0) close(v6);
    if (v4 >= 0) close(v4);
    return 0;
}
