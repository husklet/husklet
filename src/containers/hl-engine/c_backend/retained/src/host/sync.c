#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "sync.h"

#include <errno.h>
#include <stdlib.h>

/*
 * Everything below the primitive layer -- chunked storage, the
 * (generation << 32) | (index + 1) encoding, refcounting and the free-slot scan
 * -- is host independent, so the primitive layer is the only part a new host
 * supplies. POSIX gets an errorcheck pthread mutex, which already reports
 * self-relock and unlock-by-non-owner.
 *
 * Windows has neither. CRITICAL_SECTION is recursive, which the opaque-mutex
 * contract forbids outright, and SRWLOCK is non-recursive but silently corrupts
 * on misuse rather than reporting it. The SRWLOCK below therefore carries an
 * owner thread id and synthesizes the same two verdicts an errorcheck mutex
 * returns, so both hosts reject the same programs.
 */
#if defined(_WIN32)

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

/* Registry-internal lock. No ownership discipline is asked of it, so exclusive
 * mode on a bare slim lock is the whole implementation. */
typedef SRWLOCK hl_sync_lock;

typedef struct hl_sync_mutex_primitive {
    SRWLOCK lock;
    volatile LONG owner;
} hl_sync_mutex_primitive;

/* A private error space rather than errno: these values travel no further than
 * hl_sync_status and the opaque hl_host_result.detail, and mapping Win32 codes
 * here would leak a native error number through that field. */
enum { HL_SYNC_ERROR_NONE = 0, HL_SYNC_ERROR_INVALID = 1, HL_SYNC_ERROR_BUSY = 2, HL_SYNC_ERROR_MEMORY = 3 };

static hl_status hl_sync_status(int error) {
    switch (error) {
    case HL_SYNC_ERROR_NONE: return HL_STATUS_OK;
    case HL_SYNC_ERROR_INVALID: return HL_STATUS_INVALID_ARGUMENT;
    case HL_SYNC_ERROR_BUSY: return HL_STATUS_BUSY;
    case HL_SYNC_ERROR_MEMORY: return HL_STATUS_OUT_OF_MEMORY;
    default: return HL_STATUS_PLATFORM_FAILURE;
    }
}

static int hl_sync_lock_init(hl_sync_lock *lock) {
    InitializeSRWLock(lock);
    return 0;
}

static void hl_sync_lock_destroy(hl_sync_lock *lock) {
    (void)lock;
}

static int hl_sync_lock_acquire(hl_sync_lock *lock) {
    AcquireSRWLockExclusive(lock);
    return 0;
}

static int hl_sync_lock_release(hl_sync_lock *lock) {
    ReleaseSRWLockExclusive(lock);
    return 0;
}

static int hl_sync_mutex_init(hl_sync_mutex_primitive *mutex) {
    InitializeSRWLock(&mutex->lock);
    mutex->owner = 0;
    return HL_SYNC_ERROR_NONE;
}

static void hl_sync_mutex_destroy(hl_sync_mutex_primitive *mutex) {
    (void)mutex;
}

/* Read the owner through an interlocked compare so the field is never torn
 * against a concurrent release from the thread that holds the lock. */
static LONG hl_sync_mutex_owner(hl_sync_mutex_primitive *mutex) {
    return InterlockedCompareExchange(&mutex->owner, 0, 0);
}

static int hl_sync_mutex_acquire(hl_sync_mutex_primitive *mutex) {
    const LONG self = (LONG)GetCurrentThreadId();
    if (hl_sync_mutex_owner(mutex) == self) return HL_SYNC_ERROR_BUSY;
    AcquireSRWLockExclusive(&mutex->lock);
    InterlockedExchange(&mutex->owner, self);
    return HL_SYNC_ERROR_NONE;
}

static int hl_sync_mutex_release(hl_sync_mutex_primitive *mutex) {
    const LONG self = (LONG)GetCurrentThreadId();
    if (hl_sync_mutex_owner(mutex) != self) return HL_SYNC_ERROR_INVALID;
    InterlockedExchange(&mutex->owner, 0);
    ReleaseSRWLockExclusive(&mutex->lock);
    return HL_SYNC_ERROR_NONE;
}

static int hl_sync_mutex_try_acquire(hl_sync_mutex_primitive *mutex) {
    const LONG self = (LONG)GetCurrentThreadId();
    if (hl_sync_mutex_owner(mutex) == self) return HL_SYNC_ERROR_BUSY;
    if (!TryAcquireSRWLockExclusive(&mutex->lock)) return HL_SYNC_ERROR_BUSY;
    InterlockedExchange(&mutex->owner, self);
    return HL_SYNC_ERROR_NONE;
}

#else

#include <pthread.h>

typedef pthread_mutex_t hl_sync_lock;
typedef pthread_mutex_t hl_sync_mutex_primitive;

enum { HL_SYNC_ERROR_NONE = 0, HL_SYNC_ERROR_BUSY = EBUSY, HL_SYNC_ERROR_MEMORY = ENOMEM };

