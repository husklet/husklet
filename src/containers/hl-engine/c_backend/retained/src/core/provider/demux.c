#define _POSIX_C_SOURCE 200809L
#include "demux.h"

#include <errno.h>

/* The demultiplexer's arena is MAP_SHARED|MAP_ANONYMOUS and its mutexes and condition variables are
 * PTHREAD_PROCESS_SHARED for one reason: a guest that forks keeps talking to the same provider, so waiters
 * are keyed by getpid(), a fork child re-owns its own pumps, and teardown unmaps the arena instead of
 * destroying synchronization objects a dead foreign owner may still be parked on. Every one of those is a
 * property of fork(2). Windows has no fork, winpthreads implements no process-shared attribute at all, and
 * the wire underneath is an AF_UNIX stream with MSG_NOSIGNAL.
 *
 * Routing only the arena through the memory seam would compile nothing further and would claim a portability
 * this file does not have, so the feature is guarded whole. A Windows demultiplexer wants a private heap
 * arena, ordinary intra-process locks and a duplicated handle per child -- a different object with the same
 * interface, not this one with its allocator swapped. Until it exists, every entry point reports -ENOSYS in
 * the negative-errno channel callers already check, so a caller sees a provider that is unavailable rather
 * than one that accepted a request it will never answer. */
#if defined(_WIN32)

int hl_provider_demux_create(hl_provider_demux **out, int fd, uint32_t max_payload, uint32_t max_waiters,
                             uint32_t max_subscriptions, uint32_t event_capacity) {
    (void)fd;
    (void)max_payload;
    (void)max_waiters;
    (void)max_subscriptions;
    (void)event_capacity;
    if (out != NULL) *out = NULL;
    return -ENOSYS;
}

int hl_provider_demux_begin(hl_provider_demux *d, const void *bytes, uint32_t size, hl_provider_ticket *ticket) {
    (void)d;
    (void)bytes;
    (void)size;
    (void)ticket;
    return -ENOSYS;
}

int hl_provider_demux_wait(hl_provider_demux *d, hl_provider_ticket ticket, uint32_t timeout_ms,
                           hl_provider_reply *reply) {
    (void)d;
    (void)ticket;
    (void)timeout_ms;
    (void)reply;
    return -ENOSYS;
}

int hl_provider_demux_cancel(hl_provider_demux *d, hl_provider_ticket ticket) {
    (void)d;
    (void)ticket;
    return -ENOSYS;
}

int hl_provider_demux_subscribe(hl_provider_demux *d, uint64_t id, hl_provider_wake_fn wake, void *context) {
    (void)d;
    (void)id;
    (void)wake;
    (void)context;
    return -ENOSYS;
}

int hl_provider_demux_subscribe_remote(hl_provider_demux *d, uint64_t id, const void *bytes, uint32_t size,
                                       hl_provider_wake_fn wake, void *context) {
    (void)d;
    (void)id;
    (void)bytes;
    (void)size;
    (void)wake;
    (void)context;
    return -ENOSYS;
}

int hl_provider_demux_next(hl_provider_demux *d, uint64_t id, hl_provider_event *event, uint64_t *lost) {
    (void)d;
    (void)id;
    (void)event;
    (void)lost;
    return -ENOSYS;
}

int hl_provider_demux_unsubscribe(hl_provider_demux *d, uint64_t id) {
    (void)d;
    (void)id;
    return -ENOSYS;
}

int hl_provider_demux_unsubscribe_remote(hl_provider_demux *d, uint64_t id) {
    (void)d;
    (void)id;
    return -ENOSYS;
}

/* No create() ever succeeded, so no event carries storage and no demultiplexer exists to tear down. Both
 * releases stay callable so an error path that unwinds unconditionally still compiles and runs. */
void hl_provider_event_destroy(hl_provider_event *event) {
    (void)event;
}

void hl_provider_demux_destroy(hl_provider_demux *d) {
    (void)d;
}

#else

#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifndef ESHUTDOWN
#define ESHUTDOWN EPIPE
#endif
#ifndef MAP_ANONYMOUS
#if defined(__APPLE__)
#define MAP_ANONYMOUS 0x1000
#else
#define MAP_ANONYMOUS MAP_ANON
#endif
#endif

