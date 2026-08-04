#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

static pthread_mutex_t mutex;
static atomic_int owner_ready, waiter_started, release_owner;
static int owner_dead, recovered;

static void *owner(void *unused) {
    (void)unused;
    if (pthread_mutex_lock(&mutex) != 0) return 0;
    atomic_store(&owner_ready, 1);
    while (!atomic_load(&release_owner))
        sched_yield();
    return 0; /* exit while owning the robust PI mutex */
}

static void *waiter(void *unused) {
    (void)unused;
    while (!atomic_load(&owner_ready))
        sched_yield();
    atomic_store(&waiter_started, 1);
    int result = pthread_mutex_lock(&mutex);
    owner_dead = result == EOWNERDEAD;
    if (owner_dead) { recovered = pthread_mutex_consistent(&mutex) == 0 && pthread_mutex_unlock(&mutex) == 0; }
    return 0;
}

int main(void) {
    pthread_mutexattr_t attributes;
    pthread_mutexattr_init(&attributes);
    if (pthread_mutexattr_setrobust(&attributes, PTHREAD_MUTEX_ROBUST) != 0 ||
        pthread_mutexattr_setprotocol(&attributes, PTHREAD_PRIO_INHERIT) != 0 ||
        pthread_mutex_init(&mutex, &attributes) != 0)
        return 2;

    pthread_t owner_thread, waiter_thread;
    pthread_create(&owner_thread, 0, owner, 0);
    pthread_create(&waiter_thread, 0, waiter, 0);
    while (!atomic_load(&waiter_started))
        sched_yield();
    struct timespec parked = {.tv_nsec = 50000000};
    nanosleep(&parked, 0); /* waiter is now blocked in FUTEX_LOCK_PI */
    atomic_store(&release_owner, 1);
    pthread_join(owner_thread, 0);
    pthread_join(waiter_thread, 0);

    int reusable = pthread_mutex_lock(&mutex) == 0;
    if (reusable) pthread_mutex_unlock(&mutex);
    printf("robust_pi_mutex owner_dead=%d recovered=%d reusable=%d\n", owner_dead, recovered, reusable);
    return !(owner_dead && recovered && reusable);
}
