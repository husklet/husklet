// FUTEX_WAKE_BITSET selectivity and exact count. Six waiters park on one word via FUTEX_WAIT_BITSET,
// each with a single distinct bit (three on bit A, three on bit B). A FUTEX_WAKE_BITSET must:
//   - wake only waiters whose bitset overlaps the wake mask (never disturb the disjoint group), and
//   - release at most the requested count, selecting exactly that many of the eligible waiters.
// This exercises the per-address grant selection: a plain broadcast that released every parked peer
// (regardless of mask or count) would report the wrong isolated/rest tallies.
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum { PER_GROUP = 3 };
#define WBIT_A 0x1u
#define WBIT_B 0x2u

static _Atomic int word;
static _Atomic int ready;
static _Atomic int woke_a;
static _Atomic int woke_b;

static long futex(int op, int val, const struct timespec *ts, uint32_t bits) {
    return syscall(SYS_futex, &word, op | FUTEX_PRIVATE_FLAG, val, ts, NULL, bits);
}

static void *waiter(void *arg) {
    uint32_t bit = (uint32_t)(uintptr_t)arg;
    atomic_fetch_add_explicit(&ready, 1, memory_order_release);
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_sec += 3; // absolute deadline for FUTEX_WAIT_BITSET
    while (futex(FUTEX_WAIT_BITSET, 0, &deadline, bit) < 0 && errno == EINTR) {}
    atomic_fetch_add_explicit(bit == WBIT_A ? &woke_a : &woke_b, 1, memory_order_release);
    return NULL;
}

int main(void) {
    pthread_t th[2 * PER_GROUP];
    for (int i = 0; i < PER_GROUP; ++i)
        pthread_create(&th[i], NULL, waiter, (void *)(uintptr_t)WBIT_A);
    for (int i = 0; i < PER_GROUP; ++i)
        pthread_create(&th[PER_GROUP + i], NULL, waiter, (void *)(uintptr_t)WBIT_B);
    while (atomic_load_explicit(&ready, memory_order_acquire) != 2 * PER_GROUP) sched_yield();
    usleep(50000); // let every waiter reach the park

    // Wake exactly one A-group waiter; the disjoint B group must be untouched.
    long first;
    do {
        first = futex(FUTEX_WAKE_BITSET, 1, NULL, WBIT_A);
        if (first == 0) sched_yield();
    } while (first == 0);
    usleep(50000);
    int a_after_one = atomic_load_explicit(&woke_a, memory_order_acquire);
    int b_after_one = atomic_load_explicit(&woke_b, memory_order_acquire);

    // Drain both groups.
    long rest_a = futex(FUTEX_WAKE_BITSET, INT_MAX, NULL, WBIT_A);
    long all_b = futex(FUTEX_WAKE_BITSET, INT_MAX, NULL, WBIT_B);
    for (int i = 0; i < 2 * PER_GROUP; ++i) pthread_join(th[i], NULL);

    printf("futex_wake_bitset first=%ld a_after_one=%d b_after_one=%d rest_a=%ld all_b=%ld a=%d b=%d\n",
           first, a_after_one, b_after_one, rest_a, all_b,
           atomic_load_explicit(&woke_a, memory_order_acquire),
           atomic_load_explicit(&woke_b, memory_order_acquire));
    return first == 1 && a_after_one == 1 && b_after_one == 0 && rest_a == PER_GROUP - 1 &&
                   all_b == PER_GROUP
               ? 0
               : 1;
}
