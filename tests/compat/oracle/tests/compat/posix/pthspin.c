// pthread_spin_lock protects a shared counter; trylock reports EBUSY while held.
#include <pthread.h>
#include <stdio.h>
#include <errno.h>

#define N 4
#define ITERS 100000
static pthread_spinlock_t lock;
static long counter = 0;

static void *worker(void *arg) {
    (void)arg;
    for (int i = 0; i < ITERS; i++) {
        pthread_spin_lock(&lock);
        counter++;
        pthread_spin_unlock(&lock);
    }
    return NULL;
}

int main(void) {
    pthread_spin_init(&lock, PTHREAD_PROCESS_PRIVATE);
    pthread_spin_lock(&lock);
    int busy = pthread_spin_trylock(&lock) == EBUSY;
    pthread_spin_unlock(&lock);
    pthread_t t[N];
    for (int i = 0; i < N; i++) pthread_create(&t[i], NULL, worker, NULL);
    for (int i = 0; i < N; i++) pthread_join(t[i], NULL);
    pthread_spin_destroy(&lock);
    printf("spin busy=%d total_ok=%d\n", busy, counter == (long)N * ITERS);
    return 0;
}
