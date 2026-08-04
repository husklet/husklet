// AF_UNIX datagram socketpair message boundaries: MSG_PEEK leaves the datagram queued,
// MSG_TRUNC reports the full length while a short buffer discards the tail, MSG_DONTWAIT on an
// empty queue is EAGAIN, and a zero-length datagram is delivered as a real message.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv)) return 1;
    char msg[64];
    memset(msg, 'a', sizeof msg);
    ssize_t s1 = send(sv[0], msg, 40, 0);
    char buf[16];
    ssize_t p1 = recv(sv[1], buf, sizeof buf, MSG_PEEK);
    ssize_t p2 = recv(sv[1], buf, sizeof buf, MSG_PEEK | MSG_TRUNC);
    ssize_t r1 = recv(sv[1], buf, sizeof buf, 0); // truncated, remainder dropped
    ssize_t r2 = recv(sv[1], buf, sizeof buf, MSG_DONTWAIT);
    int e2 = (r2 == -1) ? errno : 0;

    ssize_t z = send(sv[0], msg, 0, 0);
    ssize_t rz = recv(sv[1], buf, sizeof buf, 0);
    ssize_t r3 = recv(sv[1], buf, sizeof buf, MSG_DONTWAIT);
    int e3 = (r3 == -1) ? errno : 0;

    // shutdown then read reports EOF, and sending on a shut-down peer is EPIPE
    ssize_t s2 = send(sv[0], msg, 8, 0);
    shutdown(sv[0], SHUT_RDWR);
    ssize_t r4 = recv(sv[1], buf, sizeof buf, MSG_DONTWAIT);
    ssize_t r5 = recv(sv[1], buf, sizeof buf, MSG_DONTWAIT);
    int e5 = (r5 == -1) ? errno : 0;
    printf("s1=%zd p1=%zd p2=%zd r1=%zd r2=%zd e2=%d z=%zd rz=%zd r3=%zd e3=%d s2=%zd r4=%zd r5=%zd e5=%d\n",
           s1, p1, p2, r1, r2, e2, z, rz, r3, e3, s2, r4, r5, e5);
    return 0;
}
