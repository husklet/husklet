// Cross-mechanism epoll: an eventfd, a timerfd, and a signalfd all registered in one epoll instance
// each fire independently. This is the self-pipe-trick replacement an event loop relies on -- a mix
// of readiness sources multiplexed through a single epoll_wait must surface every one of them.
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
    int efd = eventfd(0, EFD_NONBLOCK);
    int tfd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int sfd = signalfd(-1, &m, SFD_NONBLOCK);

    int ep = epoll_create1(0);
    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.fd = efd;
    epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ev);
    ev.data.fd = tfd;
    epoll_ctl(ep, EPOLL_CTL_ADD, tfd, &ev);
    ev.data.fd = sfd;
    epoll_ctl(ep, EPOLL_CTL_ADD, sfd, &ev);

    uint64_t one = 1;
    write(efd, &one, 8);
    struct itimerspec its = {{0, 0}, {0, 10 * 1000 * 1000}}; // 10ms one-shot
    timerfd_settime(tfd, 0, &its, NULL);
    raise(SIGUSR1);

    int saw_event = 0, saw_timer = 0, saw_signal = 0;
    // Collect until all three have fired; a bounded timeout keeps a broken engine from hanging.
    for (int iter = 0; iter < 16 && !(saw_event && saw_timer && saw_signal); iter++) {
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 1000);
        if (n <= 0) break;
        for (int i = 0; i < n; i++) {
            int fd = out[i].data.fd;
            if (fd == efd) {
                uint64_t v;
                read(efd, &v, 8);
                saw_event = 1;
            } else if (fd == tfd) {
                uint64_t v;
                read(tfd, &v, 8);
                saw_timer = 1;
            } else if (fd == sfd) {
                struct signalfd_siginfo si;
                read(sfd, &si, sizeof si);
                saw_signal = 1;
            }
        }
    }
    printf("eventfd=%d timerfd=%d signalfd=%d\n", saw_event, saw_timer, saw_signal);
    return 0;
}
