// pthread_mutex_timedlock: while another thread holds the lock, a timedlock with a short absolute
// (CLOCK_REALTIME) deadline returns ETIMEDOUT; once the holder releases, a fresh timedlock succeeds.
// Also checks sem_timedwait's ETIMEDOUT path on a zero-count semaphore. Deterministic derived booleans.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static atomic_int holder_ready = 0;
static atomic_int release = 0;

static void *holder(void *_) {
    (void)_;
    pthread_mutex_lock(&mtx);
    atomic_store(&holder_ready, 1);
    while (!atomic_load(&release)) { struct timespec ts = {0, 200000}; nanosleep(&ts, 0); }
    pthread_mutex_unlock(&mtx);
    return 0;
}

static struct timespec deadline(long ms) {
    struct timespec t;
    clock_gettime(CLOCK_REALTIME, &t);
    t.tv_nsec += ms * 1000000L;
    while (t.tv_nsec >= 1000000000L) { t.tv_nsec -= 1000000000L; t.tv_sec++; }
    return t;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, 0, holder, 0);
    while (!atomic_load(&holder_ready)) { struct timespec ts = {0, 200000}; nanosleep(&ts, 0); }

    struct timespec d1 = deadline(40);
    int timed_out = pthread_mutex_timedlock(&mtx, &d1) == ETIMEDOUT;

    atomic_store(&release, 1);
    pthread_join(t, 0);

    struct timespec d2 = deadline(1000);
    int acquired = pthread_mutex_timedlock(&mtx, &d2) == 0;
    if (acquired) pthread_mutex_unlock(&mtx);

    // sem_timedwait ETIMEDOUT on an empty semaphore
    sem_t s;
    sem_init(&s, 0, 0);
    struct timespec d3 = deadline(40);
    int sem_timeout = sem_timedwait(&s, &d3) == -1 && errno == ETIMEDOUT;
    sem_destroy(&s);

    printf("mutex_timedlock timed_out=%d acquired=%d sem_timeout=%d\n",
           timed_out, acquired, sem_timeout);
    return 0;
}
