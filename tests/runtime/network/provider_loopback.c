#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif

static int fail(int step) {
    dprintf(2, "provider-loopback step=%d errno=%d\n", step, errno);
    return step;
}

static int wait_for(int epoll, uint32_t events, uint64_t data, int timeout) {
    struct epoll_event event = {0};
    int count = epoll_wait(epoll, &event, 1, timeout);
    if (count == 1 && (event.events & events) == events && event.data.u64 == data) return 0;
    dprintf(2, "provider-loopback wait ep=%d count=%d events=%x/%x data=%llu/%llu errno=%d\n", epoll, count,
            event.events, events, (unsigned long long)event.data.u64, (unsigned long long)data, errno);
    return -1;
}

static void *close_private(void *opaque) {
    int descriptor = *(int *)opaque;
    return (void *)(intptr_t)(syscall(SYS_close_range, (unsigned)descriptor, (unsigned)descriptor,
                                      CLOSE_RANGE_UNSHARE) == 0);
}

static int epoll_matrix(int descriptor) {
    struct epoll_event interest;
    pid_t child;
    int first = epoll_create1(EPOLL_CLOEXEC), second = epoll_create1(EPOLL_CLOEXEC), status;
    if (first < 0 || second < 0) return 30;

    interest = (struct epoll_event){.events = EPOLLIN | EPOLLET, .data.u64 = 11};
    if (epoll_ctl(first, EPOLL_CTL_ADD, descriptor, &interest) != 0) return 31;
    interest = (struct epoll_event){.events = EPOLLOUT | EPOLLONESHOT, .data.u64 = 22};
    if (epoll_ctl(second, EPOLL_CTL_ADD, descriptor, &interest) != 0) return 32;
    if (wait_for(first, EPOLLIN, 11, 2000) != 0 || wait_for(second, EPOLLOUT, 22, 2000) != 0) return 33;
    if (epoll_wait(first, &interest, 1, 0) != 0 || epoll_wait(second, &interest, 1, 0) != 0) return 34;

    /* MOD and ONESHOT rearm only the selected epoll membership. */
    interest = (struct epoll_event){.events = EPOLLOUT | EPOLLONESHOT, .data.u64 = 33};
    if (epoll_ctl(first, EPOLL_CTL_MOD, descriptor, &interest) != 0 || wait_for(first, EPOLLOUT, 33, 2000) != 0 ||
        epoll_wait(first, &interest, 1, 0) != 0)
        return 35;
    if (epoll_ctl(first, EPOLL_CTL_DEL, descriptor, NULL) != 0) return 36;

    /* Rearm the other membership as level-triggered.  It must remain ready
     * without another provider edge until the condition changes. */
    interest = (struct epoll_event){.events = EPOLLIN, .data.u64 = 44};
    if (epoll_ctl(second, EPOLL_CTL_MOD, descriptor, &interest) != 0 || wait_for(second, EPOLLIN, 44, 2000) != 0 ||
        wait_for(second, EPOLLIN, 44, 0) != 0)
        return 37;
    if (epoll_ctl(second, EPOLL_CTL_DEL, descriptor, NULL) != 0 || epoll_wait(second, &interest, 1, 0) != 0) return 38;

    /* A registration inherited across fork remains usable in the child. */
    interest = (struct epoll_event){.events = EPOLLIN | EPOLLET, .data.u64 = 55};
    if (epoll_ctl(first, EPOLL_CTL_ADD, descriptor, &interest) != 0) return 39;
    child = fork();
    if (child < 0) return 40;
    if (child == 0) _exit(wait_for(first, EPOLLIN, 55, 2000) == 0 ? 0 : 41);
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 42;
    if (epoll_ctl(first, EPOLL_CTL_DEL, descriptor, NULL) != 0) return 43;
    if (close(first) != 0 || close(second) != 0) return 44;
    return 0;
}