static hl_status hl_sync_status(int error) {
    switch (error) {
    case 0: return HL_STATUS_OK;
    case EINVAL:
    case EPERM: return HL_STATUS_INVALID_ARGUMENT;
    case ENOMEM: return HL_STATUS_OUT_OF_MEMORY;
    case EAGAIN: return HL_STATUS_RESOURCE_LIMIT;
    case EBUSY:
    case EDEADLK: return HL_STATUS_BUSY;
    default: return HL_STATUS_PLATFORM_FAILURE;
    }
}

static int hl_sync_lock_init(hl_sync_lock *lock) {
    return pthread_mutex_init(lock, NULL);
}

static void hl_sync_lock_destroy(hl_sync_lock *lock) {
    (void)pthread_mutex_destroy(lock);
}

static int hl_sync_lock_acquire(hl_sync_lock *lock) {
    return pthread_mutex_lock(lock);
}

static int hl_sync_lock_release(hl_sync_lock *lock) {
    return pthread_mutex_unlock(lock);
}

static int hl_sync_mutex_init(hl_sync_mutex_primitive *mutex) {
    pthread_mutexattr_t attributes;
    int error = pthread_mutexattr_init(&attributes);
    if (error != 0) return error;
    error = pthread_mutexattr_settype(&attributes, PTHREAD_MUTEX_ERRORCHECK);
    if (error == 0) error = pthread_mutex_init(mutex, &attributes);
    pthread_mutexattr_destroy(&attributes);
    return error;
}

static void hl_sync_mutex_destroy(hl_sync_mutex_primitive *mutex) {
    (void)pthread_mutex_destroy(mutex);
}

static int hl_sync_mutex_acquire(hl_sync_mutex_primitive *mutex) {
    return pthread_mutex_lock(mutex);
}

static int hl_sync_mutex_release(hl_sync_mutex_primitive *mutex) {
    return pthread_mutex_unlock(mutex);
}

static int hl_sync_mutex_try_acquire(hl_sync_mutex_primitive *mutex) {
    return pthread_mutex_trylock(mutex);
}

#endif

/*
 * The parking primitive: block on a word until someone wakes the address, with the value test and
 * the enqueue as one step. Three entry points, one arm per host.
 *
 *   hl_park_wait    compare and block; the interrupted pointer lets an arm whose wake is not
 *                   genuinely address-keyed re-test the interruption record without racing.
 *   hl_park_wake    release waiters on an address, reporting only what it can prove.
 *   hl_park_signal  wake every waiter on an address, for interruption. Called from a
 *                   signal-delivery context, so an arm must not allocate or take a lock that
 *                   ordinary code can be holding.
 *
 * A wake never reads the word. Releasing a waiter whose value is unchanged is the whole mechanism
 * by which an interruption reaches a thread that is blocked on a value nobody is going to change.
 */

#define HL_PARK_SEGMENT_NS UINT64_C(50000000)

#if defined(_WIN32)

/* Windows has both tiers available and neither is written here. The private tier is an
 * address-keyed wait/wake pair the OS provides directly; the process-shared tier is not, because
 * that wait is keyed on a virtual address within one process and does not cross a process boundary
 * whatever the backing -- a shared spot there has to be a named kernel object selected by the key
 * this contract carries beside the address. Both belong with the rest of the Windows backend, so
 * this arm reports a typed absence rather than a wait that does not wait. */
static hl_status hl_park_wait(uint32_t scope, const void *address, uint64_t expected, uint32_t compare_size,
                              uint64_t deadline_ns, const volatile uint32_t *interrupted) {
    (void)scope;
    (void)address;
    (void)expected;
    (void)compare_size;
    (void)deadline_ns;
    (void)interrupted;
    return HL_STATUS_NOT_SUPPORTED;
}

static hl_status hl_park_wake(uint32_t scope, const void *address, uint32_t count, uint64_t *released) {
    (void)scope;
    (void)address;
    (void)count;
    *released = 0;
    return HL_STATUS_NOT_SUPPORTED;
}

static void hl_park_signal(uint32_t scope, const void *address) {
    (void)scope;
    (void)address;
}

#elif defined(__linux__)

#include <limits.h>
#include <linux/futex.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/*
 * The kernel's own address-keyed wait serves both scopes: the private flag selects a process-local
 * queue, and without it the key is the physical page, which is what makes a spot in a shared
 * mapping reachable from a process that never saw the waiter's virtual address. The caller's
 * cross-process key is therefore unused here -- it exists for a host whose shared queue is a named
 * object rather than a page.
 *
 * The bitset form is used rather than the plain wait because its timeout is an ABSOLUTE monotonic
 * deadline, which is what this contract passes. The plain form takes a relative interval, so every
 * spurious wake would have to recompute it and the deadline would drift outwards each time.
 */
