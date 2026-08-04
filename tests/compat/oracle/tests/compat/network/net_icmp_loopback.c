// Unprivileged ICMP datagram socket (socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP))
// pinging 127.0.0.1 -- the container-healthcheck / `ping localhost` case. On the
// oracle VM net.ipv4.ping_group_range admits the caller's gid, so create + echo
// round-trip over the loopback stack succeeds. The engine models loopback with a
// private AF_UNIX switch; this asserts the create errno and (if created) that a
// send+recv completes cleanly with a matching echo reply, and never hangs
// (SO_RCVTIMEO + watchdog bound the call).
#include "net_util.h"
#include <netinet/in.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <stdint.h>

struct echo {
    uint8_t type, code;
    uint16_t checksum, identifier, sequence;
    uint8_t payload[8];
};

static uint16_t checksum(const void *data, size_t size) {
    const uint8_t *b = data;
    uint32_t sum = 0;
    for (size_t i = 0; i + 1 < size; i += 2)
        sum += (uint32_t)((b[i] << 8) | b[i + 1]);
    if (size & 1) sum += (uint32_t)b[size - 1] << 8;
    while (sum >> 16)
        sum = (sum & 0xffffu) + (sum >> 16);
    return htons((uint16_t)~sum);
}

int main(void) {
    net_watchdog(10);
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP);
    if (fd < 0) {
        printf("icmp_create=%s\n", err_name(errno));
        return 0;
    }
    printf("icmp_create=OK\n");

    struct timeval to = {.tv_sec = 1};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);

    struct sockaddr_in d;
    memset(&d, 0, sizeof d);
    d.sin_family = AF_INET;
    d.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    struct echo req;
    memset(&req, 0, sizeof req);
    req.type = 8;
    req.identifier = htons(0x4321);
    req.sequence = htons(1);
    memcpy(req.payload, "hlping", 6);
    req.checksum = checksum(&req, sizeof req);

    ssize_t w = sendto(fd, &req, sizeof req, 0, (struct sockaddr *)&d, sizeof d);
    printf("icmp_send=%s\n", w == (ssize_t)sizeof req ? "OK" : err_name(errno));

    struct echo reply;
    ssize_t r = recv(fd, &reply, sizeof reply, 0);
    // Do not assert the identifier: an unprivileged ICMP datagram socket has the kernel rewrite the echo
    // id to the socket's port, so the reply id is kernel-chosen and not container-invariant. Sequence and
    // payload round-trip on both native and the engine's local reflection.
    int echo_ok = r == (ssize_t)sizeof reply && reply.type == 0 && reply.code == 0 && reply.sequence == req.sequence &&
                  memcmp(reply.payload, req.payload, sizeof reply.payload) == 0;
    printf("icmp_reply=%s echo_ok=%d\n", r > 0 ? "OK" : err_name(errno), echo_ok);

    int flags = fcntl(fd, F_GETFL);
    int flood_ok = flags >= 0 && fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0;
    for (unsigned i = 0; flood_ok && i < 2048; i++) {
        req.sequence = htons((uint16_t)(i + 2));
        req.checksum = 0;
        req.checksum = checksum(&req, sizeof req);
        w = sendto(fd, &req, sizeof req, 0, (struct sockaddr *)&d, sizeof d);
        if (w != (ssize_t)sizeof req && errno != EAGAIN && errno != ENOBUFS) flood_ok = 0;
    }
    r = recv(fd, &reply, sizeof reply, 0);
    int drained =
        r == (ssize_t)sizeof reply && reply.type == 0 && memcmp(reply.payload, req.payload, sizeof reply.payload) == 0;
    printf("icmp_flood=%d drained=%d\n", flood_ok, drained);
    close(fd);
    return 0;
}
