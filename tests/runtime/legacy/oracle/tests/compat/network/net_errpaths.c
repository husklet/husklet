// Datagram/stream error paths with symbolic errno: send() on an unconnected UDP
// socket with no destination fails EDESTADDRREQ; an oversized UDP datagram fails
// EMSGSIZE; and write() to the local end of a pair whose peer has closed, after our
// own SHUT_WR, fails EPIPE (SIGPIPE suppressed). Deterministic errno set -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    signal(SIGPIPE, SIG_IGN);

    int u = socket(AF_INET, SOCK_DGRAM, 0);
    errno = 0;
    ssize_t s1 = send(u, "x", 1, 0); // no peer -> EDESTADDRREQ
    const char *e_noaddr = err_name(errno);

    // oversized datagram to loopback -> EMSGSIZE
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = htons(9); // discard-ish; message never leaves due to size check
    static char big[70000];
    memset(big, 'A', sizeof big);
    errno = 0;
    ssize_t s2 = sendto(u, big, sizeof big, 0, (struct sockaddr *)&a, sizeof a);
    const char *e_size = err_name(errno);
    close(u);

    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    close(sv[1]);          // peer gone
    errno = 0;
    ssize_t s3 = send(sv[0], "y", 1, 0); // write to broken pipe -> EPIPE
    const char *e_pipe = err_name(errno);
    close(sv[0]);

    printf("noaddr=%zd/%s emsgsize=%zd/%s epipe=%zd/%s\n", s1, e_noaddr, s2, e_size, s3, e_pipe);
    return 0;
}
