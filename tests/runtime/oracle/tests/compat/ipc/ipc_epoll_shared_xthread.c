// SHARED epoll instance across threads: one waiter thread blocks in epoll_wait while OTHER threads
// register (epoll_ctl ADD, EPOLLET) fds that are ALREADY readable. This is the exact case hl's W3E epoll
// fast path (event.c: ep_flush + EVFILT_USER NOTE_TRIGGER wake + g_ep_prime registration-time readiness)
// exists to serve -- and the one case NO existing gate exercises (every pump gate is single-thread-per-
// instance). On Linux the EPOLLET ADD of an already-ready fd wakes the blocked waiter at once (the
// registration edge); hl must reproduce that cross-thread via the NOTE_TRIGGER + prime path. A lost
// wake => the just-registered ready fd is never delivered => the token it carries is never drained =>
// a bounded per-fd retry backlog builds and the watchdog fails the run. Run under host CPU load to widen
// the waiter's unlocked "returned-from-kevent / about-to-re-block" window where a trigger can be dropped.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

#define NPROD   4
#define ROUNDS  8000    // registrations per producer
#define TOTAL   (NPROD * ROUNDS)

static int g_ep;
static _Atomic long g_delivered = 0;   // fds whose readiness the waiter delivered + drained
static _Atomic long g_registered = 0;
static _Atomic int g_stop = 0;
static _Atomic long g_addfail = 0; // spurious epoll_ctl(ADD) rejections (must stay 0: a real EEXIST leaves registered<TOTAL)

// The waiter: the ONLY thread that calls epoll_wait on g_ep. It drains every delivered fd (a pipe with one
// byte already in it) and closes it, so a delivered registration makes forward progress.
static void *waiter_fn(void *arg) {
    (void)arg;
    struct epoll_event out[64];
    while (!atomic_load(&g_stop) || atomic_load(&g_delivered) < atomic_load(&g_registered)) {
        int n = epoll_wait(g_ep, out, 64, 500);
        for (int i = 0; i < n; i++) {
            int fd = out[i].data.fd;
            char b[8];
            while (read(fd, b, sizeof b) > 0) {} // drain (EPOLLET)
            epoll_ctl(g_ep, EPOLL_CTL_DEL, fd, NULL);
            close(fd);
            atomic_fetch_add(&g_delivered, 1);
        }
    }
    return NULL;
}

// Producers: create an ALREADY-READABLE pipe (write a byte first) and register its read end EPOLLET on the
// shared instance while the waiter is (usually) blocked in epoll_wait -- the cross-thread registration edge.
static void *producer_fn(void *arg) {
    (void)arg;
    for (long r = 0; r < ROUNDS; r++) {
        // backpressure: keep outstanding (registered-but-not-yet-drained) bounded so we test the
        // cross-thread wakeup path, not the fd limit. If the waiter falls behind, yield until it catches up.
        while (atomic_load(&g_registered) - atomic_load(&g_delivered) > 200 && !atomic_load(&g_stop)) {
            struct timespec ts = {0, 50 * 1000};
            nanosleep(&ts, NULL);
        }
        int p[2];
        if (pipe(p) != 0) { // transient fd pressure: back off and retry this round, don't abort
            struct timespec ts = {0, 200 * 1000};
            nanosleep(&ts, NULL);
            r--;
            continue;
        }
        char one = 'x';
        if (write(p[1], &one, 1) != 1) {}   // make the read end ALREADY readable BEFORE registering
        close(p[1]);                        // read end stays readable (1 byte buffered) + will EOF too
        fcntl(p[0], F_SETFL, fcntl(p[0], F_GETFL) | O_NONBLOCK);
        struct epoll_event ev = {.events = EPOLLIN | EPOLLET, .data.fd = p[0]};
        if (epoll_ctl(g_ep, EPOLL_CTL_ADD, p[0], &ev) != 0) {
            atomic_fetch_add(&g_addfail, 1);
            close(p[0]);
            continue;
        }
        atomic_fetch_add(&g_registered, 1);
        // brief spacing so registrations land across the waiter's whole wait/return/re-block cycle
        if ((r & 7) == 0) { struct timespec ts = {0, 30 * 1000}; nanosleep(&ts, NULL); }
    }
    return NULL;
}

static void *watchdog_fn(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 500 * 1000 * 1000};
        nanosleep(&ts, NULL);
        long d = atomic_load(&g_delivered);
        if (d >= TOTAL) return NULL;
        if (d == last && atomic_load(&g_registered) > d) {
            fprintf(stderr, "STALL delivered=%ld registered=%ld total=%d\n", d, atomic_load(&g_registered), TOTAL);
            fflush(stderr);
            _exit(7); // a cross-thread registration edge was lost: waiter parked with a ready fd pending
        }
        last = d;
    }
}

int main(void) {
    g_ep = epoll_create1(EPOLL_CLOEXEC);
    if (g_ep < 0) { perror("epoll_create1"); return 1; }

    pthread_t waiter, wd, prod[NPROD];
    pthread_create(&waiter, NULL, waiter_fn, NULL);
    pthread_create(&wd, NULL, watchdog_fn, NULL);
    for (int i = 0; i < NPROD; i++) pthread_create(&prod[i], NULL, producer_fn, NULL);
    for (int i = 0; i < NPROD; i++) pthread_join(prod[i], NULL);

    atomic_store(&g_stop, 1);
    pthread_join(waiter, NULL);
    long d = atomic_load(&g_delivered), reg = atomic_load(&g_registered);
    // addfail/lasterrno are kept internal (a spurious EEXIST would leave registered<TOTAL -> ok=0); print
    // the stable golden line so this doubles as a regression guard for the membership-bitmap atomicity fix.
    printf("epoll_shared_xthread registered=%ld delivered=%ld ok=%d\n", reg, d, d == reg && reg == TOTAL);
    return (d == reg && reg == TOTAL) ? 0 : 5;
}
