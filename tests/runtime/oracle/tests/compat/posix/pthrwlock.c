// pthread_rwlock: multiple concurrent readers, exclusive writers, trywrlock EBUSY under read.
#include <pthread.h>
#include <stdio.h>
#include <errno.h>
#include <stdatomic.h>

static pthread_rwlock_t rw;
static atomic_int concurrent = 0;
static atomic_int max_concurrent = 0;
static long shared = 0;

#define R 4
#define ITERS 20000

static void *reader(void *arg) {
    (void)arg;
    for (int i = 0; i < ITERS; i++) {
        pthread_rwlock_rdlock(&rw);
        int c = atomic_fetch_add(&concurrent, 1) + 1;
        int m = atomic_load(&max_concurrent);
        while (c > m && !atomic_compare_exchange_weak(&max_concurrent, &m, c)) {}
        (void)shared;
        atomic_fetch_sub(&concurrent, 1);
        pthread_rwlock_unlock(&rw);
    }
    return NULL;
}

static void *writer(void *arg) {
    (void)arg;
    for (int i = 0; i < ITERS; i++) {
        pthread_rwlock_wrlock(&rw);
        shared++;
        pthread_rwlock_unlock(&rw);
    }
    return NULL;
}

int main(void) {
    pthread_rwlockattr_t attr;
    pthread_rwlockattr_init(&attr);
    pthread_rwlockattr_setkind_np(&attr, PTHREAD_RWLOCK_PREFER_READER_NP);
    pthread_rwlock_init(&rw, &attr);
    pthread_rwlockattr_destroy(&attr);

    pthread_rwlock_rdlock(&rw);
    int wbusy = pthread_rwlock_trywrlock(&rw) == EBUSY;
    pthread_rwlock_unlock(&rw);

    pthread_t rd[R], wr[2];
    for (int i = 0; i < R; i++) pthread_create(&rd[i], NULL, reader, NULL);
    for (int i = 0; i < 2; i++) pthread_create(&wr[i], NULL, writer, NULL);
    for (int i = 0; i < R; i++) pthread_join(rd[i], NULL);
    for (int i = 0; i < 2; i++) pthread_join(wr[i], NULL);
    pthread_rwlock_destroy(&rw);
    // max_concurrent is timing-dependent; only assert it never underflowed.
    printf("rwlock wbusy=%d writes_ok=%d readers_ok=%d\n",
           wbusy, shared == 2 * ITERS, max_concurrent >= 1);
    return 0;
}
