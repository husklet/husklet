#include "logical_vma.h"

#include "fdhandle.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#if !defined(_WIN32)
#include <sys/mman.h>
#endif

/* The backing's private reference to the object the guest descriptor named.  At
 * most one member is live: a duplicate descriptor where a descriptor IS the
 * object, a duplicate host handle where it is not. */
typedef struct logical_backing_ref {
    int fd;
    hl_host_handle handle;
} logical_backing_ref;

struct hl_logical_vma_backing {
    void *mapping;
    size_t length;
    _Atomic size_t references;
    uint64_t device;
    uint64_t inode;
    uint64_t file_offset;
    logical_backing_ref retained;
    int writable;
    int borrowed;
};

struct hl_logical_vma_plan {
    hl_logical_vma_ledger *live;
    hl_logical_vma_ledger staged;
};

static hl_logical_vma_ledger g_logical_vmas;
static pthread_once_t g_logical_vmas_once = PTHREAD_ONCE_INIT;
static _Atomic int g_fail_next_prepare;

/* The reference a guest descriptor denotes: the descriptor itself where a descriptor IS the object, and the
 * handle filed under it where it is not.  A descriptor with no binding on such a host fails here rather than
 * later, because the number denotes nothing at all and the caller must unwind rather than map something
 * else. */
static int logical_backing_ref_open(int fd, logical_backing_ref *reference) {
    reference->fd = -1;
    reference->handle = HL_HOST_HANDLE_INVALID;
#if defined(_WIN32)
    if (!hl_fdhandle_lookup(fd, &reference->handle)) {
        errno = ENOSYS;
        return -1;
    }
#else
    reference->fd = fd;
#endif
    return 0;
}

/* The canonical host backing behind a shared logical mapping.  This is the one operation in this file that
 * the typed memory seam ought to own -- map_file() is its exact shape, down to the offset and the shared
 * flag -- and the single thing that used to be in the way is that the whole logical-VMA API was keyed on a
 * NATIVE descriptor: the guest mmap path hands down the host fd that the Linux fd table holds, while the
 * seam speaks in opaque handles.  The descriptor -> host-handle table (fdhandle.h) is what closes that, and
 * keeping both halves behind these calls is what made it the edit it is.
 *
 * A descriptor with no binding still reports its absence through the same NULL-and-errno channel a refused
 * mmap already uses; every caller below unwinds on that, so no half-built backing and no unbacked ledger
 * entry can result. */
static void *logical_backing_map(const logical_backing_ref *reference, size_t length, uint64_t offset, int writable) {
#if defined(_WIN32)
    const hl_host_services *host = hl_fdhandle_host();
    hl_host_file_mapping mapping = {HL_HOST_FILE_MAPPING_ABI, sizeof(mapping), 0, 0, 0, 0};
    hl_host_result result;
    if (reference->handle == HL_HOST_HANDLE_INVALID || host == NULL || host->memory == NULL ||
        host->memory->map_file == NULL) {
        errno = ENOSYS;
        return NULL;
    }
    /* Canonical storage is engine-private and RW; SHARED because the whole point of this backing is that
       every logical view of the same file region observes one another's stores. */
    result = host->memory->map_file(host->context, reference->handle, 0, offset, (uint64_t)length,
                                    HL_HOST_MEMORY_READ | (writable ? HL_HOST_MEMORY_WRITE : 0u), HL_HOST_MEMORY_SHARED,
                                    &mapping);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return NULL;
    }
    /* The ledger keeps an address, not a token, and unmaps by address below; a live ownership handle over
       this range would make that address-keyed release refuse. */
    if (host->memory->discard != NULL) (void)host->memory->discard(host->context, mapping.handle);
    return (void *)(uintptr_t)mapping.address;
#else
    /* Canonical storage is engine-private and RW. Guest permission checks stay
       in the logical entry and never depend on host VMA protection. */
    void *mapping =
        mmap(NULL, length, PROT_READ | (writable ? PROT_WRITE : 0), MAP_SHARED, reference->fd, (off_t)offset);
    return mapping == MAP_FAILED ? NULL : mapping;
#endif
}

static void logical_backing_unmap(void *mapping, size_t length) {
#if defined(_WIN32)
    const hl_host_services *host = hl_fdhandle_host();
    if (host == NULL || host->memory == NULL || host->memory->unmap_address == NULL) return;
    (void)host->memory->unmap_address(host->context, (uint64_t)(uintptr_t)mapping, (uint64_t)length);
#else
    (void)munmap(mapping, length);
#endif
}

/* The backing outlives the guest descriptor it was built from, so it keeps a private duplicate: the guest may
 * close its fd, and a checkpoint re-maps from this one. Close-on-exec is not decoration -- the engine execve's
 * a fresh guest image in place, and an inherited canonical descriptor would be a stray open file the guest
 * descriptor scan cannot account for. Where the reference is a host handle rather than a descriptor that
 * concern does not arise at all: clone_for_fork() produces an independent reference to the same open file
 * description, and a handle is not in the descriptor namespace an exec sweep walks. */
static int logical_backing_retain(const logical_backing_ref *source, logical_backing_ref *reference) {
    reference->fd = -1;
    reference->handle = HL_HOST_HANDLE_INVALID;
#if defined(_WIN32)
    {
        const hl_host_services *host = hl_fdhandle_host();
        hl_host_result cloned;
        if (source->handle == HL_HOST_HANDLE_INVALID || host == NULL || host->file == NULL ||
            host->file->clone_for_fork == NULL) {
            errno = ENOSYS;
            return -1;
        }
        cloned = host->file->clone_for_fork(host->context, source->handle);
        if (cloned.status != HL_STATUS_OK || cloned.value == HL_HOST_HANDLE_INVALID) {
            errno = hl_fdhandle_errno(cloned.status);
            return -1;
        }
        reference->handle = (hl_host_handle)cloned.value;
        return 0;
    }
#else
    {
        int retained = fcntl(source->fd, F_DUPFD, 0);
        if (retained < 0) return -1;
        if (fcntl(retained, F_SETFD, FD_CLOEXEC) != 0) {
            int saved = errno;
            close(retained);
            errno = saved;
            return -1;
        }
        reference->fd = retained;
        return 0;
    }
#endif
}

