// mq_notify with SIGEV_THREAD: a delivered message spawns the notification thread callback exactly once.
#include <mqueue.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int fired = 0;

static void handler(union sigval sv) {
    (void)sv;
    pthread_mutex_lock(&mtx);
    fired++;
    pthread_cond_signal(&cv);
    pthread_mutex_unlock(&mtx);
}

int main(void) {
    char name[64];
    snprintf(name, sizeof name, "/hl_mqn_%d", (int)getpid());
    mq_unlink(name);
    struct mq_attr attr = {0};
    attr.mq_maxmsg = 2;
    attr.mq_msgsize = 16;
    mqd_t q = mq_open(name, O_CREAT | O_RDWR | O_NONBLOCK, 0600, &attr);
    if (q == (mqd_t)-1) { printf("mqnotify open=0\n"); return 0; }

    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_THREAD;
    sev.sigev_notify_function = handler;
    sev.sigev_value.sival_int = 0;
    int reg = mq_notify(q, &sev) == 0;

    mq_send(q, "ping", 4, 1);

    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_sec += 3;
    pthread_mutex_lock(&mtx);
    int timedout = 0;
    while (fired == 0 && !timedout)
        timedout = pthread_cond_timedwait(&cv, &mtx, &ts) != 0;
    int got = fired;
    pthread_mutex_unlock(&mtx);

    mq_close(q);
    mq_unlink(name);
    printf("mqnotify reg=%d fired=%d\n", reg, got == 1);
    return 0;
}
