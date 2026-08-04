// socketpair(AF_UNIX, SOCK_STREAM): full-duplex round-trip in both directions over
// the same connected pair, plus SO_TYPE confirmation. Verifies each endpoint can
// send and receive independently. Deterministic content echoes -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) < 0) { perror("socketpair"); return 1; }
    int ty = -1;
    socklen_t l = sizeof ty;
    getsockopt(sv[0], SOL_SOCKET, SO_TYPE, &ty, &l);

    write(sv[0], "a->b", 4);
    char b1[8] = {0};
    ssize_t n1 = read(sv[1], b1, sizeof b1 - 1);
    b1[n1 > 0 ? n1 : 0] = 0;

    write(sv[1], "b->a!", 5);
    char b2[8] = {0};
    ssize_t n2 = read(sv[0], b2, sizeof b2 - 1);
    b2[n2 > 0 ? n2 : 0] = 0;

    printf("type_stream=%d fwd=%s rev=%s\n", ty == SOCK_STREAM, b1, b2);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
