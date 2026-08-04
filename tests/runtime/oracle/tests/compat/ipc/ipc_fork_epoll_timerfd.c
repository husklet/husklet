// epoll + timerfd inheritance across fork(): a child inherits the parent's epoll interest list and its
// armed timerfd (Linux COW-inherits both; the engine rebuilds the kqueue-backed instances in the fork
// child). This is exactly what the kqueue_rebuild_after_fork pass must keep working when it is gated on
// "epoll-family ever used" -- if the gate wrongly skips the rebuild, the child's epoll_wait/timerfd read
// hang (caught by the watchdog -> non-deterministic FAIL). Output is deterministic and diffed vs native.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <unistd.h>

static void die_alarm(int s) {
    (void)s;
    _exit(7); // hard exit on hang so the harness never blocks
}

int main(void) {
    signal(SIGALRM, die_alarm);
    alarm(10);

    // Pipe watched (level-triggered EPOLLIN) by an epoll instance; data pre-written so it is readable.
    int pfd[2];
    if (pipe(pfd) != 0) return 1;
    if (write(pfd[1], "X", 1) != 1) return 1;

    int ep = epoll_create1(0);
    if (ep < 0) return 1;
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = pfd[0]};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, pfd[0], &ev) != 0) return 1;

    // An armed timerfd (first expiry ~40ms, then periodic) -> the child inherits the arming.
    int tf = timerfd_create(CLOCK_MONOTONIC, 0);
    if (tf < 0) return 1;
    struct itimerspec its = {.it_interval = {0, 40 * 1000 * 1000}, .it_value = {0, 40 * 1000 * 1000}};
    if (timerfd_settime(tf, 0, &its, NULL) != 0) return 1;

    pid_t pid = fork();
    if (pid == 0) { // ---- child: everything below relies on the fork-inherited epoll + timerfd ----
        alarm(10);
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 2000); // inherited interest -> pipe reports readable
        int saw_pipe = (n >= 1 && out[0].data.fd == pfd[0] && (out[0].events & EPOLLIN));
        char rb = 0;
        int rd = (read(pfd[0], &rb, 1) == 1 && rb == 'X');
        uint64_t exp = 0;
        int tr = (read(tf, &exp, 8) == 8 && exp >= 1); // inherited armed timerfd fires
        printf("child epoll_n=%d saw_pipe=%d pipe_rd=%d timerfd_fired=%d\n", n >= 1 ? 1 : 0, saw_pipe, rd,
               tr ? 1 : 0);
        fflush(stdout);
        _exit((saw_pipe && rd && tr) ? 0 : 2);
    }

    int st = 0;
    waitpid(pid, &st, 0);
    printf("parent child_exit=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    close(ep);
    close(tf);
    close(pfd[0]);
    close(pfd[1]);
    return WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : 1;
}
