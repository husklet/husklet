// Cross-thread translation-cache safety: N threads concurrently execute the SAME hot code region
// (one shared pure function) for millions of iterations each. The engine may translate the region on
// one thread while others enter it; a race in the shared code cache (double-translate, torn chain
// link, publish-before-init) would corrupt a thread's result or crash. Each thread's work is a pure
// deterministic function of its id, so the aggregate checksum is schedule-independent. Deterministic.
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

#define NT 8

// Shared hot function -- every thread runs THIS exact code region concurrently.
static uint64_t hot(uint64_t a) {
    for (int i = 0; i < 2000000; i++) {
        a += (a << 7) ^ (a >> 3);
        a *= 0x100000001b3ULL;
        a ^= (a >> 29);
    }
    return a;
}

static uint64_t results[NT];

static void *worker(void *arg) {
    long id = (long)arg;
    results[id] = hot(0x9e3779b97f4a7c15ULL + (uint64_t)id);
    return NULL;
}

int main(void) {
    pthread_t th[NT];
    for (long i = 0; i < NT; i++) pthread_create(&th[i], NULL, worker, (void *)i);
    for (int i = 0; i < NT; i++) pthread_join(th[i], NULL);
    uint64_t acc = 0;
    for (int i = 0; i < NT; i++) acc = acc * 1000003ULL + results[i];
    // Cross-check against a single-threaded recompute (must match regardless of concurrency).
    uint64_t chk = 0;
    for (int i = 0; i < NT; i++) chk = chk * 1000003ULL + hot(0x9e3779b97f4a7c15ULL + (uint64_t)i);
    printf("conc-same acc=%llu match=%d\n", (unsigned long long)acc, acc == chk);
    return 0;
}
