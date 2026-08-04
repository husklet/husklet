// errno is thread-local: two pools of threads each force a *different* errno via a failing syscall,
// synchronize on a barrier so the failures overlap in time, then each verifies it still sees its own
// errno value -- proving no cross-thread clobber. Deterministic derived counts.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <unistd.h>

#define N 8
static pthread_barrier_t bar;
static atomic_int isolated = 0;

static void *w(void *arg) {
    long idx = (long)arg;
    if (idx % 2 == 0) {
        // EBADF: read from a surely-invalid fd
        errno = 0;
        (void)!read(-1, NULL, 1);
        pthread_barrier_wait(&bar);         // overlap the window where errno is live
        if (errno == EBADF) atomic_fetch_add(&isolated, 1);
    } else {
        // EINVAL: fcntl with a bogus command
        errno = 0;
        (void)fcntl(0, -1);
        pthread_barrier_wait(&bar);
        if (errno == EINVAL) atomic_fetch_add(&isolated, 1);
    }
    return 0;
}

int main(void) {
    pthread_barrier_init(&bar, 0, N);
    pthread_t t[N];
    for (long i = 0; i < N; i++) pthread_create(&t[i], 0, w, (void *)i);
    for (int i = 0; i < N; i++) pthread_join(t[i], 0);
    printf("errno_threadlocal isolated=%d\n", atomic_load(&isolated)); // 8
    return 0;
}
