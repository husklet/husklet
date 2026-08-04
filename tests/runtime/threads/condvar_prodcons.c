// Condition-variable bounded-buffer producer/consumer: 4 producers push a fixed number of items into
// a small ring guarded by a mutex + two condvars (not-full / not-empty); 4 consumers drain them and
// accumulate a checksum. The total consumed count and checksum are deterministic regardless of
// interleaving, which is exactly what a correct condvar implementation must guarantee.
#include <pthread.h>
#include <stdio.h>

#define PRODUCERS 4
#define CONSUMERS 4
#define PER_PRODUCER 500
#define CAP 8
#define TOTAL (PRODUCERS * PER_PRODUCER)

static int ring[CAP];
static int head, tail, count;
static long produced_sum, consumed_sum, consumed_n;
static int producers_done;
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER;
static pthread_cond_t not_empty = PTHREAD_COND_INITIALIZER;

static void *producer(void *arg) {
    long base = (long)arg;
    for (int i = 0; i < PER_PRODUCER; i++) {
        int val = (int)(base * 1000 + i);
        pthread_mutex_lock(&m);
        while (count == CAP) pthread_cond_wait(&not_full, &m);
        ring[tail] = val; tail = (tail + 1) % CAP; count++;
        produced_sum += val;
        pthread_cond_signal(&not_empty);
        pthread_mutex_unlock(&m);
    }
    return 0;
}

static void *consumer(void *arg) {
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&m);
        while (count == 0 && !(producers_done && count == 0)) {
            if (producers_done && count == 0) break;
            pthread_cond_wait(&not_empty, &m);
        }
        if (count == 0 && producers_done) { pthread_mutex_unlock(&m); break; }
        int val = ring[head]; head = (head + 1) % CAP; count--;
        consumed_sum += val; consumed_n++;
        pthread_cond_signal(&not_full);
        pthread_mutex_unlock(&m);
    }
    return 0;
}

int main(void) {
    pthread_t p[PRODUCERS], c[CONSUMERS];
    for (long i = 0; i < CONSUMERS; i++) pthread_create(&c[i], 0, consumer, (void *)i);
    for (long i = 0; i < PRODUCERS; i++) pthread_create(&p[i], 0, producer, (void *)(i + 1));
    for (int i = 0; i < PRODUCERS; i++) pthread_join(p[i], 0);
    pthread_mutex_lock(&m);
    producers_done = 1;
    pthread_cond_broadcast(&not_empty);   // wake consumers to observe drain-complete
    pthread_mutex_unlock(&m);
    for (int i = 0; i < CONSUMERS; i++) pthread_join(c[i], 0);
    printf("prodcons n=%ld sums_equal=%d total_ok=%d\n",
           consumed_n, produced_sum == consumed_sum, consumed_n == TOTAL);
    return 0;
}