static void logical_backing_release(logical_backing_ref *reference) {
    if (reference->fd >= 0) {
        close(reference->fd);
        reference->fd = -1;
    }
#if defined(_WIN32)
    if (reference->handle != HL_HOST_HANDLE_INVALID) {
        const hl_host_services *host = hl_fdhandle_host();
        if (host != NULL && host->file != NULL && host->file->close != NULL)
            (void)host->file->close(host->context, reference->handle);
        reference->handle = HL_HOST_HANDLE_INVALID;
    }
#endif
}

static int power_of_two(size_t value) {
    return value != 0 && (value & (value - 1)) == 0;
}

static int grow(hl_logical_vma_ledger *ledger, size_t need) {
    if (need <= ledger->capacity) return 0;
    size_t capacity = ledger->capacity ? ledger->capacity : 8;
    while (capacity < need) {
        if (capacity > SIZE_MAX / 2) {
            errno = ENOMEM;
            return -1;
        }
        capacity *= 2;
    }
    void *entries = realloc(ledger->entries, capacity * sizeof(*ledger->entries));
    if (entries == NULL) return -1;
    ledger->entries = entries;
    ledger->capacity = capacity;
    return 0;
}

static void backing_put(hl_logical_vma_backing *backing) {
    if (atomic_fetch_sub_explicit(&backing->references, 1, memory_order_acq_rel) != 1) return;
    if (!backing->borrowed) logical_backing_unmap(backing->mapping, backing->length);
    logical_backing_release(&backing->retained);
    free(backing);
}

static void backing_get(hl_logical_vma_backing *backing) {
    (void)atomic_fetch_add_explicit(&backing->references, 1, memory_order_relaxed);
}

static int view_compare(const void *left, const void *right) {
    const hl_logical_vma_view *a = left, *b = right;
    if (a->guest_first < b->guest_first) return -1;
    if (a->guest_first > b->guest_first) return 1;
    return 0;
}

static void snapshot_free(hl_logical_vma_snapshot *snapshot) {
    if (snapshot == NULL) return;
    hl_logical_vma_backing **backings = (hl_logical_vma_backing **)(void *)(snapshot->views + snapshot->count);
    for (size_t index = 0; index < snapshot->count; ++index)
        backing_put(backings[index]);
    free(snapshot);
}

static hl_logical_vma_snapshot *snapshot_allocate(size_t capacity) {
    size_t item_size = sizeof(hl_logical_vma_view) + sizeof(hl_logical_vma_backing *);
    if (capacity > (SIZE_MAX - sizeof(hl_logical_vma_snapshot)) / item_size) {
        errno = ENOMEM;
        return NULL;
    }
    size_t bytes = sizeof(hl_logical_vma_snapshot) + capacity * item_size;
    hl_logical_vma_snapshot *snapshot = malloc(bytes);
    if (snapshot == NULL) return NULL;
    snapshot->next = NULL;
    snapshot->count = 0;
    return snapshot;
}

static void publish_locked(hl_logical_vma_ledger *ledger, hl_logical_vma_snapshot *snapshot) {
    snapshot->count = ledger->count;
    hl_logical_vma_backing **backings = (hl_logical_vma_backing **)(void *)(snapshot->views + ledger->count);
    for (size_t index = 0; index < ledger->count; ++index) {
        const hl_logical_vma_entry *entry = &ledger->entries[index];
        uint64_t host_first = (uint64_t)(uintptr_t)entry->backing->mapping + entry->backing_offset;
        snapshot->views[index] = (hl_logical_vma_view){
            entry->guest_first,
            entry->guest_last,
            host_first - entry->guest_first,
            (uint64_t)(uintptr_t)entry->backing,
            entry->backing_offset,
            entry->protection,
            entry->flags,
        };
        backings[index] = entry->backing;
        backing_get(backings[index]);
    }
    qsort(snapshot->views, snapshot->count, sizeof(*snapshot->views), view_compare);
    hl_logical_vma_snapshot *old = atomic_exchange_explicit(&ledger->current, snapshot, memory_order_acq_rel);
    /* Bump AFTER the exchange: a reader samples the generation first, so this
       order lets it see a stale snapshot with a stale generation (harmless --
       it refills) but never a stale snapshot with a fresh one. */
    atomic_fetch_add_explicit(&ledger->generation, 1, memory_order_release);
    if (old != NULL) {
        old->next = ledger->retired;
        ledger->retired = old;
    }
}

static int unmap_locked(hl_logical_vma_ledger *ledger, uint64_t first, uint64_t last) {
    for (size_t index = 0; index < ledger->count;) {
        hl_logical_vma_entry *entry = &ledger->entries[index];
        if (last <= entry->guest_first || first >= entry->guest_last) {
            ++index;
            continue;
        }
        uint64_t old_first = entry->guest_first;
        uint64_t old_last = entry->guest_last;
        if (first <= old_first && last >= old_last) {
            hl_logical_vma_backing *backing = entry->backing;
            ledger->entries[index] = ledger->entries[--ledger->count];
            backing_put(backing);
            continue;
        }
        if (first > old_first && last < old_last) {
            if (grow(ledger, ledger->count + 1) != 0) return -1;
            entry = &ledger->entries[index];
            hl_logical_vma_entry tail = *entry;
            tail.guest_first = last;
            tail.backing_offset += last - old_first;
            backing_get(tail.backing);
            entry->guest_last = first;
            ledger->entries[ledger->count++] = tail;
            ++index;
            continue;
        }
        if (first <= old_first) {
            uint64_t removed = last - old_first;
            entry->guest_first = last;
            entry->backing_offset += removed;
        } else {
            entry->guest_last = first;
        }
        ++index;
    }
    return 0;
}

