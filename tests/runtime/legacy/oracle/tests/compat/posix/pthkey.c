// pthread_key_create destructors: run on thread exit for non-NULL values, in the thread context.
#include <pthread.h>
#include <stdio.h>
#include <stdatomic.h>

static pthread_key_t key;
static atomic_int destroyed = 0;
static atomic_int seen_value = 0;

static void destructor(void *v) {
    if (v == (void *)0x1234) atomic_fetch_add(&seen_value, 1);
    atomic_fetch_add(&destroyed, 1);
}

static void *worker(void *arg) {
    (void)arg;
    pthread_setspecific(key, (void *)0x1234);
    // value visible within the thread
    if (pthread_getspecific(key) != (void *)0x1234) return (void *)1;
    return NULL;
}

int main(void) {
    pthread_key_create(&key, destructor);
    // main thread never sets a value -> its destructor must NOT run.
    pthread_t t[3];
    for (int i = 0; i < 3; i++) pthread_create(&t[i], NULL, worker, NULL);
    for (int i = 0; i < 3; i++) pthread_join(t[i], NULL);
    int main_val_null = pthread_getspecific(key) == NULL;
    pthread_key_delete(key);
    printf("key destroyed=%d value_ok=%d main_null=%d\n",
           destroyed == 3, seen_value == 3, main_val_null);
    return 0;
}
