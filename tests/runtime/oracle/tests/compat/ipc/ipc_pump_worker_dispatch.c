// In-process worker-shaped model: an IO epoll pump (SEQPACKET channel + ScheduleWork eventfd, EPOLLET)
// that, per inbound message, dispatches work to one of N worker threads parked on a condvar (glibc
// condvar => FUTEX_WAIT under hl). Mirrors the observed dormant multi-process application-worker park state -- several
// threads in FUTEX_WAIT plus the epoll pump -- and exercises the pump->worker handoff that couples the
// epoll-readiness wakeup to a futex wakeup (no existing gate combines the two). A missed wakeup anywhere
// (eventfd edge, socket edge, or the FUTEX handoff) stalls the pipeline; a watchdog turns any stall into
// a deterministic nonzero exit. All ROUNDS must complete.
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
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define NWORK   7
#define ROUNDS  30000

static int chan[2], wake_fd, ack_fd;
static _Atomic long g_done_rounds = 0;
static _Atomic int g_quit = 0;

// worker dispatch: a tiny queue slot per worker + condvar (glibc condvar => FUTEX under hl)
struct worker {
    pthread_mutex_t m;
    pthread_cond_t cv;
    int njobs;      // pending job count (counting queue -> no job dropped on collision)
    pthread_t th;
};
static struct worker W[NWORK];

static void *worker_fn(void *arg) {
    struct worker *w = arg;
    for (;;) {
        pthread_mutex_lock(&w->m);
        while (w->njobs == 0 && !atomic_load(&g_quit)) pthread_cond_wait(&w->cv, &w->m); // FUTEX_WAIT
        if (w->njobs == 0 && atomic_load(&g_quit)) { pthread_mutex_unlock(&w->m); return NULL; }
        w->njobs--;
        pthread_mutex_unlock(&w->m);
        // "process" the job, then ack back to the IO pump so it can pace the producer
        atomic_fetch_add(&g_done_rounds, 1);
        uint64_t one = 1;
        if (write(ack_fd, &one, 8) != 8) {}
    }
}

static void dispatch(long job) {
    struct worker *w = &W[job % NWORK];
    pthread_mutex_lock(&w->m);
    w->njobs++;
    pthread_cond_signal(&w->cv); // FUTEX_WAKE the parked worker
    pthread_mutex_unlock(&w->m);
}

static void *io_pump(void *arg) {
    (void)arg;
    fcntl(chan[1], F_SETFL, fcntl(chan[1], F_GETFL) | O_NONBLOCK);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ce = {.events = EPOLLIN | EPOLLET, .data.fd = chan[1]};
    struct epoll_event we = {.events = EPOLLIN | EPOLLET, .data.fd = wake_fd};
    epoll_ctl(ep, EPOLL_CTL_ADD, chan[1], &ce);
    epoll_ctl(ep, EPOLL_CTL_ADD, wake_fd, &we);
    struct epoll_event out[8];
    for (;;) {
        int n = epoll_wait(ep, out, 8, 2000);
        if (n <= 0) { if (atomic_load(&g_quit)) return NULL; continue; }
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd == wake_fd) {
                uint64_t v; while (read(wake_fd, &v, 8) == 8) {}
            } else if (out[i].data.fd == chan[1]) {
                char buf[128];
                for (;;) {
                    ssize_t r = recv(chan[1], buf, sizeof buf, 0);
                    if (r <= 0) break;
                    long job; memcpy(&job, buf, 8);
                    dispatch(job); // hand off to a futex-parked worker
                }
            }
        }
        if (atomic_load(&g_quit)) return NULL;
    }
}

static void *watchdog(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 300 * 1000 * 1000};
        nanosleep(&ts, NULL);
        long d = atomic_load(&g_done_rounds);
        if (d >= ROUNDS) return NULL;
        if (d == last) { fprintf(stderr, "STALL done=%ld/%d\n", d, ROUNDS); fflush(stderr); _exit(7); }
        last = d;
    }
}

int main(void) {
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, chan) != 0) { perror("socketpair"); return 1; }
    wake_fd = eventfd(0, EFD_NONBLOCK);
    ack_fd = eventfd(0, 0);
    if (wake_fd < 0 || ack_fd < 0) { perror("eventfd"); return 1; }

    for (int i = 0; i < NWORK; i++) {
        pthread_mutex_init(&W[i].m, NULL);
        pthread_cond_init(&W[i].cv, NULL);
        W[i].njobs = 0;
        pthread_create(&W[i].th, NULL, worker_fn, &W[i]);
    }
    pthread_t io, wd;
    pthread_create(&io, NULL, io_pump, NULL);
    pthread_create(&wd, NULL, watchdog, NULL);

    // producer: send job seq; occasionally ScheduleWork the eventfd; pace via ack (bounded outstanding)
    long outstanding = 0;
    char msg[16];
    for (long r = 0; r < ROUNDS; r++) {
        memcpy(msg, &r, 8);
        if (send(chan[0], msg, sizeof msg, 0) < 0) { perror("send"); return 2; }
        if ((r & 1) == 0) { uint64_t one = 1; if (write(wake_fd, &one, 8) != 8) {} }
        outstanding++;
        // keep a bounded number outstanding so the drain/re-arm + futex handoff windows stay hot
        while (outstanding >= 32) { uint64_t a; if (read(ack_fd, &a, 8) == 8) outstanding -= (long)a; else break; }
    }
    // drain remaining acks
    while (atomic_load(&g_done_rounds) < ROUNDS) {
        uint64_t a; if (read(ack_fd, &a, 8) != 8) break;
    }
    atomic_store(&g_quit, 1);
    for (int i = 0; i < NWORK; i++) {
        pthread_mutex_lock(&W[i].m); pthread_cond_signal(&W[i].cv); pthread_mutex_unlock(&W[i].m);
        pthread_join(W[i].th, NULL);
    }
    uint64_t one = 1; if (write(wake_fd, &one, 8) != 8) {}
    pthread_join(io, NULL);
    long d = atomic_load(&g_done_rounds);
    printf("worker rounds=%d done=%ld ok=%d\n", ROUNDS, d, d == ROUNDS);
    return d == ROUNDS ? 0 : 5;
}