int hl_logical_vma_init(hl_logical_vma_ledger *ledger) {
    if (ledger == NULL) {
        errno = EINVAL;
        return -1;
    }
    *ledger = (hl_logical_vma_ledger){0};
    atomic_init(&ledger->current, NULL);
    atomic_init(&ledger->generation, 1);
    return pthread_mutex_init(&ledger->lock, NULL);
}

void hl_logical_vma_destroy(hl_logical_vma_ledger *ledger) {
    if (ledger == NULL) return;
    pthread_mutex_lock(&ledger->lock);
    while (ledger->count != 0)
        backing_put(ledger->entries[--ledger->count].backing);
    hl_logical_vma_snapshot *snapshot = atomic_exchange_explicit(&ledger->current, NULL, memory_order_acq_rel);
    atomic_fetch_add_explicit(&ledger->generation, 1, memory_order_release);
    snapshot_free(snapshot);
    while (ledger->retired != NULL) {
        snapshot = ledger->retired;
        ledger->retired = snapshot->next;
        snapshot_free(snapshot);
    }
    free(ledger->entries);
    ledger->entries = NULL;
    ledger->capacity = 0;
    pthread_mutex_unlock(&ledger->lock);
    pthread_mutex_destroy(&ledger->lock);
}

int hl_logical_vma_map_shared(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length, uint32_t protection,
                              int fd, uint64_t file_offset, size_t host_page) {
    if (ledger == NULL || fd < 0 || length == 0 || guest_first > UINT64_MAX - length ||
        file_offset > (uint64_t)INT64_MAX || !power_of_two(host_page) || (guest_first & UINT64_C(4095)) ||
        (file_offset & UINT64_C(4095))) {
        errno = EINVAL;
        return -1;
    }
    struct stat status;
    if (fstat(fd, &status) != 0) return -1;
    size_t head = (size_t)(file_offset & (host_page - 1));
    if (length > SIZE_MAX - head || length + head > SIZE_MAX - (host_page - 1) || file_offset > UINT64_MAX - length) {
        errno = ENOMEM;
        return -1;
    }
    pthread_mutex_lock(&ledger->lock);
    if (grow(ledger, ledger->count + 2) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    hl_logical_vma_snapshot *next_snapshot = snapshot_allocate(ledger->count + 2);
    if (next_snapshot == NULL) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }

    /*
     * Coalesce every transitively overlapping canonical extent of the same
     * vnode.  Mapping transactions are stop-the-world, so entries can be
     * rebound to the union before publishing one immutable snapshot.  This
     * makes separate fds and partially overlapping aliases resolve identical
     * file offsets to identical host addresses, including cross-page atomics.
     */
    uint64_t canonical_first = file_offset - head;
    uint64_t requested_last = file_offset + length;
    if (requested_last > UINT64_MAX - (host_page - 1)) {
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        errno = ENOMEM;
        return -1;
    }
    uint64_t canonical_last = (requested_last + host_page - 1) & ~((uint64_t)host_page - 1);
    int requested_writable = (protection & HL_LOGICAL_VMA_WRITE) != 0;
    int canonical_writable = requested_writable;
    logical_backing_ref mapping_ref;
    int expanded;
    if (logical_backing_ref_open(fd, &mapping_ref) != 0) {
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    do {
        expanded = 0;
        for (size_t index = 0; index < ledger->count; ++index) {
            hl_logical_vma_backing *candidate = ledger->entries[index].backing;
            if (candidate->device != (uint64_t)status.st_dev || candidate->inode != (uint64_t)status.st_ino) continue;
            uint64_t candidate_last = candidate->file_offset + candidate->length;
            if (candidate_last < canonical_first || candidate->file_offset > canonical_last) continue;
            if (candidate->writable) {
                canonical_writable = 1;
                /* A read-only request widening onto a writable backing has to map through a writable
                   reference, and the candidate's retained one is the only one in reach. */
                if (!requested_writable) mapping_ref = candidate->retained;
            }
            if (candidate->file_offset < canonical_first) {
                canonical_first = candidate->file_offset;
                expanded = 1;
            }
            if (candidate_last > canonical_last) {
                canonical_last = candidate_last;
                expanded = 1;
            }
        }
    } while (expanded);
    if (canonical_last <= canonical_first || canonical_last - canonical_first > SIZE_MAX ||
        canonical_first > (uint64_t)INT64_MAX) {
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        errno = ENOMEM;
        return -1;
    }
    size_t mapped_length = (size_t)(canonical_last - canonical_first);
    void *mapping = logical_backing_map(&mapping_ref, mapped_length, canonical_first, canonical_writable);
    if (mapping == NULL) {
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    hl_logical_vma_backing *backing = malloc(sizeof(*backing));
    if (backing == NULL) {
        int saved = errno;
        logical_backing_unmap(mapping, mapped_length);
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        errno = saved;
        return -1;
    }
    logical_backing_ref retained;
    if (logical_backing_retain(&mapping_ref, &retained) != 0) {
        int saved = errno;
        logical_backing_unmap(mapping, mapped_length);
        free(backing);
        free(next_snapshot);
        pthread_mutex_unlock(&ledger->lock);
        errno = saved;
        return -1;
    }
    *backing = (hl_logical_vma_backing){
        mapping,  mapped_length,      1, (uint64_t)status.st_dev, (uint64_t)status.st_ino, canonical_first,
        retained, canonical_writable, 0};
    for (size_t index = 0; index < ledger->count; ++index) {
        hl_logical_vma_entry *entry = &ledger->entries[index];
        hl_logical_vma_backing *old = entry->backing;
        if (old->device != backing->device || old->inode != backing->inode) continue;
        uint64_t old_last = old->file_offset + old->length;
        if (old_last < canonical_first || old->file_offset > canonical_last) continue;
        uint64_t absolute = old->file_offset + entry->backing_offset;
        entry->backing = backing;
        entry->backing_offset = absolute - canonical_first;
        backing_get(backing);
        backing_put(old);
    }

    uint64_t last = guest_first + length;
    (void)unmap_locked(ledger, guest_first, last);
    backing_get(backing);
    ledger->entries[ledger->count++] = (hl_logical_vma_entry){
        guest_first, last, file_offset - canonical_first, protection, HL_LOGICAL_VMA_SHARED, backing};
    publish_locked(ledger, next_snapshot);
    backing_put(backing); /* drop construction reference */
    pthread_mutex_unlock(&ledger->lock);
    return 0;
}

int hl_logical_vma_prepare_shared(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length,
                                  uint32_t protection, int fd, uint64_t file_offset, size_t host_page,
                                  hl_logical_vma_plan **result) {
    if (ledger == NULL || result == NULL) {
        errno = EINVAL;
        return -1;
    }
    *result = NULL;
    if (atomic_exchange_explicit(&g_fail_next_prepare, 0, memory_order_acq_rel)) {
        errno = ENOMEM;
        return -1;
    }
    hl_logical_vma_plan *plan = calloc(1, sizeof(*plan));
    if (plan == NULL) return -1;
    if (hl_logical_vma_init(&plan->staged) != 0) {
        free(plan);
        return -1;
    }
    plan->live = ledger;
    pthread_mutex_lock(&ledger->lock);
    if (grow(&plan->staged, ledger->count) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        hl_logical_vma_destroy(&plan->staged);
        free(plan);
        return -1;
    }
    for (size_t index = 0; index < ledger->count; ++index) {
        plan->staged.entries[index] = ledger->entries[index];
        backing_get(plan->staged.entries[index].backing);
    }
    plan->staged.count = ledger->count;
    if (hl_logical_vma_map_shared(&plan->staged, guest_first, length, protection, fd, file_offset, host_page) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        hl_logical_vma_destroy(&plan->staged);
        free(plan);
        return -1;
    }
    *result = plan;
    return 0;
}

void hl_logical_vma_commit_shared(hl_logical_vma_plan *plan) {
    if (plan == NULL) return;
    hl_logical_vma_ledger *live = plan->live;
    while (live->count != 0)
        backing_put(live->entries[--live->count].backing);
    free(live->entries);
    live->entries = plan->staged.entries;
    live->count = plan->staged.count;
    live->capacity = plan->staged.capacity;
    plan->staged.entries = NULL;
    plan->staged.count = plan->staged.capacity = 0;

    hl_logical_vma_snapshot *next = atomic_exchange_explicit(&plan->staged.current, NULL, memory_order_acq_rel);
    hl_logical_vma_snapshot *old = atomic_exchange_explicit(&live->current, next, memory_order_acq_rel);
    atomic_fetch_add_explicit(&live->generation, 1, memory_order_release); /* publish_locked's rule, applied here */
    if (old != NULL) {
        old->next = live->retired;
        live->retired = old;
    }
    /* A staged ledger begins without a snapshot, so normally has no retired
       views. Keep this splice exact if that invariant changes later. */
    while (plan->staged.retired != NULL) {
        hl_logical_vma_snapshot *retired = plan->staged.retired;
        plan->staged.retired = retired->next;
        retired->next = live->retired;
        live->retired = retired;
    }
    pthread_mutex_destroy(&plan->staged.lock);
    pthread_mutex_unlock(&live->lock);
    free(plan);
}

void hl_logical_vma_abort_shared(hl_logical_vma_plan *plan) {
    if (plan == NULL) return;
    pthread_mutex_unlock(&plan->live->lock);
    hl_logical_vma_destroy(&plan->staged);
    free(plan);
}

int hl_logical_vma_unmap(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length) {
    if (ledger == NULL || guest_first > UINT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }
    if (length == 0) return 0;
    pthread_mutex_lock(&ledger->lock);
    if (grow(ledger, ledger->count + 1) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    hl_logical_vma_snapshot *next_snapshot = snapshot_allocate(ledger->count + 1);
    if (next_snapshot == NULL) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    int result = unmap_locked(ledger, guest_first, guest_first + length);
    publish_locked(ledger, next_snapshot);
    pthread_mutex_unlock(&ledger->lock);
    return result;
}

int hl_logical_vma_protect(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length, uint32_t protection) {
    if (ledger == NULL || length == 0 || guest_first > UINT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }
    uint64_t last = guest_first + length;
    pthread_mutex_lock(&ledger->lock);
    if (grow(ledger, ledger->count + 2) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    hl_logical_vma_snapshot *next_snapshot = snapshot_allocate(ledger->count + 2);
    if (next_snapshot == NULL) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    size_t original_count = ledger->count;
    for (size_t index = 0; index < original_count; ++index) {
        hl_logical_vma_entry *entry = &ledger->entries[index];
        uint64_t old_first = entry->guest_first, old_last = entry->guest_last;
        if (last <= old_first || guest_first >= old_last) continue;
        uint64_t overlap_first = guest_first > old_first ? guest_first : old_first;
        uint64_t overlap_last = last < old_last ? last : old_last;
        if (overlap_first == old_first && overlap_last == old_last) {
            entry->protection = protection;
            continue;
        }
        hl_logical_vma_entry original = *entry;
        if (overlap_first > old_first) {
            entry->guest_last = overlap_first;
            hl_logical_vma_entry middle = original;
            middle.guest_first = overlap_first;
            middle.guest_last = overlap_last;
            middle.backing_offset += overlap_first - old_first;
            middle.protection = protection;
            backing_get(middle.backing);
            ledger->entries[ledger->count++] = middle;
            if (overlap_last < old_last) {
                hl_logical_vma_entry tail = original;
                tail.guest_first = overlap_last;
                tail.backing_offset += overlap_last - old_first;
                backing_get(tail.backing);
                ledger->entries[ledger->count++] = tail;
            }
        } else {
            entry->guest_last = overlap_last;
            entry->protection = protection;
            if (overlap_last < old_last) {
                hl_logical_vma_entry tail = original;
                tail.guest_first = overlap_last;
                tail.backing_offset += overlap_last - old_first;
                backing_get(tail.backing);
                ledger->entries[ledger->count++] = tail;
            }
        }
    }
    publish_locked(ledger, next_snapshot);
    pthread_mutex_unlock(&ledger->lock);
    return 0;
}

int hl_logical_vma_prepare_protect(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length,
                                   uint32_t protection, hl_logical_vma_plan **result) {
    if (ledger == NULL || result == NULL) {
        errno = EINVAL;
        return -1;
    }
    *result = NULL;
    hl_logical_vma_plan *plan = calloc(1, sizeof(*plan));
    if (plan == NULL) return -1;
    if (hl_logical_vma_init(&plan->staged) != 0) {
        free(plan);
        return -1;
    }
    plan->live = ledger;
    pthread_mutex_lock(&ledger->lock);
    if (grow(&plan->staged, ledger->count) != 0) goto fail;
    for (size_t index = 0; index < ledger->count; ++index) {
        plan->staged.entries[index] = ledger->entries[index];
        backing_get(plan->staged.entries[index].backing);
    }
    plan->staged.count = ledger->count;
    if (hl_logical_vma_protect(&plan->staged, guest_first, length, protection) != 0) goto fail;
    *result = plan;
    return 0;
fail:
    pthread_mutex_unlock(&ledger->lock);
    hl_logical_vma_destroy(&plan->staged);
    free(plan);
    return -1;
}

int hl_logical_vma_resolve(hl_logical_vma_ledger *ledger, uint64_t guest_address, size_t length,
                           uint32_t required_protection, hl_logical_vma_resolution *resolution) {
    if (ledger == NULL || resolution == NULL || guest_address > UINT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(&ledger->current, memory_order_acquire);
    size_t low = 0, high = snapshot != NULL ? snapshot->count : 0;
    while (low < high) {
        size_t middle = low + (high - low) / 2;
        const hl_logical_vma_view *candidate = &snapshot->views[middle];
        if (guest_address < candidate->guest_first)
            high = middle;
        else if (guest_address >= candidate->guest_last)
            low = middle + 1;
        else {
            const hl_logical_vma_view *entry = candidate;
            if ((entry->protection & required_protection) != required_protection) {
                errno = EACCES;
                return -1;
            }
            uint64_t available = entry->guest_last - guest_address;
            if ((uint64_t)length > available) {
                errno = EFAULT;
                return -1;
            }
            *resolution = (hl_logical_vma_resolution){(void *)(uintptr_t)(guest_address + entry->host_delta),
                                                      (size_t)available, entry->protection, entry->flags};
            return 1;
        }
    }
    return 0;
}

void hl_logical_vma_reclaim_quiescent(hl_logical_vma_ledger *ledger) {
    if (ledger == NULL) return;
    pthread_mutex_lock(&ledger->lock);
    hl_logical_vma_snapshot *retired = ledger->retired;
    ledger->retired = NULL;
    pthread_mutex_unlock(&ledger->lock);
    while (retired != NULL) {
        hl_logical_vma_snapshot *next = retired->next;
        snapshot_free(retired);
        retired = next;
    }
}

size_t hl_logical_vma_count(hl_logical_vma_ledger *ledger) {
    if (ledger == NULL) return 0;
    pthread_mutex_lock(&ledger->lock);
    size_t count = ledger->count;
    pthread_mutex_unlock(&ledger->lock);
    return count;
}

static void global_init(void) {
    if (hl_logical_vma_init(&g_logical_vmas) != 0) abort();
}

int hl_logical_vma_global_map_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                     uint64_t file_offset, size_t host_page) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return hl_logical_vma_map_shared(&g_logical_vmas, guest_first, length, protection, fd, file_offset, host_page);
}

static int logical_vma_map_direct(hl_logical_vma_ledger *ledger, uint64_t guest_first, uint64_t length,
                                  uint32_t protection, uint64_t host_first) {
    if (length == 0 || guest_first > UINT64_MAX - length || host_first > UINTPTR_MAX ||
        length > UINTPTR_MAX - host_first) {
        errno = EINVAL;
        return -1;
    }
    pthread_mutex_lock(&ledger->lock);
    if (grow(ledger, ledger->count + 1) != 0) {
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    hl_logical_vma_snapshot *next = snapshot_allocate(ledger->count + 1);
    hl_logical_vma_backing *backing = calloc(1, sizeof(*backing));
    if (next == NULL || backing == NULL) {
        free(next);
        free(backing);
        pthread_mutex_unlock(&ledger->lock);
        return -1;
    }
    backing->mapping = (void *)(uintptr_t)host_first;
    backing->length = (size_t)length;
    atomic_init(&backing->references, 1);
    backing->retained.fd = -1;
    backing->retained.handle = HL_HOST_HANDLE_INVALID;
    backing->borrowed = 1;
    uint64_t last = guest_first + length;
    (void)unmap_locked(ledger, guest_first, last);
    backing_get(backing);
    ledger->entries[ledger->count++] =
        (hl_logical_vma_entry){guest_first, last, 0, protection, HL_LOGICAL_VMA_DIRECT, backing};
    publish_locked(ledger, next);
    backing_put(backing);
    pthread_mutex_unlock(&ledger->lock);
    return 0;
}

int hl_logical_vma_global_map_direct(uint64_t guest_first, uint64_t length, uint32_t protection, uint64_t host_first) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return logical_vma_map_direct(&g_logical_vmas, guest_first, length, protection, host_first);
}

int hl_logical_vma_global_restore_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                         uint64_t file_offset, size_t host_page) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    uint32_t construction = protection | HL_LOGICAL_VMA_WRITE;
    if (hl_logical_vma_map_shared(&g_logical_vmas, guest_first, length, construction, fd, file_offset, host_page) != 0)
        return -1;
    if (construction == protection) return 0;
    return hl_logical_vma_protect(&g_logical_vmas, guest_first, length, protection);
}

int hl_logical_vma_global_prepare_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                         uint64_t file_offset, size_t host_page, hl_logical_vma_plan **plan) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return hl_logical_vma_prepare_shared(&g_logical_vmas, guest_first, length, protection, fd, file_offset, host_page,
                                         plan);
}

