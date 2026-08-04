// POSIX timer with SIGEV_THREAD: a periodic timer invokes the notify function on a helper thread.
// We count invocations of a periodic timer and confirm it fires at least N times, then that a
// one-shot SIGEV_THREAD fires exactly once within the observation window.
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static volatile int periodic_count;
static volatile int oneshot_count;

static void periodic_cb(union sigval sv) { (void)sv; __sync_fetch_and_add(&periodic_count, 1); }
static void oneshot_cb(union sigval sv) { (void)sv; __sync_fetch_and_add(&oneshot_count, 1); }

int main(void) {
    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_THREAD;
    ev.sigev_notify_function = periodic_cb;
    timer_t t;
    int created = timer_create(CLOCK_MONOTONIC, &ev, &t) == 0;

    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 20 * 1000 * 1000;
    its.it_interval.tv_nsec = 20 * 1000 * 1000;
    timer_settime(t, 0, &its, NULL);
    for (int i = 0; i < 200 && periodic_count < 3; i++) usleep(5000);
    int periodic_ok = periodic_count >= 3;
    timer_delete(t);

    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_THREAD;
    ev.sigev_notify_function = oneshot_cb;
    timer_t t2;
    timer_create(CLOCK_MONOTONIC, &ev, &t2);
    struct itimerspec one;
    memset(&one, 0, sizeof one);
    one.it_value.tv_nsec = 30 * 1000 * 1000;
    timer_settime(t2, 0, &one, NULL);
    for (int i = 0; i < 100 && oneshot_count < 1; i++) usleep(5000);
    usleep(60 * 1000); // give any spurious extra fire a chance (must not happen)
    int oneshot_ok = oneshot_count == 1;
    timer_delete(t2);

    printf("timerthread created=%d periodic=%d oneshot=%d\n", created, periodic_ok, oneshot_ok);
    return 0;
}
