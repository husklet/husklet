// pthread_once establishes a value that every racing thread observes: 24 threads race through
// pthread_once to lazily compute a shared result; the initializer must run exactly once and every
// thread must read the fully-published value (memory-ordering guarantee of pthread_once). Deterministic.
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>

#define N 24
static pthread_once_t once = PTHREAD_ONCE_INIT;
static int value;            // published by the initializer
static atomic_int init_runs = 0;
static atomic_int correct_reads = 0;

static void init(void) {
    // simulate nontrivial init so a racing thread would see a partial value if once() were broken
    for (int i = 0; i < 1000; i++) value += 42;
    atomic_fetch_add(&init_runs, 1);
}

static void *w(void *_) {
    (void)_;
    pthread_once(&once, init);
    if (value == 42000) atomic_fetch_add(&correct_reads, 1);
    return 0;
}

int main(void) {
    pthread_t t[N];
    for (int i = 0; i < N; i++) pthread_create(&t[i], 0, w, 0);
    for (int i = 0; i < N; i++) pthread_join(t[i], 0);
    printf("once_value init_runs=%d correct_reads=%d\n",
           atomic_load(&init_runs), atomic_load(&correct_reads)); // 1 24
    return 0;
}
