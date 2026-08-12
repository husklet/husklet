/*
 * The event group: a pollset built on WaitForMultipleObjects, plus waitable
 * timers.
 *
 * IOCP is the scalable Windows readiness primitive and it is deliberately not
 * used here, because the shape of the caller above this seam erases the only
 * advantage it has. That caller registers ONE constant interest per object and
 * discards the readiness word this group returns; it re-derives every object's
 * state itself on every wake, walking its whole watch set. So what it needs
 * from a pollset is a wakeup bus and a token, not a completion queue -- and its
 * cost is already linear in the watch count before a host answer reaches it.
 * WaitForMultipleObjects delivers exactly that, over handles the other groups
 * already own, with no per-object association step and no completion packets to
 * invent for objects that never had an I/O operation in flight.
 *
 * There is one place where exactness is not optional, and it is why the timer
 * bookkeeping below is keyed rather than derived. A caller draining POSIX guest
 * timers discards any record whose token is wrong or whose readiness lacks
 * HL_HOST_READY_TIMER, silently -- so an approximate token means every guest
 * timer stops firing and nothing reports an error. A waitable timer is directly
 * waitable by WaitForMultipleObjects, so a timer expiry is a signalled handle
 * this group owns, whose token it looked up rather than guessed. Auto-reset is
 * what makes that a count rather than a level: the wait consumes exactly one
 * expiry, and a periodic timer re-signals for the next.
 *
 * WaitForMultipleObjects tops out at 64 handles, and one of those is the wake
 * event, so a pollset with more than 63 waitable objects fans the whole set out
 * to thread-pool waits and blocks on a single aggregate event instead. The
 * callbacks record which slot fired in a bitmap before signalling, which they
 * must: a thread-pool wait CONSUMES an auto-reset object's signal, so a timer
 * that fired would otherwise be invisible to the sampling pass that follows.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>
#include <limits.h>

typedef struct hl_windows_registration {
    HANDLE object; /* borrowed; NULL when the object has no waitable form */
    uint64_t token;
    uint32_t interests;
    uint32_t active;
} hl_windows_registration;

typedef struct hl_windows_timer {
    HANDLE object; /* auto-reset waitable timer */
    uint64_t token;
} hl_windows_timer;

typedef struct hl_windows_pollset {
    CRITICAL_SECTION lock;
    HANDLE wake;      /* manual-reset: a wake releases every waiter, as a wakeup bus must */
    HANDLE aggregate; /* manual-reset: set by a thread-pool callback in the fanned-out path */
    hl_windows_registration *registrations;
    uint32_t registration_count;
    uint32_t registration_capacity;
    hl_windows_timer *timers;
    uint32_t timer_count;
    uint32_t timer_capacity;
} hl_windows_pollset;

/* One entry of the snapshot a single wait() call works from. */
typedef struct hl_windows_wait_slot {
    HANDLE object;
    uint64_t token;
    uint32_t readiness; /* what to report when this slot is signalled */
} hl_windows_wait_slot;

typedef struct hl_windows_wait_context {
    hl_windows_pollset *pollset;
    volatile LONG *fired;
    uint32_t index;
} hl_windows_wait_context;

/* --- lifetime --------------------------------------------------------------- */

static void hl_windows_pollset_free(hl_windows_pollset *pollset) {
    uint32_t index;
    for (index = 0; index < pollset->timer_count; ++index)
        if (pollset->timers[index].object != NULL) {
            (void)CancelWaitableTimer(pollset->timers[index].object);
            CloseHandle(pollset->timers[index].object);
        }
    if (pollset->wake != NULL) CloseHandle(pollset->wake);
    if (pollset->aggregate != NULL) CloseHandle(pollset->aggregate);
    free(pollset->registrations);
    free(pollset->timers);
    DeleteCriticalSection(&pollset->lock);
    free(pollset);
}

void hl_windows_event_destroy_entry(hl_windows_handle_entry *entry) {
    hl_windows_pollset *pollset = entry->payload;
    entry->payload = NULL;
    if (pollset != NULL) hl_windows_pollset_free(pollset);
}

