// poll() readiness transitions on a socketpair: a fresh writable endpoint reports
// POLLOUT but not POLLIN; after the peer writes, POLLIN appears; after draining, POLLIN
// clears again. Bounded zero/short timeouts, event-bit assertions -> oracle.
#include "net_util.h"
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static short revents(int fd, short want, int timeout) {
    struct pollfd pf = {fd, want, 0};
    poll(&pf, 1, timeout);
    return pf.revents;
}

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    short r0 = revents(sv[1], POLLIN | POLLOUT, 0);
    int out_ready = (r0 & POLLOUT) != 0;
    int in_before = (r0 & POLLIN) != 0;
    write(sv[0], "x", 1);
    short r1 = revents(sv[1], POLLIN, 1000);
    int in_after = (r1 & POLLIN) != 0;
    char b[1];
    read(sv[1], b, 1);
    short r2 = revents(sv[1], POLLIN, 0);
    int in_drained = (r2 & POLLIN) != 0;
    printf("out_ready=%d in_before=%d in_after=%d in_drained=%d\n", out_ready, in_before, in_after,
           in_drained);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
