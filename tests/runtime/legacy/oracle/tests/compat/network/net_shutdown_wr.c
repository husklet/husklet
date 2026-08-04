// shutdown(SHUT_WR) sends a FIN so the peer's recv observes end-of-file (0) after
// draining any buffered data, while this endpoint can still receive. Confirms the
// write-half-close signals EOF exactly once over a loopback pair. -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    write(sv[0], "last", 4);
    shutdown(sv[0], SHUT_WR);

    char b[8] = {0};
    ssize_t n1 = recv(sv[1], b, sizeof b - 1, 0); // buffered data
    b[n1 > 0 ? n1 : 0] = 0;
    ssize_t n2 = recv(sv[1], b, sizeof b, 0);      // EOF
    // reverse direction still works
    write(sv[1], "reply", 5);
    char r[8] = {0};
    ssize_t n3 = recv(sv[0], r, sizeof r - 1, 0);
    r[n3 > 0 ? n3 : 0] = 0;
    printf("data=%s eof=%zd reverse=%s\n", b, n2, r);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