static hl_windows_pollset *hl_windows_pollset_lookup(hl_host_windows *host, hl_host_handle handle) {
    const hl_windows_handle_entry *entry;
    hl_windows_pollset *pollset;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_POLLSET);
    pollset = entry == NULL ? NULL : entry->payload;
    hl_windows_unlock(host);
    return pollset;
}

static hl_host_result hl_windows_event_create(void *context) {
    hl_host_windows *host = context;
    hl_windows_pollset *pollset = calloc(1, sizeof(*pollset));
    hl_windows_handle_entry *entry;
    hl_host_result allocated;
    if (pollset == NULL) return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    InitializeCriticalSection(&pollset->lock);
    pollset->wake = CreateEventW(NULL, TRUE, FALSE, NULL);
    pollset->aggregate = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (pollset->wake == NULL || pollset->aggregate == NULL) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_pollset_free(pollset);
        return failure;
    }
    allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_POLLSET);
    if (allocated.status != HL_STATUS_OK) {
        hl_windows_pollset_free(pollset);
        return allocated;
    }
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_POLLSET);
    entry->payload = pollset;
    hl_windows_unlock(host);
    return allocated;
}

/* --- registration ----------------------------------------------------------- */

/*
 * The waitable form of an object, by group. A group that has one hands it over
 * borrowed; a group that has none is registered with a NULL handle rather than
 * refused. That distinction is the honest one here: the registration is real
 * and the token is real, and what the caller loses is a wake it re-derives
 * anyway on the next pass. Refusing instead would turn a missing wake into a
 * failed epoll_ctl, which is a strictly worse answer.
 */
static HANDLE hl_windows_event_waitable(hl_host_windows *host, hl_host_handle object, int *known) {
    const hl_windows_handle_entry *entry;
    HANDLE handle = NULL;
    *known = 0;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, object, HL_WINDOWS_HANDLE_COUNTER);
    if (entry != NULL) {
        handle = hl_windows_counter_wait_handle_locked(entry);
        *known = 1;
    }
    if (*known == 0) {
        entry = hl_windows_lookup_locked(host, object, HL_WINDOWS_HANDLE_STREAM);
        if (entry != NULL) {
            handle = hl_windows_stream_wait_handle_locked(entry);
            *known = 1;
        }
    }
    if (*known == 0) {
        /* A process handle is signalled on exit, which is genuine readiness. */
        entry = hl_windows_lookup_locked(host, object, HL_WINDOWS_HANDLE_PROCESS);
        if (entry != NULL) {
            handle = entry->object;
            *known = 1;
        }
    }
    if (*known == 0) {
        /* A file handle's signalled state means "the last synchronous operation
         * on this handle finished", not "this file is readable", so it is
         * registered without one. Regular files are always ready in any case. */
        entry = hl_windows_lookup_locked(host, object, HL_WINDOWS_HANDLE_FILE);
        if (entry != NULL) *known = 1;
    }
    hl_windows_unlock(host);
    return handle;
}

static hl_windows_registration *hl_windows_registration_find(hl_windows_pollset *pollset, uint64_t token) {
    uint32_t index;
    for (index = 0; index < pollset->registration_count; ++index)
        if (pollset->registrations[index].active && pollset->registrations[index].token == token)
            return &pollset->registrations[index];
    return NULL;
}

static hl_windows_registration *hl_windows_registration_claim(hl_windows_pollset *pollset) {
    uint32_t index;
    for (index = 0; index < pollset->registration_count; ++index)
        if (!pollset->registrations[index].active) return &pollset->registrations[index];
    if (pollset->registration_count == pollset->registration_capacity) {
        const uint32_t capacity = pollset->registration_capacity == 0 ? 8u : pollset->registration_capacity * 2u;
        hl_windows_registration *grown =
            realloc(pollset->registrations, (size_t)capacity * sizeof(*pollset->registrations));
        if (grown == NULL) return NULL;
        memset(&grown[pollset->registration_capacity], 0,
               (size_t)(capacity - pollset->registration_capacity) * sizeof(*grown));
        pollset->registrations = grown;
        pollset->registration_capacity = capacity;
    }
    return &pollset->registrations[pollset->registration_count++];
}

