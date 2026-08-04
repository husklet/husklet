// __thread thread-local storage isolation across many threads: each of 16 threads writes its index
// into a __thread variable, spins, then re-reads it -- no thread may observe another's value. Sums
// derived facts (all-isolated boolean, count of correct reads). Deterministic.
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

#define N 16
static __thread long tls_slot = -1;
static __thread long tls_second = 0xBEEF;
static atomic_int correct = 0;
static atomic_int leaked = 0;

static void *w(void *arg) {
    long idx = (long)arg;
    tls_slot = idx;
    tls_second = idx * 3 + 1;
    // give the scheduler room to interleave writers before we re-read
    for (int i = 0; i < 50; i++) { struct timespec ts = {0, 100000}; nanosleep(&ts, 0); }
    if (tls_slot == idx && tls_second == idx * 3 + 1) atomic_fetch_add(&correct, 1);
    else atomic_fetch_add(&leaked, 1);
    return 0;
}

int main(void) {
    pthread_t t[N];
    for (long i = 0; i < N; i++) pthread_create(&t[i], 0, w, (void *)i);
    for (int i = 0; i < N; i++) pthread_join(t[i], 0);
    // main's TLS untouched by workers
    int main_pristine = tls_slot == -1 && tls_second == 0xBEEF;
    printf("tls correct=%d leaked=%d main_pristine=%d\n",
           atomic_load(&correct), atomic_load(&leaked), main_pristine);
    return 0;
}