enum {
    HLPR_HEADER = 32,
    HLPR_REQUEST = 3,
    HLPR_REPLY = 4,
    HLPR_CANCEL = 5,
    HLPR_SUBSCRIBE = 9,
    HLPR_UNSUBSCRIBE = 10,
    HLPR_EVENT = 11
};

typedef struct hl_provider_waiter {
    uint64_t request;
    pid_t owner;
    int active;
    int waiting;
    int complete;
    int status;
    unsigned char *buffer;
    uint32_t reply_size;
    int reply_linux_errno;
    pthread_cond_t ready;
} hl_provider_waiter;

typedef struct hl_provider_subscription {
    uint64_t id;
    uint64_t generation;
    pid_t owner;
    int active;
    uint64_t notification;
    pthread_cond_t changed;
    uint32_t *sizes;
    unsigned char *storage;
    uint32_t head;
    uint32_t count;
    uint64_t lost;
} hl_provider_subscription;

struct hl_provider_demux {
    int descriptor;
    uint32_t maximum_payload;
    uint32_t maximum_event_payload;
    uint32_t maximum_waiters;
    uint32_t maximum_subscriptions;
    uint32_t event_capacity;
    uint64_t next_request;
    int peer_status;
    int stopping;
    pthread_mutex_t lock;
    pthread_mutex_t write_lock;
    pthread_cond_t callbacks_done;
    pthread_t reader;
    int reader_started;
    hl_provider_waiter *waiters;
    unsigned char *reply_storage;
    size_t waiters_size;
    size_t reply_storage_size;
    size_t subscription_storage_size;
    size_t mapping_size;
    hl_provider_subscription *subscriptions;
};

enum { HL_PROVIDER_LOCAL_PUMPS = 64 };

typedef struct hl_provider_local_pump {
    hl_provider_demux *demux;
    uint64_t id;
    uint64_t generation;
    pid_t owner;
    hl_provider_wake_fn wake;
    void *context;
    pthread_t thread;
    int active;
    _Atomic int stopping;
} hl_provider_local_pump;

static pthread_mutex_t local_pump_lock = PTHREAD_MUTEX_INITIALIZER;
static hl_provider_local_pump local_pumps[HL_PROVIDER_LOCAL_PUMPS];
static pid_t local_pump_pid;

static void put16(unsigned char *p, uint16_t v) {
    p[0] = (unsigned char)v;
    p[1] = (unsigned char)(v >> 8);
}

static void put32(unsigned char *p, uint32_t v) {
    put16(p, (uint16_t)v);
    put16(p + 2, (uint16_t)(v >> 16));
}

static void put64(unsigned char *p, uint64_t v) {
    put32(p, (uint32_t)v);
    put32(p + 4, (uint32_t)(v >> 32));
}

static uint16_t get16(const unsigned char *p) {
    return (uint16_t)((uint16_t)p[0] | (uint16_t)((uint16_t)p[1] << 8));
}

static uint32_t get32(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static uint64_t get64(const unsigned char *p) {
    return (uint64_t)get32(p) | (uint64_t)get32(p + 4) << 32;
}

static void make_header(unsigned char p[HLPR_HEADER], uint16_t kind, uint32_t size, uint64_t request) {
    memset(p, 0, HLPR_HEADER);
    put32(p, UINT32_C(0x484c5052));
    put16(p + 4, 1);
    put16(p + 6, kind);
    put32(p + 8, size);
    put64(p + 12, request);
}

static int exact(int fd, void *buffer, size_t size, int writing) {
    unsigned char *p = buffer;
    size_t used = 0;
    while (used < size) {
        ssize_t n = writing ? send(fd, p + used, size - used, MSG_NOSIGNAL) : read(fd, p + used, size - used);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) return -errno;
        if (n == 0) return -ECONNRESET;
        used += (size_t)n;
    }
    return 0;
}

static int send_frame(hl_provider_demux *d, uint16_t kind, uint64_t request, const void *bytes, uint32_t size) {
    unsigned char header[HLPR_HEADER];
    int status;
    make_header(header, kind, size, request);
    if (pthread_mutex_lock(&d->write_lock) != 0) return -EIO;
    status = exact(d->descriptor, header, sizeof(header), 1);
    if (status == 0 && size != 0) status = exact(d->descriptor, (void *)(uintptr_t)bytes, size, 1);
    (void)pthread_mutex_unlock(&d->write_lock);
    return status;
}

