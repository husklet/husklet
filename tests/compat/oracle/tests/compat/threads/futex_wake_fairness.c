// FUTEX_WAKE(1) must make progress across an address's wait queue. A waiter that
// wakes and immediately waits again joins behind an already-parked peer; it must
// not monopolize every subsequent wake merely because its engine bookkeeping
// slot has the lowest index.
#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static _Atomic int word;
static _Atomic int first_ready;
static _Atomic int first_wakes;
static _Atomic int first_rewaiting;
static _Atomic int second_ready;
static _Atomic int second_woke;

static long wait_word(void) {
    return syscall(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, NULL, NULL, 0);
}

static long wake_word(int count) {
    return syscall(SYS_futex, &word, FUTEX_WAKE_PRIVATE, count, NULL, NULL, 0);
}

static void pause_ms(long milliseconds) {
    struct timespec delay = {.tv_nsec = milliseconds * 1000000};
    nanosleep(&delay, NULL);
}

static int await(_Atomic int *value, int expected) {
    for (int i = 0; i < 200; i++) {
        if (atomic_load_explicit(value, memory_order_acquire) >= expected) return 1;
        pause_ms(5);
    }
    return 0;
}

static void *first_waiter(void *unused) {
    (void)unused;
    atomic_store_explicit(&first_ready, 1, memory_order_release);
    if (wait_word() == 0) atomic_fetch_add_explicit(&first_wakes, 1, memory_order_release);
    atomic_store_explicit(&first_rewaiting, 1, memory_order_release);
    (void)wait_word();
    return NULL;
}

static void *second_waiter(void *unused) {
    (void)unused;
    atomic_store_explicit(&second_ready, 1, memory_order_release);
    if (wait_word() == 0) atomic_store_explicit(&second_woke, 1, memory_order_release);
    return NULL;
}

int main(void) {
    alarm(8);
    pthread_t first;
    pthread_t second;
    pthread_create(&first, NULL, first_waiter, NULL);
    if (!await(&first_ready, 1)) return 2;
    pause_ms(100); // first waiter is queued before the second starts
    pthread_create(&second, NULL, second_waiter, NULL);
    if (!await(&second_ready, 1)) return 3;
    pause_ms(100); // both waiters are parked

    if (wake_word(1) != 1 || !await(&first_wakes, 1) || !await(&first_rewaiting, 1)) return 4;
    pause_ms(100); // the first waiter has rejoined behind the second
    if (wake_word(1) != 1) return 5;
    pause_ms(100);

    int fair = atomic_load_explicit(&second_woke, memory_order_acquire) == 1;
    atomic_store_explicit(&word, 1, memory_order_release);
    (void)wake_word(2);
    pthread_join(first, NULL);
    pthread_join(second, NULL);
    printf("futex_wake_fairness second=%d\n", fair);
    return fair ? 0 : 1;
}
