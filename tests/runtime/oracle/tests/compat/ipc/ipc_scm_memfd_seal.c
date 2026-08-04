#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif

static int send_fd(int sock, int fd) {
    char b = 'm';
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
    int fd = memfd_create("hl_scm_memfd", MFD_ALLOW_SEALING);
    if (fd < 0) {
        perror("memfd_create");
        return 1;
    }
    if (write(fd, "abcd", 4) != 4) return 1;
    int seal_ok = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) == 0;

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) != 0) return 1;
    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]);
        int got = recv_fd(sv[1]);
        int seals = got >= 0 ? fcntl(got, F_GET_SEALS) : -1;
        int wr = got >= 0 ? (int)write(got, "z", 1) : -1;
        _exit((got >= 0 && (seals & F_SEAL_WRITE) && wr < 0 && errno == EPERM) ? 0 : 2);
    }
    close(sv[1]);
    int send_ok = send_fd(sv[0], fd) == 0;
    int st = 0;
    waitpid(pid, &st, 0);
    int child = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("scm_memfd_seal seal=%d send=%d child=%d\n", seal_ok, send_ok, child);
    return (seal_ok && send_ok && child == 0) ? 0 : 1;
}