int hl_logical_vma_global_unmap(uint64_t guest_first, uint64_t length) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return hl_logical_vma_unmap(&g_logical_vmas, guest_first, length);
}

int hl_logical_vma_global_protect(uint64_t guest_first, uint64_t length, uint32_t protection) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return hl_logical_vma_protect(&g_logical_vmas, guest_first, length, protection);
}

int hl_logical_vma_global_prepare_protect(uint64_t guest_first, uint64_t length, uint32_t protection,
                                          hl_logical_vma_plan **plan) {
    if (plan == NULL) {
        errno = EINVAL;
        return -1;
    }
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (*plan != NULL) {
        if ((*plan)->live != &g_logical_vmas) {
            errno = EINVAL;
            return -1;
        }
        return hl_logical_vma_protect(&(*plan)->staged, guest_first, length, protection);
    }
    return hl_logical_vma_prepare_protect(&g_logical_vmas, guest_first, length, protection, plan);
}

int hl_logical_vma_global_prepare_direct(uint64_t guest_first, uint64_t length, uint32_t protection,
                                         uint64_t host_first, hl_logical_vma_plan **result) {
    if (result == NULL) {
        errno = EINVAL;
        return -1;
    }
    (void)pthread_once(&g_logical_vmas_once, global_init);
    hl_logical_vma_plan *plan = *result;
    if (plan == NULL) {
        plan = calloc(1, sizeof(*plan));
        if (plan == NULL) return -1;
        if (hl_logical_vma_init(&plan->staged) != 0) {
            free(plan);
            return -1;
        }
        plan->live = &g_logical_vmas;
        pthread_mutex_lock(&g_logical_vmas.lock);
        if (grow(&plan->staged, g_logical_vmas.count) != 0) goto fail;
        for (size_t index = 0; index < g_logical_vmas.count; ++index) {
            plan->staged.entries[index] = g_logical_vmas.entries[index];
            backing_get(plan->staged.entries[index].backing);
        }
        plan->staged.count = g_logical_vmas.count;
        *result = plan;
    } else if (plan->live != &g_logical_vmas) {
        errno = EINVAL;
        return -1;
    }
    return logical_vma_map_direct(&plan->staged, guest_first, length, protection, host_first);
fail:
    pthread_mutex_unlock(&g_logical_vmas.lock);
    hl_logical_vma_destroy(&plan->staged);
    free(plan);
    return -1;
}

