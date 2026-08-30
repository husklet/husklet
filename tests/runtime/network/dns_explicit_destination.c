#define _GNU_SOURCE
#include <arpa/inet.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int query(const char *server, int connected, int message) {
    static const unsigned char request[] = {
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x09, 'l',  'o',  'c',  'a',  'l',  'h',  'o',  's',  't',  0x00, 0x00, 0x01, 0x00, 0x01,
    };
    struct sockaddr_in ns = {.sin_family = AF_INET, .sin_port = htons(53)};
    ns.sin_addr.s_addr = inet_addr(server);
    int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    if (fd < 0 || (connected && connect(fd, (struct sockaddr *)&ns, sizeof ns) != 0)) return 0;

    ssize_t sent;
    if (message) {
        struct iovec iov = {.iov_base = (void *)request, .iov_len = sizeof request};
        struct msghdr msg = {.msg_name = connected ? NULL : &ns,
                             .msg_namelen = connected ? 0 : sizeof ns,
                             .msg_iov = &iov,
                             .msg_iovlen = 1};
        sent = sendmsg(fd, &msg, 0);
    } else {
        sent = connected ? send(fd, request, sizeof request, 0)
                         : sendto(fd, request, sizeof request, 0, (struct sockaddr *)&ns, sizeof ns);
    }
    struct pollfd pollfd = {.fd = fd, .events = POLLIN};
    unsigned char response[512];
    struct sockaddr_in from;
    socklen_t from_size = sizeof from;
    int ready = poll(&pollfd, 1, 200);
    ssize_t received = ready == 1 ? recvfrom(fd, response, sizeof response, 0, (struct sockaddr *)&from, &from_size) : -1;
    close(fd);
    return sent == (ssize_t)sizeof request && received >= 12 && (response[3] & 15) == 0 &&
           from.sin_family == AF_INET && from.sin_port == htons(53) && from.sin_addr.s_addr == ns.sin_addr.s_addr;
}

static int ordinary_udp(void) {
    int receiver = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    int sender = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_port = 0, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    socklen_t size = sizeof address;
    if (receiver < 0 || sender < 0 || bind(receiver, (struct sockaddr *)&address, sizeof address) != 0 ||
        getsockname(receiver, (struct sockaddr *)&address, &size) != 0 || address.sin_port == htons(53))
        return 0;
    char sent = 'x', received = 0;
    int ok = sendto(sender, &sent, 1, 0, (struct sockaddr *)&address, sizeof address) == 1 &&
             recv(receiver, &received, 1, 0) == 1 && received == sent;
    close(sender);
    close(receiver);
    return ok;
}

int main(void) {
    int send_connected = query("8.8.8.8", 1, 0);
    int send_to = query("8.8.8.8", 0, 0);
    int message_connected = query("8.8.8.8", 1, 1);
    int message_to = query("8.8.8.8", 0, 1);
    // The preceding closes leave their descriptor numbers available. A new embedded-NS socket must not
    // inherit 8.8.8.8 as its reported source when that number is reused.
    int reused = query("127.0.0.11", 0, 0);
    printf("dns-explicit send=%d sendto=%d sendmsg-connected=%d sendmsg-to=%d reused=%d udp=%d\n", send_connected,
           send_to, message_connected, message_to, reused, ordinary_udp());
    return 0;
}
