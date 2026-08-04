// POSIX timer_create with SIGEV_THREAD: periodic timer fires the callback repeatedly, then disarm.
#include <signal.h>
#include <time.h>
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int ticks = 0;

static void tick(union sigval sv) {
    (void)sv;
    pthread_mutex_lock(&mtx);
    ticks++;
    pthread_cond_signal(&cv);
    pthread_mutex_unlock(&mtx);
}

int main(void) {
    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_THREAD;
    sev.sigev_notify_function = tick;
    timer_t tid;
    if (timer_create(CLOCK_MONOTONIC, &sev, &tid) != 0) { printf("timerthread create=0\n"); return 0; }

    struct itimerspec its = {0};
    its.it_value.tv_nsec = 20 * 1000 * 1000;    // first at 20ms
    its.it_interval.tv_nsec = 20 * 1000 * 1000; // then every 20ms
    timer_settime(tid, 0, &its, NULL);

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 3;
    pthread_mutex_lock(&mtx);
    while (ticks < 3) {
        if (pthread_cond_timedwait(&cv, &mtx, &deadline) != 0) break;
    }
    int enough = ticks >= 3;
    pthread_mutex_unlock(&mtx);

    struct itimerspec disarm = {0};
    timer_settime(tid, 0, &disarm, NULL);
    timer_delete(tid);
    printf("timerthread fired=%d\n", enough);
    return 0;
}