int hl_logical_vma_global_overlap(uint64_t guest_first, uint64_t length) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (length == 0 || guest_first > UINT64_MAX - length) return 0;
    uint64_t last = guest_first + length;
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(&g_logical_vmas.current, memory_order_acquire);
    if (snapshot == NULL) return 0;
    for (size_t index = 0; index < snapshot->count; ++index) {
        const hl_logical_vma_view *entry = &snapshot->views[index];
        if (entry->guest_first >= last) break;
        if (entry->guest_last > guest_first) return 1;
    }
    return 0;
}

void hl_logical_vma_global_reclaim_quiescent(void) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    hl_logical_vma_reclaim_quiescent(&g_logical_vmas);
}

void hl_logical_vma_global_reset_quiescent(void) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    hl_logical_vma_snapshot *next_snapshot = snapshot_allocate(0);
    if (next_snapshot == NULL) abort();
    while (g_logical_vmas.count != 0)
        backing_put(g_logical_vmas.entries[--g_logical_vmas.count].backing);
    publish_locked(&g_logical_vmas, next_snapshot);
    pthread_mutex_unlock(&g_logical_vmas.lock);
    hl_logical_vma_reclaim_quiescent(&g_logical_vmas);
}

int hl_logical_vma_resolve_exec(uint64_t guest, size_t length, const void **host, size_t *contiguous) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (host == NULL || contiguous == NULL) {
        errno = EINVAL;
        return -1;
    }
    hl_logical_vma_resolution resolution;
    int result = hl_logical_vma_resolve(&g_logical_vmas, guest, length, HL_LOGICAL_VMA_EXEC, &resolution);
    if (result == 1) {
        *host = resolution.host;
        *contiguous = resolution.contiguous;
    }
    return result;
}

