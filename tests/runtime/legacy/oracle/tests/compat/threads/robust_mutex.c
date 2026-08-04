// Robust mutex owner-death recovery: a thread locks a PTHREAD_MUTEX_ROBUST mutex and terminates
// without unlocking. The next locker must get EOWNERDEAD from pthread_mutex_lock, mark the state
// consistent with pthread_mutex_consistent, and thereafter lock/unlock normally. This drives the
// robust-futex owner-died list end to end. Deterministic derived booleans.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>

static pthread_mutex_t mtx;

static void *killer(void *_) {
    (void)_;
    pthread_mutex_lock(&mtx);   // acquire and deliberately never unlock
    return 0;                   // thread ends holding the lock
}

int main(void) {
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_setrobust(&a, PTHREAD_MUTEX_ROBUST);
    pthread_mutex_init(&mtx, &a);
    pthread_mutexattr_destroy(&a);

    pthread_t t;
    pthread_create(&t, 0, killer, 0);
    pthread_join(t, 0);         // owner thread is now dead while holding mtx

    int rc = pthread_mutex_lock(&mtx);
    int owner_died = rc == EOWNERDEAD;
    int made_consistent = pthread_mutex_consistent(&mtx) == 0;
    int unlocked = pthread_mutex_unlock(&mtx) == 0;

    // after recovery the mutex is usable again
    int relock = pthread_mutex_lock(&mtx) == 0;
    pthread_mutex_unlock(&mtx);
    pthread_mutex_destroy(&mtx);

    printf("robust_mutex owner_died=%d consistent=%d unlocked=%d relock=%d\n",
           owner_died, made_consistent, unlocked, relock);
    return 0;
}
