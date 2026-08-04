#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef MSG_CMSG_CLOEXEC
#define MSG_CMSG_CLOEXEC 0x40000000
#endif

static int fd_cloexec(int fd) {
    int fl = fcntl(fd, F_GETFD);
    return fl >= 0 && (fl & FD_CLOEXEC);
}

static int fd_nonblock(int fd) {
    int fl = fcntl(fd, F_GETFL);
    return fl >= 0 && (fl & O_NONBLOCK);
}

int main(void) {
    int sv[2];
    int clo = 0, nb = 0, msg = 0, recvclo = 0, passcred = 0;
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC | SOCK_NONBLOCK, 0, sv) != 0) {
        printf("seqpacket_flags cloexec=0 nonblock=0 msg=0 recvclo=0 passcred=0\n");
        return 1;
    }
    clo = fd_cloexec(sv[0]) && fd_cloexec(sv[1]);
    nb = fd_nonblock(sv[0]) && fd_nonblock(sv[1]);

    int on = 1;
    setsockopt(sv[1], SOL_SOCKET, SO_PASSCRED, &on, sizeof(on));

    int p[2];
    if (pipe(p) != 0) return 1;
    char x = 'Z';
    if (write(p[1], &x, 1) != 1) return 1;

    char payload[] = "abcdef";
    struct iovec siov = {.iov_base = payload, .iov_len = sizeof(payload) - 1};
    char scbuf[CMSG_SPACE(sizeof(int))];
    memset(scbuf, 0, sizeof(scbuf));
    struct msghdr smsg = {0};
    smsg.msg_iov = &siov;
    smsg.msg_iovlen = 1;
    smsg.msg_control = scbuf;
    smsg.msg_controllen = sizeof(scbuf);
    struct cmsghdr *sc = CMSG_FIRSTHDR(&smsg);
    sc->cmsg_level = SOL_SOCKET;
    sc->cmsg_type = SCM_RIGHTS;
    sc->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(sc), &p[0], sizeof(int));
    smsg.msg_controllen = sc->cmsg_len;
    if (sendmsg(sv[0], &smsg, 0) != (ssize_t)(sizeof(payload) - 1)) return 1;

    char rb[3] = {0};
    char rcbuf[CMSG_SPACE(sizeof(int)) + CMSG_SPACE(sizeof(struct ucred))];
    memset(rcbuf, 0, sizeof(rcbuf));
    struct iovec riov = {.iov_base = rb, .iov_len = sizeof(rb)};
    struct msghdr rmsg = {0};
    rmsg.msg_iov = &riov;
    rmsg.msg_iovlen = 1;
    rmsg.msg_control = rcbuf;
    rmsg.msg_controllen = sizeof(rcbuf);
    ssize_t rn = recvmsg(sv[1], &rmsg, MSG_CMSG_CLOEXEC);
    msg = rn == 3 && (rmsg.msg_flags & MSG_TRUNC) && !memcmp(rb, "abc", 3);

    int gotfd = -1;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&rmsg); c; c = CMSG_NXTHDR(&rmsg, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
            memcpy(&gotfd, CMSG_DATA(c), sizeof(int));
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
            struct ucred cr;
            memcpy(&cr, CMSG_DATA(c), sizeof(cr));
            passcred = cr.uid == getuid() && cr.gid == getgid() && cr.pid > 0;
        }
    }
    if (gotfd >= 0) {
        recvclo = fd_cloexec(gotfd);
        char got = 0;
        msg = msg && read(gotfd, &got, 1) == 1 && got == 'Z';
        close(gotfd);
    }
    close(p[0]);
    close(p[1]);
    close(sv[0]);
    close(sv[1]);

    printf("seqpacket_flags cloexec=%d nonblock=%d msg=%d recvclo=%d passcred=%d\n",
           clo, nb, msg, recvclo, passcred);
    return clo && nb && msg && recvclo && passcred ? 0 : 1;
}