int hl_logical_vma_resolve_exec_span(uint64_t guest, uint64_t *generation, uint64_t *first, uint64_t *last,
                                     uint64_t *delta) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (generation == NULL || first == NULL || last == NULL || delta == NULL) {
        errno = EINVAL;
        return -1;
    }
    *generation = atomic_load_explicit(&g_logical_vmas.generation, memory_order_acquire);
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(&g_logical_vmas.current, memory_order_acquire);
    size_t count = snapshot != NULL ? snapshot->count : 0;
    size_t low = 0, high = count;
    while (low < high) {
        size_t middle = low + (high - low) / 2;
        const hl_logical_vma_view *candidate = &snapshot->views[middle];
        if (guest < candidate->guest_first)
            high = middle;
        else if (guest >= candidate->guest_last)
            low = middle + 1;
        else {
            if ((candidate->protection & HL_LOGICAL_VMA_EXEC) == 0) {
                errno = EACCES;
                return -1;
            }
            *first = candidate->guest_first;
            *last = candidate->guest_last;
            *delta = candidate->host_delta;
            return 1;
        }
    }
    /* Miss: views are sorted and disjoint, so `low` is the first view past
       `guest` and low-1 the last one before it.  The gap between them is
       ordinary for its whole width. */
    *first = low != 0 ? snapshot->views[low - 1].guest_last : 0;
    *last = low != count ? snapshot->views[low].guest_first : UINT64_MAX;
    *delta = 0;
    return 0;
}

const _Atomic uint64_t *hl_logical_vma_global_exec_generation(void) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return &g_logical_vmas.generation;
}

int hl_logical_vma_resolve_data(uint64_t guest, size_t length, uint32_t required, void **host, size_t *contiguous) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (host == NULL || contiguous == NULL || length == 0) {
        errno = EINVAL;
        return -1;
    }
    hl_logical_vma_resolution resolution;
    /*
     * Resolve the first byte and report the VMA boundary. A caller whose
     * access exceeds contiguous must split or take its architectural slow
     * path; adjacent guest VMAs need not be adjacent canonical host storage.
     */
    int result = hl_logical_vma_resolve(&g_logical_vmas, guest, 1, required, &resolution);
    if (result == 1) {
        *host = resolution.host;
        *contiguous = resolution.contiguous;
    }
    return result;
}

