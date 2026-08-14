#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

static int directed(int signal) {
    return (int)syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), signal);
}

static int pending(int signal) {
    sigset_t set;
    sigpending(&set);
    return sigismember(&set, signal);
}

int main(void) {
    int realtime = SIGRTMIN;
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, realtime);
    sigprocmask(SIG_BLOCK, &mask, NULL);

    int descriptor = signalfd(-1, &mask, SFD_NONBLOCK | SFD_CLOEXEC);
    int epoll = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event registration = {.events = EPOLLIN, .data.u64 = 0x51};
    epoll_ctl(epoll, EPOLL_CTL_ADD, descriptor, &registration);

    int standard_sent = directed(SIGUSR1) == 0 && directed(SIGUSR1) == 0;
    struct epoll_event event = {0};
    int standard_ready = epoll_wait(epoll, &event, 1, 0) == 1 && event.data.u64 == 0x51;
    struct signalfd_siginfo standard = {0};
    ssize_t standard_bytes = read(descriptor, &standard, sizeof standard);
    errno = 0;
    struct signalfd_siginfo empty = {0};
    ssize_t standard_empty = read(descriptor, &empty, sizeof empty);
    int standard_eagain = standard_empty == -1 && errno == EAGAIN;

    int realtime_sent = directed(realtime) == 0 && directed(realtime) == 0;
    struct signalfd_siginfo first = {0}, second = {0};
    ssize_t first_bytes = read(descriptor, &first, sizeof first);
    int realtime_still_ready = epoll_wait(epoll, &event, 1, 0) == 1;
    ssize_t second_bytes = read(descriptor, &second, sizeof second);
    int drained = epoll_wait(epoll, &event, 1, 0) == 0;

    printf("self_tgkill standard_sent=%d ready=%d bytes=%zd signo=%u code=%d eagain=%d pending=%d "
           "realtime_sent=%d first=%zd/%u/%d still_ready=%d second=%zd/%u/%d drained=%d pending_rt=%d\n",
           standard_sent, standard_ready, standard_bytes, standard.ssi_signo, standard.ssi_code, standard_eagain,
           pending(SIGUSR1), realtime_sent, first_bytes, first.ssi_signo, first.ssi_code, realtime_still_ready,
           second_bytes, second.ssi_signo, second.ssi_code, drained, pending(realtime));
    return 0;
}
