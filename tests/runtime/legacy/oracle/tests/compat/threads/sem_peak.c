// Counting semaphore admission control: a sem initialized to PERMITS gates a critical section that 24
// threads hammer. An atomic "currently inside" counter tracks live occupancy; its peak must never
// exceed PERMITS. Also verifies total entries == iterations and the semaphore value returns to PERMITS.
#include <pthread.h>
#include <semaphore.h>
#include <stdatomic.h>
#include <stdio.h>

#define THREADS 24
#define PERMITS 3
#define ITERS 200

static sem_t sem;
static atomic_int inside = 0;
static atomic_int peak = 0;
static atomic_long entries = 0;

static void *w(void *_) {
    (void)_;
    for (int i = 0; i < ITERS; i++) {
        sem_wait(&sem);
        int now = atomic_fetch_add(&inside, 1) + 1;
        int prev = atomic_load(&peak);
        while (now > prev && !atomic_compare_exchange_weak(&peak, &prev, now)) {}
        atomic_fetch_add(&entries, 1);
        atomic_fetch_sub(&inside, 1);
        sem_post(&sem);
    }
    return 0;
}

int main(void) {
    sem_init(&sem, 0, PERMITS);
    pthread_t t[THREADS];
    for (int i = 0; i < THREADS; i++) pthread_create(&t[i], 0, w, 0);
    for (int i = 0; i < THREADS; i++) pthread_join(t[i], 0);
    int val = -1;
    sem_getvalue(&sem, &val);
    printf("sem_peak within_limit=%d entries_ok=%d restored=%d\n",
           atomic_load(&peak) <= PERMITS, atomic_load(&entries) == (long)THREADS * ITERS,
           val == PERMITS);
    sem_destroy(&sem);
    return 0;
}
