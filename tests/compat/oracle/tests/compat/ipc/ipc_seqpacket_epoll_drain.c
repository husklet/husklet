#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MSG_CMSG_CLOEXEC
#define MSG_CMSG_CLOEXEC 0x40000000
#endif

#define BOOTSTRAP_PAYLOAD "worker-bootstrap"

struct result {
    int ep1;
    int got_msg;
    int got_cred;
    int got_fd;
    int fd_cloexec;
    int drained;
    int quiet;
};

static int fd_cloexec(int fd) {
    int fl = fcntl(fd, F_GETFD);
    return fl >= 0 && (fl & FD_CLOEXEC);
}

static int send_bootstrap(int sock, int fd) {
    char payload[] = BOOTSTRAP_PAYLOAD;
    struct iovec iov = {.iov_base = payload, .iov_len = sizeof(payload) - 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    mh.msg_controllen = c->cmsg_len;
    return sendmsg(sock, &mh, MSG_DONTWAIT) == (ssize_t)(sizeof(payload) - 1) ? 0 : -1;
}

static void child_main(int sock, int ready_wr, int out_wr) {
    struct result r = {0};
    int on = 1;
    setsockopt(sock, SOL_SOCKET, SO_PASSCRED, &on, sizeof on);

    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (ep >= 0) {
        struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0xfeed5};
        if (epoll_ctl(ep, EPOLL_CTL_ADD, sock, &ev) == 0) {
            char b = 'R';
            (void)write(ready_wr, &b, 1);
            struct epoll_event out;
            r.ep1 = epoll_wait(ep, &out, 1, 2000);
            if (r.ep1 == 1 && out.data.u64 == 0xfeed5 && (out.events & EPOLLIN)) {
                char buf[64] = {0};
                struct iovec iov = {.iov_base = buf, .iov_len = sizeof buf};
                char ctl[CMSG_SPACE(sizeof(int)) + CMSG_SPACE(sizeof(struct ucred))];
                memset(ctl, 0, sizeof ctl);
                struct msghdr mh = {
                    .msg_iov = &iov,
                    .msg_iovlen = 1,
                    .msg_control = ctl,
                    .msg_controllen = sizeof ctl,
                };
                ssize_t n = recvmsg(sock, &mh, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
                r.got_msg = n == (ssize_t)(sizeof(BOOTSTRAP_PAYLOAD) - 1) &&
                            memcmp(buf, BOOTSTRAP_PAYLOAD, sizeof(BOOTSTRAP_PAYLOAD) - 1) == 0;
                int recvfd = -1;
                for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c)) {
                    if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
                        memcpy(&recvfd, CMSG_DATA(c), sizeof recvfd);
                    if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
                        struct ucred cr;
                        memcpy(&cr, CMSG_DATA(c), sizeof cr);
                        r.got_cred = cr.pid > 0 && cr.uid == getuid() && cr.gid == getgid();
                    }
                }
                if (recvfd >= 0) {
                    char x = 0;
                    r.got_fd = read(recvfd, &x, 1) == 1 && x == 'Z';
                    r.fd_cloexec = fd_cloexec(recvfd);
                    close(recvfd);
                }
                n = recvmsg(sock, &mh, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
                r.drained = n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK);
                r.quiet = epoll_wait(ep, &out, 1, 0) == 0;
            }
        }
    }
    (void)write(out_wr, &r, sizeof r);
    _exit(r.ep1 == 1 && r.got_msg && r.got_cred && r.got_fd && r.fd_cloexec && r.drained && r.quiet ? 0 : 7);
}

int main(void) {
    int sv[2], ready[2], out[2], p[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC | SOCK_NONBLOCK, 0, sv) != 0 ||
        pipe(ready) != 0 || pipe(out) != 0 || pipe(p) != 0) {
        perror("setup");
        return 1;
    }
    char z = 'Z';
    if (write(p[1], &z, 1) != 1) {
        perror("pipe write");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        close(sv[0]);
        close(ready[0]);
        close(out[0]);
        close(p[0]);
        close(p[1]);
        child_main(sv[1], ready[1], out[1]);
    }

    close(sv[1]);
    close(ready[1]);
    close(out[1]);
    char rb = 0;
    if (read(ready[0], &rb, 1) != 1) {
        perror("ready");
        return 1;
    }
    int send_ok = send_bootstrap(sv[0], p[0]) == 0;
    close(p[0]);
    close(p[1]);

    struct result r = {0};
    ssize_t rn = read(out[0], &r, sizeof r);
    int st = 0;
    waitpid(pid, &st, 0);
    int child = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("seqpacket_epoll_drain send=%d ep1=%d msg=%d cred=%d fd=%d clo=%d drained=%d quiet=%d child=%d\n",
           send_ok, r.ep1, r.got_msg, r.got_cred, r.got_fd, r.fd_cloexec, r.drained, r.quiet, child);
    return send_ok && rn == (ssize_t)sizeof r && r.ep1 == 1 && r.got_msg && r.got_cred && r.got_fd &&
                   r.fd_cloexec && r.drained && r.quiet && child == 0
               ? 0
               : 1;
}
