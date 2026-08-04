#include <arpa/inet.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

struct echo {
    uint8_t type;
    uint8_t code;
    uint16_t checksum;
    uint16_t identifier;
    uint16_t sequence;
    uint8_t payload[8];
};

static uint16_t checksum(const void *data, size_t size) {
    const uint8_t *bytes = data;
    uint32_t sum = 0;
    while (size > 1) {
        sum += (uint32_t)((bytes[0] << 8) | bytes[1]);
        bytes += 2;
        size -= 2;
    }
    if (size) sum += (uint32_t)bytes[0] << 8;
    while (sum >> 16) sum = (sum & 0xffffu) + (sum >> 16);
    return htons((uint16_t)~sum);
}

int main(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP);
    if (fd < 0) return 1;
    struct timeval timeout = {.tv_sec = 1};
    if (setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout) < 0) return 2;

    struct sockaddr_in peer = {.sin_family = AF_INET};
    if (inet_pton(AF_INET, "172.28.0.9", &peer.sin_addr) != 1) return 3;
    struct echo request = {.type = 8, .identifier = htons(0x1234), .sequence = htons(7)};
    memcpy(request.payload, "hl-icmp", 7);
    request.checksum = checksum(&request, sizeof request);
    if (connect(fd, (struct sockaddr *)&peer, sizeof peer) < 0) return 4;
    int moved = dup(fd);
    close(fd);
    if (moved < 0 || write(moved, &request, sizeof request) != sizeof request) return 4;
    fd = moved;

    struct echo packet;
    struct sockaddr_in source;
    socklen_t source_size = sizeof source;
    ssize_t size = recvfrom(fd, &packet, sizeof packet, 0, (struct sockaddr *)&source, &source_size);
    close(fd);
    struct echo *reply = &packet;
    if (size != sizeof *reply || reply->type != 0 || reply->code != 0 || reply->identifier != request.identifier ||
        reply->sequence != request.sequence || memcmp(reply->payload, request.payload, sizeof reply->payload) != 0 ||
        source.sin_addr.s_addr != peer.sin_addr.s_addr)
        return 5;
    puts("icmp echo=1 source=172.28.0.9");
    return 0;
}
