/*
 * The counter group: a pollable unsigned 64-bit counter with UINT64_MAX
 * reserved, built from a manual-reset event and a lock.
 *
 * There is no Windows object with eventfd's shape. A semaphore counts but
 * cannot be read without consuming, cannot be written by more than its release
 * count and saturates at a chosen maximum rather than at UINT64_MAX; an event
 * is waitable but carries no value at all. So the value lives in this file and
 * the event carries exactly one bit of it -- "non-zero" -- which is precisely
 * what the contract's readiness is. The invariant is one line long and every
 * mutation restores it under the object lock:
 *
 *     the ready event is signalled if and only if value != 0
 *
 * Manual-reset is load-bearing on both sides of that. readiness() has to be
 * able to sample the state without consuming it, and a blocking read() has to
 * be able to wake every thread that is waiting for a value some other thread
 * has just published. An auto-reset event does neither.
 *
 * Two handles onto one counter (create + duplicate) alias the same object, the
 * way dup(2) aliases one open file description. The object is reference
 * counted; the slots are not copies of it. That is what makes a value written
 * through one handle readable through the other, and it is what
 * counter.duplicate is for.
 *
 * Subscriptions use the thread pool rather than a thread of their own.
 * RegisterWaitForSingleObject with WT_EXECUTEONLYONCE fires the callback once
 * and stops -- which is what a manual-reset event demands, because a wait that
 * is not one-shot would re-queue the callback in a loop for as long as the
 * event stayed signalled. The re-arm therefore happens exactly where the level
 * actually falls: in read(), when the value reaches zero and the event is
 * reset. A subscriber is told once per empty-to-non-empty transition, which is
 * the same notification density the POSIX backends produce.
 *
 * unsubscribe is required to have quiesced the callback before it returns, and
 * UnregisterWaitEx with an INVALID_HANDLE_VALUE completion event is that
 * guarantee exactly: it does not return until a callback already running has
 * finished. The re-arm path uses the non-blocking UnregisterWait instead, on
 * purpose -- re-arming happens while the caller may hold locks the callback's
 * notify() also takes, and blocking there would be a lock-order inversion the
 * subscriber cannot see.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

typedef struct hl_windows_counter_subscription hl_windows_counter_subscription;

typedef struct hl_windows_counter_object {
    CRITICAL_SECTION lock;
    HANDLE ready; /* manual-reset; signalled iff value != 0 */
    uint64_t value;
    uint32_t flags; /* HL_HOST_COUNTER_* */
    uint32_t references;
    hl_windows_counter_subscription *subscriptions;
} hl_windows_counter_object;

struct hl_windows_counter_subscription {
    hl_windows_counter_subscription *next;
    hl_windows_counter_object *object;
    hl_host_handle counter;
    HANDLE wait; /* RegisterWaitForSingleObject registration, NULL when unarmed */
    void (*notify)(void *, uint64_t);
    void *observer;
    uint64_t token;
};

/* --- object lifetime -------------------------------------------------------- */

static void hl_windows_counter_object_release(hl_windows_counter_object *object) {
    uint32_t remaining;
    EnterCriticalSection(&object->lock);
    remaining = object->references == 0 ? 0 : --object->references;
    LeaveCriticalSection(&object->lock);
    if (remaining != 0) return;
    if (object->ready != NULL) CloseHandle(object->ready);
    DeleteCriticalSection(&object->lock);
    free(object);
}

void hl_windows_counter_destroy_entry(hl_windows_handle_entry *entry) {
    hl_windows_counter_object *object = entry->payload;
    entry->payload = NULL;
    if (object != NULL) hl_windows_counter_object_release(object);
}

HANDLE hl_windows_counter_wait_handle_locked(const hl_windows_handle_entry *entry) {
    const hl_windows_counter_object *object = entry->payload;
    return object == NULL ? NULL : object->ready;
}

/* --- handle resolution ------------------------------------------------------ */

/* Resolve a counter handle and take a reference, so the object cannot be freed
 * by a concurrent close() while an operation is using it. */
static hl_windows_counter_object *hl_windows_counter_acquire(hl_host_windows *host, hl_host_handle handle,
                                                             uint32_t right, hl_status *status) {
    hl_windows_handle_entry *entry;
    hl_windows_counter_object *object = NULL;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_COUNTER);
    if (entry == NULL || entry->payload == NULL)
        *status = HL_STATUS_INVALID_ARGUMENT;
    else if (right != 0 && (entry->rights & right) == 0)
        *status = HL_STATUS_PERMISSION_DENIED;
    else {
        object = entry->payload;
        EnterCriticalSection(&object->lock);
        object->references++;
        LeaveCriticalSection(&object->lock);
        *status = HL_STATUS_OK;
    }
    hl_windows_unlock(host);
    return object;
}

