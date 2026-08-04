// SO_PASSCRED / SCM_CREDENTIALS / SO_PEERCRED over a UNIX socketpair: the receiver
// enables credential passing and confirms the ancillary struct ucred carries the
// process's own pid/uid/gid. Compared against getpid/getuid/getgid as booleans so
// the golden output is identity-independent and deterministic. -> oracle.
#define _GNU_SOURCE
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    int one = 1;
    setsockopt(sv[1], SOL_SOCKET, SO_PASSCRED, &one, sizeof one);

    struct ucred peer = {0};
    socklen_t pl = sizeof peer;
    int pr = getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &peer, &pl);
    int peer_ok = pr == 0 && peer.pid == getpid() && peer.uid == getuid() && peer.gid == getgid();

    write(sv[0], "c", 1);
    struct msghdr msg = {0};
    char io[1] = {0};
    struct iovec iov = {io, 1};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    char cbuf[CMSG_SPACE(sizeof(struct ucred))];
    memset(cbuf, 0, sizeof cbuf);
    msg.msg_control = cbuf;
    msg.msg_controllen = sizeof cbuf;
    recvmsg(sv[1], &msg, 0);
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    int cred_ok = 0;
    if (c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
        struct ucred u;
        memcpy(&u, CMSG_DATA(c), sizeof u);
        cred_ok = u.pid == getpid() && u.uid == getuid() && u.gid == getgid();
    }
    printf("peercred_ok=%d scm_creds_ok=%d\n", peer_ok, cred_ok);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
