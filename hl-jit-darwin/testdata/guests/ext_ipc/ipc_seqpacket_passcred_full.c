#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int send_fd(int sock, int fd) {
    char data[] = "mojo!";
    struct iovec io = {.iov_base = data, .iov_len = sizeof(data) - 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    mh.msg_controllen = c->cmsg_len;
    return sendmsg(sock, &mh, 0) == (ssize_t)(sizeof(data) - 1) ? 0 : -1;
}

int main(void) {
    alarm(10);
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) {
        printf("seqpass_full setup=0\n");
        return 1;
    }
    int on = 1;
    if (setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &on, sizeof on) != 0) {
        printf("seqpass_full passcred=0\n");
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        printf("seqpass_full fork=0\n");
        return 1;
    }
    if (child == 0) {
        close(sv[0]);
        int p[2];
        if (pipe(p) != 0) _exit(2);
        char b = 'R';
        if (write(p[1], &b, 1) != 1) _exit(3);
        int rc = send_fd(sv[1], p[0]);
        close(p[0]);
        close(p[1]);
        _exit(rc == 0 ? 0 : 4);
    }

    close(sv[1]);
    char data[16] = {0};
    struct iovec io = {.iov_base = data, .iov_len = sizeof data};
    char ctl[CMSG_SPACE(sizeof(int)) + CMSG_SPACE(sizeof(struct ucred))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    ssize_t n = recvmsg(sv[0], &mh, 0);

    int gotfd = -1, rights = 0, cred = 0, cred_pid = 0;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
            memcpy(&gotfd, CMSG_DATA(c), sizeof gotfd);
            rights = gotfd >= 0;
        } else if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
            struct ucred cr;
            memcpy(&cr, CMSG_DATA(c), sizeof cr);
            cred = cr.uid == getuid() && cr.gid == getgid();
            cred_pid = cr.pid == child;
        }
    }

    char fdbyte = '-';
    if (gotfd >= 0) {
        if (read(gotfd, &fdbyte, 1) != 1) fdbyte = '?';
        close(gotfd);
    }
    int st = 0;
    waitpid(child, &st, 0);
    int no_trunc = (mh.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) == 0;
    int child_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;
    int ok = n == 5 && strcmp(data, "mojo!") == 0 && no_trunc && rights && fdbyte == 'R' && cred && cred_pid && child_ok;
    printf("seqpass_full n=%ld data=%s trunc=%d ctrunc=%d rights=%d fdbyte=%c cred=%d credpid=%d child=%d\n",
           (long)n, data, !!(mh.msg_flags & MSG_TRUNC), !!(mh.msg_flags & MSG_CTRUNC), rights, fdbyte, cred,
           cred_pid, child_ok);
    return ok ? 0 : 1;
}
