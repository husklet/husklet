// A spinlock whose release is a PLAIN store, not a read-modify-write: acquire with a compare-exchange
// that leaves the owner's id in the word, then release by storing 0. The holder re-reads the word, which
// can only name the holder. Guest stores that reach memory more than once (a translator copying them with
// a runtime-sized memcpy does exactly that) resurrect the released 0 over the next acquirer's exchange and
// hand two threads the lock at the same time.
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>

#define THREADS 8
#define ROUNDS 300000

static int lock_word __attribute__((aligned(128)));
static long breaches __attribute__((aligned(128)));

static void *worker(void *argument) {
    int id = (int)(long)argument + 1;
    for (long i = 0; i < ROUNDS; i++) {
        int free_word = 0;
        while (!__atomic_compare_exchange_n(&lock_word, &free_word, id, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
            free_word = 0;
        if (__atomic_load_n(&lock_word, __ATOMIC_RELAXED) != id)
            __atomic_fetch_add(&breaches, 1, __ATOMIC_SEQ_CST);
        __atomic_store_n(&lock_word, 0, __ATOMIC_RELEASE);
    }
    return NULL;
}

int main(void) {
    pthread_t thread[THREADS];
    for (int i = 0; i < THREADS; i++) pthread_create(&thread[i], NULL, worker, (void *)(long)i);
    for (int i = 0; i < THREADS; i++) pthread_join(thread[i], NULL);
    printf("spin release-store exclusive=%d\n", breaches == 0);
    return 0;
}
