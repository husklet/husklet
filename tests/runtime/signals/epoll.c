#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <unistd.h>

int signalfd_epoll(void) {
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &mask, 0) != 0) return 20;
    int signal_fd = signalfd(-1, &mask, SFD_NONBLOCK | SFD_CLOEXEC);
    int epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event control = {.events = EPOLLIN, .data.u64 = 0x5349474e414cULL};
    if (signal_fd < 0 || epoll_fd < 0 || epoll_ctl(epoll_fd, EPOLL_CTL_ADD, signal_fd, &control) != 0) return 21;
    struct epoll_event event;
    int empty = epoll_wait(epoll_fd, &event, 1, 0);
    if (raise(SIGUSR1) != 0) return 22;
    int ready = epoll_wait(epoll_fd, &event, 1, 100);
    int event_ok = ready == 1 && event.events == EPOLLIN;
    int data_ok = ready == 1 && event.data.u64 == 0x5349474e414cULL;
    struct signalfd_siginfo info;
    ssize_t bytes = read(signal_fd, &info, sizeof(info));
    int drained = epoll_wait(epoll_fd, &event, 1, 0);
    printf("signalfd_epoll empty=%d ready=%d event=%d data=%d bytes=%ld signo=%u code=%d drained=%d\n", empty, ready,
           event_ok, data_ok, (long)bytes, info.ssi_signo, info.ssi_code, drained);
    return 0;
}