static hl_provider_waiter *waiter(hl_provider_demux *d, uint64_t request) {
    uint32_t i;
    for (i = 0; i < d->maximum_waiters; ++i)
        if (d->waiters[i].active && d->waiters[i].request == request) return &d->waiters[i];
    return NULL;
}

static hl_provider_subscription *subscription(hl_provider_demux *d, uint64_t id, pid_t owner) {
    uint32_t i;
    for (i = 0; i < d->maximum_subscriptions; ++i)
        if (d->subscriptions[i].active && d->subscriptions[i].id == id && d->subscriptions[i].owner == owner)
            return &d->subscriptions[i];
    return NULL;
}

static void subscription_clear(hl_provider_subscription *s) {
    s->active = 0;
    s->id = 0;
    s->owner = 0;
    s->head = 0;
    s->count = 0;
    s->lost = 0;
    s->notification++;
    pthread_cond_broadcast(&s->changed);
}

static void subscriptions_reclaim_dead(hl_provider_demux *d) {
    uint32_t i;
    for (i = 0; i < d->maximum_subscriptions; ++i) {
        hl_provider_subscription *s = &d->subscriptions[i];
        if (s->active && kill(s->owner, 0) != 0 && errno == ESRCH) subscription_clear(s);
    }
}

static void fail_all(hl_provider_demux *d, int status) {
    uint32_t i;
    d->peer_status = status;
    for (i = 0; i < d->maximum_waiters; ++i)
        if (d->waiters[i].active && !d->waiters[i].complete) {
            d->waiters[i].status = status;
            d->waiters[i].complete = 1;
            pthread_cond_broadcast(&d->waiters[i].ready);
        }
    /* Wake callbacks are dispatched by the reader after dropping the lock. */
}

static void peer_failed(hl_provider_demux *d, int status) {
    uint32_t i;
    pthread_mutex_lock(&d->lock);
    fail_all(d, status);
    for (i = 0; i < d->maximum_subscriptions; ++i)
        if (d->subscriptions[i].active) {
            d->subscriptions[i].notification++;
            pthread_cond_broadcast(&d->subscriptions[i].changed);
        }
    pthread_mutex_unlock(&d->lock);
}

static void *reader_main(void *opaque) {
    hl_provider_demux *d = opaque;
    for (;;) {
        unsigned char header[HLPR_HEADER];
        unsigned char *payload = NULL;
        uint16_t kind;
        uint32_t size;
        uint64_t id;
        int status = exact(d->descriptor, header, sizeof(header), 0);
        if (status != 0) {
            peer_failed(d, status);
            return NULL;
        }
        kind = get16(header + 6);
        size = get32(header + 8);
        id = get64(header + 12);
        if (get32(header) != UINT32_C(0x484c5052) || get16(header + 4) != 1 ||
            (kind != HLPR_REPLY && kind != HLPR_EVENT) || size > d->maximum_payload ||
            memcmp(header + 28, "\0\0\0\0", 4) != 0) {
            peer_failed(d, size > d->maximum_payload ? -EMSGSIZE : -EPROTO);
            return NULL;
        }
        if (size != 0) {
            payload = malloc(size);
            if (payload == NULL || exact(d->descriptor, payload, size, 0) != 0) {
                free(payload);
                peer_failed(d, -ECONNRESET);
                return NULL;
            }
        }
        pthread_mutex_lock(&d->lock);
        if (kind == HLPR_REPLY) {
            hl_provider_waiter *w = waiter(d, id);
            if (w != NULL && !w->complete) {
                if (size != 0) memcpy(w->buffer, payload, size);
                w->reply_size = size;
                w->reply_linux_errno = size >= 7 && payload[0] == 0xff ? (int32_t)get32(payload + 1) : 0;
                w->status = 0;
                w->complete = 1;
                pthread_cond_broadcast(&w->ready);
            }
        } else {
            uint32_t index;
            for (index = 0; index < d->maximum_subscriptions; ++index) {
                hl_provider_subscription *s = &d->subscriptions[index];
                if (!s->active || s->id != id) continue;
                if (s->count == d->event_capacity || size > d->maximum_event_payload) {
                    s->lost++;
                } else {
                    uint32_t tail = (s->head + s->count) % d->event_capacity;
                    if (size != 0) memcpy(s->storage + (size_t)tail * d->maximum_event_payload, payload, size);
                    s->sizes[tail] = size;
                    s->count++;
                }
                s->notification++;
                pthread_cond_broadcast(&s->changed);
            }
            pthread_mutex_unlock(&d->lock);
            free(payload);
            continue;
        }
        pthread_mutex_unlock(&d->lock);
        free(payload);
    }
}

