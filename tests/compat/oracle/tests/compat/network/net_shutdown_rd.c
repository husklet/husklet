// shutdown(SHUT_RD) makes subsequent recv return end-of-file (0) even though the peer
// is still connected, while the write direction stays open so the peer keeps receiving.
// Confirms half-close read semantics over a loopback pair. -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    shutdown(sv[0], SHUT_RD);
    char b[8];
    ssize_t rd = recv(sv[0], b, sizeof b, 0); // must be EOF
    // write direction still usable
    ssize_t wr = write(sv[0], "still", 5);
    char pb[8] = {0};
    ssize_t pn = read(sv[1], pb, sizeof pb - 1);
    pb[pn > 0 ? pn : 0] = 0;
    printf("recv_after_shutrd=%zd wrote=%zd peer_got=%s\n", rd, wr, pb);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
