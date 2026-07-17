#define _GNU_SOURCE
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#include <fcntl.h>
#include <signal.h>

static int send_fds(int sock, const int *fds, int nfds) {
    char byte = 'F';
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int) * 3)];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf;
    msg.msg_controllen = CMSG_SPACE(sizeof(int) * nfds);

    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int) * nfds);
    memcpy(CMSG_DATA(c), fds, sizeof(int) * nfds);
    msg.msg_controllen = c->cmsg_len;

    return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fds(int sock, int *fds, int maxfds) {
    char byte = 0;
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int) * 3)];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf;
    msg.msg_controllen = sizeof cbuf;

    if (recvmsg(sock, &msg, 0) != 1) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    if (!c || c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) return -1;
    int n = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
    if (n > maxfds) n = maxfds;
    memcpy(fds, CMSG_DATA(c), sizeof(int) * n);
    return n;
}

static int read_all(int fd, char *buf, size_t cap) {
    size_t n = 0;
    while (n + 1 < cap) {
        ssize_t r = read(fd, buf + n, cap - 1 - n);
        if (r < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (r == 0) break;
        n += (size_t)r;
    }
    buf[n] = 0;
    return (int)n;
}

static int valid_status(const char *s, pid_t self) {
    char needle[64];
    snprintf(needle, sizeof needle, "\nPid:\t%d\n", (int)self);
    return strstr(s, "Name:") && strstr(s, "\nState:") && strstr(s, needle);
}

static int valid_statm(const char *s) {
    int fields = 0;
    const char *p = s;
    while (*p) {
        while (isspace((unsigned char)*p)) p++;
        if (!*p) break;
        if (!isdigit((unsigned char)*p)) return 0;
        while (isdigit((unsigned char)*p)) p++;
        fields++;
    }
    return fields == 7;
}

static int valid_maps(const char *s) {
    return strstr(s, "r-xp") && strstr(s, "[stack]");
}

int main(void) {
    alarm(15);
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) {
        perror("socketpair");
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }

    if (child == 0) {
        close(sv[0]);
        int fds[3] = {-1, -1, -1};
        int n = recv_fds(sv[1], fds, 3);
        int status_ok = n == 3;
        int statm_ok = n == 3;
        int maps_ok = n == 3;
        for (int i = 0; i < 64 && status_ok && statm_ok && maps_ok; i++) {
            char status[4096], statm[256], maps[8192];
            status_ok = lseek(fds[0], 0, SEEK_SET) == 0 &&
                        read_all(fds[0], status, sizeof status) > 0 &&
                        valid_status(status, getpid());
            statm_ok = lseek(fds[1], 0, SEEK_SET) == 0 &&
                       read_all(fds[1], statm, sizeof statm) > 0 &&
                       valid_statm(statm);
            maps_ok = lseek(fds[2], 0, SEEK_SET) == 0 &&
                      read_all(fds[2], maps, sizeof maps) > 0 &&
                      valid_maps(maps);
            char ping = (char)i;
            char ack = 0;
            if (write(sv[1], &ping, 1) != 1 || read(sv[1], &ack, 1) != 1 || ack != ping) {
                status_ok = 0;
                statm_ok = 0;
                maps_ok = 0;
                break;
            }
        }
        if (fds[0] >= 0) close(fds[0]);
        if (fds[1] >= 0) close(fds[1]);
        if (fds[2] >= 0) close(fds[2]);
        char out[64];
        int len = snprintf(out, sizeof out, "%d %d %d %d", n, status_ok, statm_ok, maps_ok);
        if (write(sv[1], out, (size_t)len) != len) _exit(2);
        _exit(status_ok && statm_ok && maps_ok ? 0 : 3);
    }

    close(sv[1]);
    char path[128];
    snprintf(path, sizeof path, "/proc/%d/status", (int)child);
    int status_fd = open(path, O_RDONLY | O_CLOEXEC);
    snprintf(path, sizeof path, "/proc/%d/statm", (int)child);
    int statm_fd = open(path, O_RDONLY | O_CLOEXEC);
    snprintf(path, sizeof path, "/proc/%d/maps", (int)child);
    int maps_fd = open(path, O_RDONLY | O_CLOEXEC);

    int open_ok = status_fd >= 0 && statm_fd >= 0 && maps_fd >= 0;
    int fds[3] = {status_fd, statm_fd, maps_fd};
    int send_ok = open_ok && send_fds(sv[0], fds, 3) == 0;
    if (status_fd >= 0) close(status_fd);
    if (statm_fd >= 0) close(statm_fd);
    if (maps_fd >= 0) close(maps_fd);

    for (int i = 0; i < 64 && send_ok; i++) {
        char ping = 0;
        if (read(sv[0], &ping, 1) != 1) break;
        if (write(sv[0], &ping, 1) != 1) break;
    }

    char reply[64] = {0};
    ssize_t rn = read(sv[0], reply, sizeof reply - 1);
    if (rn < 0) rn = 0;
    reply[rn] = 0;

    int st = 0;
    waitpid(child, &st, 0);
    int nrecv = 0, status_ok = 0, statm_ok = 0, maps_ok = 0;
    sscanf(reply, "%d %d %d %d", &nrecv, &status_ok, &statm_ok, &maps_ok);
    int child_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;
    printf("chrome_procfd open=%d send=%d recv=%d status=%d statm=%d maps=%d child=%d\n",
           open_ok, send_ok, nrecv, status_ok, statm_ok, maps_ok, child_ok);
    return open_ok && send_ok && nrecv == 3 && status_ok && statm_ok && maps_ok && child_ok ? 0 : 1;
}
