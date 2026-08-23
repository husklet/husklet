#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#define THREADS 96

struct state {
    int descriptor;
    _Atomic int ready;
    _Atomic int go;
    _Atomic int passed;
};

static void *inspect(void *opaque) {
    struct state *state = opaque;
    atomic_fetch_add(&state->ready, 1);
    while (!atomic_load(&state->go))
        sched_yield();
    if (fcntl(state->descriptor, F_GETFD) >= 0) atomic_fetch_add(&state->passed, 1);
    while (atomic_load(&state->go) == 1)
        sched_yield();
    return NULL;
}

int main(void) {
    char path[] = "/tmp/hl-binding-overflow.XXXXXX";
    struct state state = {.descriptor = mkstemp(path)};
    if (state.descriptor < 0) return 10;
    unlink(path);

    pthread_t threads[THREADS];
    for (int i = 0; i < THREADS; i++)
        if (pthread_create(&threads[i], NULL, inspect, &state) != 0) return 11;
    while (atomic_load(&state.ready) != THREADS)
        sched_yield();
    atomic_store(&state.go, 1);
    while (atomic_load(&state.passed) != THREADS)
        sched_yield();
    atomic_store(&state.go, 2);
    for (int i = 0; i < THREADS; i++)
        if (pthread_join(threads[i], NULL) != 0) return 12;
    int final_close = close(state.descriptor) == 0;
    printf("sentry_binding_overflow passed=%d final_close=%d\n", atomic_load(&state.passed), final_close);
    return atomic_load(&state.passed) == THREADS && final_close ? 0 : 1;
}
