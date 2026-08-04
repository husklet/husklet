#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static int send_fd(int socket, int fd) {
    char byte = 'x';
    struct iovec vector = {&byte, 1};
    char control[CMSG_SPACE(sizeof(fd))] = {0};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1, .msg_control = control,
                             .msg_controllen = sizeof(control)};
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(fd));
    memcpy(CMSG_DATA(header), &fd, sizeof(fd));
    return sendmsg(socket, &message, 0) == 1 ? 0 : -1;
}

static int receive_fd(int socket) {
    char byte;
    struct iovec vector = {&byte, 1};
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1, .msg_control = control,
                             .msg_controllen = sizeof(control)};
    struct cmsghdr *header;
    int fd = -1;
    if (recvmsg(socket, &message, MSG_CMSG_CLOEXEC) != 1) return -1;
    header = CMSG_FIRSTHDR(&message);
    if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
        header->cmsg_len != CMSG_LEN(sizeof(fd)))
        return -1;
    memcpy(&fd, CMSG_DATA(header), sizeof(fd));
    return fd;
}

static int receiver(int socket) {
    int fd = receive_fd(socket);
    int copy;
    int epoll;
    int cloexec;
    int shared_flags;
    int semaphore = 0;
    int empty = 0;
    int woke = 0;
    uint64_t value = 0;
    uint64_t add = 4;
    struct epoll_event interest = {.events = EPOLLIN, .data.u64 = 7};
    struct epoll_event ready;
    if (fd < 0) return 10;
    cloexec = (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0;
    copy = dup(fd);
    if (copy < 0) return 11;
    if (read(fd, &value, sizeof(value)) == (ssize_t)sizeof(value) && value == 1 &&
        read(copy, &value, sizeof(value)) == (ssize_t)sizeof(value) && value == 1)
        semaphore = 1;
    empty = read(fd, &value, sizeof(value)) < 0 && errno == EAGAIN;
    if (fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) & ~O_NONBLOCK) != 0) return 12;
    shared_flags = (fcntl(copy, F_GETFL) & O_NONBLOCK) == 0;
    if (fcntl(copy, F_SETFL, fcntl(copy, F_GETFL) | O_NONBLOCK) != 0) return 13;
    epoll = epoll_create1(EPOLL_CLOEXEC);
    if (epoll < 0 || epoll_ctl(epoll, EPOLL_CTL_ADD, copy, &interest) != 0) return 14;
    close(fd);
    if (write(copy, &add, sizeof(add)) != (ssize_t)sizeof(add)) return 15;
    woke = epoll_wait(epoll, &ready, 1, 1000) == 1 && ready.data.u64 == 7;
    close(epoll);
    close(copy);
    printf("eventfd_lifecycle cloexec=%d semaphore=%d empty=%d shared_flags=%d woke=%d\n", cloexec, semaphore,
           empty, shared_flags, woke);
    fflush(stdout);
    return cloexec && semaphore && empty && shared_flags && woke ? 0 : 16;
}

int main(void) {
    int sockets[2];
    int fd;
    pid_t child;
    int status;
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sockets) != 0) return 1;
    fd = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK | EFD_CLOEXEC);
    if (fd < 0) return 2;
    child = fork();
    if (child < 0) return 3;
    if (child == 0) {
        close(sockets[0]);
        _exit(receiver(sockets[1]));
    }
    close(sockets[1]);
    if (send_fd(sockets[0], fd) != 0) return 4;
    close(fd); /* The queued SCM object must outlive its last sender-side descriptor. */
    close(sockets[0]);
    if (waitpid(child, &status, 0) != child) return 5;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 6;
}
