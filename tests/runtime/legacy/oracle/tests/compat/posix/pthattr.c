// pthread_attr: explicit stacksize/guardsize/detachstate honored; thread runs on the set stack size.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>

#define STACK (256 * 1024)
#define GUARD 8192
static size_t observed_stack = 0;

static void *worker(void *arg) {
    (void)arg;
    pthread_attr_t a;
    if (pthread_getattr_np(pthread_self(), &a) == 0) {
        void *base;
        size_t sz;
        if (pthread_attr_getstack(&a, &base, &sz) == 0) observed_stack = sz;
        pthread_attr_destroy(&a);
    }
    return NULL;
}

int main(void) {
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, STACK);
    pthread_attr_setguardsize(&attr, GUARD);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_JOINABLE);

    size_t gs = 0, ss = 0;
    int ds = -1;
    pthread_attr_getstacksize(&attr, &ss);
    pthread_attr_getguardsize(&attr, &gs);
    pthread_attr_getdetachstate(&attr, &ds);

    pthread_t t;
    pthread_create(&t, NULL, worker, &attr);
    pthread_join(t, NULL);
    pthread_attr_destroy(&attr);

    printf("attr ss=%d gs=%d joinable=%d stack_ge=%d\n",
           ss == STACK, gs == GUARD, ds == PTHREAD_CREATE_JOINABLE,
           observed_stack >= STACK);
    return 0;
}
