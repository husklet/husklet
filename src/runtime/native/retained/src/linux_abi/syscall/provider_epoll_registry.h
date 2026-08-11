#ifndef HL_LINUX_ABI_SYSCALL_PROVIDER_EPOLL_REGISTRY_H
#define HL_LINUX_ABI_SYSCALL_PROVIDER_EPOLL_REGISTRY_H

#include <stdatomic.h>
#include <stdint.h>

#include "../../../include/hl/host_services.h"

#define EP_PROVIDER_WATCH_LIMIT 4096u

typedef struct ep_provider_watch {
    int epoll;
    int descriptor;
    uint32_t epoll_generation;
    uint32_t descriptor_generation;
    _Atomic uint32_t serial;
    hl_host_handle handle;
    uint32_t events;
    uint32_t interests;
    uint64_t data;
    _Atomic uint32_t ready;
    _Atomic uint32_t state;
    _Atomic uint32_t callbacks;
} ep_provider_watch;

enum { EP_PROVIDER_FREE = 0, EP_PROVIDER_ACTIVE = 1, EP_PROVIDER_RESERVED = 2, EP_PROVIDER_RETIRING = 3 };

static inline uint32_t ep_provider_next(uint32_t value) {
    value++;
    return value == 0 ? 1 : value;
}

static inline ep_provider_watch *ep_provider_find(ep_provider_watch *watches, uint32_t capacity, int epoll,
                                                  uint32_t epoll_generation, int descriptor,
                                                  uint32_t descriptor_generation) {
    for (uint32_t index = 0; index < capacity; ++index) {
        ep_provider_watch *watch = &watches[index];
        if (atomic_load_explicit(&watch->state, memory_order_acquire) == EP_PROVIDER_ACTIVE && watch->epoll == epoll &&
            watch->epoll_generation == epoll_generation && watch->descriptor == descriptor &&
            watch->descriptor_generation == descriptor_generation)
            return watch;
    }
    return NULL;
}

static inline ep_provider_watch *ep_provider_alloc(ep_provider_watch *watches, uint32_t capacity) {
    for (uint32_t index = 0; index < capacity; ++index) {
        uint32_t expected = EP_PROVIDER_FREE;
        if (atomic_compare_exchange_strong_explicit(&watches[index].state, &expected, EP_PROVIDER_RESERVED,
                                                    memory_order_acquire, memory_order_relaxed))
            return &watches[index];
    }
    return NULL;
}

static inline void ep_provider_activate(ep_provider_watch *watch, int epoll, uint32_t epoll_generation, int descriptor,
                                        uint32_t descriptor_generation, uint32_t serial, hl_host_handle handle,
                                        uint32_t events, uint32_t interests, uint64_t data) {
    watch->epoll = epoll;
    watch->descriptor = descriptor;
    watch->epoll_generation = epoll_generation;
    watch->descriptor_generation = descriptor_generation;
    atomic_store(&watch->serial, serial);
    watch->handle = handle;
    watch->events = events;
    watch->interests = interests;
    watch->data = data;
    atomic_store(&watch->ready, 0);
    /* Do not reset the callback counter here. A stale provider invocation may
     * begin after unsubscribe and briefly pin this fixed-address slot; its
     * serial check will reject it, but resetting would race its decrement and
     * underflow the new generation. FREE guarantees a zero stable count. */
    atomic_store_explicit(&watch->state, EP_PROVIDER_ACTIVE, memory_order_release);
}

static inline int ep_provider_retire_begin(ep_provider_watch *watch) {
    uint32_t expected = EP_PROVIDER_ACTIVE;
    return watch != NULL && atomic_compare_exchange_strong_explicit(&watch->state, &expected, EP_PROVIDER_RETIRING,
                                                                    memory_order_acq_rel, memory_order_acquire);
}

static inline void ep_provider_retire_finish(ep_provider_watch *watch) {
    atomic_store(&watch->ready, 0);
    atomic_store_explicit(&watch->state, EP_PROVIDER_FREE, memory_order_release);
}

static inline void ep_provider_reservation_cancel(ep_provider_watch *watch) {
    uint32_t expected = EP_PROVIDER_RESERVED;
    (void)atomic_compare_exchange_strong_explicit(&watch->state, &expected, EP_PROVIDER_FREE, memory_order_release,
                                                  memory_order_relaxed);
}

static inline int ep_provider_callback_enter(ep_provider_watch *watch, uint64_t token) {
    if (watch == NULL || atomic_load_explicit(&watch->state, memory_order_acquire) != EP_PROVIDER_ACTIVE ||
        atomic_load(&watch->serial) != token)
        return 0;
    atomic_fetch_add_explicit(&watch->callbacks, 1, memory_order_acquire);
    if (atomic_load_explicit(&watch->state, memory_order_acquire) != EP_PROVIDER_ACTIVE ||
        atomic_load(&watch->serial) != token) {
        atomic_fetch_sub_explicit(&watch->callbacks, 1, memory_order_release);
        return 0;
    }
    return 1;
}

static inline void ep_provider_callback_leave(ep_provider_watch *watch) {
    atomic_fetch_sub_explicit(&watch->callbacks, 1, memory_order_release);
}

static inline uint32_t ep_provider_take_ready(ep_provider_watch *watch, uint32_t level_sample, int *out_unsubscribe) {
    uint32_t ready = atomic_exchange(&watch->ready, 0);
    *out_unsubscribe = 0;
    if (ready == 0) return 0;
    if (watch->events & UINT32_C(0x40000000)) { /* EPOLLONESHOT */
        watch->interests = 0;
        *out_unsubscribe = 1;
    } else if (!(watch->events & UINT32_C(0x80000000))) { /* not EPOLLET */
        atomic_fetch_or(&watch->ready, level_sample);
    }
    return ready;
}

static inline uint32_t ep_provider_linux_events(uint32_t readiness) {
    uint32_t events = 0;
    if (readiness & UINT32_C(1)) events |= UINT32_C(0x001);  /* EPOLLIN */
    if (readiness & UINT32_C(2)) events |= UINT32_C(0x004);  /* EPOLLOUT */
    if (readiness & UINT32_C(4)) events |= UINT32_C(0x002);  /* EPOLLPRI */
    if (readiness & UINT32_C(8)) events |= UINT32_C(0x008);  /* EPOLLERR */
    if (readiness & UINT32_C(16)) events |= UINT32_C(0x010); /* EPOLLHUP */
    return events;
}

#endif