int main(int argc, char **argv) {
    char bytes[16] = {0};
    struct stat metadata;
    struct pollfd ready;
    pid_t child;
    int descriptor, duplicate, status;
    if (argc == 3 && strcmp(argv[1], "check-cloexec") == 0) {
        descriptor = atoi(argv[2]);
        errno = 0;
        return fcntl(descriptor, F_GETFD) == -1 && errno == EBADF ? 0 : fail(20);
    }
    descriptor = open("/run/domain/control", O_RDWR | O_CLOEXEC);
    if (descriptor < 0) return fail(1);
    if (fstat(descriptor, &metadata) != 0 || !S_ISREG(metadata.st_mode) || metadata.st_size != 5) return fail(2);
    ready = (struct pollfd){.fd = descriptor, .events = POLLIN | POLLOUT};
    if (poll(&ready, 1, 1000) != 1 || (ready.revents & (POLLIN | POLLOUT)) != (POLLIN | POLLOUT)) return fail(3);
    if (argc == 2 && strcmp(argv[1], "sentry") == 0) {
        int sockets[2];
        if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) != 0) return fail(50);
        char byte = 'x';
        struct iovec iov = {.iov_base = &byte, .iov_len = 1};
        char control[CMSG_SPACE(sizeof(int))] = {0};
        struct msghdr message = {
            .msg_iov = &iov,
            .msg_iovlen = 1,
            .msg_control = control,
            .msg_controllen = sizeof control,
        };
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cmsg), &descriptor, sizeof descriptor);
        errno = 0;
        if (sendmsg(sockets[0], &message, 0) != -1 || errno != EPERM) return fail(51);
        errno = 0;
        void *mapping = mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, descriptor, 0);
        if (mapping != MAP_FAILED || errno != EBADF) return fail(52);
        close(sockets[0]);
        close(sockets[1]);

        duplicate = dup(descriptor);
        if (duplicate < 0) return fail(60);
        pthread_t private_thread;
        if (pthread_create(&private_thread, NULL, close_private, &duplicate) != 0) return fail(61);
        void *private_result = NULL;
        if (pthread_join(private_thread, &private_result) != 0 || !(int)(intptr_t)private_result ||
            fcntl(duplicate, F_GETFD) < 0)
            return fail(62);

        child = fork();
        if (child < 0) return fail(63);
        if (child == 0) {
            char one = 0;
            _exit(pread(descriptor, &one, 1, 0) == 1 && one == 'h' ? 0 : 64);
        }
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return fail(65);

        int aliases[1100], aliases_count = 0;
        while (aliases_count < (int)(sizeof aliases / sizeof aliases[0])) {
            int alias = fcntl(descriptor, F_DUPFD, 0);
            if (alias < 0) break;
            aliases[aliases_count++] = alias;
        }
        if (errno != EMFILE || aliases_count == 0) return fail(66);
        if (close(aliases[--aliases_count]) != 0) return fail(67);
        int recovered_alias = fcntl(descriptor, F_DUPFD, 0);
        if (recovered_alias < 0 || close(recovered_alias) != 0) return fail(68);
        while (aliases_count > 0)
            if (close(aliases[--aliases_count]) != 0) return fail(69);

        child = fork();
        if (child < 0) return fail(70);
        if (child == 0) {
            char number[32];
            snprintf(number, sizeof number, "%d", descriptor);
            execl("/provider-loopback", "/provider-loopback", "check-cloexec", number, NULL);
            _exit(71);
        }
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return fail(72);
        if (close(duplicate) != 0 || close(descriptor) != 0) return fail(73);
        puts("provider-loopback ok");
        return 0;
    }
    status = epoll_matrix(descriptor);
    if (status != 0) return fail(status);
    if (read(descriptor, bytes, 2) != 2 || memcmp(bytes, "he", 2) != 0) return fail(4);
    duplicate = dup(descriptor);
    if (duplicate < 0 || read(duplicate, bytes, 3) != 3 || memcmp(bytes, "llo", 3) != 0) return fail(5);
    if (lseek(descriptor, 0, SEEK_SET) != 0) return fail(6);
    child = fork();
    if (child < 0) return fail(7);
    if (child == 0) {
        int epoll = epoll_create1(EPOLL_CLOEXEC);
        struct epoll_event interest = {.events = EPOLLIN, .data.fd = descriptor};
        struct epoll_event event;
        if (epoll < 0 || epoll_ctl(epoll, EPOLL_CTL_ADD, descriptor, &interest) != 0 ||
            epoll_wait(epoll, &event, 1, 1000) != 1 || (event.events & EPOLLIN) == 0)
            _exit(8);
        close(epoll);
        _exit(read(descriptor, bytes, 1) == 1 && bytes[0] == 'h' ? 0 : 22);
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return fail(9);
    if (read(duplicate, bytes, 1) != 1 || bytes[0] != 'e') return fail(10);
    if (pwrite(descriptor, "XY", 2, 1) != 2 || pread(descriptor, bytes, 5, 0) != 5 || memcmp(bytes, "hXYlo", 5) != 0)
        return fail(11);
    child = fork();
    if (child < 0) return fail(16);
    if (child == 0) _exit(pwrite(descriptor, "C", 1, 4) == 1 ? 0 : 17);
    /* Disjoint positioned writes may run concurrently without changing the
     * shared open-file-description offset. */
    if (pwrite(descriptor, "P", 1, 3) != 1) return fail(18);
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return fail(19);
    if (pread(descriptor, bytes, 5, 0) != 5 || memcmp(bytes, "hXYPC", 5) != 0) return fail(21);
    child = fork();
    if (child < 0) return fail(12);
    if (child == 0) {
        char number[32];
        snprintf(number, sizeof(number), "%d", descriptor);
        execl("/provider-loopback", "/provider-loopback", "check-cloexec", number, NULL);
        _exit(13);
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return fail(14);

    if (close(duplicate) != 0) return fail(15);
    int stale_epoll = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event stale_interest = {.events = EPOLLIN | EPOLLET, .data.u64 = 66}, stale_event;
    if (stale_epoll < 0 || epoll_ctl(stale_epoll, EPOLL_CTL_ADD, descriptor, &stale_interest) != 0 ||
        wait_for(stale_epoll, EPOLLIN, 66, 2000) != 0)
        return fail(45);
    int reused = descriptor;
    if (close(descriptor) != 0) return fail(46);
    int nullfd = open("/dev/null", O_RDONLY);
    if (nullfd < 0 || (nullfd != reused && dup2(nullfd, reused) != reused)) return fail(47);
    if (nullfd != reused) close(nullfd);
    if (epoll_wait(stale_epoll, &stale_event, 1, 200) != 0) return fail(48);
    errno = 0;
    if (epoll_ctl(stale_epoll, EPOLL_CTL_DEL, reused, NULL) != -1 || errno != ENOENT) return fail(49);
    close(reused);
    close(stale_epoll);
    puts("provider-loopback ok");
    return 0;
}
