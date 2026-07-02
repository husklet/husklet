// Direct futex(2) FUTEX_WAIT/FUTEX_WAKE between two threads (no pthread mutex): a waiter blocks on a
// shared word; the main thread stores the new value and wakes exactly one waiter. Linux-only -> oracle.
#define _GNU_SOURCE
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static atomic_int word = 0;
static volatile int woke = 0;

static long fwait(int *addr, int expected) {
    return syscall(SYS_futex, addr, FUTEX_WAIT, expected, NULL, NULL, 0);
}
static long fwake(int *addr, int n) {
    return syscall(SYS_futex, addr, FUTEX_WAKE, n, NULL, NULL, 0);
}

static void *waiter(void *arg) {
    (void)arg;
    // spin until the value flips to 1 via a futex wait; re-check on spurious wakeups
    while (atomic_load(&word) == 0) {
        fwait((int *)&word, 0);
    }
    woke = 1;
    return NULL;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, NULL, waiter, NULL);
    struct timespec s = { .tv_sec = 0, .tv_nsec = 50000000 };
    nanosleep(&s, NULL);            // let the waiter block in FUTEX_WAIT
    atomic_store(&word, 1);
    long n = fwake((int *)&word, 1);
    pthread_join(t, NULL);
    int woke_one = n >= 0;          // number actually woken (0 or 1 depending on timing)
    printf("futex woke=%d wake_rc_ok=%d\n", woke, woke_one);
    return 0;
}
