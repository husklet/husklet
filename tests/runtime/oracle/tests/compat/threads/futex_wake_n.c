#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum { THREADS = 6 };
static _Atomic int word;
static _Atomic int ready;
static _Atomic int returned;

static long futex(int op, int count, const struct timespec *timeout) {
    return syscall(SYS_futex, &word, op | FUTEX_PRIVATE_FLAG, count, timeout, NULL, 0);
}

static void *waiter(void *unused) {
    (void)unused;
    atomic_fetch_add_explicit(&ready, 1, memory_order_release);
    struct timespec timeout = {.tv_sec = 2};
    while (futex(FUTEX_WAIT, 0, &timeout) < 0 && errno == EINTR) {}
    atomic_fetch_add_explicit(&returned, 1, memory_order_release);
    return NULL;
}

int main(void) {
    pthread_t threads[THREADS];
    for (int i = 0; i < THREADS; ++i) pthread_create(&threads[i], NULL, waiter, NULL);
    while (atomic_load_explicit(&ready, memory_order_acquire) != THREADS) sched_yield();

    long first;
    do {
        first = futex(FUTEX_WAKE, 1, NULL);
        if (first == 0) sched_yield();
    } while (first == 0);
    usleep(50000);
    int isolated = atomic_load_explicit(&returned, memory_order_acquire) == 1;
    long rest = futex(FUTEX_WAKE, INT_MAX, NULL);
    for (int i = 0; i < THREADS; ++i) pthread_join(threads[i], NULL);

    int total = atomic_load_explicit(&returned, memory_order_acquire);
    printf("futex_wake_n first=%ld isolated=%d rest=%ld total=%d\n", first, isolated, rest, total);
    return first == 1 && isolated && rest == THREADS - 1 && total == THREADS ? 0 : 1;
}
