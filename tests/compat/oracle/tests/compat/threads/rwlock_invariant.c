// rwlock reader/writer exclusion invariant under contention: writers hold the lock exclusively while
// mutating a pair of words that readers must always observe as consistent (equal). 4 writers bump both
// words under the write lock; 12 readers repeatedly acquire the read lock and check the invariant.
// The lock is configured writer-preferring so writers cannot be starved (bounded, terminating run),
// and readers do a fixed number of iterations. Deterministic booleans.
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>

#define READERS 12
#define WRITERS 4
#define WITERS 2000
#define RITERS 4000

static pthread_rwlock_t lk;
static long a, b;                 // invariant: a == b outside the write critical section
static atomic_int live_writers = 0;
static atomic_long violations = 0;

static void *writer(void *_) {
    (void)_;
    for (int i = 0; i < WITERS; i++) {
        pthread_rwlock_wrlock(&lk);
        int w = atomic_fetch_add(&live_writers, 1) + 1;
        if (w != 1) atomic_fetch_add(&violations, 1); // two writers at once
        a++;                       // transiently a != b between these stores
        b++;
        atomic_fetch_sub(&live_writers, 1);
        pthread_rwlock_unlock(&lk);
    }
    return 0;
}

static void *reader(void *_) {
    (void)_;
    for (int i = 0; i < RITERS; i++) {
        pthread_rwlock_rdlock(&lk);
        if (a != b || atomic_load(&live_writers) != 0) atomic_fetch_add(&violations, 1);
        pthread_rwlock_unlock(&lk);
        sched_yield();             // let writers in -> avoids reader monopoly
    }
    return 0;
}

int main(void) {
    pthread_rwlockattr_t attr;
    pthread_rwlockattr_init(&attr);
    pthread_rwlockattr_setkind_np(&attr, PTHREAD_RWLOCK_PREFER_WRITER_NONRECURSIVE_NP);
    pthread_rwlock_init(&lk, &attr);
    pthread_rwlockattr_destroy(&attr);

    pthread_t r[READERS], w[WRITERS];
    for (int i = 0; i < READERS; i++) pthread_create(&r[i], 0, reader, 0);
    for (int i = 0; i < WRITERS; i++) pthread_create(&w[i], 0, writer, 0);
    for (int i = 0; i < WRITERS; i++) pthread_join(w[i], 0);
    for (int i = 0; i < READERS; i++) pthread_join(r[i], 0);
    pthread_rwlock_destroy(&lk);
    printf("rwlock_invariant violations=%ld final_ok=%d\n",
           atomic_load(&violations), a == b && a == (long)WRITERS * WITERS);
    return 0;
}
