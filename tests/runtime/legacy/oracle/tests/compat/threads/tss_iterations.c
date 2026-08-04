// TSD destructor re-run semantics: when a key destructor sets the key again to a non-NULL value, the
// pthread runtime must call the destructor again, up to PTHREAD_DESTRUCTOR_ITERATIONS times. A single
// thread arms a destructor that re-arms the key ITER-1 more times; the observed call count must be
// exactly ITER (bounded and deterministic), confirming the iteration loop in thread teardown.
#include <limits.h>
#include <pthread.h>
#include <stdio.h>

#define ITER 4   // must be <= PTHREAD_DESTRUCTOR_ITERATIONS (32 on Linux)
static pthread_key_t key;
static int calls;

static void dtor(void *p) {
    long n = (long)p;
    calls++;
    if (n > 1) pthread_setspecific(key, (void *)(n - 1)); // re-arm -> destructor runs again
}

static void *w(void *_) {
    (void)_;
    pthread_setspecific(key, (void *)(long)ITER);
    return 0;
}

int main(void) {
    int destructor_iterations_ok = PTHREAD_DESTRUCTOR_ITERATIONS >= ITER;
    pthread_key_create(&key, dtor);
    pthread_t t;
    pthread_create(&t, 0, w, 0);
    pthread_join(t, 0);
    pthread_key_delete(key);
    printf("tss_iterations calls=%d iters_ok=%d\n", calls, destructor_iterations_ok); // 4 1
    return 0;
}
