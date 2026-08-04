#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static int epoll_fd;

static void *waiter(void *unused) {
    (void)unused;
    struct epoll_event event;
    return (void *)(intptr_t)epoll_wait(epoll_fd, &event, 1, 2000);
}

int main(void) {
    epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    if (epoll_fd < 0) return 1;

    /* The first registration arms the engine's internal EVFILT_USER wake. Keep it non-readable so the
     * waiter below really blocks; the guest socket created afterwards is then the first fd-reuse candidate. */
    int idle[2];
    if (pipe2(idle, O_CLOEXEC) != 0) return 2;
    struct epoll_event idle_registration = {.events = EPOLLIN, .data.fd = idle[0]};
    if (epoll_ctl(epoll_fd, EPOLL_CTL_ADD, idle[0], &idle_registration) != 0) return 3;

    pthread_t thread;
    if (pthread_create(&thread, NULL, waiter, NULL) != 0) return 4;
    struct timespec settle = {0, 50 * 1000 * 1000};
    nanosleep(&settle, NULL);

    int stream[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, stream) != 0) return 5;
    int ready[2];
    if (pipe2(ready, O_CLOEXEC) != 0) return 6;
    if (write(ready[1], "x", 1) != 1) return 7;
    struct epoll_event registration = {.events = EPOLLIN, .data.fd = ready[0]};
    if (epoll_ctl(epoll_fd, EPOLL_CTL_ADD, ready[0], &registration) != 0) return 8;

    unsigned char buffer[16] = {0};
    ssize_t early = recv(stream[0], buffer, sizeof buffer, MSG_DONTWAIT);
    if (early >= 0 || errno != EAGAIN) {
        fprintf(stderr, "injected wake bytes=%zd first=%u\n", early, early > 0 ? buffer[0] : 0);
        return 9;
    }

    static const unsigned char frame[9] = {'R', 0, 0, 0, 8, 0, 0, 0, 0};
    if (send(stream[1], frame, sizeof frame, 0) != (ssize_t)sizeof frame) return 10;
    ssize_t received = recv(stream[0], buffer, sizeof buffer, 0);
    if (received != (ssize_t)sizeof frame || memcmp(buffer, frame, sizeof frame) != 0) return 11;

    void *wait_result = NULL;
    pthread_join(thread, &wait_result);
    close(idle[0]);
    close(idle[1]);
    close(ready[0]);
    close(ready[1]);
    close(stream[0]);
    close(stream[1]);
    close(epoll_fd);

    /* More than the private registry's historical 256 cells: every timer close must release its adopted
     * EVFILT_TIMER wake descriptor, or a later arm fails despite no guest descriptors remaining open. */
    for (int i = 0; i < 300; ++i) {
        int timer = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
        struct itimerspec setting = {.it_value = {0, 1000000}};
        if (timer < 0 || timerfd_settime(timer, 0, &setting, NULL) != 0 || close(timer) != 0) return 13;
    }
    printf("epoll_wake_socket_collision clean=1 delivered=%d\n", (int)(intptr_t)wait_result);
    return (intptr_t)wait_result == 1 ? 0 : 14;
}