int hl_logical_vma_pin_data(uint64_t guest, size_t length, uint32_t required, hl_logical_vma_pin *pin) {
    if (pin == NULL || length == 0 || guest > UINT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }
    *pin = (hl_logical_vma_pin){0};
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    for (size_t index = 0; index < g_logical_vmas.count; ++index) {
        hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
        if (guest < entry->guest_first || guest >= entry->guest_last) continue;
        if ((entry->protection & required) != required) {
            pthread_mutex_unlock(&g_logical_vmas.lock);
            errno = EACCES;
            return -1;
        }
        uint64_t available = entry->guest_last - guest;
        size_t contiguous = available > SIZE_MAX ? SIZE_MAX : (size_t)available;
        if (contiguous > length) contiguous = length;
        backing_get(entry->backing);
        pin->host = (char *)entry->backing->mapping + entry->backing_offset + (guest - entry->guest_first);
        pin->contiguous = contiguous;
        pin->protection = entry->protection;
        pin->flags = entry->flags;
        pin->token = entry->backing;
        pthread_mutex_unlock(&g_logical_vmas.lock);
        return 1;
    }
    size_t contiguous = length;
    for (size_t index = 0; index < g_logical_vmas.count; ++index) {
        uint64_t next = g_logical_vmas.entries[index].guest_first;
        if (next > guest && next - guest < contiguous) contiguous = (size_t)(next - guest);
    }
    pin->host = (void *)(uintptr_t)guest;
    pin->contiguous = contiguous;
    pin->protection = HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE | HL_LOGICAL_VMA_EXEC;
    pthread_mutex_unlock(&g_logical_vmas.lock);
    return 0;
}

void hl_logical_vma_unpin(hl_logical_vma_pin *pin) {
    if (pin == NULL || pin->token == NULL) return;
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    backing_put((hl_logical_vma_backing *)pin->token);
    pthread_mutex_unlock(&g_logical_vmas.lock);
    *pin = (hl_logical_vma_pin){0};
}

void hl_logical_vma_visit_exec_aliases_pin(const hl_logical_vma_pin *pin, size_t offset, size_t length,
                                           hl_logical_vma_alias_visitor visitor, void *opaque) {
    if (pin == NULL || pin->token == NULL || visitor == NULL || length == 0 || offset > pin->contiguous ||
        length > pin->contiguous - offset)
        return;

    struct alias_range {
        uint64_t first, last;
    };
    struct alias_range *ranges = NULL;
    size_t count = 0, capacity = 0;
    int overflow = 0;
    hl_logical_vma_backing *backing = pin->token;
    uint64_t source_offset = (uint64_t)((char *)pin->host - (char *)backing->mapping) + offset;
    uint64_t source_last = source_offset + length;
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    for (size_t index = 0; index < g_logical_vmas.count; ++index) {
        const hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
        if (entry->backing != backing || !(entry->protection & HL_LOGICAL_VMA_EXEC)) continue;
        uint64_t alias_first = entry->backing_offset;
        uint64_t alias_last = alias_first + (entry->guest_last - entry->guest_first);
        uint64_t first = source_offset > alias_first ? source_offset : alias_first;
        uint64_t last = source_last < alias_last ? source_last : alias_last;
        if (last <= first) continue;
        if (count == capacity) {
            size_t next = capacity ? capacity * 2 : 8;
            if (next < capacity || next > SIZE_MAX / sizeof(*ranges)) {
                overflow = 1;
                break;
            }
            void *grown = realloc(ranges, next * sizeof(*ranges));
            if (grown == NULL) {
                overflow = 1;
                break;
            }
            ranges = grown;
            capacity = next;
        }
        uint64_t guest = entry->guest_first + (first - alias_first);
        ranges[count++] = (struct alias_range){guest, guest + (last - first)};
    }
    pthread_mutex_unlock(&g_logical_vmas.lock);
    if (overflow)
        visitor(0, UINT64_MAX, opaque);
    else
        for (size_t index = 0; index < count; ++index)
            visitor(ranges[index].first, ranges[index].last, opaque);
    free(ranges);
}

int hl_logical_vma_global_active(void) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(&g_logical_vmas.current, memory_order_acquire);
    return snapshot != NULL && snapshot->count != 0;
}

size_t hl_logical_vma_global_export(hl_logical_vma_descriptor *descriptors, size_t capacity) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    if (descriptors == NULL) capacity = 0;
    pthread_mutex_lock(&g_logical_vmas.lock);
    size_t count = 0;
    for (size_t index = 0; index < g_logical_vmas.count; ++index)
        if ((g_logical_vmas.entries[index].flags & HL_LOGICAL_VMA_DIRECT) == 0) ++count;
    size_t copied = 0;
    for (size_t index = 0; index < g_logical_vmas.count && copied < capacity; ++index) {
        const hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
        if (entry->flags & HL_LOGICAL_VMA_DIRECT) continue;
        descriptors[copied++] = (hl_logical_vma_descriptor){
            entry->guest_first,
            entry->guest_last - entry->guest_first,
            entry->backing->device,
            entry->backing->inode,
            entry->backing->file_offset + entry->backing_offset,
            entry->protection,
            entry->flags,
        };
    }
    pthread_mutex_unlock(&g_logical_vmas.lock);
    return count;
}

int hl_logical_vma_global_describe(uint64_t guest, hl_logical_vma_descriptor *descriptor) {
    if (descriptor == NULL) {
        errno = EINVAL;
        return -1;
    }
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    for (size_t index = 0; index < g_logical_vmas.count; ++index) {
        const hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
        if (guest < entry->guest_first || guest >= entry->guest_last) continue;
        if (entry->flags & HL_LOGICAL_VMA_DIRECT) {
            pthread_mutex_unlock(&g_logical_vmas.lock);
            return 0;
        }
        *descriptor = (hl_logical_vma_descriptor){
            entry->guest_first,
            entry->guest_last - entry->guest_first,
            entry->backing->device,
            entry->backing->inode,
            entry->backing->file_offset + entry->backing_offset,
            entry->protection,
            entry->flags,
        };
        pthread_mutex_unlock(&g_logical_vmas.lock);
        return 1;
    }
    pthread_mutex_unlock(&g_logical_vmas.lock);
    return 0;
}