/* --- subscriptions ---------------------------------------------------------- */

static void CALLBACK hl_windows_counter_wait_callback(PVOID parameter, BOOLEAN timed_out) {
    hl_windows_counter_subscription *subscription = parameter;
    (void)timed_out;
    /* No lock is taken here on purpose. The record stays allocated until
     * unsubscribe's UnregisterWaitEx has confirmed this callback finished, and
     * notify() is the subscriber's own code, which may take the subscriber's
     * locks -- taking a host lock around it would invert them. */
    subscription->notify(subscription->observer, subscription->token);
}

/* Arm one subscription against the object's ready event. The object lock is
 * held. A registration that is still outstanding is discarded without waiting:
 * see the file header for why this is the non-blocking form. */
static void hl_windows_counter_arm_locked(hl_windows_counter_subscription *subscription) {
    HANDLE registration = NULL;
    if (subscription->wait != NULL) {
        (void)UnregisterWait(subscription->wait);
        subscription->wait = NULL;
    }
    if (RegisterWaitForSingleObject(&registration, subscription->object->ready, hl_windows_counter_wait_callback,
                                    subscription, INFINITE, WT_EXECUTEONLYONCE))
        subscription->wait = registration;
}

static void hl_windows_counter_rearm_locked(hl_windows_counter_object *object) {
    hl_windows_counter_subscription *subscription;
    for (subscription = object->subscriptions; subscription != NULL; subscription = subscription->next)
        hl_windows_counter_arm_locked(subscription);
}

/* --- the group -------------------------------------------------------------- */

static hl_host_result hl_windows_counter_create(void *context, uint64_t initial, uint32_t flags) {
    hl_host_windows *host = context;
    hl_windows_counter_object *object;
    hl_windows_handle_entry *entry;
    hl_host_result allocated;
    if (initial == UINT64_MAX || (flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = calloc(1, sizeof(*object));
    if (object == NULL) return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    object->ready = CreateEventW(NULL, TRUE, initial != 0, NULL);
    if (object->ready == NULL) {
        hl_host_result failure = hl_windows_last_error_result();
        free(object);
        return failure;
    }
    InitializeCriticalSection(&object->lock);
    object->value = initial;
    object->flags = flags;
    object->references = 1;
    allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_COUNTER);
    if (allocated.status != HL_STATUS_OK) {
        CloseHandle(object->ready);
        DeleteCriticalSection(&object->lock);
        free(object);
        return allocated;
    }
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_COUNTER);
    entry->payload = object;
    entry->rights = HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
    hl_windows_unlock(host);
    return allocated;
}

static hl_host_result hl_windows_counter_read(void *context, hl_host_handle counter) {
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object = hl_windows_counter_acquire(context, counter, HL_HOST_TRANSFER_READ, &status);
    uint64_t consumed;
    if (object == NULL) return hl_windows_result(status, 0, 0);
    for (;;) {
        EnterCriticalSection(&object->lock);
        if (object->value != 0) break;
        if ((object->flags & HL_HOST_COUNTER_NONBLOCK) != 0) {
            LeaveCriticalSection(&object->lock);
            hl_windows_counter_object_release(object);
            return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        LeaveCriticalSection(&object->lock);
        if (WaitForSingleObject(object->ready, INFINITE) == WAIT_FAILED) {
            hl_host_result failure = hl_windows_last_error_result();
            hl_windows_counter_object_release(object);
            return failure;
        }
    }
    consumed = (object->flags & HL_HOST_COUNTER_SEMAPHORE) != 0 ? UINT64_C(1) : object->value;
    object->value -= consumed;
    if (object->value == 0) {
        ResetEvent(object->ready);
        hl_windows_counter_rearm_locked(object);
    }
    LeaveCriticalSection(&object->lock);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, consumed, 0);
}

