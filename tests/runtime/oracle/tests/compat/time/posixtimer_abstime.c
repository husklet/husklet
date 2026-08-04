// timer_settime with TIMER_ABSTIME arms a POSIX timer against an absolute clock deadline. A near-
// future absolute deadline fires once; an absolute deadline already in the past fires almost
// immediately. timer_gettime on a TIMER_ABSTIME-armed timer still reports the relative remaining.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define SIGNO SIGRTMIN
static volatile sig_atomic_t fired;
static void h(int s, siginfo_t *si, void *u) { (void)s; (void)si; (void)u; fired++; }

static int abs_fires(clockid_t clk, long ahead_ns) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = h;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGNO, &sa, NULL);

    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_SIGNAL;
    ev.sigev_signo = SIGNO;
    timer_t t;
    if (timer_create(clk, &ev, &t) != 0) return 0;

    struct timespec now;
    clock_gettime(clk, &now);
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value = now;
    its.it_value.tv_nsec += ahead_ns;
    while (its.it_value.tv_nsec >= 1000000000L) { its.it_value.tv_nsec -= 1000000000L; its.it_value.tv_sec++; }
    fired = 0;
    if (timer_settime(t, TIMER_ABSTIME, &its, NULL) != 0) { timer_delete(t); return 0; }
    for (int i = 0; i < 500 && !fired; i++) usleep(2000);
    int ok = fired == 1;
    timer_delete(t);
    return ok;
}

int main(void) {
    int future = abs_fires(CLOCK_MONOTONIC, 40 * 1000 * 1000); // +40ms
    int real = abs_fires(CLOCK_REALTIME, 40 * 1000 * 1000);

    // A long absolute deadline: gettime reports a relative remaining <= the interval.
    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_NONE;
    timer_t t;
    timer_create(CLOCK_MONOTONIC, &ev, &t);
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value = now;
    its.it_value.tv_sec += 100; // 100s in the future, absolute
    timer_settime(t, TIMER_ABSTIME, &its, NULL);
    struct itimerspec cur;
    memset(&cur, 0, sizeof cur);
    timer_gettime(t, &cur);
    int rel_ok = cur.it_value.tv_sec > 90 && cur.it_value.tv_sec <= 100;
    timer_delete(t);

    printf("posixabs future=%d real=%d rel=%d\n", future, real, rel_ok);
    return 0;
}