static int global_copy(uint64_t guest, void *buffer, size_t length, int into_backing) {
    if ((buffer == NULL && length != 0) || guest > UINT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    size_t done = 0;
    while (done != length) {
        hl_logical_vma_entry *found = NULL;
        uint64_t address = guest + done;
        for (size_t index = 0; index < g_logical_vmas.count; ++index) {
            hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
            if (address >= entry->guest_first && address < entry->guest_last) {
                found = entry;
                break;
            }
        }
        if (found == NULL) {
            pthread_mutex_unlock(&g_logical_vmas.lock);
            errno = EFAULT;
            return -1;
        }
        uint64_t available64 = found->guest_last - address;
        size_t chunk = available64 > SIZE_MAX ? SIZE_MAX : (size_t)available64;
        if (chunk > length - done) chunk = length - done;
        void *canonical = (char *)found->backing->mapping + found->backing_offset + (address - found->guest_first);
        if (into_backing)
            memcpy(canonical, (const char *)buffer + done, chunk);
        else
            memcpy((char *)buffer + done, canonical, chunk);
        done += chunk;
    }
    pthread_mutex_unlock(&g_logical_vmas.lock);
    return 0;
}

int hl_logical_vma_global_copy_out(uint64_t guest, void *destination, size_t length) {
    return global_copy(guest, destination, length, 0);
}

int hl_logical_vma_global_copy_in(uint64_t guest, const void *source, size_t length) {
    return global_copy(guest, (void *)source, length, 1);
}

const _Atomic(hl_logical_vma_snapshot *) *hl_logical_vma_global_snapshot_source(void) {
    (void)pthread_once(&g_logical_vmas_once, global_init);
    return &g_logical_vmas.current;
}

int hl_logical_vma_visit_exec_aliases(uint64_t guest_first, uint64_t guest_last, hl_logical_vma_alias_visitor visitor,
                                      void *opaque) {
    if (visitor == NULL || guest_last <= guest_first) return 0;

    struct alias_range {
        uint64_t first, last;
    };
    struct alias_range *ranges = NULL;
    size_t count = 0, capacity = 0;
    int overflow = 0;
    int matched = 0;
    (void)pthread_once(&g_logical_vmas_once, global_init);
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(&g_logical_vmas.current, memory_order_acquire);
    if (snapshot == NULL) return 0;
    size_t source = 0;
    size_t high = snapshot->count;
    while (source < high) {
        size_t middle = source + (high - source) / 2;
        if (snapshot->views[middle].guest_last <= guest_first)
            source = middle + 1;
        else
            high = middle;
    }
    for (; source < snapshot->count; ++source) {
        const hl_logical_vma_view *sv = &snapshot->views[source];
        if (sv->guest_first >= guest_last) break;
        uint64_t first = guest_first > sv->guest_first ? guest_first : sv->guest_first;
        uint64_t last = guest_last < sv->guest_last ? guest_last : sv->guest_last;
        if (last <= first) continue;
        matched = 1;
        uint64_t source_offset = sv->backing_offset + (first - sv->guest_first);
        uint64_t source_last = source_offset + (last - first);
        for (size_t alias = 0; alias < snapshot->count; ++alias) {
            const hl_logical_vma_view *av = &snapshot->views[alias];
            if (av->backing != sv->backing || !(av->protection & HL_LOGICAL_VMA_EXEC)) continue;
            uint64_t alias_offset = av->backing_offset;
            uint64_t alias_last = alias_offset + (av->guest_last - av->guest_first);
            uint64_t overlap_first = source_offset > alias_offset ? source_offset : alias_offset;
            uint64_t overlap_last = source_last < alias_last ? source_last : alias_last;
            if (overlap_last <= overlap_first) continue;
            uint64_t out_first = av->guest_first + (overlap_first - alias_offset);
            if (count == capacity) {
                size_t next = capacity ? capacity * 2 : 8;
                if (next < capacity || next > SIZE_MAX / sizeof(*ranges)) {
                    overflow = 1;
                    break;
                }
                void *grown = realloc(ranges, next * sizeof(*ranges));
                if (grown == NULL) {
                    overflow = 1;
                    break;
                }
                ranges = grown;
                capacity = next;
            }
            ranges[count++] = (struct alias_range){out_first, out_first + (overlap_last - overlap_first)};
        }
        if (overflow) break;
    }
    if (overflow)
        visitor(0, UINT64_MAX, opaque);
    else
        for (size_t index = 0; index < count; ++index)
            visitor(ranges[index].first, ranges[index].last, opaque);
    free(ranges);
    return matched;
}

int hl_logical_vma_has_exec_alias_file(uint64_t device, uint64_t inode, uint64_t file_offset, size_t length) {
    if (length == 0 || file_offset > UINT64_MAX - length) return 0;
    uint64_t file_last = file_offset + length;
    int found = 0;
    (void)pthread_once(&g_logical_vmas_once, global_init);
    pthread_mutex_lock(&g_logical_vmas.lock);
    for (size_t index = 0; index < g_logical_vmas.count; ++index) {
        const hl_logical_vma_entry *entry = &g_logical_vmas.entries[index];
        if (!(entry->protection & HL_LOGICAL_VMA_EXEC) || entry->backing->device != device ||
            entry->backing->inode != inode)
            continue;
        uint64_t first = entry->backing->file_offset + entry->backing_offset;
        uint64_t last = first + (entry->guest_last - entry->guest_first);
        if (file_offset < last && file_last > first) {
            found = 1;
            break;
        }
    }
    pthread_mutex_unlock(&g_logical_vmas.lock);
    return found;
}

void hl_logical_vma_test_fail_next_prepare(void) {
    atomic_store_explicit(&g_fail_next_prepare, 1, memory_order_release);
}
