#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif

struct result {
    int descriptor;
    int owner_closed;
    int child_closed;
    _Atomic int child_ready;
    _Atomic int child_go;
    pthread_t child_thread;
};

static int is_closed(int descriptor) {
    errno = 0;
    return fcntl(descriptor, F_GETFD) == -1 && errno == EBADF;
}

static void *child(void *opaque) {
    struct result *result = opaque;
    atomic_store(&result->child_ready, 1);
    while (!atomic_load(&result->child_go))
        sched_yield();
    result->child_closed = is_closed(result->descriptor);
    return NULL;
}

static void *owner(void *opaque) {
    struct result *result = opaque;
    if (syscall(SYS_close_range, (unsigned)result->descriptor, (unsigned)result->descriptor, CLOSE_RANGE_UNSHARE) != 0)
        return NULL;
    result->owner_closed = is_closed(result->descriptor);
    if (pthread_create(&result->child_thread, NULL, child, result) != 0) return NULL;
    while (!atomic_load(&result->child_ready))
        sched_yield();
    return NULL;
}

int main(void) {
    char path[] = "/tmp/hl-close-range-inherit.XXXXXX";
    int descriptor = mkstemp(path);
    if (descriptor < 0) return 10;
    unlink(path);

    struct result result = {.descriptor = descriptor};
    pthread_t thread;
    if (pthread_create(&thread, NULL, owner, &result) != 0) return 11;
    if (pthread_join(thread, NULL) != 0) return 12;
    atomic_store(&result.child_go, 1);
    if (pthread_join(result.child_thread, NULL) != 0) return 13;
    int main_alive = fcntl(descriptor, F_GETFD) >= 0;
    int main_close = close(descriptor) == 0;
    printf("close_range_inherit owner_closed=%d child_closed=%d main_alive=%d main_close=%d\n", result.owner_closed,
           result.child_closed, main_alive, main_close);
    return result.owner_closed && result.child_closed && main_alive && main_close ? 0 : 1;
}