static hl_host_result hl_windows_event_control(void *context, hl_host_handle pollset_handle, uint32_t operation,
                                               hl_host_handle object, uint64_t token, uint32_t interests) {
    hl_host_windows *host = context;
    hl_windows_pollset *pollset = hl_windows_pollset_lookup(host, pollset_handle);
    hl_windows_registration *registration;
    HANDLE waitable;
    int known = 0;
    if (pollset == NULL || token == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (operation != HL_HOST_EVENT_ADD && operation != HL_HOST_EVENT_MODIFY && operation != HL_HOST_EVENT_DELETE)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (operation == HL_HOST_EVENT_DELETE) {
        EnterCriticalSection(&pollset->lock);
        registration = hl_windows_registration_find(pollset, token);
        if (registration != NULL) memset(registration, 0, sizeof(*registration));
        LeaveCriticalSection(&pollset->lock);
        return registration == NULL ? hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0)
                                    : hl_windows_result(HL_STATUS_OK, 0, 0);
    }
    waitable = hl_windows_event_waitable(host, object, &known);
    if (!known) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EnterCriticalSection(&pollset->lock);
    registration = hl_windows_registration_find(pollset, token);
    if (registration == NULL) {
        if (operation == HL_HOST_EVENT_MODIFY) {
            LeaveCriticalSection(&pollset->lock);
            return hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0);
        }
        registration = hl_windows_registration_claim(pollset);
        if (registration == NULL) {
            LeaveCriticalSection(&pollset->lock);
            return hl_windows_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
    } else if (operation == HL_HOST_EVENT_ADD) {
        LeaveCriticalSection(&pollset->lock);
        return hl_windows_result(HL_STATUS_ALREADY_EXISTS, 0, 0);
    }
    registration->object = waitable;
    registration->token = token;
    registration->interests = interests;
    registration->active = 1;
    LeaveCriticalSection(&pollset->lock);
    /* A newly registered object may already be ready, and a pollset that is
     * being waited on right now would not learn that until something else woke
     * it. Poking the bus costs one spurious pass and closes that window. */
    SetEvent(pollset->wake);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- timers ----------------------------------------------------------------- */

static uint64_t hl_windows_event_now(hl_host_windows *host) {
    const hl_host_result now = hl_windows_clock_services.monotonic_ns(host);
    return now.status == HL_STATUS_OK ? now.value : 0;
}

static hl_host_result hl_windows_event_arm_timer(void *context, hl_host_handle pollset_handle, uint64_t token,
                                                 uint64_t deadline_ns, uint64_t interval_ns) {
    hl_host_windows *host = context;
    hl_windows_pollset *pollset = hl_windows_pollset_lookup(host, pollset_handle);
    hl_windows_timer *timer = NULL;
    LARGE_INTEGER due;
    LONG period;
    uint64_t now;
    uint32_t index;
    if (pollset == NULL || token == 0 || deadline_ns == HL_HOST_DEADLINE_INFINITE)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EnterCriticalSection(&pollset->lock);
    for (index = 0; index < pollset->timer_count; ++index)
        if (pollset->timers[index].object != NULL && pollset->timers[index].token == token) {
            timer = &pollset->timers[index];
            break;
        }
    if (timer == NULL) {
        for (index = 0; index < pollset->timer_count; ++index)
            if (pollset->timers[index].object == NULL) {
                timer = &pollset->timers[index];
                break;
            }
    }
    if (timer == NULL) {
        if (pollset->timer_count == pollset->timer_capacity) {
            const uint32_t capacity = pollset->timer_capacity == 0 ? 8u : pollset->timer_capacity * 2u;
            hl_windows_timer *grown = realloc(pollset->timers, (size_t)capacity * sizeof(*pollset->timers));
            if (grown == NULL) {
                LeaveCriticalSection(&pollset->lock);
                return hl_windows_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
            }
            memset(&grown[pollset->timer_capacity], 0, (size_t)(capacity - pollset->timer_capacity) * sizeof(*grown));
            pollset->timers = grown;
            pollset->timer_capacity = capacity;
        }
        timer = &pollset->timers[pollset->timer_count++];
    }
    if (timer->object == NULL) {
        /* Auto-reset: one satisfied wait consumes exactly one expiry, so a
         * periodic timer delivers a count rather than a stuck level. */
        timer->object = CreateWaitableTimerExW(NULL, NULL, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
        if (timer->object == NULL) timer->object = CreateWaitableTimerExW(NULL, NULL, 0, TIMER_ALL_ACCESS);
        if (timer->object == NULL) {
            hl_host_result failure = hl_windows_last_error_result();
            LeaveCriticalSection(&pollset->lock);
            return failure;
        }
    }
    timer->token = token;
    now = hl_windows_event_now(host);
    /* Windows has no absolute monotonic due time, so an absolute deadline is
     * converted to the remaining interval. A deadline already in the past asks
     * for the shortest expiry the API can express rather than for never. */
    if (deadline_ns <= now)
        due.QuadPart = -1;
    else
        due.QuadPart = -(LONGLONG)((deadline_ns - now) / UINT64_C(100) + UINT64_C(1));
    {
        const uint64_t milliseconds = interval_ns / UINT64_C(1000000);
        if (interval_ns == 0)
            period = 0;
        else if (milliseconds == 0)
            period = 1; /* a sub-millisecond period has no Windows spelling; round up, never to zero */
        else
            period = milliseconds > (uint64_t)LONG_MAX ? LONG_MAX : (LONG)milliseconds;
    }
    if (!SetWaitableTimer(timer->object, &due, period, NULL, NULL, FALSE)) {
        hl_host_result failure = hl_windows_last_error_result();
        LeaveCriticalSection(&pollset->lock);
        return failure;
    }
    LeaveCriticalSection(&pollset->lock);
    SetEvent(pollset->wake);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_event_disarm_timer(void *context, hl_host_handle pollset_handle, uint64_t token) {
    hl_host_windows *host = context;
    hl_windows_pollset *pollset = hl_windows_pollset_lookup(host, pollset_handle);
    HANDLE object = NULL;
    uint32_t index;
    if (pollset == NULL || token == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EnterCriticalSection(&pollset->lock);
    for (index = 0; index < pollset->timer_count; ++index)
        if (pollset->timers[index].object != NULL && pollset->timers[index].token == token) {
            object = pollset->timers[index].object;
            pollset->timers[index].object = NULL;
            pollset->timers[index].token = 0;
            break;
        }
    LeaveCriticalSection(&pollset->lock);
    if (object == NULL) return hl_windows_result(HL_STATUS_NOT_FOUND, 0, 0);
    (void)CancelWaitableTimer(object);
    CloseHandle(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- wait ------------------------------------------------------------------- */

static void CALLBACK hl_windows_event_wait_callback(PTP_CALLBACK_INSTANCE instance, PVOID parameter, PTP_WAIT wait,
                                                    TP_WAIT_RESULT result) {
    hl_windows_wait_context *slot = parameter;
    (void)instance;
    (void)wait;
    if (result != WAIT_OBJECT_0) return;
    /* Record before signalling. The callback has already CONSUMED an auto-reset
     * object's signal, so the bitmap is the only remaining evidence it fired. */
    (void)InterlockedOr(&slot->fired[slot->index / 32u], (LONG)(1u << (slot->index % 32u)));
    SetEvent(slot->pollset->aggregate);
}

/* Copy the pollset's waitable objects and timers into a stable local array. The
 * pollset lock is not held across the wait itself: control() and arm_timer()
 * must stay callable from another thread while a waiter is blocked. */
static uint32_t hl_windows_event_snapshot(hl_windows_pollset *pollset, hl_windows_wait_slot **out) {
    uint32_t count = 0;
    uint32_t index;
    hl_windows_wait_slot *slots;
    EnterCriticalSection(&pollset->lock);
    slots = calloc((size_t)pollset->registration_count + pollset->timer_count + 1u, sizeof(*slots));
    if (slots != NULL) {
        for (index = 0; index < pollset->registration_count; ++index) {
            const hl_windows_registration *registration = &pollset->registrations[index];
            if (!registration->active || registration->object == NULL) continue;
            slots[count].object = registration->object;
            slots[count].token = registration->token;
            /* The readiness a wake carries is READ. Interests are recorded but
             * not translated: a signalled handle here means "look at me", and
             * the caller re-derives what the object actually permits. */
            slots[count].readiness = HL_HOST_READY_READ;
            count++;
        }
        for (index = 0; index < pollset->timer_count; ++index) {
            if (pollset->timers[index].object == NULL) continue;
            slots[count].object = pollset->timers[index].object;
            slots[count].token = pollset->timers[index].token;
            slots[count].readiness = HL_HOST_READY_TIMER;
            count++;
        }
    }
    LeaveCriticalSection(&pollset->lock);
    *out = slots;
    return count;
}

static DWORD hl_windows_event_timeout(hl_host_windows *host, uint64_t deadline_ns) {
    uint64_t now;
    uint64_t milliseconds;
    if (deadline_ns == HL_HOST_DEADLINE_INFINITE) return INFINITE;
    now = hl_windows_event_now(host);
    if (deadline_ns <= now) return 0;
    milliseconds = (deadline_ns - now + UINT64_C(999999)) / UINT64_C(1000000);
    /* INFINITE is a sentinel, not a duration, so a finite deadline never
     * becomes an unbounded wait however far away it is. */
    return milliseconds >= (uint64_t)INFINITE ? INFINITE - 1u : (DWORD)milliseconds;
}

/* Fanned-out path: every waitable slot gets a thread-pool wait, and the caller
 * blocks on wake plus one aggregate event. */
static hl_host_result hl_windows_event_wait_pooled(hl_host_windows *host, hl_windows_pollset *pollset,
                                                   hl_windows_wait_slot *slots, uint32_t count,
                                                   hl_host_event_record *events, size_t event_capacity,
                                                   uint64_t deadline_ns) {
    const uint32_t words = (count + 31u) / 32u;
    volatile LONG *fired = calloc(words, sizeof(LONG));
    hl_windows_wait_context *contexts = calloc(count, sizeof(*contexts));
    PTP_WAIT *waits = calloc(count, sizeof(*waits));
    HANDLE blocking[2];
    size_t produced = 0;
    uint32_t index;
    if (fired == NULL || contexts == NULL || waits == NULL) {
        free((void *)fired);
        free(contexts);
        free(waits);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    ResetEvent(pollset->aggregate);
    for (index = 0; index < count; ++index) {
        contexts[index].pollset = pollset;
        contexts[index].fired = fired;
        contexts[index].index = index;
        waits[index] = CreateThreadpoolWait(hl_windows_event_wait_callback, &contexts[index], NULL);
        if (waits[index] != NULL) SetThreadpoolWait(waits[index], slots[index].object, NULL);
    }
    blocking[0] = pollset->wake;
    blocking[1] = pollset->aggregate;
    (void)WaitForMultipleObjects(2, blocking, FALSE, hl_windows_event_timeout(host, deadline_ns));
    for (index = 0; index < count; ++index)
        if (waits[index] != NULL) {
            SetThreadpoolWait(waits[index], NULL, NULL);
            WaitForThreadpoolWaitCallbacks(waits[index], TRUE);
            CloseThreadpoolWait(waits[index]);
        }
    ResetEvent(pollset->wake);
    ResetEvent(pollset->aggregate);
    for (index = 0; index < count && produced < event_capacity; ++index) {
        if ((fired[index / 32u] & (LONG)(1u << (index % 32u))) == 0) continue;
        events[produced].token = slots[index].token;
        events[produced].readiness = slots[index].readiness;
        events[produced].flags = 0;
        produced++;
    }
    free((void *)fired);
    free(contexts);
    free(waits);
    return hl_windows_result(HL_STATUS_OK, produced, 0);
}

static hl_host_result hl_windows_event_wait(void *context, hl_host_handle pollset_handle, hl_host_event_record *events,
                                            size_t event_capacity, uint64_t deadline_ns) {
    hl_host_windows *host = context;
    hl_windows_pollset *pollset = hl_windows_pollset_lookup(host, pollset_handle);
    hl_windows_wait_slot *slots = NULL;
    HANDLE *handles;
    uint32_t count;
    DWORD waited;
    size_t produced = 0;
    uint32_t index;
    uint32_t satisfied = UINT32_MAX;
    if (events == NULL || event_capacity == 0 || pollset == NULL)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = hl_windows_event_snapshot(pollset, &slots);
    if (slots == NULL) return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    if (count + 1u > MAXIMUM_WAIT_OBJECTS) {
        const hl_host_result pooled =
            hl_windows_event_wait_pooled(host, pollset, slots, count, events, event_capacity, deadline_ns);
        free(slots);
        return pooled;
    }
    handles = calloc((size_t)count + 1u, sizeof(*handles));
    if (handles == NULL) {
        free(slots);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    handles[0] = pollset->wake;
    for (index = 0; index < count; ++index)
        handles[index + 1u] = slots[index].object;
    waited = WaitForMultipleObjects(count + 1u, handles, FALSE, hl_windows_event_timeout(host, deadline_ns));
    if (waited == WAIT_FAILED) {
        hl_host_result failure = hl_windows_last_error_result();
        free(handles);
        free(slots);
        return failure;
    }
    if (waited >= WAIT_OBJECT_0 && waited < WAIT_OBJECT_0 + count + 1u) satisfied = (uint32_t)(waited - WAIT_OBJECT_0);
    /* The wait that returned has already consumed the signalled object's state
     * -- an auto-reset timer is now reset -- so that one slot is reported from
     * the return value and only the others are re-sampled. */
    if (satisfied == 0)
        ResetEvent(pollset->wake);
    else if (satisfied != UINT32_MAX && produced < event_capacity) {
        events[produced].token = slots[satisfied - 1u].token;
        events[produced].readiness = slots[satisfied - 1u].readiness;
        events[produced].flags = 0;
        produced++;
    }
    for (index = 0; index < count && produced < event_capacity; ++index) {
        if (satisfied != UINT32_MAX && index + 1u == satisfied) continue;
        if (WaitForSingleObject(slots[index].object, 0) != WAIT_OBJECT_0) continue;
        events[produced].token = slots[index].token;
        events[produced].readiness = slots[index].readiness;
        events[produced].flags = 0;
        produced++;
    }
    free(handles);
    free(slots);
    return hl_windows_result(HL_STATUS_OK, produced, 0);
}

static hl_host_result hl_windows_event_wake(void *context, hl_host_handle pollset_handle) {
    hl_windows_pollset *pollset = hl_windows_pollset_lookup(context, pollset_handle);
    if (pollset == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!SetEvent(pollset->wake)) return hl_windows_last_error_result();
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_event_close(void *context, hl_host_handle pollset_handle) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_pollset *pollset;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, pollset_handle, HL_WINDOWS_HANDLE_POLLSET);
    pollset = entry == NULL ? NULL : entry->payload;
    if (entry != NULL) hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (pollset == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_pollset_free(pollset);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

const hl_host_event_services hl_windows_event_services = {.abi = HL_HOST_EVENT_ABI,
                                                          .size = sizeof(hl_host_event_services),
                                                          .create = hl_windows_event_create,
                                                          .control = hl_windows_event_control,
                                                          .wait = hl_windows_event_wait,
                                                          .wake = hl_windows_event_wake,
                                                          .close = hl_windows_event_close,
                                                          .arm_timer = hl_windows_event_arm_timer,
                                                          .disarm_timer = hl_windows_event_disarm_timer};
