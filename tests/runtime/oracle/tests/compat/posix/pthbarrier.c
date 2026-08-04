// pthread_barrier: exactly one thread observes PTHREAD_BARRIER_SERIAL_THREAD per round.
#include <pthread.h>
#include <stdio.h>
#include <stdatomic.h>

#define N 4
#define ROUNDS 3
static pthread_barrier_t bar;
static atomic_int serials = 0;
static atomic_int arrived = 0;

static void *worker(void *arg) {
    (void)arg;
    for (int r = 0; r < ROUNDS; r++) {
        atomic_fetch_add(&arrived, 1);
        int rc = pthread_barrier_wait(&bar);
        if (rc == PTHREAD_BARRIER_SERIAL_THREAD)
            atomic_fetch_add(&serials, 1);
        else if (rc != 0)
            return (void *)1;
    }
    return NULL;
}

int main(void) {
    pthread_barrier_init(&bar, NULL, N);
    pthread_t t[N];
    for (int i = 0; i < N; i++) pthread_create(&t[i], NULL, worker, NULL);
    int bad = 0;
    for (int i = 0; i < N; i++) {
        void *rv;
        pthread_join(t[i], &rv);
        if (rv) bad = 1;
    }
    pthread_barrier_destroy(&bar);
    printf("barrier serials=%d arrived=%d bad=%d\n",
           serials == ROUNDS, arrived == N * ROUNDS, bad);
    return 0;
}