static hl_host_result hl_windows_counter_write(void *context, hl_host_handle counter, uint64_t value) {
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object;
    if (value == UINT64_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_counter_acquire(context, counter, HL_HOST_TRANSFER_WRITE, &status);
    if (object == NULL) return hl_windows_result(status, 0, 0);
    EnterCriticalSection(&object->lock);
    /* UINT64_MAX is reserved and may never be stored, so the ceiling is one
     * below it. A write that would cross it does not saturate silently. */
    if (value > UINT64_MAX - UINT64_C(1) - object->value) {
        LeaveCriticalSection(&object->lock);
        hl_windows_counter_object_release(object);
        return hl_windows_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    }
    object->value += value;
    if (object->value != 0) SetEvent(object->ready);
    LeaveCriticalSection(&object->lock);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_counter_get_flags(void *context, hl_host_handle counter) {
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object = hl_windows_counter_acquire(context, counter, HL_HOST_TRANSFER_CONTROL, &status);
    uint32_t flags;
    if (object == NULL) return hl_windows_result(status, 0, 0);
    EnterCriticalSection(&object->lock);
    flags = object->flags;
    LeaveCriticalSection(&object->lock);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, flags, 0);
}

static hl_host_result hl_windows_counter_set_flags(void *context, hl_host_handle counter, uint32_t flags) {
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object;
    if ((flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_counter_acquire(context, counter, HL_HOST_TRANSFER_CONTROL, &status);
    if (object == NULL) return hl_windows_result(status, 0, 0);
    EnterCriticalSection(&object->lock);
    /* Semaphore mode is fixed at creation on every backend: it changes what a
     * read CONSUMES, and flipping it under a blocked reader would change the
     * meaning of a call already in flight. Only the blocking mode is settable. */
    if ((object->flags & HL_HOST_COUNTER_SEMAPHORE) != (flags & HL_HOST_COUNTER_SEMAPHORE)) {
        LeaveCriticalSection(&object->lock);
        hl_windows_counter_object_release(object);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object->flags = flags;
    LeaveCriticalSection(&object->lock);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_counter_duplicate(void *context, hl_host_handle counter) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_counter_object *object;
    hl_host_result allocated;
    uint32_t rights;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, counter, HL_WINDOWS_HANDLE_COUNTER);
    object = entry == NULL ? NULL : entry->payload;
    rights = entry == NULL ? 0 : entry->rights;
    if (object != NULL) {
        EnterCriticalSection(&object->lock);
        object->references++;
        LeaveCriticalSection(&object->lock);
    }
    hl_windows_unlock(host);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_COUNTER);
    if (allocated.status != HL_STATUS_OK) {
        hl_windows_counter_object_release(object);
        return allocated;
    }
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_COUNTER);
    entry->payload = object;
    entry->rights = rights;
    hl_windows_unlock(host);
    return allocated;
}

static hl_host_result hl_windows_counter_readiness(void *context, hl_host_handle counter, uint32_t interests) {
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object;
    uint32_t ready;
    if ((interests & ~(uint32_t)HL_HOST_READY_READ) != 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_counter_acquire(context, counter, HL_HOST_TRANSFER_WAIT, &status);
    if (object == NULL) return hl_windows_result(status, 0, 0);
    EnterCriticalSection(&object->lock);
    ready = object->value != 0 ? (uint32_t)HL_HOST_READY_READ : 0u;
    LeaveCriticalSection(&object->lock);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, ready & interests, 0);
}

static hl_host_result hl_windows_counter_subscribe(void *context, hl_host_handle counter,
                                                   void (*notify)(void *, uint64_t), void *observer, uint64_t token) {
    hl_host_windows *host = context;
    hl_status status = HL_STATUS_OK;
    hl_windows_counter_object *object;
    hl_windows_counter_subscription *subscription;
    hl_windows_handle_entry *entry;
    hl_host_result allocated;
    if (notify == NULL || token == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = hl_windows_counter_acquire(host, counter, HL_HOST_TRANSFER_WAIT, &status);
    if (object == NULL) return hl_windows_result(status, 0, 0);
    subscription = calloc(1, sizeof(*subscription));
    if (subscription == NULL) {
        hl_windows_counter_object_release(object);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    allocated = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_SUBSCRIPTION);
    if (allocated.status != HL_STATUS_OK) {
        free(subscription);
        hl_windows_counter_object_release(object);
        return allocated;
    }
    subscription->object = object;
    subscription->counter = counter;
    subscription->notify = notify;
    subscription->observer = observer;
    subscription->token = token;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, allocated.value, HL_WINDOWS_HANDLE_SUBSCRIPTION);
    entry->payload = subscription;
    hl_windows_unlock(host);
    /* Published, then armed. Arming can fire the callback on a pool thread
     * immediately -- the counter may already be non-zero -- so every field the
     * callback reads is filled in before the registration exists. */
    EnterCriticalSection(&object->lock);
    subscription->next = object->subscriptions;
    object->subscriptions = subscription;
    hl_windows_counter_arm_locked(subscription);
    LeaveCriticalSection(&object->lock);
    return allocated;
}

static hl_host_result hl_windows_counter_unsubscribe(void *context, hl_host_handle handle) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_counter_subscription *subscription;
    hl_windows_counter_object *object;
    HANDLE registration;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_SUBSCRIPTION);
    subscription = entry == NULL ? NULL : entry->payload;
    if (entry != NULL) hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (subscription == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    object = subscription->object;
    EnterCriticalSection(&object->lock);
    {
        hl_windows_counter_subscription **link = &object->subscriptions;
        while (*link != NULL && *link != subscription)
            link = &(*link)->next;
        if (*link == subscription) *link = subscription->next;
    }
    registration = subscription->wait;
    subscription->wait = NULL;
    LeaveCriticalSection(&object->lock);
    /* Unlinked first, so no re-arm can add a registration behind this one, and
     * only then quiesced -- outside the object lock, because the callback's
     * notify() is free to take locks this thread knows nothing about. */
    if (registration != NULL) (void)UnregisterWaitEx(registration, INVALID_HANDLE_VALUE);
    free(subscription);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* Retire subscriptions one at a time, re-scanning under the host lock between
 * each. The scan cannot hold the lock across the retirement: unsubscribe takes
 * the same lock, and it also blocks on a pool callback, which is not something
 * to do with the whole handle table pinned.
 *
 * A counter of HL_HOST_HANDLE_INVALID means "every subscription", which is what
 * host teardown needs; no live counter can carry that handle. */
static void hl_windows_counter_retire_matching(hl_host_windows *host, hl_host_handle counter) {
    for (;;) {
        hl_host_handle found = HL_HOST_HANDLE_INVALID;
        uint32_t index;
        hl_windows_lock(host);
        for (index = 0; index < host->handle_capacity; ++index) {
            const hl_windows_handle_entry *entry = &host->handles[index];
            const hl_windows_counter_subscription *subscription = entry->payload;
            if (entry->kind != (uint16_t)HL_WINDOWS_HANDLE_SUBSCRIPTION || subscription == NULL) continue;
            if (counter != HL_HOST_HANDLE_INVALID && subscription->counter != counter) continue;
            found = hl_windows_encode_handle(index, entry->generation);
            break;
        }
        hl_windows_unlock(host);
        if (found == HL_HOST_HANDLE_INVALID) return;
        (void)hl_windows_counter_unsubscribe(host, found);
    }
}

void hl_windows_counter_retire_subscriptions(hl_host_windows *host, hl_host_handle counter) {
    hl_windows_counter_retire_matching(host, counter);
}

void hl_windows_counter_retire_all_subscriptions(hl_host_windows *host) {
    hl_windows_counter_retire_matching(host, HL_HOST_HANDLE_INVALID);
}

static hl_host_result hl_windows_counter_close(void *context, hl_host_handle counter) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_counter_object *object;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, counter, HL_WINDOWS_HANDLE_COUNTER);
    hl_windows_unlock(host);
    if (entry == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* A subscription outlives its counter handle only as a dangling callback,
     * so the handle takes them with it -- and quiesces them, which is why this
     * runs before the slot is cleared rather than inside the host lock. */
    hl_windows_counter_retire_subscriptions(host, counter);
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, counter, HL_WINDOWS_HANDLE_COUNTER);
    object = entry == NULL ? NULL : entry->payload;
    if (entry != NULL) hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    if (object == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_counter_object_release(object);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

const hl_host_counter_services hl_windows_counter_services = {.abi = HL_HOST_COUNTER_ABI,
                                                              .size = sizeof(hl_host_counter_services),
                                                              .create = hl_windows_counter_create,
                                                              .read = hl_windows_counter_read,
                                                              .write = hl_windows_counter_write,
                                                              .get_flags = hl_windows_counter_get_flags,
                                                              .set_flags = hl_windows_counter_set_flags,
                                                              .duplicate = hl_windows_counter_duplicate,
                                                              .readiness = hl_windows_counter_readiness,
                                                              .subscribe = hl_windows_counter_subscribe,
                                                              .unsubscribe = hl_windows_counter_unsubscribe,
                                                              .close = hl_windows_counter_close};
