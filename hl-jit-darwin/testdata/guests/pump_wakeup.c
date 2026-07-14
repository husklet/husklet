// Synthetic repro for the Chrome "lost cross-thread wakeup" stall (hl aarch64 engine, eventfd/epoll).
//
// Chrome's browser main thread runs an idle message-pump loop: a MessagePumpEpoll blocks in epoll_pwait
// on an eventfd that peer threads write() to wake it (ScheduleWork), and a futex-backed mutex
// (base::Lock, FUTEX_WAIT val=2 = locked-with-waiters) guards the task queue. Under heavy multi-threaded
// startup the browser stops making progress -> no first paint. Root cause: hl emulated an eventfd as a
// {counter, readiness-pipe} pair mutated WITHOUT synchronization, so concurrent write()/read() interleave
// and strand the invariant "pipe-readable IFF counter>0" -- a byte left in the pipe with counter 0 makes a
// level-triggered epoll_wait report the fd endlessly ready while read() drains nothing (the pump busy-
// spins), and an edge-triggered watcher that saw no fresh edge never wakes (the "lost wakeup" park). Both
// also corrupt the accumulated counter. This program reproduces that mechanism WITHOUT chromium.
//
// Structure: ONE consumer ("main/pump") thread runs a LEVEL-triggered epoll_pwait loop on an eventfd; N
// producer threads each push M tasks (lock the futex mutex, enqueue, unlock) and write() the eventfd to
// wake the pump -- cross-waking under contention in a tight loop. The pump drains the eventfd, SUMS the
// counter values it reads, and pops every queued task.
//
// Correct engine: the pump observes every wake, processes all N*M tasks, AND the summed eventfd counter
// exactly equals the N*M writes -> prints "pump OK" -> exit 0. Buggy engine: tasks stall (watchdog fires,
// "pump STALL") OR the counter accounting is off by the raced increments ("pump COUNT ...") -> exit 1.
// Deterministic: the accounting assert fails on the unsynchronized engine every run; a dropped wake hangs.
#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef NWORKERS
#define NWORKERS 6
#endif
#ifndef NPER
#define NPER 4000
#endif
#define NTASKS (NWORKERS * NPER)

// ---- futex mutex: 0=unlocked, 1=locked no waiters, 2=locked maybe-waiters (glibc lowlevellock) ----
static int g_mtx;

static int futex(int *uaddr, int op, int val, const struct timespec *to) {
    return (int)syscall(SYS_futex, uaddr, op, val, to, NULL, 0);
}
static void mtx_lock(void) {
    int c;
    if ((c = __sync_val_compare_and_swap(&g_mtx, 0, 1)) != 0) {
        if (c != 2) c = __sync_lock_test_and_set(&g_mtx, 2);
        while (c != 0) {
            futex(&g_mtx, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 2, NULL);
            c = __sync_lock_test_and_set(&g_mtx, 2);
        }
    }
}
static void mtx_unlock(void) {
    if (__sync_fetch_and_sub(&g_mtx, 1) != 1) {
        g_mtx = 0;
        futex(&g_mtx, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL);
    }
}

static long g_queued;    // tasks pushed but not yet popped (under g_mtx)
static _Atomic long g_processed, g_counted, g_done;
static int g_efd, g_epfd;

static void *producer(void *arg) {
    (void)arg;
    for (int i = 0; i < NPER; i++) {
        mtx_lock();
        g_queued++;
        mtx_unlock();
        uint64_t one = 1;
        if (write(g_efd, &one, 8) != 8) { /* EAGAIN on a saturated counter is still readable */
        }
    }
    atomic_fetch_add(&g_done, 1);
    return NULL;
}

// Watchdog: no pump progress for ~8s => a wake was dropped (a real bug would hang forever, like chromium).
static void *watchdog(void *arg) {
    (void)arg;
    long last = -1, stall = 0;
    for (;;) {
        struct timespec t = {0, 200 * 1000 * 1000};
        nanosleep(&t, NULL);
        long now = atomic_load(&g_processed);
        if (now >= NTASKS) return NULL;
        if (now == last) {
            if (++stall >= 40) {
                fprintf(stderr, "pump STALL processed=%ld/%d queued=%ld (lost wakeup)\n", now, NTASKS,
                        __atomic_load_n(&g_queued, __ATOMIC_SEQ_CST));
                fflush(stderr);
                _exit(1);
            }
        } else {
            stall = 0;
            last = now;
        }
    }
}

int main(void) {
    g_efd = eventfd(0, EFD_NONBLOCK);
    if (g_efd < 0) { perror("eventfd"); return 2; }
    g_epfd = epoll_create1(EPOLL_CLOEXEC);
    if (g_epfd < 0) { perror("epoll_create1"); return 2; }
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 42}; // level-triggered wake fd (as Chrome)
    if (epoll_ctl(g_epfd, EPOLL_CTL_ADD, g_efd, &ev) < 0) { perror("epoll_ctl"); return 2; }

    pthread_t wtd, prod[NWORKERS];
    pthread_create(&wtd, NULL, watchdog, NULL);
    for (int i = 0; i < NWORKERS; i++) pthread_create(&prod[i], NULL, producer, NULL);

    // Pump: block in epoll_pwait; on any wake, sum+drain the eventfd counter and pop all queued tasks.
    // Exit once every task is processed AND (producers done, pipe quiet) so the counter is fully accounted.
    for (;;) {
        struct epoll_event out[8];
        int n = epoll_pwait(g_epfd, out, 8, 100, NULL);
        if (n < 0) {
            if (errno == EINTR) continue;
            perror("epoll_pwait");
            return 2;
        }
        uint64_t v;
        while (read(g_efd, &v, 8) == 8) atomic_fetch_add(&g_counted, (long)v); // exact counter accounting
        mtx_lock();
        long take = g_queued;
        g_queued = 0;
        mtx_unlock();
        if (take > 0) atomic_fetch_add(&g_processed, take);
        if (n == 0 && atomic_load(&g_done) == NWORKERS && atomic_load(&g_processed) >= NTASKS &&
            __atomic_load_n(&g_queued, __ATOMIC_SEQ_CST) == 0)
            break; // all work drained and the eventfd is quiet
    }

    for (int i = 0; i < NWORKERS; i++) pthread_join(prod[i], NULL);
    pthread_join(wtd, NULL);

    long processed = atomic_load(&g_processed), counted = atomic_load(&g_counted);
    if (processed != NTASKS) {
        fprintf(stderr, "pump TASKS processed=%ld want=%d\n", processed, NTASKS);
        return 1;
    }
    if (counted != NTASKS) { // the eventfd counter must account for EVERY write, exactly (no races)
        fprintf(stderr, "pump COUNT eventfd-counted=%ld want=%d (eventfd counter race)\n", counted, NTASKS);
        return 1;
    }
    printf("pump OK processed=%ld counted=%ld\n", processed, counted);
    return 0;
}