static hl_status hl_park_wait(uint32_t scope, const void *address, uint64_t expected, uint32_t compare_size,
                              uint64_t deadline_ns, const volatile uint32_t *interrupted) {
    struct timespec deadline;
    int operation = FUTEX_WAIT_BITSET | (scope == HL_HOST_PARK_PRIVATE ? FUTEX_PRIVATE_FLAG : 0);
    long result;
    (void)interrupted; /* the wake below reaches this waiter directly */
    /* The kernel's futex word is 32 bits. A wider compare is refused rather than silently narrowed:
     * comparing half of the caller's word would park on a value it never asked about. */
    if (compare_size != 4u) return HL_STATUS_NOT_SUPPORTED;
    if (expected > UINT32_MAX) return HL_STATUS_INVALID_ARGUMENT;
    deadline.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    deadline.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    result = syscall(SYS_futex, address, operation, (int)(uint32_t)expected,
                     deadline_ns == HL_HOST_DEADLINE_INFINITE ? NULL : &deadline, NULL, FUTEX_BITSET_MATCH_ANY);
    if (result == 0) return HL_STATUS_OK;
    switch (errno) {
    case EAGAIN: return HL_STATUS_WOULD_BLOCK;
    case ETIMEDOUT: return HL_STATUS_TIMED_OUT;
    /* A host-level interruption is not the caller's: it means only that the block ended early, and
     * HL_STATUS_OK already means "re-read the word". The caller's own interruption record is
     * consulted above this layer, so reporting INTERRUPTED here would invent one. */
    case EINTR: return HL_STATUS_OK;
    case EFAULT:
    case EINVAL: return HL_STATUS_INVALID_ARGUMENT;
    default: return HL_STATUS_PLATFORM_FAILURE;
    }
}

static hl_status hl_park_wake(uint32_t scope, const void *address, uint32_t count, uint64_t *released) {
    int operation = FUTEX_WAKE | (scope == HL_HOST_PARK_PRIVATE ? FUTEX_PRIVATE_FLAG : 0);
    long result =
        syscall(SYS_futex, address, operation, count > (uint32_t)INT_MAX ? INT_MAX : (int)count, NULL, NULL, 0);
    *released = 0;
    if (result < 0)
        return (errno == EFAULT || errno == EINVAL) ? HL_STATUS_INVALID_ARGUMENT : HL_STATUS_PLATFORM_FAILURE;
    *released = (uint64_t)result;
    return HL_STATUS_OK;
}

/* One syscall, no allocation and no userspace lock, so this is usable from a signal handler. */
static void hl_park_signal(uint32_t scope, const void *address) {
    uint64_t released = 0;
    (void)hl_park_wake(scope, address, UINT32_MAX, &released);
}

#else

#include <pthread.h>
#include <string.h>
#include <time.h>

/*
 * The arm for a POSIX host with no address-keyed wait exposed to userspace. A fixed table of
 * condition variables stands in for the kernel's queues: the address selects a bucket, the value
 * test happens under that bucket's lock, and a release takes the same lock, so the compare and the
 * enqueue are as indivisible as the native arm's.
 *
 * Two honest costs, both consequences of the queues being process-local objects rather than kernel
 * ones. A process-shared spot cannot be built on them at all and is refused. And an interruption
 * cannot take the bucket lock -- it arrives in a signal-delivery context, where the interrupted
 * thread may be the one holding it -- so it stores its record, releases the bucket without the
 * lock, and this arm bounds each block at a segment so a release that crossed with the last test is
 * still noticed within one segment rather than never.
 */
enum { HL_PARK_BUCKET_COUNT = 64u };

static pthread_mutex_t hl_park_bucket_lock[HL_PARK_BUCKET_COUNT];
static pthread_cond_t hl_park_bucket_wake[HL_PARK_BUCKET_COUNT];
static pthread_once_t hl_park_bucket_once = PTHREAD_ONCE_INIT;

static void hl_park_bucket_init(void) {
    uint32_t index;
    for (index = 0; index < HL_PARK_BUCKET_COUNT; ++index) {
        pthread_mutex_init(&hl_park_bucket_lock[index], NULL);
        pthread_cond_init(&hl_park_bucket_wake[index], NULL);
    }
}

static uint32_t hl_park_bucket(const void *address) {
    uint64_t value = (uint64_t)(uintptr_t)address;
    value ^= value >> 33;
    value *= UINT64_C(0xff51afd7ed558ccd);
    value ^= value >> 29;
    return (uint32_t)(value % HL_PARK_BUCKET_COUNT);
}

