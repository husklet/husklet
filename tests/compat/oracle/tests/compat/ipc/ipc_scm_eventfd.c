#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int send_fd(int sock, int fd) {
    char b = 'x';
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    return sendmsg(sock, &mh, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock) {
    char b;
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    if (recvmsg(sock, &mh, MSG_CMSG_CLOEXEC) != 1) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    if (!c || c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) return -1;
    int fd = -1;
    memcpy(&fd, CMSG_DATA(c), sizeof fd);
    return fd;
}

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) != 0) {
        perror("socketpair");
        return 1;
    }
    int efd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (efd < 0) {
        perror("eventfd");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        close(sv[0]);
        int got = recv_fd(sv[1]);
        if (got < 0) _exit(10);
        uint64_t one = 1;
        if (write(got, &one, sizeof one) != (ssize_t)sizeof one) _exit(11);
        _exit(0);
    }

    close(sv[1]);
    if (send_fd(sv[0], efd) != 0) {
        perror("send_fd");
        return 1;
    }

    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (ep < 0) {
        perror("epoll_create1");
        return 1;
    }
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = efd};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ev) != 0) {
        perror("epoll_ctl");
        return 1;
    }
    struct epoll_event out;
    int n = epoll_wait(ep, &out, 1, 2000);
    uint64_t val = 0;
    int rr = n == 1 ? (int)read(efd, &val, sizeof val) : -1;
    int st = 0;
    waitpid(pid, &st, 0);

    printf("scm_eventfd epoll=%d read=%d val=%llu child=%d\n", n, rr,
           (unsigned long long)val, WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    return (n == 1 && rr == 8 && val == 1 && WIFEXITED(st) && WEXITSTATUS(st) == 0) ? 0 : 1;
}
