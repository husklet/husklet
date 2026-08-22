#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/epoll.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_ready(int descriptor, short events) {
    struct pollfd item = {.fd = descriptor, .events = events};
    return poll(&item, 1, 2000) == 1 && (item.revents & events) != 0;
}

static int wait_epoll(int descriptor, uint32_t events) {
    int epoll = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event request = {.events = events, .data.fd = descriptor};
    struct epoll_event result = {0};
    int ready = epoll >= 0 && epoll_ctl(epoll, EPOLL_CTL_ADD, descriptor, &request) == 0 &&
                epoll_wait(epoll, &result, 1, 2000) == 1 && (result.events & events) != 0;
    if (epoll >= 0) close(epoll);
    return ready;
}

static int tcp4(void) {
    int listener = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    int one = 1;
    struct sockaddr_in local = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    socklen_t length = sizeof(local);
    if (listener < 0 || setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one)) ||
        bind(listener, (struct sockaddr *)&local, sizeof(local)) ||
        getsockname(listener, (struct sockaddr *)&local, &length) || local.sin_port == 0 || listen(listener, 4))
        return 0;
    int client = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    int buffer = 32768;
    if (client < 0 || setsockopt(client, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof(one)) ||
        setsockopt(client, SOL_SOCKET, SO_SNDBUF, &buffer, sizeof(buffer)) ||
        setsockopt(client, SOL_SOCKET, SO_RCVBUF, &buffer, sizeof(buffer)) ||
        setsockopt(client, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one)))
        return 0;
    buffer = 0;
    length = sizeof(buffer);
    if (getsockopt(client, SOL_SOCKET, SO_SNDBUF, &buffer, &length) || buffer <= 0) return 0;
    buffer = 0;
    length = sizeof(buffer);
    if (getsockopt(client, SOL_SOCKET, SO_RCVBUF, &buffer, &length) || buffer <= 0) return 0;
    int connected = connect(client, (struct sockaddr *)&local, sizeof(local));
    if (connected < 0 && errno != EINPROGRESS) return 0;
    if (!wait_ready(client, POLLOUT) || !wait_ready(listener, POLLIN)) return 0;
    int error = -1;
    length = sizeof(error);
    if (getsockopt(client, SOL_SOCKET, SO_ERROR, &error, &length) || error) return 0;
    struct sockaddr_in peer = {0};
    length = sizeof(peer);
    int accepted = accept4(listener, (struct sockaddr *)&peer, &length, SOCK_NONBLOCK | SOCK_CLOEXEC);
    if (accepted < 0) return 0;
    struct sockaddr_in client_peer = {0};
    length = sizeof(client_peer);
    if (getpeername(client, (struct sockaddr *)&client_peer, &length) || client_peer.sin_family != AF_INET ||
        client_peer.sin_port != local.sin_port)
        return 0;
    struct sockaddr_in server_peer = {0};
    length = sizeof(server_peer);
    if (getpeername(accepted, (struct sockaddr *)&server_peer, &length) || server_peer.sin_family != AF_INET ||
        server_peer.sin_port == 0)
        return 0;
    int bits = (fcntl(accepted, F_GETFD) & FD_CLOEXEC) ? 1 : 0;
    char sent[] = "tcp4";
    char received[8] = {0};
    pid_t child = fork();
    if (child == 0) _exit(send(client, sent, sizeof(sent), 0) == (ssize_t)sizeof(sent) ? 0 : 1);
    int status = 0;
    if (child > 0) bits |= 2;
    if (waitpid(child, &status, 0) == child) bits |= 32;
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) bits |= 64;
    if (wait_ready(accepted, POLLIN) && wait_epoll(accepted, EPOLLIN)) bits |= 4;
    if (recv(accepted, received, sizeof(received), 0) == (ssize_t)sizeof(sent)) bits |= 8;
    if (!memcmp(sent, received, sizeof(sent))) bits |= 16;
    if (shutdown(client, SHUT_WR) == 0) bits |= 128;
    close(listener);
    close(client);
    close(accepted);
    int recycled = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (recycled < 0) return 0;
    close(recycled);
    return bits == 255;
}

static int udp_family(int family) {
    int receiver = socket(family, SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    int sender = socket(family, SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (receiver < 0 || sender < 0) return 0;
    struct sockaddr_storage address = {0};
    socklen_t length;
    if (family == AF_INET) {
        struct sockaddr_in *value = (struct sockaddr_in *)&address;
        value->sin_family = AF_INET;
        value->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        length = sizeof(*value);
    } else {
        int one = 1;
        if (setsockopt(receiver, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof(one))) return 0;
        struct sockaddr_in6 *value = (struct sockaddr_in6 *)&address;
        value->sin6_family = AF_INET6;
        value->sin6_addr = in6addr_loopback;
        length = sizeof(*value);
    }
    if (bind(receiver, (struct sockaddr *)&address, length) ||
        getsockname(receiver, (struct sockaddr *)&address, &length))
        return 0;
    char sent[] = "udp";
    if (sendto(sender, sent, sizeof(sent), 0, (struct sockaddr *)&address, length) != (ssize_t)sizeof(sent) ||
        !wait_ready(receiver, POLLIN))
        return 0;
    char received[8] = {0};
    struct sockaddr_storage source = {0};
    socklen_t source_length = sizeof(source);
    int count = recvfrom(receiver, received, sizeof(received), 0, (struct sockaddr *)&source, &source_length);
    int ok = count == (int)sizeof(sent) && !memcmp(sent, received, sizeof(sent)) && source.ss_family == family;
    close(sender);
    close(receiver);
    return ok;
}

int main(void) {
    int stream4 = tcp4();
    int datagram4 = udp_family(AF_INET);
    int datagram6 = udp_family(AF_INET6);
    printf("inet stream4=%d datagram4=%d datagram6=%d\n", stream4, datagram4, datagram6);
    return !(stream4 && datagram4 && datagram6);
}
