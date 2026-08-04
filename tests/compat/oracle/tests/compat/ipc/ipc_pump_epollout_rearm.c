// EPOLLOUT (write-readiness) EDGE re-arm on a SOCK_STREAM socketpair -- the "message written but never
// flushed" wall. multi-process application's IPC PlatformChannel is SOCK_STREAM; when a channel write would block, the
// Channel arms EPOLLOUT|EPOLLET, waits for the writable edge, flushes the rest, then disarms. Every
// existing pump gate only exercises EPOLLIN readiness; none covers the WRITE side's drain->re-arm edge,
// which is a distinct kqueue path (EVFILT_WRITE + EV_CLEAR) that can drop the writable transition
// between the reader draining the peer's recv buffer and the writer re-blocking. A lost writable edge
// leaves the sender parked with a half-written message forever -- the coordinator->worker node-channel
// write that never lands. Two threads: a writer that fills the socket then EPOLLOUT-waits for room, and
// a reader thread that drains slowly; a watchdog turns any stall into a deterministic nonzero exit.
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
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define TOTAL   (64L * 1024 * 1024) // bytes to stream through a deliberately small socket buffer
#define CHUNK   4096

static int sv[2];
static _Atomic long g_sent = 0, g_recv = 0;
static _Atomic int g_done = 0;

static void *reader_fn(void *arg) {
    (void)arg;
    // drain slowly-ish so the writer genuinely fills the buffer and must EPOLLOUT-wait each time
    char buf[CHUNK];
    while (atomic_load(&g_recv) < TOTAL) {
        ssize_t r = read(sv[1], buf, sizeof buf);
        if (r > 0) {
            atomic_fetch_add(&g_recv, r);
        } else if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct timespec ts = {0, 200 * 1000};
            nanosleep(&ts, NULL);
        } else if (r == 0) {
            break;
        }
    }
    atomic_store(&g_done, 1);
    return NULL;
}

static void *watchdog_fn(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 400 * 1000 * 1000};
        nanosleep(&ts, NULL);
        if (atomic_load(&g_done)) return NULL;
        long s = atomic_load(&g_sent);
        if (s == last) {
            fprintf(stderr, "STALL sent=%ld recv=%ld total=%ld\n", s, atomic_load(&g_recv), TOTAL);
            fflush(stderr);
            _exit(7); // lost EPOLLOUT edge: writer parked with room available
        }
        last = s;
    }
}

int main(void) {
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { perror("socketpair"); return 1; }
    // small buffers so the sender blocks often -> the EPOLLOUT re-arm path is exercised thousands of times
    int bs = 8192;
    setsockopt(sv[0], SOL_SOCKET, SO_SNDBUF, &bs, sizeof bs);
    setsockopt(sv[1], SOL_SOCKET, SO_RCVBUF, &bs, sizeof bs);
    fcntl(sv[0], F_SETFL, fcntl(sv[0], F_GETFL) | O_NONBLOCK);
    fcntl(sv[1], F_SETFL, fcntl(sv[1], F_GETFL) | O_NONBLOCK);

    int ep = epoll_create1(EPOLL_CLOEXEC);

    pthread_t rd, wd;
    pthread_create(&rd, NULL, reader_fn, NULL);
    pthread_create(&wd, NULL, watchdog_fn, NULL);

    static char buf[CHUNK];
    memset(buf, 0xC3, sizeof buf);
    int armed = 0;
    struct epoll_event out[4];
    struct epoll_event we = {.events = EPOLLOUT | EPOLLET, .data.fd = sv[0]};
    while (atomic_load(&g_sent) < TOTAL) {
        ssize_t w = write(sv[0], buf, sizeof buf);
        if (w > 0) {
            atomic_fetch_add(&g_sent, w);
            continue;
        }
        if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            // would block: arm EPOLLOUT|EPOLLET and wait for the writable edge (multi-process application's channel write watch)
            if (!armed) {
                if (epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &we) != 0) return 2;
                armed = 1;
            }
            int n = epoll_wait(ep, out, 4, 4000);
            if (n <= 0) { fprintf(stderr, "EPOLLOUT wait n=%d sent=%ld\n", n, atomic_load(&g_sent)); return 3; }
            // edge consumed; disarm and retry the write (matches Channel's write-watch lifecycle)
            epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], &we);
            armed = 0;
            continue;
        }
        perror("write");
        return 4;
    }
    // let the reader finish
    while (!atomic_load(&g_done)) {
        struct timespec ts = {0, 1000 * 1000};
        nanosleep(&ts, NULL);
        if (atomic_load(&g_recv) >= TOTAL) break;
    }
    pthread_join(rd, NULL);
    printf("epollout_rearm sent=%ld recv=%ld ok=%d\n", atomic_load(&g_sent), atomic_load(&g_recv),
           atomic_load(&g_sent) == TOTAL && atomic_load(&g_recv) == TOTAL);
    return (atomic_load(&g_sent) == TOTAL && atomic_load(&g_recv) == TOTAL) ? 0 : 5;
}