static uint64_t hl_park_now_ns(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static int hl_park_word_differs(const void *address, uint64_t expected, uint32_t compare_size) {
    if (compare_size == 4u) {
        uint32_t observed;
        memcpy(&observed, address, sizeof(observed));
        return (uint64_t)observed != expected;
    }
    {
        uint64_t observed;
        memcpy(&observed, address, sizeof(observed));
        return observed != expected;
    }
}

static hl_status hl_park_wait(uint32_t scope, const void *address, uint64_t expected, uint32_t compare_size,
                              uint64_t deadline_ns, const volatile uint32_t *interrupted) {
    uint32_t bucket;
    hl_status status = HL_STATUS_OK;
    if (scope != HL_HOST_PARK_PRIVATE) return HL_STATUS_NOT_SUPPORTED;
    pthread_once(&hl_park_bucket_once, hl_park_bucket_init);
    bucket = hl_park_bucket(address);
    pthread_mutex_lock(&hl_park_bucket_lock[bucket]);
    if (hl_park_word_differs(address, expected, compare_size)) {
        pthread_mutex_unlock(&hl_park_bucket_lock[bucket]);
        return HL_STATUS_WOULD_BLOCK;
    }
    for (;;) {
        struct timespec segment;
        struct timespec realtime;
        uint64_t now = hl_park_now_ns();
        uint64_t limit = now + HL_PARK_SEGMENT_NS;
        uint64_t absolute;
        if (interrupted != NULL && *interrupted != 0) break;
        if (deadline_ns != HL_HOST_DEADLINE_INFINITE) {
            if (now >= deadline_ns) {
                status = HL_STATUS_TIMED_OUT;
                break;
            }
            if (deadline_ns < limit) limit = deadline_ns;
        }
        clock_gettime(CLOCK_REALTIME, &realtime);
        absolute = (uint64_t)realtime.tv_sec * UINT64_C(1000000000) + (uint64_t)realtime.tv_nsec + (limit - now);
        segment.tv_sec = (time_t)(absolute / UINT64_C(1000000000));
        segment.tv_nsec = (long)(absolute % UINT64_C(1000000000));
        /* Only a segment that ran out carries no information. Anything else -- a release, or a
         * spurious wake the condition variable is allowed -- ends the block and is reported as
         * such, because HL_STATUS_OK already means no more than "re-read the word". */
        if (pthread_cond_timedwait(&hl_park_bucket_wake[bucket], &hl_park_bucket_lock[bucket], &segment) != ETIMEDOUT)
            break;
    }
    pthread_mutex_unlock(&hl_park_bucket_lock[bucket]);
    return status;
}

static hl_status hl_park_wake(uint32_t scope, const void *address, uint32_t count, uint64_t *released) {
    uint32_t bucket;
    *released = 0;
    if (scope != HL_HOST_PARK_PRIVATE) return HL_STATUS_NOT_SUPPORTED;
    if (count == 0) return HL_STATUS_OK;
    pthread_once(&hl_park_bucket_once, hl_park_bucket_init);
    bucket = hl_park_bucket(address);
    pthread_mutex_lock(&hl_park_bucket_lock[bucket]);
    pthread_cond_broadcast(&hl_park_bucket_wake[bucket]);
    pthread_mutex_unlock(&hl_park_bucket_lock[bucket]);
    /* A bucket holds waiters on other addresses too, so nothing here can be proven released.
     * Reporting zero is the honest answer and the contract admits it. */
    return HL_STATUS_OK;
}

static void hl_park_signal(uint32_t scope, const void *address) {
    uint32_t bucket;
    if (scope != HL_HOST_PARK_PRIVATE) return;
    pthread_once(&hl_park_bucket_once, hl_park_bucket_init);
    bucket = hl_park_bucket(address);
    /* Deliberately without the bucket lock: the thread this interrupts may be holding it. The
     * bounded segment above is what makes the resulting missed release recoverable. */
    pthread_cond_broadcast(&hl_park_bucket_wake[bucket]);
}

#endif

enum { HL_SYNC_CHUNK_SIZE = 256, HL_SYNC_CHUNK_COUNT = 256 };

/*
 * One record per waiter that has ever blocked or been interrupted.
 *
 * The table is a fixed array rather than the chunked, lock-protected storage the mutexes use, and
 * every field is touched with a sequentially consistent atomic, for one reason: an interruption
 * arrives in a signal-delivery context. Growing storage there would allocate and taking the
 * registry lock there could deadlock against the very thread being interrupted, so the record an
 * interruption has to find and mark must be reachable without a lock.
 *
 * That makes claiming a record a lock-free set insertion, and the race it has to survive is the
 * only one that matters here: a thread's first block and an interruption aimed at that same thread
 * arriving together, from two threads, with nothing to serialise them. Both may claim a record.
 * The rule that settles it is total and needs no agreement: every party walks the table in the same
 * order (derived from the waiter), and the EARLIEST record for a waiter is the real one -- so after
 * claiming, each party folds every later duplicate into the earliest, moving any interruption
 * across rather than dropping it, and then works with what the fold returned. A party whose record
 * was folded away under it sees its own claim gone and starts again.
 *
 * A record is kept once claimed, so only the first block for a waiter pays for the fold. Records
 * are reclaimed on demand: when nothing is free, a record that is neither blocked nor holding an
 * interruption carries no information and is taken. Only when none of those exists either does
 * claiming fail, and it says so rather than discarding an interruption someone is owed.
 */
enum { HL_PARK_SLOT_COUNT = 1024u };

typedef struct hl_park_slot {
    uint64_t waiter; /* 0 while free; claimed and released by compare-exchange */
    uint64_t address;
    uint32_t scope;
    uint32_t parked;
    uint32_t pending;
    uint32_t reserved;
} hl_park_slot;

typedef struct hl_host_mutex_entry {
    hl_sync_mutex_primitive mutex;
    uint32_t generation;
    uint32_t active;
    uint32_t users;
} hl_host_mutex_entry;

struct hl_host_sync_registry {
    hl_sync_lock lock;
    uint32_t destroying;
    uint32_t next_free;
    hl_host_mutex_entry *chunks[HL_SYNC_CHUNK_COUNT];
    hl_park_slot parks[HL_PARK_SLOT_COUNT];
};

static hl_host_result hl_sync_result(hl_status status, uint64_t value, int detail) {
    return (hl_host_result){(int32_t)status, 0, value, (uint64_t)(unsigned int)detail};
}

static hl_host_mutex_entry *hl_sync_lookup(hl_host_sync_registry *registry, hl_host_handle handle) {
    uint32_t low = (uint32_t)handle;
    uint32_t index;
    hl_host_mutex_entry *chunk;
    hl_host_mutex_entry *entry;
    if (low == 0) return NULL;
    index = low - 1u;
    chunk = registry->chunks[index / HL_SYNC_CHUNK_SIZE];
    if (chunk == NULL) return NULL;
    entry = &chunk[index % HL_SYNC_CHUNK_SIZE];
    if (!entry->active || entry->generation != (uint32_t)(handle >> 32)) return NULL;
    return entry;
}

hl_status hl_host_sync_registry_create(hl_host_sync_registry **output) {
    hl_host_sync_registry *registry;
    if (output == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *output = NULL;
    registry = calloc(1, sizeof(*registry));
    if (registry == NULL) return HL_STATUS_OUT_OF_MEMORY;
    if (hl_sync_lock_init(&registry->lock) != 0) {
        free(registry);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    *output = registry;
    return HL_STATUS_OK;
}

void hl_host_sync_registry_destroy(hl_host_sync_registry *registry) {
    uint32_t chunk_index;
    uint32_t park_index;
    if (registry == NULL) return;
    hl_sync_lock_acquire(&registry->lock);
    registry->destroying = 1;
    hl_sync_lock_release(&registry->lock);
    /* Release anything still blocked before the storage it is blocked against goes away. This does
     * not make destroying a registry with waiters outstanding safe -- the caller owes the same
     * exclusion the mutexes already require -- it only removes the case where a waiter would
     * otherwise never be reached at all. */
    for (park_index = 0; park_index < HL_PARK_SLOT_COUNT; ++park_index) {
        hl_park_slot *slot = &registry->parks[park_index];
        uint64_t published;
        if (__atomic_load_n(&slot->parked, __ATOMIC_ACQUIRE) == 0) continue;
        __atomic_store_n(&slot->pending, 1, __ATOMIC_RELEASE);
        published = __atomic_load_n(&slot->address, __ATOMIC_RELAXED);
        if (published != 0)
            hl_park_signal(__atomic_load_n(&slot->scope, __ATOMIC_RELAXED), (const void *)(uintptr_t)published);
    }
    for (chunk_index = 0; chunk_index < HL_SYNC_CHUNK_COUNT; ++chunk_index) {
        hl_host_mutex_entry *chunk = registry->chunks[chunk_index];
        uint32_t entry_index;
        if (chunk == NULL) continue;
        for (entry_index = 0; entry_index < HL_SYNC_CHUNK_SIZE; ++entry_index)
            if (chunk[entry_index].active) hl_sync_mutex_destroy(&chunk[entry_index].mutex);
        free(chunk);
    }
    hl_sync_lock_destroy(&registry->lock);
    free(registry);
}

hl_host_result hl_host_sync_mutex_create(hl_host_sync_registry *registry) {
    uint32_t scan;
    int error;
    if (registry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_sync_lock_acquire(&registry->lock);
    if (registry->destroying) {
        hl_sync_lock_release(&registry->lock);
        return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (scan = 0; scan < HL_SYNC_CHUNK_COUNT * HL_SYNC_CHUNK_SIZE; ++scan) {
        uint32_t index = (registry->next_free + scan) % (HL_SYNC_CHUNK_COUNT * HL_SYNC_CHUNK_SIZE);
        uint32_t chunk_index = index / HL_SYNC_CHUNK_SIZE;
        uint32_t entry_index = index % HL_SYNC_CHUNK_SIZE;
        hl_host_mutex_entry *chunk = registry->chunks[chunk_index];
        if (chunk == NULL) {
            chunk = calloc(HL_SYNC_CHUNK_SIZE, sizeof(*chunk));
            if (chunk == NULL) {
                hl_sync_lock_release(&registry->lock);
                return hl_sync_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
            }
            registry->chunks[chunk_index] = chunk;
        }
        {
            hl_host_mutex_entry *entry = &chunk[entry_index];
            if (entry->active) continue;
            error = hl_sync_mutex_init(&entry->mutex);
            if (error != 0) {
                hl_sync_lock_release(&registry->lock);
                return hl_sync_result(hl_sync_status(error), 0, error);
            }
            entry->generation++;
            if (entry->generation == 0) entry->generation = 1;
            entry->active = 1;
            registry->next_free = (index + 1u) % (HL_SYNC_CHUNK_COUNT * HL_SYNC_CHUNK_SIZE);
            hl_sync_lock_release(&registry->lock);
            return hl_sync_result(HL_STATUS_OK, ((uint64_t)entry->generation << 32) | (uint64_t)(index + 1u), 0);
        }
    }
    hl_sync_lock_release(&registry->lock);
    return hl_sync_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

static hl_host_mutex_entry *hl_sync_ref(hl_host_sync_registry *registry, hl_host_handle handle) {
    hl_host_mutex_entry *entry;
    if (registry == NULL) return NULL;
    hl_sync_lock_acquire(&registry->lock);
    entry = registry->destroying ? NULL : hl_sync_lookup(registry, handle);
    if (entry != NULL) entry->users++;
    hl_sync_lock_release(&registry->lock);
    return entry;
}

static void hl_sync_unref(hl_host_sync_registry *registry, hl_host_mutex_entry *entry) {
    hl_sync_lock_acquire(&registry->lock);
    if (entry->users != 0) entry->users--;
    hl_sync_lock_release(&registry->lock);
}

hl_host_result hl_host_sync_mutex_lock(hl_host_sync_registry *registry, hl_host_handle handle) {
    hl_host_mutex_entry *entry = hl_sync_ref(registry, handle);
    int error;
    if (entry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    error = hl_sync_mutex_acquire(&entry->mutex);
    hl_sync_unref(registry, entry);
    return hl_sync_result(hl_sync_status(error), 0, error);
}

hl_host_result hl_host_sync_mutex_unlock(hl_host_sync_registry *registry, hl_host_handle handle) {
    hl_host_mutex_entry *entry = hl_sync_ref(registry, handle);
    int error;
    if (entry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    error = hl_sync_mutex_release(&entry->mutex);
    hl_sync_unref(registry, entry);
    return hl_sync_result(hl_sync_status(error), 0, error);
}

hl_host_result hl_host_sync_mutex_close(hl_host_sync_registry *registry, hl_host_handle handle) {
    hl_host_mutex_entry *entry;
    int error;
    if (registry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_sync_lock_acquire(&registry->lock);
    entry = registry->destroying ? NULL : hl_sync_lookup(registry, handle);
    if (entry == NULL) {
        hl_sync_lock_release(&registry->lock);
        return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    error = entry->users == 0 ? hl_sync_mutex_try_acquire(&entry->mutex) : HL_SYNC_ERROR_BUSY;
    if (error != 0) {
        hl_sync_lock_release(&registry->lock);
        return hl_sync_result(HL_STATUS_BUSY, 0, error);
    }
    hl_sync_mutex_release(&entry->mutex);
    hl_sync_mutex_destroy(&entry->mutex);
    entry->active = 0;
    {
        uint32_t index = (uint32_t)handle - 1u;
        if (index < registry->next_free) registry->next_free = index;
    }
    hl_sync_lock_release(&registry->lock);
    return hl_sync_result(HL_STATUS_OK, 0, 0);
}

/* Both parties looking for one waiter walk the table in the same order, which is what lets the
 * duplicate-claim race below settle on a single record without a lock. */
static uint32_t hl_park_start(uint64_t waiter) {
    uint64_t value = waiter;
    value ^= value >> 33;
    value *= UINT64_C(0xff51afd7ed558ccd);
    value ^= value >> 29;
    return (uint32_t)(value % HL_PARK_SLOT_COUNT);
}

static hl_park_slot *hl_park_slot_at(hl_host_sync_registry *registry, uint64_t waiter, uint32_t step) {
    return &registry->parks[(hl_park_start(waiter) + step) % HL_PARK_SLOT_COUNT];
}

/*
 * Collapse this waiter onto its earliest record and return it.
 *
 * "Earliest in the walk order" is a total rule every party computes identically from the waiter
 * alone, so no agreement is needed to pick the same winner. A later duplicate is folded in rather
 * than discarded: its interruption, if it has one, moves across first. A duplicate that is already
 * blocked is left alone -- duplicates only exist in the instant between two claims, before either
 * has blocked, and a record someone is asleep against is not free to reclaim.
 */
static hl_park_slot *hl_park_resolve(hl_host_sync_registry *registry, uint64_t waiter, hl_park_slot *claimed) {
    hl_park_slot *earliest = NULL;
    uint32_t step;
    for (step = 0; step < HL_PARK_SLOT_COUNT; ++step) {
        hl_park_slot *slot = hl_park_slot_at(registry, waiter, step);
        if (__atomic_load_n(&slot->waiter, __ATOMIC_SEQ_CST) != waiter) continue;
        if (earliest == NULL) {
            earliest = slot;
            continue;
        }
        if (__atomic_load_n(&slot->parked, __ATOMIC_SEQ_CST) != 0) continue;
        if (__atomic_exchange_n(&slot->pending, 0, __ATOMIC_SEQ_CST) != 0)
            __atomic_store_n(&earliest->pending, 1, __ATOMIC_SEQ_CST);
        __atomic_store_n(&slot->address, 0, __ATOMIC_SEQ_CST);
        __atomic_store_n(&slot->waiter, 0, __ATOMIC_SEQ_CST);
    }
    return earliest != NULL ? earliest : claimed;
}

/*
 * Find this waiter's record, creating one if it has none.
 *
 * Two threads can reach here for the same waiter at once -- the thread about to block, and the
 * thread delivering it a signal -- and neither may take a lock, so both may end up creating one.
 * Resolving above is what makes that harmless. When nothing is free, a record that is neither
 * blocked nor holding an interruption is taken instead: it carries nothing, and its waiter will
 * simply create another the next time it needs one.
 */
static hl_park_slot *hl_park_claim(hl_host_sync_registry *registry, uint64_t waiter) {
    uint32_t attempt;
    for (attempt = 0; attempt < 8u; ++attempt) {
        hl_park_slot *vacant = NULL;
        uint32_t step;
        for (step = 0; step < HL_PARK_SLOT_COUNT; ++step) {
            hl_park_slot *slot = hl_park_slot_at(registry, waiter, step);
            uint64_t owner = __atomic_load_n(&slot->waiter, __ATOMIC_SEQ_CST);
            if (owner == waiter) return hl_park_resolve(registry, waiter, slot);
            if (owner == 0 && vacant == NULL) vacant = slot;
        }
        if (vacant != NULL) {
            uint64_t expected = 0;
            if (!__atomic_compare_exchange_n(&vacant->waiter, &expected, waiter, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST))
                continue; /* someone took it; rescan, which may now find our own waiter */
            __atomic_store_n(&vacant->address, 0, __ATOMIC_SEQ_CST);
            __atomic_store_n(&vacant->scope, 0, __ATOMIC_SEQ_CST);
            __atomic_store_n(&vacant->parked, 0, __ATOMIC_SEQ_CST);
            return hl_park_resolve(registry, waiter, vacant);
        }
        for (step = 0; step < HL_PARK_SLOT_COUNT; ++step) {
            hl_park_slot *slot = hl_park_slot_at(registry, waiter, step);
            uint64_t owner = __atomic_load_n(&slot->waiter, __ATOMIC_SEQ_CST);
            if (owner == 0 || owner == waiter) break; /* the table changed; rescan from the top */
            if (__atomic_load_n(&slot->parked, __ATOMIC_SEQ_CST) != 0) continue;
            if (__atomic_load_n(&slot->pending, __ATOMIC_SEQ_CST) != 0) continue;
            if (!__atomic_compare_exchange_n(&slot->waiter, &owner, waiter, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST))
                continue;
            __atomic_store_n(&slot->address, 0, __ATOMIC_SEQ_CST);
            __atomic_store_n(&slot->scope, 0, __ATOMIC_SEQ_CST);
            return hl_park_resolve(registry, waiter, slot);
        }
    }
    return NULL;
}

void hl_host_sync_park_reset(hl_host_sync_registry *registry) {
    uint32_t index;
    if (registry == NULL) return;
    for (index = 0; index < HL_PARK_SLOT_COUNT; ++index) {
        __atomic_store_n(&registry->parks[index].pending, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&registry->parks[index].parked, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&registry->parks[index].address, 0, __ATOMIC_RELAXED);
        __atomic_store_n(&registry->parks[index].waiter, 0, __ATOMIC_RELEASE);
    }
}

hl_host_result hl_host_sync_park(hl_host_sync_registry *registry, uint64_t waiter, uint32_t scope, uint64_t key,
                                 const void *address, uint64_t expected, uint32_t compare_size, uint64_t deadline_ns) {
    hl_park_slot *slot;
    hl_status status;
    uint32_t attempt;
    (void)key; /* both arms here key the queue on the address; a named-object host needs the key */
    if (registry == NULL || waiter == 0 || address == NULL ||
        (scope != HL_HOST_PARK_PRIVATE && scope != HL_HOST_PARK_SHARED) || (compare_size != 4u && compare_size != 8u) ||
        ((uintptr_t)address % compare_size) != 0)
        return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (attempt = 0;; ++attempt) {
        /* Bounded, because an unbounded retry here would be a spin inside a blocking call. A fold
         * can only happen once per competing claim, so exhausting this many is not contention, it
         * is a record that cannot be kept -- reported rather than spun on. */
        if (attempt == 8u) return hl_sync_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        slot = hl_park_claim(registry, waiter);
        if (slot == NULL) return hl_sync_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        /* An interruption recorded before this call is consumed here, without blocking. That is the
         * whole reason the record is kept against the waiter rather than against an outstanding
         * block: the signal that lands in the instant before a thread parks would otherwise be
         * lost, and the thread would then wait for a wake nobody is going to send. */
        if (__atomic_exchange_n(&slot->pending, 0, __ATOMIC_SEQ_CST) != 0)
            return hl_sync_result(HL_STATUS_INTERRUPTED, 0, 0);
        __atomic_store_n(&slot->address, (uint64_t)(uintptr_t)address, __ATOMIC_SEQ_CST);
        __atomic_store_n(&slot->scope, scope, __ATOMIC_SEQ_CST);
        __atomic_store_n(&slot->parked, 1, __ATOMIC_SEQ_CST);
        /* Claiming and marking the record blocked are two steps, and a concurrent claim for the
         * same waiter can fold this record away in between. Blocking against a record nobody can
         * find would be unreachable by an interruption, so check and start again instead. */
        if (__atomic_load_n(&slot->waiter, __ATOMIC_SEQ_CST) == waiter) break;
        __atomic_store_n(&slot->parked, 0, __ATOMIC_SEQ_CST);
    }
    status = hl_park_wait(scope, address, expected, compare_size, deadline_ns, &slot->pending);
    __atomic_store_n(&slot->parked, 0, __ATOMIC_SEQ_CST);
    /* A word that already differed is not an interruption and must not be reported as one: the
     * caller has to see HL_STATUS_WOULD_BLOCK to know it never enqueued, and the interruption stays
     * recorded for the block that does happen. */
    if ((status == HL_STATUS_OK || status == HL_STATUS_TIMED_OUT) &&
        __atomic_exchange_n(&slot->pending, 0, __ATOMIC_SEQ_CST) != 0)
        status = HL_STATUS_INTERRUPTED;
    return hl_sync_result(status, 0, 0);
}

hl_host_result hl_host_sync_unpark(hl_host_sync_registry *registry, uint32_t scope, uint64_t key, const void *address,
                                   uint32_t count) {
    uint64_t released = 0;
    hl_status status;
    (void)key;
    if (registry == NULL || address == NULL || (scope != HL_HOST_PARK_PRIVATE && scope != HL_HOST_PARK_SHARED))
        return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = hl_park_wake(scope, address, count, &released);
    return hl_sync_result(status, released, 0);
}

hl_host_result hl_host_sync_interrupt_park(hl_host_sync_registry *registry, uint64_t waiter) {
    uint32_t attempt;
    if (registry == NULL || waiter == 0) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (attempt = 0; attempt < 8u; ++attempt) {
        hl_park_slot *slot = hl_park_claim(registry, waiter);
        uint64_t published;
        uint32_t scope;
        if (slot == NULL) return hl_sync_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        __atomic_store_n(&slot->pending, 1, __ATOMIC_SEQ_CST);
        /* If this record was folded away while the interruption was being written, the fold moved
         * whatever it held at the time and may have missed this. Nothing is owed to a record the
         * waiter can no longer find, so write it again where the waiter will look. */
        if (__atomic_load_n(&slot->waiter, __ATOMIC_SEQ_CST) != waiter) continue;
        /* Release the address the waiter published, so a thread already blocked leaves its wait and
         * re-reads the record above. Ordered after the record so the waiter cannot wake, see
         * nothing, and block again. */
        if (__atomic_load_n(&slot->parked, __ATOMIC_SEQ_CST) == 0) return hl_sync_result(HL_STATUS_OK, 0, 0);
        published = __atomic_load_n(&slot->address, __ATOMIC_SEQ_CST);
        scope = __atomic_load_n(&slot->scope, __ATOMIC_SEQ_CST);
        if (published != 0) hl_park_signal(scope, (const void *)(uintptr_t)published);
        return hl_sync_result(HL_STATUS_OK, 0, 0);
    }
    return hl_sync_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

hl_host_result hl_host_sync_fork_prepare(hl_host_sync_registry *registry) {
    if (registry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (hl_sync_lock_acquire(&registry->lock) != 0) return hl_sync_result(HL_STATUS_PLATFORM_FAILURE, 0, errno);
    return hl_sync_result(HL_STATUS_OK, 0, 0);
}

hl_host_result hl_host_sync_fork_complete(hl_host_sync_registry *registry) {
    if (registry == NULL) return hl_sync_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_sync_result(hl_sync_status(hl_sync_lock_release(&registry->lock)), 0, 0);
}
