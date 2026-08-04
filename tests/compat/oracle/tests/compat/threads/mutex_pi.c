// Priority-inheritance mutex (PTHREAD_PRIO_INHERIT): correctness of lock/unlock and mutual exclusion
// under contention when the mutex is backed by the PI-futex path. 16 threads each increment a shared
// counter ITERS times under the PI mutex; the final total must be exact (no lost updates), and a
// trylock while held must fail with EBUSY. Deterministic.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>

#define THREADS 16
#define ITERS 5000

static pthread_mutex_t mtx;
static long counter;

static void *w(void *_) {
    (void)_;
    for (int i = 0; i < ITERS; i++) {
        pthread_mutex_lock(&mtx);
        counter++;
        pthread_mutex_unlock(&mtx);
    }
    return 0;
}

int main(void) {
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_setprotocol(&a, PTHREAD_PRIO_INHERIT);
    pthread_mutex_init(&mtx, &a);
    pthread_mutexattr_destroy(&a);

    // trylock semantics on a PI mutex: held -> EBUSY
    pthread_mutex_lock(&mtx);
    int busy = pthread_mutex_trylock(&mtx) == EBUSY;
    pthread_mutex_unlock(&mtx);

    pthread_t t[THREADS];
    for (int i = 0; i < THREADS; i++) pthread_create(&t[i], 0, w, 0);
    for (int i = 0; i < THREADS; i++) pthread_join(t[i], 0);

    pthread_mutex_destroy(&mtx);
    printf("mutex_pi busy=%d total_ok=%d\n", busy, counter == (long)THREADS * ITERS);
    return 0;
}