static int ensure_reader(hl_provider_demux *d) {
    int status = 0;
    pthread_mutex_lock(&d->lock);
    if (d->stopping)
        status = -ESHUTDOWN;
    else if (!d->reader_started)
        status = d->peer_status != 0 ? d->peer_status : -ESHUTDOWN;
    pthread_mutex_unlock(&d->lock);
    return status;
}

int hl_provider_demux_create(hl_provider_demux **out, int fd, uint32_t max_payload, uint32_t max_waiters,
                             uint32_t max_subscriptions, uint32_t event_capacity) {
    hl_provider_demux *d;
    pthread_mutexattr_t mutex_attributes;
    pthread_condattr_t condition_attributes;
    size_t waiters_offset = (sizeof(*d) + 63u) & ~(size_t)63u;
    size_t storage_offset, subscriptions_offset, event_sizes_offset, event_storage_offset;
    size_t mapping_size;
    uint32_t i = 0, waiter_conditions = 0, subscription_conditions = 0;
    int lock_ready = 0, write_lock_ready = 0, callback_ready = 0;
    if (out == NULL || fd < 0 || max_payload == 0 || max_waiters == 0 || max_subscriptions == 0 || event_capacity == 0)
        return -EINVAL;
    *out = NULL;
    storage_offset = waiters_offset + (size_t)max_waiters * sizeof(hl_provider_waiter);
    subscriptions_offset = storage_offset + (size_t)max_waiters * max_payload;
    event_sizes_offset = subscriptions_offset + (size_t)max_subscriptions * sizeof(hl_provider_subscription);
    uint32_t max_event_payload = max_payload < UINT32_C(65536) ? max_payload : UINT32_C(65536);
    event_storage_offset = event_sizes_offset + (size_t)max_subscriptions * event_capacity * sizeof(uint32_t);
    mapping_size = event_storage_offset + (size_t)max_subscriptions * event_capacity * max_event_payload;
    d = mmap(NULL, mapping_size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (d == MAP_FAILED) return -ENOMEM;
    memset(d, 0, mapping_size);
    d->mapping_size = mapping_size;
    d->waiters_size = (size_t)max_waiters * sizeof(*d->waiters);
    d->reply_storage_size = (size_t)max_waiters * max_payload;
    d->subscription_storage_size = mapping_size - subscriptions_offset;
    d->waiters = (void *)((unsigned char *)d + waiters_offset);
    d->reply_storage = (unsigned char *)d + storage_offset;
    d->subscriptions = (void *)((unsigned char *)d + subscriptions_offset);
    for (i = 0; i < max_waiters; ++i)
        d->waiters[i].buffer = d->reply_storage + (size_t)i * max_payload;
    for (i = 0; i < max_subscriptions; ++i) {
        d->subscriptions[i].sizes = (uint32_t *)((unsigned char *)d + event_sizes_offset) + (size_t)i * event_capacity;
        d->subscriptions[i].storage =
            (unsigned char *)d + event_storage_offset + (size_t)i * event_capacity * max_event_payload;
    }
    d->descriptor = fd;
    d->maximum_payload = max_payload;
    d->maximum_event_payload = max_event_payload;
    d->maximum_waiters = max_waiters;
    d->maximum_subscriptions = max_subscriptions;
    d->event_capacity = event_capacity;
    d->next_request = 1;
    if (pthread_mutexattr_init(&mutex_attributes) != 0) goto failed;
    if (pthread_condattr_init(&condition_attributes) != 0) {
        pthread_mutexattr_destroy(&mutex_attributes);
        goto failed;
    }
    if (pthread_mutexattr_setpshared(&mutex_attributes, PTHREAD_PROCESS_SHARED) != 0 ||
        pthread_condattr_setpshared(&condition_attributes, PTHREAD_PROCESS_SHARED) != 0)
        goto failed_attributes;
    if (pthread_mutex_init(&d->lock, &mutex_attributes) != 0) goto failed_attributes;
    lock_ready = 1;
    if (pthread_mutex_init(&d->write_lock, &mutex_attributes) != 0) goto failed_attributes;
    write_lock_ready = 1;
    if (pthread_cond_init(&d->callbacks_done, &condition_attributes) != 0) goto failed_attributes;
    callback_ready = 1;
    for (i = 0; i < max_waiters; ++i) {
        if (pthread_cond_init(&d->waiters[i].ready, &condition_attributes) != 0) goto failed_attributes;
        waiter_conditions++;
    }
    for (i = 0; i < max_subscriptions; ++i) {
        if (pthread_cond_init(&d->subscriptions[i].changed, &condition_attributes) != 0) goto failed_attributes;
        subscription_conditions++;
    }
    pthread_condattr_destroy(&condition_attributes);
    pthread_mutexattr_destroy(&mutex_attributes);
    if (pthread_create(&d->reader, NULL, reader_main, d) != 0) goto failed_initialized;
    d->reader_started = 1;
    *out = d;
    return 0;
failed_attributes:
    pthread_condattr_destroy(&condition_attributes);
    pthread_mutexattr_destroy(&mutex_attributes);
failed_initialized:
    while (subscription_conditions != 0)
        pthread_cond_destroy(&d->subscriptions[--subscription_conditions].changed);
    while (waiter_conditions != 0)
        pthread_cond_destroy(&d->waiters[--waiter_conditions].ready);
    if (callback_ready) pthread_cond_destroy(&d->callbacks_done);
    if (write_lock_ready) pthread_mutex_destroy(&d->write_lock);
    if (lock_ready) pthread_mutex_destroy(&d->lock);
failed:
    munmap(d, mapping_size);
    return -EIO;
}

int hl_provider_demux_begin(hl_provider_demux *d, const void *bytes, uint32_t size, hl_provider_ticket *ticket) {
    hl_provider_waiter *w = NULL;
    uint32_t i;
    int status;
    if (d == NULL || bytes == NULL || ticket == NULL || size > d->maximum_payload) return -EINVAL;
    status = ensure_reader(d);
    if (status != 0) return status;
    pthread_mutex_lock(&d->lock);
    if (d->peer_status != 0 || d->stopping) {
        status = d->peer_status ? d->peer_status : -ESHUTDOWN;
        pthread_mutex_unlock(&d->lock);
        return status;
    }
    for (i = 0; i < d->maximum_waiters; ++i)
        if (!d->waiters[i].active) {
            w = &d->waiters[i];
            break;
        }
    if (w == NULL) {
        pthread_mutex_unlock(&d->lock);
        return -ENOSPC;
    }
    w->reply_size = 0;
    w->reply_linux_errno = 0;
    w->request = d->next_request++;
    w->owner = getpid();
    if (d->next_request == 0) d->next_request = 1;
    w->active = 1;
    w->complete = 0;
    w->status = 0;
    ticket->request = w->request;
    pthread_mutex_unlock(&d->lock);
    status = send_frame(d, HLPR_REQUEST, ticket->request, bytes, size);
    if (status != 0) {
        pthread_mutex_lock(&d->lock);
        w = waiter(d, ticket->request);
        if (w != NULL) {
            w->active = 0;
            w->complete = 0;
        }
        pthread_mutex_unlock(&d->lock);
    }
    return status;
}

static struct timespec deadline(uint32_t timeout_ms) {
    struct timespec t;
    clock_gettime(CLOCK_REALTIME, &t);
    t.tv_sec += timeout_ms / 1000;
    t.tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
    if (t.tv_nsec >= 1000000000L) {
        t.tv_sec++;
        t.tv_nsec -= 1000000000L;
    }
    return t;
}

int hl_provider_demux_wait(hl_provider_demux *d, hl_provider_ticket ticket, uint32_t timeout_ms,
                           hl_provider_reply *reply) {
    hl_provider_waiter *w;
    struct timespec until;
    int status = 0, timed_out = 0;
    if (d == NULL || ticket.request == 0 || timeout_ms == 0 || reply == NULL) return -EINVAL;
    memset(reply, 0, sizeof(*reply));
    until = deadline(timeout_ms);
    pthread_mutex_lock(&d->lock);
    w = waiter(d, ticket.request);
    if (w == NULL) {
        pthread_mutex_unlock(&d->lock);
        return -ENOENT;
    }
    w->waiting = 1;
    while (!w->complete) {
        int rc = pthread_cond_timedwait(&w->ready, &d->lock, &until);
        if (rc == ETIMEDOUT) {
            w->status = -ETIMEDOUT;
            w->complete = 1;
            timed_out = 1;
            break;
        }
        if (rc != 0) {
            w->status = -EIO;
            w->complete = 1;
            break;
        }
    }
    status = w->status;
    if (status == 0 && w->reply_size != 0) {
        reply->bytes = malloc(w->reply_size);
        if (reply->bytes == NULL)
            status = -ENOMEM;
        else {
            memcpy(reply->bytes, w->buffer, w->reply_size);
            reply->size = w->reply_size;
            reply->linux_errno = w->reply_linux_errno;
        }
    }
    w->active = 0;
    w->complete = 0;
    w->waiting = 0;
    pthread_cond_broadcast(&d->callbacks_done);
    pthread_mutex_unlock(&d->lock);
    if (timed_out) (void)send_frame(d, HLPR_CANCEL, ticket.request, NULL, 0);
    return status;
}

int hl_provider_demux_cancel(hl_provider_demux *d, hl_provider_ticket ticket) {
    hl_provider_waiter *w;
    if (d == NULL || ticket.request == 0) return -EINVAL;
    pthread_mutex_lock(&d->lock);
    w = waiter(d, ticket.request);
    if (w == NULL) {
        pthread_mutex_unlock(&d->lock);
        return -ENOENT;
    }
    if (!w->complete) {
        w->status = -ECANCELED;
        w->complete = 1;
        pthread_cond_broadcast(&w->ready);
    }
    pthread_mutex_unlock(&d->lock);
    (void)send_frame(d, HLPR_CANCEL, ticket.request, NULL, 0);
    return 0;
}

static void local_pumps_reset_after_fork(void) {
    pid_t pid = getpid();
    if (local_pump_pid == pid) return;
    pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
    memcpy(&local_pump_lock, &fresh, sizeof fresh);
    memset(local_pumps, 0, sizeof local_pumps);
    local_pump_pid = pid;
}

static void *local_pump_main(void *opaque) {
    hl_provider_local_pump *pump = opaque;
    hl_provider_demux *d = pump->demux;
    uint64_t observed = 0;
    for (;;) {
        hl_provider_wake_fn wake = NULL;
        void *context = NULL;
        pthread_mutex_lock(&d->lock);
        hl_provider_subscription *s = subscription(d, pump->id, pump->owner);
        while (!atomic_load(&pump->stopping) && s != NULL && s->generation == pump->generation &&
               s->notification == observed)
            pthread_cond_wait(&s->changed, &d->lock);
        if (atomic_load(&pump->stopping) || s == NULL || s->generation != pump->generation) {
            pthread_mutex_unlock(&d->lock);
            break;
        }
        uint64_t notifications = s->notification - observed;
        observed = s->notification;
        wake = pump->wake;
        context = pump->context;
        pthread_mutex_unlock(&d->lock);
        while (wake != NULL && notifications-- != 0)
            wake(context, pump->id);
    }
    return NULL;
}

static int local_pump_start(hl_provider_demux *d, hl_provider_subscription *s, hl_provider_wake_fn wake,
                            void *context) {
    uint32_t i;
    local_pumps_reset_after_fork();
    pthread_mutex_lock(&local_pump_lock);
    for (i = 0; i < HL_PROVIDER_LOCAL_PUMPS; ++i)
        if (!local_pumps[i].active) {
            hl_provider_local_pump *pump = &local_pumps[i];
            memset(pump, 0, sizeof(*pump));
            pump->demux = d;
            pump->id = s->id;
            pump->generation = s->generation;
            pump->owner = s->owner;
            pump->wake = wake;
            pump->context = context;
            pump->active = 1;
            if (pthread_create(&pump->thread, NULL, local_pump_main, pump) != 0) {
                memset(pump, 0, sizeof(*pump));
                pthread_mutex_unlock(&local_pump_lock);
                return -EIO;
            }
            pthread_mutex_unlock(&local_pump_lock);
            return 0;
        }
    pthread_mutex_unlock(&local_pump_lock);
    return -ENOSPC;
}

static int local_pump_stop(hl_provider_demux *d, uint64_t id, pid_t owner) {
    uint32_t i;
    pthread_t thread;
    hl_provider_local_pump *found = NULL;
    local_pumps_reset_after_fork();
    pthread_mutex_lock(&local_pump_lock);
    for (i = 0; i < HL_PROVIDER_LOCAL_PUMPS; ++i)
        if (local_pumps[i].active && local_pumps[i].demux == d && local_pumps[i].id == id &&
            local_pumps[i].owner == owner) {
            found = &local_pumps[i];
            atomic_store(&found->stopping, 1);
            thread = found->thread;
            break;
        }
    pthread_mutex_unlock(&local_pump_lock);
    if (found == NULL) return -ENOENT;
    pthread_mutex_lock(&d->lock);
    hl_provider_subscription *s = subscription(d, id, owner);
    if (s != NULL) pthread_cond_broadcast(&s->changed);
    pthread_mutex_unlock(&d->lock);
    (void)pthread_join(thread, NULL);
    pthread_mutex_lock(&local_pump_lock);
    memset(found, 0, sizeof(*found));
    pthread_mutex_unlock(&local_pump_lock);
    return 0;
}

int hl_provider_demux_subscribe(hl_provider_demux *d, uint64_t id, hl_provider_wake_fn wake, void *context) {
    uint32_t i;
    pid_t owner = getpid();
    hl_provider_subscription *created = NULL;
    int status;
    if (d == NULL || id == 0) return -EINVAL;
    pthread_mutex_lock(&d->lock);
    subscriptions_reclaim_dead(d);
    if (subscription(d, id, owner) != NULL) {
        pthread_mutex_unlock(&d->lock);
        return -EEXIST;
    }
    for (i = 0; i < d->maximum_subscriptions; ++i)
        if (!d->subscriptions[i].active) {
            hl_provider_subscription *s = &d->subscriptions[i];
            s->id = id;
            s->owner = owner;
            s->generation++;
            if (s->generation == 0) s->generation = 1;
            s->active = 1;
            s->notification = 0;
            s->head = 0;
            s->count = 0;
            s->lost = 0;
            created = s;
            break;
        }
    pthread_mutex_unlock(&d->lock);
    if (created == NULL) return -ENOSPC;
    status = local_pump_start(d, created, wake, context);
    if (status != 0) {
        pthread_mutex_lock(&d->lock);
        if (created->active && created->owner == owner) subscription_clear(created);
        pthread_mutex_unlock(&d->lock);
    }
    return status;
}

int hl_provider_demux_subscribe_remote(hl_provider_demux *d, uint64_t id, const void *bytes, uint32_t size,
                                       hl_provider_wake_fn wake, void *context) {
    int status, first = 1;
    uint32_t i;
    if (d == NULL || bytes == NULL || size == 0 || size > d->maximum_payload) return -EINVAL;
    status = ensure_reader(d);
    if (status != 0) return status;
    status = hl_provider_demux_subscribe(d, id, wake, context);
    if (status != 0) return status;
    pthread_mutex_lock(&d->lock);
    for (i = 0; i < d->maximum_subscriptions; ++i)
        if (d->subscriptions[i].active && d->subscriptions[i].id == id && d->subscriptions[i].owner != getpid()) {
            first = 0;
            break;
        }
    pthread_mutex_unlock(&d->lock);
    status = first ? send_frame(d, HLPR_SUBSCRIBE, id, bytes, size) : 0;
    if (status != 0) (void)hl_provider_demux_unsubscribe(d, id);
    return status;
}

int hl_provider_demux_next(hl_provider_demux *d, uint64_t id, hl_provider_event *event, uint64_t *lost) {
    hl_provider_subscription *s;
    if (d == NULL || id == 0 || event == NULL || lost == NULL) return -EINVAL;
    memset(event, 0, sizeof(*event));
    pthread_mutex_lock(&d->lock);
    s = subscription(d, id, getpid());
    if (s == NULL) {
        pthread_mutex_unlock(&d->lock);
        return -ENOENT;
    }
    *lost = s->lost;
    s->lost = 0;
    if (s->count == 0) {
        int status = d->peer_status ? d->peer_status : -EAGAIN;
        pthread_mutex_unlock(&d->lock);
        return status;
    }
    event->size = s->sizes[s->head];
    if (event->size != 0) {
        event->bytes = malloc(event->size);
        if (event->bytes == NULL) {
            pthread_mutex_unlock(&d->lock);
            return -ENOMEM;
        }
        memcpy(event->bytes, s->storage + (size_t)s->head * d->maximum_event_payload, event->size);
    }
    s->sizes[s->head] = 0;
    s->head = (s->head + 1) % d->event_capacity;
    s->count--;
    pthread_mutex_unlock(&d->lock);
    return 0;
}

int hl_provider_demux_unsubscribe(hl_provider_demux *d, uint64_t id) {
    hl_provider_subscription *s;
    pid_t owner = getpid();
    if (d == NULL || id == 0) return -EINVAL;
    pthread_mutex_lock(&d->lock);
    s = subscription(d, id, owner);
    if (s == NULL) {
        pthread_mutex_unlock(&d->lock);
        return -ENOENT;
    }
    pthread_mutex_unlock(&d->lock);
    (void)local_pump_stop(d, id, owner);
    pthread_mutex_lock(&d->lock);
    s = subscription(d, id, owner);
    if (s != NULL) subscription_clear(s);
    pthread_mutex_unlock(&d->lock);
    return 0;
}

int hl_provider_demux_unsubscribe_remote(hl_provider_demux *d, uint64_t id) {
    int status, last = 1;
    uint32_t i;
    if (d == NULL || id == 0) return -EINVAL;
    status = hl_provider_demux_unsubscribe(d, id);
    if (status != 0) return status;
    pthread_mutex_lock(&d->lock);
    subscriptions_reclaim_dead(d);
    for (i = 0; i < d->maximum_subscriptions; ++i)
        if (d->subscriptions[i].active && d->subscriptions[i].id == id) {
            last = 0;
            break;
        }
    pthread_mutex_unlock(&d->lock);
    status = last ? send_frame(d, HLPR_UNSUBSCRIBE, id, NULL, 0) : 0;
    return status == -EPIPE || status == -ECONNRESET ? 0 : status;
}

void hl_provider_event_destroy(hl_provider_event *event) {
    if (event != NULL) {
        free(event->bytes);
        memset(event, 0, sizeof(*event));
    }
}

void hl_provider_demux_destroy(hl_provider_demux *d) {
    uint32_t i;
    int reader_started;
    if (d == NULL) return;
    local_pumps_reset_after_fork();
    for (;;) {
        uint64_t id = 0;
        pthread_mutex_lock(&local_pump_lock);
        for (i = 0; i < HL_PROVIDER_LOCAL_PUMPS; ++i)
            if (local_pumps[i].active && local_pumps[i].demux == d && local_pumps[i].owner == getpid()) {
                id = local_pumps[i].id;
                break;
            }
        pthread_mutex_unlock(&local_pump_lock);
        if (id == 0) break;
        (void)local_pump_stop(d, id, getpid());
    }
    pthread_mutex_lock(&d->lock);
    d->stopping = 1;
    reader_started = d->reader_started;
    pthread_mutex_unlock(&d->lock);
    if (reader_started) {
        (void)shutdown(d->descriptor, SHUT_RDWR);
        (void)pthread_join(d->reader, NULL);
    }
    pthread_mutex_lock(&d->lock);
    for (;;) {
        int local_waiter = 0;
        for (i = 0; i < d->maximum_waiters; ++i)
            if (d->waiters[i].waiting && d->waiters[i].owner == getpid()) {
                local_waiter = 1;
                break;
            }
        if (!local_waiter) break;
        pthread_cond_wait(&d->callbacks_done, &d->lock);
    }
    pthread_mutex_unlock(&d->lock);
    /* These process-shared synchronization objects may retain kernel waiter
     * bookkeeping for a fork child that exited mid-request.  The owner has
     * joined every local worker above; unmapping is the teardown operation.
     * pthread_*_destroy can otherwise wait forever on a dead foreign owner. */
    (void)munmap(d, d->mapping_size);
}

#endif
