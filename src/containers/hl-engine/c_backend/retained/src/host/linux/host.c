#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "hl/linux.h"
#include "probe.h"
#include "../host_cpu.h"
#include "../range.h"
#include "../system.h"
#include "../resolve.h"
#include "../sync.h"

#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/vfs.h>
#include <sys/eventfd.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <netinet/in.h>
#include <netinet/tcp.h> /* the TCP_* option names the network group's table maps onto */
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define HL_LINUX_HANDLE_CAPACITY 4096u
#define HL_LINUX_TIMER_CAPACITY 256u
#define HL_LINUX_COUNTER_SUBSCRIPTIONS_INITIAL 128u

typedef enum hl_linux_handle_kind {
    HL_LINUX_HANDLE_NONE = 0,
    HL_LINUX_HANDLE_MAPPING = 1,
    HL_LINUX_HANDLE_FILE = 2,
    HL_LINUX_HANDLE_SOCKET = 3,
    HL_LINUX_HANDLE_POLLSET = 4,
    HL_LINUX_HANDLE_SHARED_MEMORY = 5,
    HL_LINUX_HANDLE_PROCESS = 6,
    HL_LINUX_HANDLE_COUNTER = 7,
    HL_LINUX_HANDLE_TRANSFER = 8,
    HL_LINUX_HANDLE_DIRECTORY = 9,
    HL_LINUX_HANDLE_WATCH = 10,
    HL_LINUX_HANDLE_STREAM = 11
} hl_linux_handle_kind;

typedef struct hl_linux_handle_entry {
    uint32_t generation;
    uint16_t kind;
    uint16_t reserved;
    int descriptor;
    void *address;
    void *executable_address;
    uint64_t size;
    /* Mapping handles only: the subranges of [address, address + size) a partial unmap gave back.
     * address and size stay the addressing frame every offset-keyed call is measured against, so
     * what the handle still holds has to be recorded beside them rather than folded into them. */
    hl_host_hole_set retired;
    int wake_descriptor;
    uint32_t process_reaped;
    uint32_t process_waiting;
    uint32_t process_waiters;
    uint32_t process_exit_kind;
    uint32_t process_exit_value;
} hl_linux_handle_entry;

typedef struct hl_linux_timer_entry {
    hl_host_handle pollset;
    uint64_t token;
    int descriptor;
} hl_linux_timer_entry;

typedef struct hl_linux_watch {
    int watch_id;
    uint64_t delivered_generation;
    uint64_t modified_ns;
    uint64_t changed_ns;
    nlink_t links;
    hl_host_watch_record record;
} hl_linux_watch;

typedef struct hl_linux_counter_subscription {
    struct hl_host_linux *host;
    uint32_t generation;
    uint32_t active;
    uint32_t retiring;
    hl_host_handle counter;
    int descriptor;
    int wake[2];
    pthread_t thread;
    void (*notify)(void *, uint64_t);
    void *observer;
    uint64_t token;
} hl_linux_counter_subscription;

#define HL_LINUX_DIRECTORY_WATCHES 256u

typedef struct hl_linux_directory_watch {
    int watch;
    uint64_t token;
    uint32_t interests;
    uint32_t active;
} hl_linux_directory_watch;

typedef struct hl_linux_directory_object {
    uint32_t references;
    uint32_t pending_count;
    uint32_t pending_capacity;
    hl_linux_directory_watch *watches;
    uint32_t watch_capacity;
    hl_host_directory_record *pending;
} hl_linux_directory_object;

struct hl_host_linux {
    pthread_mutex_t lock;
    pthread_mutex_t fork_gate;
    pthread_cond_t process_changed;
    uint32_t destroying;
    hl_host_sync_registry *sync;
    hl_linux_handle_entry *handles;
    uint32_t handle_capacity;
    hl_linux_timer_entry *timers;
    uint32_t timer_capacity;
    hl_linux_counter_subscription **counter_subscriptions;
    uint32_t counter_subscription_capacity;
};

uint32_t hl_host_linux_active_mappings(hl_host_linux *host) {
    uint32_t active = 0;
    uint32_t index;
    if (host == NULL) return 0;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->handle_capacity; ++index)
        if (host->handles[index].kind == HL_LINUX_HANDLE_MAPPING) ++active;
    pthread_mutex_unlock(&host->lock);
    return active;
}

static hl_host_result hl_linux_fork_complete(void *context);
static hl_host_result hl_linux_fork_child(void *context);
static hl_host_result hl_linux_counter_unsubscribe(void *context, hl_host_handle subscription);
static hl_host_result hl_linux_close_descriptor(void *context, hl_host_handle handle);
static hl_host_result hl_linux_close_descriptor_kind(void *context, hl_host_handle handle,
                                                     hl_linux_handle_kind expected);
static void hl_linux_counter_unsubscribe_all(hl_host_linux *host, hl_host_handle counter);
static int hl_linux_descriptor(hl_host_linux *host, hl_host_handle handle, hl_linux_handle_kind first,
                               hl_linux_handle_kind second);

static int hl_linux_grow_capacity(uint32_t current, uint32_t initial, size_t element_size, uint32_t *next) {
    uint32_t capacity;
    if (initial == 0 || element_size == 0 || next == NULL) return -1;
    if (current == 0) {
        capacity = initial;
    } else {
        if (current > UINT32_MAX / 2u) return -1;
        capacity = current * 2u;
    }
    if ((size_t)capacity > SIZE_MAX / element_size) return -1;
    *next = capacity;
    return 0;
}

static uint64_t hl_linux_monotonic_value(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static void hl_linux_sleep_until(uint64_t deadline_ns) {
    uint64_t now = hl_linux_monotonic_value();
    struct timespec delay;
    uint64_t remaining;
    if (now >= deadline_ns) return;
    remaining = deadline_ns - now;
    if (remaining > UINT64_C(1000000)) remaining = UINT64_C(1000000);
    delay.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
    delay.tv_nsec = (long)(remaining % UINT64_C(1000000000));
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
}

static void hl_linux_process_changed_wait(hl_host_linux *host, uint64_t deadline_ns) {
    struct timespec realtime;
    uint64_t now;
    uint64_t remaining;
    uint64_t absolute;
    if (deadline_ns == HL_HOST_DEADLINE_INFINITE) {
        pthread_cond_wait(&host->process_changed, &host->lock);
        return;
    }
    now = hl_linux_monotonic_value();
    if (now >= deadline_ns) return;
    remaining = deadline_ns - now;
    clock_gettime(CLOCK_REALTIME, &realtime);
    absolute = (uint64_t)realtime.tv_sec * UINT64_C(1000000000) + (uint64_t)realtime.tv_nsec + remaining;
    realtime.tv_sec = (time_t)(absolute / UINT64_C(1000000000));
    realtime.tv_nsec = (long)(absolute % UINT64_C(1000000000));
    pthread_cond_timedwait(&host->process_changed, &host->lock, &realtime);
}

static hl_host_result hl_linux_result(hl_status status, uint64_t value, uint64_t detail) {
    return (hl_host_result){(int32_t)status, 1, value, detail};
}

static hl_status hl_linux_status_from_errno(int error) {
    switch (error) {
    case 0: return HL_STATUS_OK;
    case EINVAL: return HL_STATUS_INVALID_ARGUMENT;
    case ENOMEM: return HL_STATUS_OUT_OF_MEMORY;
    case EMFILE: return HL_STATUS_PROCESS_LIMIT;
    case ENFILE: return HL_STATUS_RESOURCE_LIMIT;
    case ENOENT: return HL_STATUS_NOT_FOUND;
    case EEXIST: return HL_STATUS_ALREADY_EXISTS;
    case EACCES:
    case EPERM: return HL_STATUS_PERMISSION_DENIED;
    case EAGAIN: return HL_STATUS_WOULD_BLOCK;
    case EINTR: return HL_STATUS_INTERRUPTED;
    case ENOTSUP:
    case ENOSYS: return HL_STATUS_NOT_SUPPORTED;
    case EBUSY: return HL_STATUS_BUSY;
    case ENOTDIR: return HL_STATUS_NOT_DIRECTORY;
    case EISDIR: return HL_STATUS_IS_DIRECTORY;
    case ENAMETOOLONG: return HL_STATUS_NAME_TOO_LONG;
    case ELOOP: return HL_STATUS_SYMLINK_LOOP;
    case EROFS: return HL_STATUS_READ_ONLY;
    case EPIPE: return HL_STATUS_DISCONNECTED;
    case EXDEV: return HL_STATUS_CROSS_DEVICE;
    case ENOTEMPTY: return HL_STATUS_NOT_EMPTY;
    case ENOSPC: return HL_STATUS_NO_SPACE;
    case EDQUOT: return HL_STATUS_QUOTA;
    case EFBIG: return HL_STATUS_FILE_TOO_LARGE;
    case ETIMEDOUT: return HL_STATUS_TIMED_OUT;
    case ECONNREFUSED: return HL_STATUS_CONNECTION_REFUSED;
    case ECONNRESET: return HL_STATUS_CONNECTION_RESET;
    case ENETUNREACH: return HL_STATUS_NETWORK_UNREACHABLE;
    case EADDRINUSE: return HL_STATUS_ADDRESS_IN_USE;
    default: return HL_STATUS_IO;
    }
}

static hl_host_result hl_linux_errno_result(void) {
    const int error = errno;
    return hl_linux_result(hl_linux_status_from_errno(error), 0, (uint64_t)(unsigned int)error);
}

static hl_host_handle hl_linux_encode_handle(uint32_t index, uint32_t generation) {
    return ((uint64_t)generation << 32) | (uint64_t)(index + 1u);
}

static hl_linux_handle_entry *hl_linux_lookup_locked(hl_host_linux *host, hl_host_handle handle,
                                                     hl_linux_handle_kind kind) {
    uint32_t low = (uint32_t)handle;
    uint32_t index;
    uint32_t generation = (uint32_t)(handle >> 32);
    hl_linux_handle_entry *entry;
    if (low == 0) return NULL;
    index = low - 1u;
    if (index >= host->handle_capacity) return NULL;
    entry = &host->handles[index];
    if (entry->generation != generation || entry->kind != kind) return NULL;
    return entry;
}

/* Retire a mapping slot. The frame, the record of what it gave back, and the kind go together:
 * keeping them apart is what once left a handle claiming a hole it had already unmapped. */
static void hl_linux_retire_mapping_locked(hl_linux_handle_entry *entry) {
    hl_host_hole_set_release(&entry->retired);
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->address = NULL;
    entry->executable_address = NULL;
    entry->size = 0;
}

/* True when [low, high) touches a byte this mapping still holds. The frame alone is not the answer,
 * because a partial unmap gives bytes back without consuming the handle. Both aliases of a code
 * mapping count, because releasing either one out from under the owner is the failure the callers
 * of this exist to prevent; only the writable alias is reachable by a subrange unmap, so only it
 * carries holes. */
static inline int hl_linux_entry_holds_locked(const hl_linux_handle_entry *entry, uintptr_t low, uintptr_t high) {
    if (entry->kind != HL_LINUX_HANDLE_MAPPING || entry->size == 0) return 0;
    if (entry->address != NULL) {
        uintptr_t base = (uintptr_t)entry->address;
        uintptr_t end = base + (uintptr_t)entry->size;
        if (low < end && base < high) {
            uint64_t from = low > base ? (uint64_t)(low - base) : 0;
            uint64_t to = high < end ? (uint64_t)(high - base) : entry->size;
            if (hl_host_hole_set_holds(&entry->retired, from, to - from)) return 1;
        }
    }
    if (entry->executable_address != NULL) {
        uintptr_t base = (uintptr_t)entry->executable_address;
        if (low < base + (uintptr_t)entry->size && base < high) return 1;
    }
    return 0;
}

static hl_host_result hl_linux_allocate_handle(hl_host_linux *host, hl_linux_handle_kind kind, int descriptor,
                                               void *address, void *executable_address, uint64_t size,
                                               int wake_descriptor) {
    uint32_t index;
    hl_host_handle handle = 0;
    /* Process handles store a pid in this field, not a descriptor. */
    if (descriptor >= 0 && kind != HL_LINUX_HANDLE_PROCESS) {
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) { return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0); }
        descriptor = adopted;
    }
    if (wake_descriptor >= 0 && kind != HL_LINUX_HANDLE_PROCESS) {
        int adopted = hl_host_process_fd_private_adopt(wake_descriptor);
        if (adopted < 0) {
            hl_host_process_fd_private_remove(descriptor);
            close(descriptor);
            return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        wake_descriptor = adopted;
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->handle_capacity; ++index) {
        hl_linux_handle_entry *entry = &host->handles[index];
        if (entry->kind == HL_LINUX_HANDLE_NONE) {
            /* A reused slot must not inherit the previous tenant's holes. Every mapping retirement
             * already drops them; this is the belt that makes that impossible to get wrong. */
            hl_host_hole_set_release(&entry->retired);
            entry->generation++;
            if (entry->generation == 0) entry->generation = 1;
            entry->kind = (uint16_t)kind;
            entry->descriptor = descriptor;
            entry->address = address;
            entry->executable_address = executable_address;
            entry->size = size;
            entry->wake_descriptor = wake_descriptor;
            handle = hl_linux_encode_handle(index, entry->generation);
            break;
        }
    }
    if (handle == 0) {
        uint32_t capacity =
            host->handle_capacity > (UINT32_MAX - 1u) / 2u ? UINT32_MAX - 1u : host->handle_capacity * 2u;
        hl_linux_handle_entry *grown =
            capacity > host->handle_capacity ? realloc(host->handles, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            for (index = host->handle_capacity; index < capacity; ++index) {
                grown[index] = (hl_linux_handle_entry){0};
                grown[index].descriptor = -1;
                grown[index].wake_descriptor = -1;
            }
            index = host->handle_capacity;
            host->handles = grown;
            host->handle_capacity = capacity;
            hl_linux_handle_entry *entry = &host->handles[index];
            entry->generation = 1;
            entry->kind = (uint16_t)kind;
            entry->descriptor = descriptor;
            entry->address = address;
            entry->executable_address = executable_address;
            entry->size = size;
            entry->wake_descriptor = wake_descriptor;
            handle = hl_linux_encode_handle(index, entry->generation);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == 0) {
        hl_host_process_fd_private_remove(descriptor);
        hl_host_process_fd_private_remove(wake_descriptor);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    return hl_linux_result(HL_STATUS_OK, handle, 0);
}

static int hl_linux_protection(uint32_t flags) {
    int protection = 0;
    if ((flags & HL_HOST_MEMORY_READ) != 0) protection |= PROT_READ;
    if ((flags & HL_HOST_MEMORY_WRITE) != 0) protection |= PROT_WRITE;
    if ((flags & HL_HOST_MEMORY_EXECUTE) != 0) protection |= PROT_EXEC;
    return protection;
}

static hl_host_result hl_linux_memory_reserve(void *context, uint64_t size, uint64_t alignment, uint32_t flags) {
    hl_host_linux *host = context;
    void *address;
    long page = sysconf(_SC_PAGESIZE);
    if (size == 0 || size > SIZE_MAX || page <= 0 || alignment > (uint64_t)page)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    address = mmap(NULL, (size_t)size, hl_linux_protection(flags), MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return hl_linux_errno_result();
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, address, NULL, size, -1);
    if (result.status != HL_STATUS_OK) munmap(address, (size_t)size);
    return result;
}

static hl_host_result hl_linux_memory_protect(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size,
                                              uint32_t flags) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = mprotect((char *)entry->address + offset, (size_t)size, hl_linux_protection(flags));
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_release(void *context, hl_host_handle mapping) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* Unmap what the handle still holds, not the frame. A partial unmap can have given a subrange
     * back, and the address space is free to have handed that subrange to someone else since. With
     * no holes this is the one whole-frame munmap it has always been. */
    {
        uint64_t held_offset;
        uint64_t held_size;
        uint32_t part = 0;
        result = 0;
        while (result == 0 &&
               hl_host_hole_set_held_range(&entry->retired, entry->size, part, &held_offset, &held_size)) {
            result = munmap((char *)entry->address + held_offset, (size_t)held_size);
            ++part;
        }
    }
    if (result == 0 && entry->executable_address != NULL && entry->executable_address != entry->address)
        result = munmap(entry->executable_address, (size_t)entry->size);
    if (result == 0 && entry->descriptor >= 0) {
        // A dual-alias code mapping's fd was privatized (adopted) by hl_linux_memory_reserve_code; drop its
        // private-registry cell before closing (mirrors hl_linux_memory_repair_code). No-op for a plain
        // mapping whose fd was never adopted, so it is safe for every mapping kind.
        hl_host_process_fd_private_remove(entry->descriptor);
        result = close(entry->descriptor);
    }
    if (result == 0) {
        hl_linux_retire_mapping_locked(entry);
        entry->descriptor = -1;
    }
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_discard(void *context, hl_host_handle mapping) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    hl_linux_retire_mapping_locked(entry);
    entry->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static int hl_linux_memory_repair_signal_page(void *context, uint64_t address, uint64_t size, uint32_t protection) {
    (void)context;
    if (address == 0 || address > UINTPTR_MAX || size != UINT64_C(4096) || (address & UINT64_C(4095)) != 0 ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0)
        return 0;
    void *page = (void *)(uintptr_t)address;
    int native_protection = hl_linux_protection(protection);
    if (mprotect(page, (size_t)size, native_protection) == 0) return 1;
#ifdef MAP_FIXED_NOREPLACE
    if (mmap(page, (size_t)size, native_protection, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0) == page)
        return 1;
    return mprotect(page, (size_t)size, native_protection) == 0;
#else
    return 0;
#endif
}

static hl_host_result hl_linux_memory_map_file(void *context, hl_host_handle file, uint64_t requested_address,
                                               uint64_t offset, uint64_t size, uint32_t protection, uint32_t flags,
                                               hl_host_file_mapping *output) {
    hl_host_linux *host = context;
    hl_host_result registered;
    void *address;
    int descriptor;
    long page = sysconf(_SC_PAGESIZE);
    int native_flags;
    uint32_t placement = flags & (HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    uint32_t sharing = flags & (HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_FILE_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || offset > INT64_MAX || page <= 0 || offset % (uint64_t)page != 0 ||
        requested_address > UINTPTR_MAX || (requested_address != 0 && requested_address % (uint64_t)page != 0) ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_SHARED && sharing != HL_HOST_MEMORY_PRIVATE) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, NULL, NULL, size, -1);
    if (registered.status != HL_STATUS_OK) return registered;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    native_flags = sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE;
    if (placement == HL_HOST_MEMORY_FIXED) native_flags |= MAP_FIXED;
#ifdef MAP_FIXED_NOREPLACE
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) native_flags |= MAP_FIXED_NOREPLACE;
#else
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
#endif
    address = mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_linux_protection(protection), native_flags,
                   descriptor, (off_t)offset);
    if (address == MAP_FAILED) {
        hl_host_result failure = hl_linux_errno_result();
        (void)hl_linux_memory_discard(context, registered.value);
        return failure;
    }
    /* MAP_FIXED replaced these VMAs atomically. Retire stale ownership handles without unmapping the
     * new VMA -- but only the ones that still held a byte of it. A handle whose overlap with this
     * range is entirely inside a hole it already gave back kept nothing here to go stale. */
    if (placement == HL_HOST_MEMORY_FIXED) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->handle_capacity; ++index) {
            hl_linux_handle_entry *entry = &host->handles[index];
            if (hl_linux_encode_handle(index, entry->generation) != registered.value &&
                hl_linux_entry_holds_locked(entry, low, high))
                hl_linux_retire_mapping_locked(entry);
        }
        pthread_mutex_unlock(&host->lock);
    }
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *owned = hl_linux_lookup_locked(host, registered.value, HL_LINUX_HANDLE_MAPPING);
    if (owned != NULL) owned->address = address;
    pthread_mutex_unlock(&host->lock);
    output->handle = registered.value;
    output->address = (uint64_t)(uintptr_t)address;
    output->mapped_size = size;
    output->reserved = 0;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_map_anonymous(void *context, uint64_t requested_address, uint64_t size,
                                                    uint32_t protection, uint32_t flags,
                                                    hl_host_memory_mapping *output) {
    hl_host_linux *host = context;
    hl_host_result registered;
    void *address;
    long page = sysconf(_SC_PAGESIZE);
    uint32_t placement = flags & (HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    uint32_t sharing = flags & (HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_MEMORY_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || page <= 0 || requested_address > UINTPTR_MAX ||
        (requested_address != 0 && requested_address % (uint64_t)page != 0) ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_PRIVATE && sharing != HL_HOST_MEMORY_SHARED) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, NULL, NULL, size, -1);
    if (registered.status != HL_STATUS_OK) return registered;
    int native_flags = (sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE) | MAP_ANONYMOUS;
    if (placement == HL_HOST_MEMORY_FIXED) native_flags |= MAP_FIXED;
#ifdef MAP_FIXED_NOREPLACE
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) native_flags |= MAP_FIXED_NOREPLACE;
#else
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
#endif
    address =
        mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_linux_protection(protection), native_flags, -1, 0);
    if (address == MAP_FAILED) {
        hl_host_result failure = hl_linux_errno_result();
        (void)hl_linux_memory_discard(context, registered.value);
        return failure;
    }
    if (placement == HL_HOST_MEMORY_FIXED) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->handle_capacity; ++index) {
            hl_linux_handle_entry *entry = &host->handles[index];
            if (hl_linux_encode_handle(index, entry->generation) != registered.value &&
                hl_linux_entry_holds_locked(entry, low, high))
                hl_linux_retire_mapping_locked(entry);
        }
        pthread_mutex_unlock(&host->lock);
    }
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *owned = hl_linux_lookup_locked(host, registered.value, HL_LINUX_HANDLE_MAPPING);
    if (owned != NULL) owned->address = address;
    pthread_mutex_unlock(&host->lock);
    *output = (hl_host_memory_mapping){
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(*output), registered.value, (uint64_t)(uintptr_t)address, size, 0};
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_sync(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int status;
    if (size == 0 || size > SIZE_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = msync((char *)entry->address + offset, (size_t)size, MS_SYNC);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_unmap_range(void *context, hl_host_handle mapping, uint64_t offset,
                                                  uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int status;
    long page = sysconf(_SC_PAGESIZE);
    /* Linux requires the start address (and therefore the mapping-relative offset) to be page aligned,
     * but accepts an arbitrary positive length and rounds its end up internally. File mappings commonly
     * retain their exact byte length in the engine handle; memmap2 then passes that same non-page-aligned
     * length to munmap. Rejecting it here incorrectly surfaced EINVAL to an otherwise valid Linux guest. */
    if (size == 0 || size > SIZE_MAX || page <= 0 || offset % (uint64_t)page != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = munmap((char *)entry->address + offset, (size_t)size);
    if (status == 0) {
        /* A full-range unmap consumes the handle. A partial one keeps it, so the subrange it just
         * gave back has to leave the handle's coverage too -- otherwise the handle goes on claiming
         * a hole the address space is free to hand to someone else. When repeated partial unmaps
         * finally leave nothing held, the handle is consumed exactly as a single full one would
         * have consumed it: a live mapping handle always holds at least one byte.
         *
         * If the record cannot grow, the subrange stays claimed. That refuses a reuse that would
         * have been legal, which is recoverable; the other direction is not. The same reasoning
         * applies to the tail of a non-page-aligned length: the kernel rounds it up and this does
         * not, so the handle keeps claiming those last bytes rather than guessing them away. */
        if ((offset == 0 && size == entry->size) || (hl_host_hole_set_retire(&entry->retired, offset, size) &&
                                                     !hl_host_hole_set_holds(&entry->retired, 0, entry->size)))
            hl_linux_retire_mapping_locked(entry);
    }
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* True while any live mapping handle still holds a byte of [low, high). */
static int hl_linux_range_owned_locked(hl_host_linux *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->handle_capacity; ++index)
        if (hl_linux_entry_holds_locked(&host->handles[index], low, high)) return 1;
    return 0;
}

/* Reject before acting, so a refused range is left exactly as it was found. */
static hl_host_result hl_linux_memory_unmap_address(void *context, uint64_t address, uint64_t size) {
    hl_host_linux *host = context;
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t low;
    int status;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    low = (uintptr_t)address;
    pthread_mutex_lock(&host->lock);
    if (hl_linux_range_owned_locked(host, low, low + (uintptr_t)size)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    status = munmap((void *)low, (size_t)size);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* True while any live CODE mapping handle still holds a byte of [low, high). A code mapping is the
 * one whose protection an address-keyed caller must not touch: the writable and executable views
 * are a pair, the per-thread write gate flips between them, and a caller holding only an address
 * cannot put back what it changed because it does not hold the handle that knows the other view. */
static int hl_linux_code_range_owned_locked(hl_host_linux *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->handle_capacity; ++index) {
        const hl_linux_handle_entry *entry = &host->handles[index];
        if (entry->executable_address != NULL && hl_linux_entry_holds_locked(entry, low, high)) return 1;
    }
    return 0;
}

/* Page-align an address-keyed span the way mprotect(2) and msync(2) do: the address must already be
 * aligned and the length is rounded up. Returns zero when the request cannot be expressed. */
static int hl_linux_address_span(uint64_t address, uint64_t size, uintptr_t *low, size_t *span) {
    long page = sysconf(_SC_PAGESIZE);
    uint64_t rounded;
    if (address == 0 || size == 0 || page <= 0 || address > UINTPTR_MAX || address % (uint64_t)page != 0) return 0;
    rounded = size + ((uint64_t)page - 1u);
    if (rounded < size) return 0;
    rounded -= rounded % (uint64_t)page;
    if (rounded > SIZE_MAX || rounded > (uint64_t)UINTPTR_MAX - address) return 0;
    *low = (uintptr_t)address;
    *span = (size_t)rounded;
    return 1;
}

/* Reject before acting, so a refused range is left exactly as it was found. */
static hl_host_result hl_linux_memory_protect_address(void *context, uint64_t address, uint64_t size,
                                                      uint32_t protection) {
    hl_host_linux *host = context;
    uintptr_t low;
    size_t span;
    int status;
    if ((protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        !hl_linux_address_span(address, size, &low, &span))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    if (hl_linux_code_range_owned_locked(host, low, low + (uintptr_t)span)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    status = mprotect((void *)low, span, hl_linux_protection(protection));
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_sync_address(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    uintptr_t low;
    size_t span;
    int native_flags;
    (void)context;
    if ((flags & ~(uint32_t)(HL_HOST_MEMORY_SYNC_ASYNC | HL_HOST_MEMORY_SYNC_INVALIDATE)) != 0 ||
        !hl_linux_address_span(address, size, &low, &span))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    native_flags = (flags & HL_HOST_MEMORY_SYNC_ASYNC) != 0 ? MS_ASYNC : MS_SYNC;
    if ((flags & HL_HOST_MEMORY_SYNC_INVALIDATE) != 0) native_flags |= MS_INVALIDATE;
    /* No ownership question is asked. Flushing takes nothing away from a handle that covers the
     * range: the mapping, its protection and its contents are all exactly as they were. */
    return msync((void *)low, span, native_flags) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* mlock(2) pins against reclaim and charges RLIMIT_MEMLOCK, so this host reports HL_HOST_WIRE_RESIDENT.
 * The length follows mlock's own rule and is rounded up to whole pages by the kernel. */
static hl_host_result hl_linux_memory_wire_range(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX || flags != 0 ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (mlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_linux_memory_unwire_range(void *context, uint64_t address, uint64_t size) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (munlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_linux_memory_publish(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    char *address = entry->executable_address != NULL ? entry->executable_address : entry->address;
    __builtin___clear_cache(address + offset, address + offset + size);
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_code_write(void *context) {
    (void)context;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static void *hl_linux_map_aligned(int descriptor, uint64_t size, uint64_t alignment, int protection, int flags) {
    size_t reserve_size;
    void *reservation;
    uintptr_t base;
    uintptr_t aligned;
    if (alignment <= (uint64_t)sysconf(_SC_PAGESIZE)) return mmap(NULL, (size_t)size, protection, flags, descriptor, 0);
    if (size > SIZE_MAX - alignment) {
        errno = ENOMEM;
        return MAP_FAILED;
    }
    reserve_size = (size_t)(size + alignment);
    reservation = mmap(NULL, reserve_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return MAP_FAILED;
    base = (uintptr_t)reservation;
    aligned = (base + (uintptr_t)alignment - 1) & ~((uintptr_t)alignment - 1);
    if (aligned != base) (void)munmap((void *)base, (size_t)(aligned - base));
    if (base + reserve_size != aligned + size)
        (void)munmap((void *)(aligned + size), (size_t)(base + reserve_size - aligned - size));
    reservation = mmap((void *)aligned, (size_t)size, protection, flags | MAP_FIXED, descriptor, 0);
    return reservation;
}

static hl_host_result hl_linux_memory_reserve_code(void *context, uint64_t size, uint64_t alignment, uint32_t flags,
                                                   hl_host_code_mapping *output) {
    hl_host_linux *host = context;
    long page = sysconf(_SC_PAGESIZE);
    int descriptor;
    void *writable;
    void *executable;
    hl_host_result handle;
    if (output == NULL || size == 0 || size > SIZE_MAX || size > INT64_MAX || page <= 0 || alignment == 0 ||
        (alignment & (alignment - 1)) != 0 || alignment < (uint64_t)page)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    if ((flags & HL_HOST_CODE_DUAL_ALIAS) == 0) {
        writable =
            hl_linux_map_aligned(-1, size, alignment, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS);
        if (writable == MAP_FAILED) return hl_linux_errno_result();
        handle = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, writable, writable, size, -1);
        if (handle.status != HL_STATUS_OK) {
            munmap(writable, (size_t)size);
            return handle;
        }
        output->abi = 1;
        output->size = sizeof(*output);
        output->handle = handle.value;
        output->writable_address = (uint64_t)(uintptr_t)writable;
        output->executable_address = (uint64_t)(uintptr_t)writable;
        output->mapped_size = size;
        return handle;
    }
    descriptor = memfd_create("hl-code", MFD_CLOEXEC);
    if (descriptor < 0) return hl_linux_errno_result();
    if (ftruncate(descriptor, (off_t)size) != 0) {
        close(descriptor);
        return hl_linux_errno_result();
    }
    writable = hl_linux_map_aligned(descriptor, size, alignment, PROT_READ | PROT_WRITE, MAP_SHARED);
    if (writable == MAP_FAILED) {
        close(descriptor);
        return hl_linux_errno_result();
    }
    executable = hl_linux_map_aligned(descriptor, size, alignment, PROT_READ | PROT_EXEC, MAP_SHARED);
    if (executable == MAP_FAILED) {
        munmap(writable, (size_t)size);
        close(descriptor);
        return hl_linux_errno_result();
    }
    handle = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, descriptor, writable, executable, size, -1);
    if (handle.status != HL_STATUS_OK) {
        munmap(executable, (size_t)size);
        munmap(writable, (size_t)size);
        close(descriptor);
        return handle;
    }
    output->abi = 1;
    output->size = sizeof(*output);
    output->handle = handle.value;
    output->writable_address = (uint64_t)(uintptr_t)writable;
    output->executable_address = (uint64_t)(uintptr_t)executable;
    output->mapped_size = size;
    return hl_linux_result(HL_STATUS_OK, handle.value, 0);
}

static hl_host_result hl_linux_memory_repair_code(void *context, hl_host_code_mapping *mapping, uint32_t preserve) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_handle_entry inherited;
    int descriptor = -1;
    void *writable = MAP_FAILED;
    void *executable = MAP_FAILED;
    if (mapping == NULL || mapping->abi != 1 || mapping->size < sizeof(*mapping))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);

    /* This entry point is called only in the fork child. A sibling may have
       owned the process-private registry lock when fork cloned the caller, so
       its inherited pthread state cannot be acquired or destroyed safely. */
    {
        pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
        memcpy(&host->lock, &fresh, sizeof(fresh));
    }
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping->handle, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || entry->executable_address == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    inherited = *entry;
    pthread_mutex_unlock(&host->lock);
    if (mapping->content_size > inherited.size) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);

    if (inherited.executable_address != inherited.address) {
        descriptor = memfd_create("hl-code", MFD_CLOEXEC);
        if (descriptor < 0) return hl_linux_errno_result();
        if (ftruncate(descriptor, (off_t)inherited.size) != 0) goto fresh_failed;
        writable = mmap(NULL, (size_t)inherited.size, PROT_READ | PROT_WRITE, MAP_SHARED, descriptor, 0);
        if (writable == MAP_FAILED) goto fresh_failed;
        executable = mmap(NULL, (size_t)inherited.size, PROT_READ | PROT_EXEC, MAP_SHARED, descriptor, 0);
        if (executable == MAP_FAILED) goto fresh_failed;

        if (preserve != 0) {
            /* Linux inherits MAP_SHARED memfd pages as genuinely process-shared
               pages. Give the child a private backing object while retaining
               the exact cache addresses and bytes expected by every map entry,
               chain, and inline-cache pointer. */
            memcpy(writable, inherited.address, (size_t)mapping->content_size);
            if (mmap(inherited.address, (size_t)inherited.size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED,
                     descriptor, 0) == MAP_FAILED ||
                mmap(inherited.executable_address, (size_t)inherited.size, PROT_READ | PROT_EXEC,
                     MAP_SHARED | MAP_FIXED, descriptor, 0) == MAP_FAILED)
                goto fresh_failed;
            (void)munmap(executable, (size_t)inherited.size);
            (void)munmap(writable, (size_t)inherited.size);
            writable = inherited.address;
            executable = inherited.executable_address;
        }

        /* Publish the replacement under the same opaque handle and generation.
           The inherited VMAs remain mapped until publication is complete, so a
           new alias can never be accidentally removed through a reused VA. */
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) goto fresh_failed;
        descriptor = adopted;
        pthread_mutex_lock(&host->lock);
        entry = hl_linux_lookup_locked(host, mapping->handle, HL_LINUX_HANDLE_MAPPING);
        if (entry == NULL || entry->descriptor != inherited.descriptor || entry->address != inherited.address ||
            entry->executable_address != inherited.executable_address || entry->size != inherited.size) {
            pthread_mutex_unlock(&host->lock);
            hl_host_process_fd_private_remove(descriptor);
            goto fresh_failed;
        }
        entry->descriptor = descriptor;
        entry->address = writable;
        entry->executable_address = executable;
        pthread_mutex_unlock(&host->lock);

        hl_host_process_fd_private_remove(inherited.descriptor);
        if (preserve == 0) {
            (void)munmap(inherited.executable_address, (size_t)inherited.size);
            (void)munmap(inherited.address, (size_t)inherited.size);
        }
        if (inherited.descriptor >= 0) (void)close(inherited.descriptor);
    } else {
        writable = inherited.address;
        executable = inherited.executable_address;
    }

    mapping->writable_address = (uint64_t)(uintptr_t)writable;
    mapping->executable_address = (uint64_t)(uintptr_t)executable;
    mapping->mapped_size = inherited.size;
    return hl_linux_result(HL_STATUS_OK, mapping->handle, 0);

fresh_failed: {
    int error = errno;
    if (executable != MAP_FAILED) (void)munmap(executable, (size_t)inherited.size);
    if (writable != MAP_FAILED) (void)munmap(writable, (size_t)inherited.size);
    if (descriptor >= 0) (void)close(descriptor);
    errno = error;
    return hl_linux_errno_result();
}
}

static hl_host_result hl_linux_clock(int clock_id) {
    struct timespec time;
    if (clock_gettime(clock_id, &time) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)time.tv_sec * UINT64_C(1000000000) + (uint64_t)time.tv_nsec, 0);
}

static hl_host_result hl_linux_monotonic(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_MONOTONIC);
}

static hl_host_result hl_linux_realtime(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_REALTIME);
}

static hl_host_result hl_linux_raw_monotonic(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_MONOTONIC_RAW);
}

static hl_host_result hl_linux_process_cpu(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_PROCESS_CPUTIME_ID);
}

static hl_host_result hl_linux_thread_cpu(void *context) {
    (void)context;
    return hl_linux_clock(CLOCK_THREAD_CPUTIME_ID);
}

/* Reports the tick RATE of the CPU's free-running counter, which the translator bakes into a tick->ns
 * multiplier to serve guest clock reads inline.  x86-64's TSC frequency is not architecturally readable
 * (CPUID.15H is absent or lies on most parts) and is not calibrated here; NOT_SUPPORTED is a first-class
 * answer, and s1_calibrate then clears its fast-clock flag. */
static hl_host_result hl_linux_architectural_counter(void *context) {
    (void)context;
#if defined(HL_HOST_CPU_AARCH64)
    uint64_t frequency;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(frequency));
    if (frequency != 0) return hl_linux_result(HL_STATUS_OK, frequency, 0);
#endif
    return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_linux_backoff(void *context, uint64_t interval_ns) {
    struct timespec remaining;
    (void)context;
    remaining.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    remaining.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR) return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_clock_sleep_until(void *context, uint32_t clock_kind, uint64_t deadline_ns) {
    clockid_t clock_id;
    struct timespec deadline;
    int error;
    (void)context;
    switch (clock_kind) {
    case HL_HOST_CLOCK_MONOTONIC: clock_id = CLOCK_MONOTONIC; break;
    case HL_HOST_CLOCK_REALTIME: clock_id = CLOCK_REALTIME; break;
    case HL_HOST_CLOCK_PROCESS_CPU: clock_id = CLOCK_PROCESS_CPUTIME_ID; break;
    default: return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
    deadline.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    deadline.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    error = clock_nanosleep(clock_id, TIMER_ABSTIME, &deadline, NULL);
    if (error != 0) {
        errno = error;
        return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static void hl_linux_log(void *context, uint32_t event, const char *message, size_t message_size) {
    size_t written = 0;
    (void)context;
    (void)event;
    while (written < message_size) {
        ssize_t result = write(STDERR_FILENO, message + written, message_size - written);
        if (result > 0) {
            written += (size_t)result;
            continue;
        }
        if (result < 0 && errno == EINTR) continue;
        break;
    }
}

static int hl_linux_descriptor(hl_host_linux *host, hl_host_handle handle, hl_linux_handle_kind first,
                               hl_linux_handle_kind second) {
    uint32_t low = (uint32_t)handle;
    uint32_t index;
    hl_linux_handle_entry *entry;
    if (handle == HL_HOST_HANDLE_CWD) return AT_FDCWD;
    if (low == 0) return -1;
    index = low - 1u;
    if (index >= host->handle_capacity) return -1;
    entry = &host->handles[index];
    if (entry->generation != (uint32_t)(handle >> 32) || (entry->kind != first && entry->kind != second)) return -1;
    return entry->descriptor;
}

static hl_host_result hl_linux_file_open(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t access, uint32_t creation, uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    int descriptor;
    int append_descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int flags;
    if ((access & HL_HOST_FILE_PATH_ONLY) != 0)
        flags = O_PATH;
    else if ((access & HL_HOST_FILE_READ) != 0 && (access & HL_HOST_FILE_WRITE) != 0)
        flags = O_RDWR;
    else if ((access & HL_HOST_FILE_WRITE) != 0)
        flags = O_WRONLY;
    else if ((access & HL_HOST_FILE_READ) != 0)
        flags = O_RDONLY;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0) flags |= O_NOFOLLOW;
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) flags |= O_DIRECTORY;
    /* Linux O_PATH ignores creation and truncation flags. Keep that contract
       in the portable service instead of relying on host-specific open flags. */
    if ((access & HL_HOST_FILE_PATH_ONLY) == 0) {
        if ((creation & HL_HOST_FILE_CREATE) != 0) flags |= O_CREAT;
        if ((creation & HL_HOST_FILE_EXCLUSIVE) != 0) flags |= O_EXCL;
        if ((creation & HL_HOST_FILE_TRUNCATE) != 0) flags |= O_TRUNC;
    }
    descriptor = openat(directory_fd, local, flags | O_CLOEXEC, (mode_t)(permissions & 07777u));
    if (descriptor < 0) return hl_linux_errno_result();
    if ((access & HL_HOST_FILE_APPEND) != 0) {
        char descriptor_path[64];
        /* O_NOFOLLOW governs the caller's original path.  The trusted
         * /proc/self/fd indirection below must follow its magic link to the
         * already-validated file description. */
        int append_flags = flags & ~(O_CREAT | O_EXCL | O_TRUNC | O_NOFOLLOW);
        int length = snprintf(descriptor_path, sizeof(descriptor_path), "/proc/self/fd/%d", descriptor);
        if (length < 0 || (size_t)length >= sizeof(descriptor_path)) {
            close(descriptor);
            return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
        }
        append_descriptor = open(descriptor_path, append_flags | O_APPEND | O_CLOEXEC, 0);
        if (append_descriptor < 0) {
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            return error;
        }
        {
            struct stat primary_status;
            struct stat append_status;
            if (fstat(descriptor, &primary_status) != 0 || fstat(append_descriptor, &append_status) != 0 ||
                primary_status.st_dev != append_status.st_dev || primary_status.st_ino != append_status.st_ino) {
                hl_host_result error = hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
                close(append_descriptor);
                close(descriptor);
                return error;
            }
        }
    }
    hl_host_result result =
        hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
    }
    return result;
}

static hl_host_result hl_linux_file_standard_stream(void *context, uint32_t stream) {
    hl_host_linux *host = context;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    uint32_t detail = 0;
    hl_host_result result;
    if (stream > HL_HOST_STANDARD_ERROR) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    flags = fcntl((int)stream, F_GETFL);
    if (flags < 0) return hl_linux_errno_result();
    descriptor = fcntl((int)stream, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_linux_errno_result();
    if ((flags & O_ACCMODE) == O_RDONLY)
        detail |= HL_HOST_FILE_READ;
    else if ((flags & O_ACCMODE) == O_WRONLY)
        detail |= HL_HOST_FILE_WRITE;
    else if ((flags & O_ACCMODE) == O_RDWR)
        detail |= HL_HOST_FILE_READ | HL_HOST_FILE_WRITE;
    if ((flags & O_APPEND) != 0) {
        detail |= HL_HOST_FILE_APPEND;
        append_descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
        if (append_descriptor < 0) {
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            return error;
        }
    }
    if ((flags & O_NONBLOCK) != 0) detail |= HL_HOST_FILE_NONBLOCK;
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
        return result;
    }
    result.detail = detail;
    return result;
}

static hl_host_result hl_linux_stream_pipe_pair(void *context, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptors[2];
    int native_flags = O_CLOEXEC;
    hl_host_result input, output;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((flags & HL_HOST_STREAM_NONBLOCK) != 0) native_flags |= O_NONBLOCK;
    if (pipe2(descriptors, native_flags) != 0) return hl_linux_errno_result();
    input = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptors[0], NULL, NULL, 0, -1);
    if (input.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        return input;
    }
    output = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptors[1], NULL, NULL, 0, -1);
    if (output.status != HL_STATUS_OK) {
        (void)hl_linux_close_descriptor(host, input.value);
        close(descriptors[1]);
        return output;
    }
    input.detail = output.value;
    return input;
}

static hl_host_result hl_linux_stream_set_status_flags(void *context, hl_host_handle stream, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor, current;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, stream, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    current = fcntl(descriptor, F_GETFL);
    if (current < 0) {
        hl_host_result error = hl_linux_errno_result();
        close(descriptor);
        return error;
    }
    current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0);
    hl_host_result result =
        fcntl(descriptor, F_SETFL, current) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
    close(descriptor);
    return result;
}

static int hl_linux_stream_descriptor(hl_host_linux *host, hl_host_handle stream) {
    int descriptor, pinned = -1;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, stream, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (descriptor >= 0) pinned = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    return pinned;
}

static hl_host_result hl_linux_stream_read(void *context, hl_host_handle stream, hl_host_bytes output) {
    int descriptor;
    ssize_t result;
    if (output.size != 0 && output.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_stream_descriptor(context, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = read(descriptor, output.data, output.size);
    hl_host_result output_result =
        result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(descriptor);
    return output_result;
}

static hl_host_result hl_linux_file_validate_private_regular(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    struct stat st;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_linux_errno_result();
    return S_ISREG(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_linux_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                         size_t path_size, hl_host_const_bytes input,
                                                         uint32_t permissions) {
    static _Atomic uint64_t sequence;
    hl_host_linux *host = context;
    char name[PATH_MAX], temporary[PATH_MAX];
    int directory_fd = AT_FDCWD, descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(name) || memchr(path, '\0', path_size) != NULL ||
        (permissions & ~0777u) != 0 || (input.size != 0 && input.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(name, path, path_size);
    name[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        pthread_mutex_lock(&host->lock);
        directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        if (directory_fd >= 0) directory_fd = fcntl(directory_fd, F_DUPFD_CLOEXEC, 0);
        pthread_mutex_unlock(&host->lock);
        if (directory_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (unsigned attempt = 0; attempt < 16; ++attempt) {
        uint64_t token = atomic_fetch_add_explicit(&sequence, 1, memory_order_relaxed);
        int count = snprintf(temporary, sizeof temporary, "%s.hl-%llx-%llx.tmp", name,
                             (unsigned long long)(uint64_t)getpid(), (unsigned long long)token);
        if (count <= 0 || (size_t)count >= sizeof temporary) break;
        descriptor =
            openat(directory_fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, (mode_t)permissions);
        if (descriptor >= 0 || errno != EEXIST) break;
    }
    if (descriptor < 0) {
        if (directory_fd != AT_FDCWD) close(directory_fd);
        return hl_linux_errno_result();
    }
    size_t done = 0;
    int saved = 0;
    while (done < input.size) {
        ssize_t count = write(descriptor, (const uint8_t *)input.data + done, input.size - done);
        if (count > 0)
            done += (size_t)count;
        else if (count < 0 && errno == EINTR)
            continue;
        else {
            saved = count == 0 ? EIO : errno;
            break;
        }
    }
    int ok = done == input.size;
    if (ok && fsync(descriptor) != 0) {
        ok = 0;
        saved = errno;
    }
    if (close(descriptor) != 0 && ok) {
        ok = 0;
        saved = errno;
    }
    if (ok && renameat(directory_fd, temporary, directory_fd, name) != 0) {
        ok = 0;
        saved = errno;
    }
    if (!ok) (void)unlinkat(directory_fd, temporary, 0);
    if (directory_fd != AT_FDCWD) close(directory_fd);
    errno = saved != 0 ? saved : EIO;
    return ok ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_validate_private_directory(void *context, hl_host_handle directory) {
    hl_host_linux *host = context;
    struct stat st;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_linux_errno_result();
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_linux_stream_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    int descriptor;
    ssize_t result;
    sigset_t blocked, previous;
    int saved_error;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_stream_descriptor(context, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    result = write(descriptor, input.data, input.size);
    saved_error = errno;
    if (result < 0 && saved_error == EPIPE && !sigismember(&previous, SIGPIPE)) {
        struct timespec immediate = {0, 0};
        (void)sigtimedwait(&blocked, NULL, &immediate);
    }
    pthread_sigmask(SIG_SETMASK, &previous, NULL);
    errno = saved_error;
    hl_host_result output_result =
        result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(descriptor);
    return output_result;
}

static hl_host_result hl_linux_stream_duplicate(void *context, hl_host_handle stream) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_stream_descriptor(host, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static hl_host_result hl_linux_stream_close(void *context, hl_host_handle stream) {
    int pinned = hl_linux_stream_descriptor(context, stream);
    if (pinned < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    close(pinned);
    return hl_linux_close_descriptor(context, stream);
}

/* Readiness is asked about any pollable host object, not just pipes. Imported
 * stdio -- a pty slave included -- is registered as HL_LINUX_HANDLE_FILE, so the
 * stream-only lookup would reject it and the caller would fall back to
 * always-ready. Accept files here (macOS does the same); the other stream entry
 * points stay stream-typed. */
static int hl_linux_pollable_descriptor(hl_host_linux *host, hl_host_handle handle) {
    int descriptor, pinned = -1;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, handle, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) pinned = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    return pinned;
}

static hl_host_result hl_linux_stream_readiness(void *context, hl_host_handle stream, uint32_t interests) {
    int descriptor = hl_linux_pollable_descriptor(context, stream);
    struct pollfd probe = {descriptor, 0, 0};
    uint32_t ready = 0;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((interests & HL_HOST_READY_READ) != 0) probe.events |= POLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) probe.events |= POLLOUT;
    if (poll(&probe, 1, 0) < 0) {
        hl_host_result error = hl_linux_errno_result();
        close(descriptor);
        return error;
    }
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    close(descriptor);
    return hl_linux_result(HL_STATUS_OK, ready & interests, 0);
}

static hl_host_result hl_linux_stream_move(void *context, hl_host_handle source, uint64_t source_offset,
                                           hl_host_handle destination, uint64_t destination_offset, uint64_t size,
                                           uint32_t flags) {
    hl_host_linux *host = context;
    int input, output;
    off_t input_offset, output_offset;
    off_t *input_pointer = NULL, *output_pointer = NULL;
    ssize_t result;
    sigset_t blocked, previous;
    int saved_error;
    uint32_t allowed = HL_HOST_STREAM_SOURCE_POSITIONED | HL_HOST_STREAM_DESTINATION_POSITIONED;
    int source_file = 0, destination_file = 0;
    if ((flags & ~allowed) != 0 || source_offset > INT64_MAX || destination_offset > INT64_MAX || size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    input = hl_linux_descriptor(host, source, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (input < 0) {
        input = hl_linux_descriptor(host, source, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        source_file = input >= 0;
    }
    output = hl_linux_descriptor(host, destination, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (output < 0) {
        output = hl_linux_descriptor(host, destination, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        destination_file = output >= 0;
    }
    if (input >= 0) input = fcntl(input, F_DUPFD_CLOEXEC, 0);
    if (output >= 0) output = fcntl(output, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (input < 0 || output < 0) {
        if (input >= 0) close(input);
        if (output >= 0) close(output);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((source_file && destination_file) || ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0 && !source_file) ||
        ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0 && !destination_file) ||
        (source_file && (flags & HL_HOST_STREAM_SOURCE_POSITIONED) == 0) ||
        (destination_file && (flags & HL_HOST_STREAM_DESTINATION_POSITIONED) == 0)) {
        close(input);
        close(output);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    input_offset = (off_t)source_offset;
    output_offset = (off_t)destination_offset;
    if ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0) input_pointer = &input_offset;
    if ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0) output_pointer = &output_offset;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    result = splice(input, input_pointer, output, output_pointer, (size_t)size, 0);
    saved_error = errno;
    if (result < 0 && saved_error == EPIPE && !sigismember(&previous, SIGPIPE)) {
        struct timespec immediate = {0, 0};
        (void)sigtimedwait(&blocked, NULL, &immediate);
    }
    pthread_sigmask(SIG_SETMASK, &previous, NULL);
    errno = saved_error;
    hl_host_result moved = result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(input);
    close(output);
    return moved;
}

static hl_host_result hl_linux_file_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        count = readlinkat(descriptor, "", output.data, output.size);
    while (count < 0 && errno == EINTR);
    if (count < 0) {
        /* An empty-path readlinkat only names the link node when the fd is an O_PATH|O_NOFOLLOW handle on a
           SYMLINK; on a regular file / directory the kernel returns ENOENT for the empty path, but Linux's
           readlink(2) contract is EINVAL ("not a symbolic link"). Distinguish the two so a readlinkat on a
           non-symlink reports EINVAL, matching native. */
        int saved = errno;
        struct stat metadata;
        if ((saved == ENOENT || saved == EINVAL) && fstatat(descriptor, "", &metadata, AT_EMPTY_PATH) == 0 &&
            !S_ISLNK(metadata.st_mode))
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        errno = saved;
        return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_linux_file_set_owner(void *context, hl_host_handle file, uint32_t uid, uint32_t gid) {
    hl_host_linux *host = context;
    int descriptor;
    int status;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchownat(descriptor, "", (uid_t)uid, (gid_t)gid, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_set_permissions(void *context, hl_host_handle file, uint32_t permissions) {
    hl_host_linux *host = context;
    int descriptor;
    int status;
    if ((permissions & ~07777u) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* File-service path handles are deliberately opened with O_PATH. fchmod(2) rejects those descriptors;
       operate on the pinned object through fchmodat2's empty relative path instead, preserving the handle
       identity without resolving the caller's original pathname again.  The legacy fchmodat syscall has no
       flags argument on Linux; libc emulation cannot implement AT_EMPTY_PATH for an O_PATH descriptor and
       returns EPERM on overlay-backed files. */
    do
        status = (int)syscall(452 /* fchmodat2 */, descriptor, "", (mode_t)permissions, AT_EMPTY_PATH);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_attachment_borrow_file_at_least(void *context, hl_host_handle file,
                                                               uint32_t minimum_descriptor) {
    hl_host_linux *host = context;
    int descriptor;
    int borrowed;
    if (minimum_descriptor > INT_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    borrowed = descriptor < 0 ? -1 : fcntl(descriptor, F_DUPFD_CLOEXEC, (int)minimum_descriptor);
    pthread_mutex_unlock(&host->lock);
    if (borrowed < 0)
        return descriptor < 0 ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_linux_errno_result();
    int adopted = hl_host_process_fd_private_adopt(borrowed);
    if (adopted < 0) {
        close(borrowed);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    borrowed = adopted;
    return hl_linux_result(HL_STATUS_OK, (uint64_t)(unsigned)borrowed, 0);
}

static hl_host_result hl_linux_attachment_borrow_file(void *context, hl_host_handle file) {
    return hl_linux_attachment_borrow_file_at_least(context, file, 0);
}

static hl_host_result hl_linux_attachment_release(void *context, uint64_t borrowed_descriptor) {
    (void)context;
    if (borrowed_descriptor > INT_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int descriptor = (int)borrowed_descriptor;
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_set_times(void *context, hl_host_handle file, const hl_host_file_time times[2]) {
    hl_host_linux *host = context;
    struct timespec native[2];
    int descriptor;
    int status;
    if (times == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (int index = 0; index < 2; ++index) {
        if (times[index].mode == HL_HOST_FILE_TIME_NOW) {
            native[index].tv_sec = 0;
            native[index].tv_nsec = UTIME_NOW;
        } else if (times[index].mode == HL_HOST_FILE_TIME_OMIT) {
            native[index].tv_sec = 0;
            native[index].tv_nsec = UTIME_OMIT;
        } else if (times[index].mode == HL_HOST_FILE_TIME_EXPLICIT && times[index].nanoseconds < 1000000000u) {
            native[index].tv_sec = (time_t)times[index].seconds;
            native[index].tv_nsec = (long)times[index].nanoseconds;
        } else {
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
    }
    do
        status = futimens(descriptor, native);
    while (status != 0 && errno == EINTR);
    /* futimens(2) rejects an O_PATH descriptor with EBADF, and PATH_ONLY opens
       are exactly O_PATH here -- utimensat(dirfd, RELATIVE) resolves its target
       with PATH_ONLY, so every *at time-setter through a directory handle failed
       (guest EIO). macOS models PATH_ONLY as a real O_RDONLY open, which is why
       only the Linux engine broke, and why no lane caught it: compat-core-syscall
       ran on macOS only. /proc/self/fd/<n> is the documented way to reach the
       inode behind an O_PATH fd. No AT_SYMLINK_NOFOLLOW: the magic link must be
       traversed to reach the target at all. */
    if (status != 0 && errno == EBADF) {
        char descriptor_path[64];
        snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", descriptor);
        do
            status = utimensat(AT_FDCWD, descriptor_path, native, 0);
        while (status != 0 && errno == EINTR);
    }
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_read(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if (output.size != 0 && output.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || offset > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pread(descriptor, output.data, output.size, (off_t)offset);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_write(void *context, hl_host_handle file, uint64_t offset,
                                          hl_host_const_bytes input) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || offset > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pwrite(descriptor, input.data, input.size, (off_t)offset);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_read_sequential(void *context, hl_host_handle file, void *output,
                                                    uint64_t output_size) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((output_size != 0 && output == NULL) || output_size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output, (size_t)output_size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_write_sequential(void *context, hl_host_handle file, const void *input,
                                                     uint64_t input_size) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((input_size != 0 && input == NULL) || input_size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input, (size_t)input_size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_clone_for_fork(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)file;
    hl_linux_handle_entry *entry = NULL;
    int descriptor = -1;
    int append_descriptor = -1;
    int needs_append = 0;
    pthread_mutex_lock(&host->lock);
    if (low != 0 && low - 1u < host->handle_capacity) entry = &host->handles[low - 1u];
    if (entry != NULL && entry->generation == (uint32_t)(file >> 32) && entry->kind == HL_LINUX_HANDLE_FILE) {
        needs_append = entry->wake_descriptor >= 0;
        descriptor = dup(entry->descriptor);
        if (descriptor >= 0 && needs_append) append_descriptor = dup(entry->wake_descriptor);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || (needs_append && append_descriptor < 0)) {
        hl_host_result error = hl_linux_errno_result();
        if (descriptor >= 0) close(descriptor);
        return error;
    }
    {
        hl_host_result result =
            hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
        if (result.status != HL_STATUS_OK) {
            close(descriptor);
            if (append_descriptor >= 0) close(append_descriptor);
        }
        return result;
    }
}

static hl_host_result hl_linux_file_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_linux *host = context;
    int descriptor;
    off_t result;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || whence > HL_HOST_FILE_SEEK_HOLE) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = lseek(descriptor, (off_t)offset, (int)whence);
    return result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
}

static hl_host_result hl_linux_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)file;
    hl_linux_handle_entry *entry;
    int descriptor;
    ssize_t count;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    if (low == 0 || low - 1u >= host->handle_capacity) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    entry = &host->handles[low - 1u];
    descriptor = entry->generation == (uint32_t)(file >> 32) && entry->kind == HL_LINUX_HANDLE_FILE
                     ? entry->wake_descriptor
                     : -1;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* The descriptor was opened O_APPEND: this write is atomic with every other O_APPEND write. */
    count = write(descriptor, input.data, input.size);
    if (count < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_linux_file_vector(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                           uint32_t count, uint64_t offset, int operation) {
    hl_host_linux *host = context;
    struct iovec native[HL_HOST_FILE_IOV_MAX];
    int descriptor;
    ssize_t transferred;
    uint32_t index;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    if (operation == 4 && (uint32_t)file != 0 && (uint32_t)file - 1u < host->handle_capacity) {
        hl_linux_handle_entry *entry = &host->handles[(uint32_t)file - 1u];
        descriptor = entry->generation == (uint32_t)(file >> 32) && entry->kind == HL_LINUX_HANDLE_FILE
                         ? entry->wake_descriptor
                         : -1;
    }
    pthread_mutex_unlock(&host->lock);
    if ((count != 0 && vectors == NULL) || count > HL_HOST_FILE_IOV_MAX || descriptor < 0 ||
        ((operation == 2 || operation == 3) && offset > INT64_MAX))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > SIZE_MAX || vectors[index].address > UINTPTR_MAX)
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        native[index].iov_base = (void *)(uintptr_t)vectors[index].address;
        native[index].iov_len = (size_t)vectors[index].size;
    }
    switch (operation) {
    case 0: transferred = readv(descriptor, native, (int)count); break;
    case 1: transferred = writev(descriptor, native, (int)count); break;
    case 2: transferred = preadv(descriptor, native, (int)count, (off_t)offset); break;
    case 3: transferred = pwritev(descriptor, native, (int)count, (off_t)offset); break;
    default: transferred = writev(descriptor, native, (int)count); break;
    }
    if (transferred < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)transferred, 0);
}

#define HL_LINUX_VECTOR_WRAPPER(name, operation)                                                                       \
    static hl_host_result name(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {     \
        return hl_linux_file_vector(context, file, vectors, count, 0, operation);                                      \
    }
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_readv, 0)
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_writev, 1)
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_appendv, 4)

static hl_host_result hl_linux_file_truncate(void *context, hl_host_handle file, uint64_t size) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || size > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return ftruncate(descriptor, (off_t)size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fsync(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync_range(void *context, hl_host_handle file, uint64_t offset, uint64_t size,
                                               uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor;
    unsigned int native = 0;
    if ((flags & ~7u) != 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (flags & HL_HOST_FILE_SYNC_WAIT_BEFORE) native |= SYNC_FILE_RANGE_WAIT_BEFORE;
    if (flags & HL_HOST_FILE_SYNC_WRITE) native |= SYNC_FILE_RANGE_WRITE;
    if (flags & HL_HOST_FILE_SYNC_WAIT_AFTER) native |= SYNC_FILE_RANGE_WAIT_AFTER;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return sync_file_range(descriptor, (off64_t)offset, (off64_t)size, native) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync_filesystem(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return syncfs(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_data_sync(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fdatasync(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_rename(void *context, hl_host_handle old_directory, const char *old_path,
                                           size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                           size_t new_path_size) {
    hl_host_linux *host = context;
    char old_local[PATH_MAX];
    char new_local[PATH_MAX];
    int old_fd;
    int new_fd;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = '\0';
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    old_fd = hl_linux_descriptor(host, old_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    new_fd = hl_linux_descriptor(host, new_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (renameat(old_fd, old_local, new_fd, new_local) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_unlink(void *context, hl_host_handle directory, const char *path,
                                           size_t path_size) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, 0) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_rmdir(void *context, hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, AT_REMOVEDIR) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_mkdir(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                          uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkdirat(directory_fd, local, (mode_t)permissions) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_fifo(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkfifoat(directory_fd, local, (mode_t)permissions) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                   : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_symlink(void *context, const char *target, size_t target_size,
                                            hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_linux *host = context;
    char target_local[PATH_MAX], path_local[PATH_MAX];
    if (target == NULL || path == NULL || target_size == 0 || path_size == 0 || target_size >= sizeof(target_local) ||
        path_size >= sizeof(path_local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(target_local, target, target_size);
    target_local[target_size] = 0;
    memcpy(path_local, path, path_size);
    path_local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return symlinkat(target_local, directory_fd, path_local) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_link(void *context, hl_host_handle old_directory, const char *old_path,
                                         size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                         size_t new_path_size, uint32_t flags) {
    hl_host_linux *host = context;
    char old_local[PATH_MAX], new_local[PATH_MAX];
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local) || (flags & ~1u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = 0;
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int old_fd = hl_linux_descriptor(host, old_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    int new_fd = hl_linux_descriptor(host, new_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int native_flags = (flags & 1u) != 0 ? AT_SYMLINK_FOLLOW : 0;
    return linkat(old_fd, old_local, new_fd, new_local, native_flags) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                           : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset) {
    return hl_linux_file_vector(context, file, vectors, count, offset, 2);
}

static hl_host_result hl_linux_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count, uint64_t offset) {
    return hl_linux_file_vector(context, file, vectors, count, offset, 3);
}

static hl_host_result hl_linux_file_metadata_get(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    hl_host_linux *host = context;
    struct stat status;
    int descriptor;
    if (output == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Define *output before any early return: hl_linux_errno_result() maps errno 0 to HL_STATUS_OK,
     * so a caller keying off .status could otherwise read an unwritten field. */
    memset(output, 0, sizeof(*output));
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstat(descriptor, &status) != 0) return hl_linux_errno_result();
    output->stable_device = (uint64_t)status.st_dev;
    output->stable_object = (uint64_t)status.st_ino;
    output->size = (uint64_t)status.st_size;
    output->allocated_size = (uint64_t)status.st_blocks * 512u;
    output->modified_ns = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    output->accessed_ns = (uint64_t)status.st_atim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_atim.tv_nsec;
    output->changed_ns = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    /* A plain fstat() carries no birth time, so consult statx() for it: a filesystem that tracks
       creation time (tmpfs/ext4) reports STATX_BTIME, and leaving created_ns non-zero here lets an
       AT_EMPTY_PATH statx advertise the mask bit honestly -- byte-identical to native and to the
       path-based statx (fs.c hl_statx_host_btime). Filesystems that do not track it (procfs) leave
       the bit clear, so created_ns stays 0. Only statx consumes created_ns; plain stat ignores it. */
#if defined(SYS_statx) && defined(STATX_BTIME)
    {
        struct statx birth;
        memset(&birth, 0, sizeof birth);
        if (syscall(SYS_statx, descriptor, "", AT_EMPTY_PATH, STATX_BTIME, &birth) == 0 &&
            (birth.stx_mask & STATX_BTIME) != 0)
            output->created_ns =
                (uint64_t)birth.stx_btime.tv_sec * UINT64_C(1000000000) + (uint64_t)birth.stx_btime.tv_nsec;
    }
#endif
    output->device = (uint64_t)status.st_rdev;
    output->link_count = (uint64_t)status.st_nlink;
    output->user = (uint32_t)status.st_uid;
    output->group = (uint32_t)status.st_gid;
    output->permissions = (uint32_t)status.st_mode & 07777u;
    if (S_ISREG(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_REGULAR;
    else if (S_ISDIR(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_DIRECTORY;
    else if (S_ISLNK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_SYMLINK;
    else if (S_ISCHR(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_CHARACTER;
    else if (S_ISBLK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_BLOCK;
    else if (S_ISFIFO(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_FIFO;
    else if (S_ISSOCK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_SOCKET;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_resolve_beneath(void *context, hl_host_handle root, const char *path,
                                                    size_t path_size, uint32_t policy,
                                                    hl_host_file_resolution *output) {
    hl_host_linux *host = context;
    hl_host_resolved_path resolved;
    hl_host_result parent;
    hl_host_result target = {HL_STATUS_OK, 0, HL_HOST_HANDLE_INVALID, 0};
    char local[PATH_MAX];
    int root_fd;
    /* Resolution must not participate in special-file I/O.  In particular,
     * opening a FIFO O_RDONLY makes blocked writers runnable and can create a
     * false reader window before the guest opens it.  O_PATH pins identity
     * for metadata without changing FIFO/socket/device lifecycle state. */
    int target_flags = O_PATH;
    if (output == NULL || path == NULL || path_size == 0 || path_size >= sizeof(local) ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS |
                              HL_HOST_RESOLVE_ALLOW_MISSING)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    root_fd = hl_linux_descriptor(host, root, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (root_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((policy & HL_HOST_RESOLVE_ALLOW_MISSING) != 0) target_flags = -1;
    if (hl_host_resolve_beneath(root_fd, local, policy & (HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS),
                                target_flags, &resolved) != 0)
        return hl_linux_errno_result();
    if (strlen(resolved.leaf) >= sizeof(output->final)) {
        hl_host_resolved_path_destroy(&resolved);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    parent = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, resolved.parent_fd, NULL, NULL, 0, -1);
    if (parent.status != HL_STATUS_OK) {
        hl_host_resolved_path_destroy(&resolved);
        return parent;
    }
    resolved.parent_fd = -1;
    if (resolved.target_fd >= 0) {
        target = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, resolved.target_fd, NULL, NULL, 0, -1);
        if (target.status != HL_STATUS_OK) {
            (void)hl_linux_close_descriptor(host, parent.value);
            hl_host_resolved_path_destroy(&resolved);
            return target;
        }
        resolved.target_fd = -1;
    }
    memset(output, 0, sizeof(*output));
    output->parent = parent.value;
    output->target = target.value;
    output->final_size = strlen(resolved.leaf);
    memcpy(output->final, resolved.leaf, output->final_size + 1);
    if (output->target != HL_HOST_HANDLE_INVALID) {
        hl_host_file_metadata metadata;
        if (hl_linux_file_metadata_get(host, output->target, &metadata).status == HL_STATUS_OK)
            output->target_type = metadata.type;
    }
    hl_host_resolved_path_destroy(&resolved);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_open_beneath(void *context, hl_host_handle root, const char *path, size_t path_size,
                                                 uint32_t access, uint32_t creation, uint32_t permissions,
                                                 uint32_t policy) {
    hl_host_file_resolution resolved = {
        .parent = HL_HOST_HANDLE_INVALID,
        .target = HL_HOST_HANDLE_INVALID,
    };
    hl_host_result result;
    if (path == NULL || path_size == 0 || path[0] == '/' || memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    uint32_t resolve_policy = policy | HL_HOST_RESOLVE_ALLOW_MISSING;
    if ((creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        resolve_policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_linux_file_resolve_beneath(context, root, path, path_size, resolve_policy, &resolved);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_linux_file_open(context, resolved.parent, resolved.final, resolved.final_size,
                                access | HL_HOST_FILE_NOFOLLOW, creation, permissions);
    if (resolved.target != HL_HOST_HANDLE_INVALID) (void)hl_linux_close_descriptor(context, resolved.target);
    (void)hl_linux_close_descriptor(context, resolved.parent);
    return result;
}

static int hl_linux_write_zeros(int descriptor, off_t begin, off_t end) {
    static const unsigned char zeros[65536];
    while (begin < end) {
        size_t request = (uint64_t)(end - begin) < sizeof(zeros) ? (size_t)(end - begin) : sizeof(zeros);
        ssize_t count = pwrite(descriptor, zeros, request, begin);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) {
            if (count == 0) errno = EIO;
            return -1;
        }
        begin += count;
    }
    return 0;
}

static hl_host_result hl_linux_file_allocate_range(void *context, hl_host_handle file, uint32_t mode, uint64_t offset,
                                                   uint64_t size) {
    const uint32_t keep = HL_HOST_FILE_ALLOC_KEEP_SIZE;
    const uint32_t punch = HL_HOST_FILE_ALLOC_PUNCH_HOLE;
    const uint32_t collapse = HL_HOST_FILE_ALLOC_COLLAPSE_RANGE;
    const uint32_t zero = HL_HOST_FILE_ALLOC_ZERO_RANGE;
    const uint32_t insert = HL_HOST_FILE_ALLOC_INSERT_RANGE;
    const uint32_t unshare = HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    const uint32_t allowed = HL_HOST_FILE_ALLOC_KEEP_SIZE | HL_HOST_FILE_ALLOC_PUNCH_HOLE |
                             HL_HOST_FILE_ALLOC_COLLAPSE_RANGE | HL_HOST_FILE_ALLOC_ZERO_RANGE |
                             HL_HOST_FILE_ALLOC_INSERT_RANGE | HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    hl_host_linux *host = context;
    struct stat metadata;
    int descriptor;
    if (size == 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size || (mode & ~allowed) != 0 ||
        ((mode & punch) != 0 && (mode & keep) == 0) || ((mode & collapse) != 0 && mode != collapse) ||
        ((mode & insert) != 0 && mode != insert) || ((mode & unshare) != 0 && (mode & ~(unshare | keep)) != 0))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fallocate(descriptor, (int)mode, (off_t)offset, (off_t)size) == 0) return hl_linux_result(HL_STATUS_OK, 0, 0);
    if ((mode & zero) == 0 || (errno != EOPNOTSUPP && errno != ENOSYS)) return hl_linux_errno_result();
    if (fstat(descriptor, &metadata) != 0) return hl_linux_errno_result();
    off_t begin = (off_t)offset;
    off_t end = begin + (off_t)size;
    off_t zero_end = (mode & keep) != 0 && end > metadata.st_size ? metadata.st_size : end;
    if ((mode & keep) == 0 && end > metadata.st_size && ftruncate(descriptor, end) != 0) return hl_linux_errno_result();
    if (begin < zero_end && hl_linux_write_zeros(descriptor, begin, zero_end) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_filesystem_metadata(void *context, hl_host_handle file,
                                                        hl_host_filesystem_metadata *output) {
    hl_host_linux *host = context;
    struct statfs status;
    int descriptor;
    if (output == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstatfs(descriptor, &status) != 0) return hl_linux_errno_result();
    memset(output, 0, sizeof(*output));
    output->blocks = status.f_blocks;
    output->blocks_free = status.f_bfree;
    output->blocks_available = status.f_bavail;
    output->files = status.f_files;
    output->files_free = status.f_ffree;
    output->filesystem_id[0] = (uint32_t)status.f_fsid.__val[0];
    output->filesystem_id[1] = (uint32_t)status.f_fsid.__val[1];
    output->block_size = (uint64_t)status.f_bsize;
    output->fragment_size = (uint64_t)status.f_bsize;
    output->name_max = NAME_MAX;
    output->flags = (uint64_t)status.f_flags;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

typedef struct hl_linux_dirent64 {
    uint64_t object;
    int64_t offset;
    uint16_t record_size;
    uint8_t type;
    char name[];
} hl_linux_dirent64;

static hl_host_result hl_linux_file_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                                   uint32_t entry_capacity, uint32_t byte_capacity) {
    hl_host_linux *host = context;
    int descriptor;
    uint8_t *buffer;
    long count;
    uint32_t at = 0, produced = 0;
    if (entries == NULL || entry_capacity == 0 || byte_capacity < 24 || byte_capacity > UINT32_C(1 << 20))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    buffer = malloc(byte_capacity);
    if (buffer == NULL) return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    do
        count = syscall(SYS_getdents64, descriptor, buffer, byte_capacity);
    while (count < 0 && errno == EINTR);
    if (count < 0) {
        free(buffer);
        return hl_linux_errno_result();
    }
    while (at < (uint32_t)count) {
        const hl_linux_dirent64 *native = (const hl_linux_dirent64 *)(buffer + at);
        size_t maximum, name_size;
        if (native->record_size < 24 || native->record_size > (uint32_t)count - at || produced == entry_capacity) {
            free(buffer);
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        }
        maximum = native->record_size - offsetof(hl_linux_dirent64, name);
        name_size = strnlen(native->name, maximum);
        if (name_size == maximum || name_size > 255) {
            free(buffer);
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        }
        entries[produced].object = native->object;
        entries[produced].next_offset = (uint64_t)native->offset;
        entries[produced].type = native->type;
        entries[produced].name_size = (uint32_t)name_size;
        memcpy(entries[produced].name, native->name, name_size + 1);
        produced++;
        at += native->record_size;
    }
    free(buffer);
    return hl_linux_result(HL_STATUS_OK, produced, (uint64_t)count);
}

static hl_host_result hl_linux_file_path(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_linux *host = context;
    char link[64];
    char path[PATH_MAX];
    int descriptor;
    ssize_t length;
    if ((output.size != 0 && output.data == 0) || output.size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) {
        snprintf(link, sizeof link, "/proc/self/fd/%d", descriptor);
        length = readlink(link, path, sizeof path);
    } else {
        length = -1;
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (length < 0) return hl_linux_errno_result();
    if ((uint64_t)length > output.size) return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, (uint64_t)length, 0);
    if (length != 0) memcpy(output.data, path, (size_t)length);
    return hl_linux_result(HL_STATUS_OK, (uint64_t)length, 0);
}

static hl_host_result hl_linux_close_descriptor(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_NONE);
}

static hl_host_result hl_linux_close_descriptor_kind(void *context, hl_host_handle handle,
                                                     hl_linux_handle_kind expected) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)handle;
    hl_linux_handle_entry *entry;
    int descriptor;
    int wake_descriptor;
    int result;
    if (low == 0 || low - 1u >= host->handle_capacity) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (expected == HL_LINUX_HANDLE_NONE || expected == HL_LINUX_HANDLE_COUNTER)
        hl_linux_counter_unsubscribe_all(host, handle);
    pthread_mutex_lock(&host->lock);
    entry = &host->handles[low - 1u];
    if (entry->generation != (uint32_t)(handle >> 32) || entry->kind == HL_LINUX_HANDLE_NONE ||
        entry->kind == HL_LINUX_HANDLE_MAPPING || (expected != HL_LINUX_HANDLE_NONE && entry->kind != expected)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = entry->descriptor;
    wake_descriptor = entry->wake_descriptor;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->wake_descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    hl_host_process_fd_private_remove(wake_descriptor);
    result = close(descriptor);
    if (wake_descriptor >= 0) close(wake_descriptor);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_FILE);
}

static hl_host_result hl_linux_network_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_SOCKET);
}

static hl_host_result hl_linux_shared_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_SHARED_MEMORY);
}

static hl_host_result hl_linux_counter_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_COUNTER);
}

static hl_host_result hl_linux_transfer_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_TRANSFER);
}

static uint32_t hl_linux_directory_mask(uint32_t interests) {
    uint32_t mask = 0;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) mask |= IN_ACCESS | IN_OPEN | IN_CLOSE;
    if ((interests & HL_HOST_DIRECTORY_MODIFY) != 0) mask |= IN_MODIFY | IN_CLOSE_WRITE;
    if ((interests & HL_HOST_DIRECTORY_CREATE) != 0) mask |= IN_CREATE;
    if ((interests & HL_HOST_DIRECTORY_DELETE) != 0) mask |= IN_DELETE | IN_DELETE_SELF;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) mask |= IN_MOVED_FROM | IN_MOVED_TO | IN_MOVE_SELF;
    if ((interests & HL_HOST_DIRECTORY_ATTRIB) != 0) mask |= IN_ATTRIB;
    if ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0) mask |= IN_ONESHOT;
    return mask;
}

static hl_linux_directory_watch *hl_linux_directory_watch_for_token(hl_linux_directory_object *object, uint64_t token) {
    uint32_t index;
    for (index = 0; index < object->watch_capacity; ++index)
        if (object->watches[index].active != 0 && object->watches[index].token == token) return &object->watches[index];
    return NULL;
}

static hl_linux_directory_watch *hl_linux_directory_watch_for_id(hl_linux_directory_object *object, int watch) {
    uint32_t index;
    for (index = 0; index < object->watch_capacity; ++index)
        if (object->watches[index].watch == watch) return &object->watches[index];
    return NULL;
}

static hl_host_result hl_linux_directory_create(void *context) {
    hl_host_linux *host = context;
    hl_linux_directory_object *object = calloc(1, sizeof(*object));
    hl_host_result result;
    uint32_t index;
    int descriptor;
    if (object == NULL) return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    descriptor = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (descriptor < 0) {
        free(object);
        return hl_linux_errno_result();
    }
    object->references = 1;
    object->watch_capacity = HL_LINUX_DIRECTORY_WATCHES;
    object->watches = calloc(object->watch_capacity, sizeof(*object->watches));
    if (object->watches == NULL) {
        close(descriptor);
        free(object);
        return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    for (index = 0; index < object->watch_capacity; ++index) {
        object->watches[index].watch = -1;
    }
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_DIRECTORY, descriptor, object, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        free(object->watches);
        free(object);
    }
    return result;
}

static hl_host_result hl_linux_directory_add(void *context, hl_host_handle instance, hl_host_handle file,
                                             uint64_t token, uint32_t interests) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *instance_entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot = NULL;
    char path[64];
    int file_descriptor;
    int watch;
    uint32_t index;
    uint32_t valid = UINT32_C(0x8000007f);
    if (token == 0 || (interests & ~valid) != 0 || (interests & ~HL_HOST_DIRECTORY_ONESHOT) == 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    instance_entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = instance_entry == NULL ? NULL : instance_entry->address;
    file_descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (object != NULL && hl_linux_directory_watch_for_token(object, token) == NULL) {
        for (index = 0; index < object->watch_capacity; ++index)
            if (object->watches[index].watch < 0) {
                slot = &object->watches[index];
                break;
            }
        if (slot == NULL) {
            uint32_t capacity;
            hl_linux_directory_watch *grown = hl_linux_grow_capacity(object->watch_capacity, HL_LINUX_DIRECTORY_WATCHES,
                                                                     sizeof(*object->watches), &capacity) == 0
                                                  ? realloc(object->watches, (size_t)capacity * sizeof(*grown))
                                                  : NULL;
            if (grown != NULL) {
                for (index = object->watch_capacity; index < capacity; ++index) {
                    grown[index] = (hl_linux_directory_watch){0};
                    grown[index].watch = -1;
                }
                slot = &grown[object->watch_capacity];
                object->watches = grown;
                object->watch_capacity = capacity;
            }
        }
    }
    if (instance_entry == NULL || slot == NULL || file_descriptor < 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(slot == NULL && object != NULL ? HL_STATUS_RESOURCE_LIMIT : HL_STATUS_INVALID_ARGUMENT,
                               0, 0);
    }
    snprintf(path, sizeof(path), "/proc/self/fd/%d", file_descriptor);
    watch = inotify_add_watch(instance_entry->descriptor, path, hl_linux_directory_mask(UINT32_C(0x7f)));
    if (watch >= 0) *slot = (hl_linux_directory_watch){watch, token, interests, 1};
    pthread_mutex_unlock(&host->lock);
    if (watch < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_directory_modify(void *context, hl_host_handle instance, uint64_t token,
                                                uint32_t interests) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot;
    uint32_t valid = UINT32_C(0x8000007f);
    if (token == 0 || (interests & ~valid) != 0 || (interests & ~HL_HOST_DIRECTORY_ONESHOT) == 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    slot = object == NULL ? NULL : hl_linux_directory_watch_for_token(object, token);
    if (slot == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    slot->interests = interests;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_directory_remove(void *context, hl_host_handle instance, uint64_t token) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    slot = object == NULL ? NULL : hl_linux_directory_watch_for_token(object, token);
    if (slot == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    result = inotify_rm_watch(entry->descriptor, slot->watch);
    slot->active = 0;
    slot->watch = -1; /* free the slot so the `watch < 0` scan in add_watch reuses it */
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static uint32_t hl_linux_directory_changes(uint32_t mask) {
    uint32_t changes = 0;
    if ((mask & (IN_ACCESS | IN_OPEN | IN_CLOSE)) != 0) changes |= HL_HOST_DIRECTORY_ACCESS;
    if ((mask & (IN_MODIFY | IN_CLOSE_WRITE)) != 0) changes |= HL_HOST_DIRECTORY_MODIFY;
    if ((mask & IN_CREATE) != 0) changes |= HL_HOST_DIRECTORY_CREATE;
    if ((mask & (IN_DELETE | IN_DELETE_SELF)) != 0) changes |= HL_HOST_DIRECTORY_DELETE;
    if ((mask & (IN_MOVED_FROM | IN_MOVED_TO | IN_MOVE_SELF)) != 0) changes |= HL_HOST_DIRECTORY_RENAME;
    if ((mask & IN_ATTRIB) != 0) changes |= HL_HOST_DIRECTORY_ATTRIB;
    if ((mask & (IN_IGNORED | IN_Q_OVERFLOW)) != 0) changes |= HL_HOST_DIRECTORY_IGNORED;
    return changes;
}

static int hl_linux_directory_append(hl_linux_directory_object *object, hl_host_directory_record record) {
    if (object->pending_count == object->pending_capacity) {
        uint32_t capacity;
        hl_host_directory_record *pending =
            hl_linux_grow_capacity(object->pending_capacity, 32u, sizeof(*object->pending), &capacity) == 0
                ? realloc(object->pending, (size_t)capacity * sizeof(*pending))
                : NULL;
        if (pending == NULL) return -1;
        object->pending = pending;
        object->pending_capacity = capacity;
    }
    object->pending[object->pending_count++] = record;
    return 0;
}

static hl_host_result hl_linux_directory_read(void *context, hl_host_handle instance, hl_host_directory_record *records,
                                              uint32_t capacity) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    _Alignas(struct inotify_event) char buffer[16384];
    ssize_t size;
    size_t offset;
    uint32_t count;
    if (records == NULL || capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    if (object == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (object->pending_count == 0) {
        size = read(entry->descriptor, buffer, sizeof(buffer));
        if (size < 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_errno_result();
        }
        for (offset = 0; offset < (size_t)size;) {
            const struct inotify_event *event = (const struct inotify_event *)(buffer + offset);
            hl_linux_directory_watch *watch = hl_linux_directory_watch_for_id(object, event->wd);
            uint64_t token = watch == NULL ? 0 : watch->token;
            uint32_t changes = hl_linux_directory_changes(event->mask);
            int oneshot = 0;
            if (watch != NULL) {
                changes = (changes & watch->interests) | (changes & HL_HOST_DIRECTORY_IGNORED);
                oneshot = (watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0 &&
                          (changes & ~(uint32_t)HL_HOST_DIRECTORY_IGNORED) != 0;
                if (oneshot) changes |= HL_HOST_DIRECTORY_IGNORED;
            } else {
                changes = 0;
            }
            if (changes != 0 && hl_linux_directory_append(object, (hl_host_directory_record){token, changes, 0}) != 0) {
                pthread_mutex_unlock(&host->lock);
                return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
            }
            if (oneshot) {
                (void)inotify_rm_watch(entry->descriptor, watch->watch);
                *watch = (hl_linux_directory_watch){-1, 0, 0, 0};
            } else if ((event->mask & IN_IGNORED) != 0 && watch != NULL) {
                *watch = (hl_linux_directory_watch){-1, 0, 0, 0};
            }
            offset += sizeof(*event) + event->len;
        }
    }
    count = capacity < object->pending_count ? capacity : object->pending_count;
    if (count != 0) memcpy(records, object->pending, count * sizeof(*records));
    object->pending_count -= count;
    if (object->pending_count != 0)
        memmove(object->pending, object->pending + count, object->pending_count * sizeof(*object->pending));
    pthread_mutex_unlock(&host->lock);
    return count == 0 ? hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0) : hl_linux_result(HL_STATUS_OK, count, 0);
}

static hl_host_result hl_linux_directory_duplicate(void *context, hl_host_handle instance) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    int descriptor;
    hl_host_result result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    descriptor = entry == NULL ? -1 : fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor >= 0) object->references++;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_DIRECTORY, descriptor, object, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_lock(&host->lock);
        object->references--;
        pthread_mutex_unlock(&host->lock);
        close(descriptor);
    }
    return result;
}

static hl_host_result hl_linux_directory_close(void *context, hl_host_handle instance) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = entry->descriptor;
    object = entry->address;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->address = NULL;
    if (--object->references == 0) {
        free(object->pending);
        free(object->watches);
        free(object);
    }
    pthread_mutex_unlock(&host->lock);
    // The dir fd was privatized (relocated + registered) by hl_linux_allocate_handle; drop its private-registry
    // cell before closing, exactly as hl_linux_close_descriptor_kind does -- otherwise the cell leaks and, once
    // the OS reuses this fd number, hl_host_process_fd_private_is() misclassifies a guest fd as engine-private.
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

// `descriptor` is the live watched fd held by the owning handle entry (entry->wake_descriptor). The watch
// struct must NOT cache the fd it was opened with: hl_linux_allocate_handle privatizes (relocates + closes)
// the passed descriptor via hl_host_process_fd_private_adopt, so any pre-adoption copy is a stale, closed fd.
static int hl_linux_watch_refresh(hl_linux_watch *watch, int descriptor) {
    struct stat status;
    if (fstat(descriptor, &status) != 0) return -1;
    uint64_t modified = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    uint64_t changed = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    uint64_t size = status.st_size < 0 ? 0 : (uint64_t)status.st_size;
    uint32_t changes = 0;
    if ((uint64_t)status.st_dev != watch->record.stable_device ||
        (uint64_t)status.st_ino != watch->record.stable_object)
        changes |= HL_HOST_WATCH_IDENTITY;
    if (size != watch->record.size) changes |= HL_HOST_WATCH_SIZE;
    if (modified != watch->modified_ns || changed != watch->changed_ns) changes |= HL_HOST_WATCH_DATA;
    if (status.st_nlink == 0 && watch->links != 0) changes |= HL_HOST_WATCH_DELETED;
    if (changes != 0) {
        if (watch->record.generation != watch->delivered_generation) changes |= watch->record.changes;
        watch->record.generation++;
        if (watch->record.generation == 0) watch->record.generation = 1;
        watch->record.stable_device = (uint64_t)status.st_dev;
        watch->record.stable_object = (uint64_t)status.st_ino;
        watch->record.size = size;
        watch->record.changes = changes;
        watch->modified_ns = modified;
        watch->changed_ns = changed;
        watch->links = status.st_nlink;
    }
    return 0;
}

static hl_host_result hl_linux_watch_open(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int source, watched = -1, notify = -1, watch_id = -1;
    pthread_mutex_lock(&host->lock);
    source = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    if (source >= 0) watched = fcntl(source, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (watched < 0) return source < 0 ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_linux_errno_result();
    notify = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    char path[64];
    if (notify >= 0 && snprintf(path, sizeof path, "/proc/self/fd/%d", watched) < (int)sizeof path)
        watch_id = inotify_add_watch(notify, path, IN_MODIFY | IN_ATTRIB | IN_MOVE_SELF | IN_DELETE_SELF);
    struct stat status;
    if (notify < 0 || watch_id < 0 || fstat(watched, &status) != 0) {
        hl_host_result error = hl_linux_errno_result();
        if (notify >= 0) close(notify);
        close(watched);
        return error;
    }
    hl_linux_watch *watch = calloc(1, sizeof(*watch));
    if (watch == NULL) {
        close(notify);
        close(watched);
        return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    watch->watch_id = watch_id;
    watch->record = (hl_host_watch_record){
        1, (uint64_t)status.st_dev, (uint64_t)status.st_ino, status.st_size < 0 ? 0 : (uint64_t)status.st_size, 0, 0};
    watch->delivered_generation = 1;
    watch->modified_ns = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    watch->changed_ns = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    watch->links = status.st_nlink;
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_WATCH, notify, watch, NULL, 0, watched);
    if (result.status != HL_STATUS_OK) {
        close(notify);
        close(watched);
        free(watch);
    }
    return result;
}

static hl_host_result hl_linux_watch_query(void *context, hl_host_handle handle, hl_host_watch_record *record) {
    hl_host_linux *host = context;
    if (record == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    hl_linux_watch *watch = entry == NULL ? NULL : entry->address;
    int result = watch == NULL ? -2 : hl_linux_watch_refresh(watch, entry->wake_descriptor);
    if (result == 0) *record = watch->record;
    pthread_mutex_unlock(&host->lock);
    if (result == -2) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_watch_drain(void *context, hl_host_handle handle, hl_host_watch_record *records,
                                           size_t capacity) {
    hl_host_linux *host = context;
    char buffer[4096];
    if (records == NULL || capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    hl_linux_watch *watch = entry == NULL ? NULL : entry->address;
    if (watch == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    uint32_t native_changes = 0;
    for (;;) {
        ssize_t count = read(entry->descriptor, buffer, sizeof buffer);
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) break;
        if (count <= 0) break;
        for (size_t offset = 0; offset < (size_t)count;) {
            struct inotify_event *event = (struct inotify_event *)(void *)(buffer + offset);
            native_changes |= event->mask;
            offset += sizeof(*event) + event->len;
        }
    }
    if (native_changes != 0) {
        watch->record.generation++;
        if (watch->record.generation == 0) watch->record.generation = 1;
        watch->record.changes = 0;
        if (native_changes & (IN_MODIFY | IN_ATTRIB)) watch->record.changes |= HL_HOST_WATCH_DATA;
        if (native_changes & IN_MOVE_SELF) watch->record.changes |= HL_HOST_WATCH_IDENTITY;
        if (native_changes & (IN_DELETE_SELF | IN_IGNORED)) watch->record.changes |= HL_HOST_WATCH_DELETED;
    }
    int refreshed = hl_linux_watch_refresh(watch, entry->wake_descriptor);
    int available = refreshed == 0 && watch->record.generation != watch->delivered_generation;
    if (available) {
        records[0] = watch->record;
        watch->delivered_generation = watch->record.generation;
    }
    pthread_mutex_unlock(&host->lock);
    if (refreshed < 0) return hl_linux_errno_result();
    return available ? hl_linux_result(HL_STATUS_OK, 1, 0) : hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
}

static hl_host_result hl_linux_watch_close(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)handle;
    if (low == 0 || low - 1u >= host->handle_capacity) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int notify = entry->descriptor, watched = entry->wake_descriptor;
    hl_linux_watch *watch = entry->address;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->wake_descriptor = -1;
    entry->address = NULL;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(notify);
    hl_host_process_fd_private_remove(watched);
    close(notify);
    close(watched);
    free(watch);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_event_create(void *context) {
    hl_host_linux *host = context;
    struct epoll_event event = {0};
    int pollset = epoll_create1(EPOLL_CLOEXEC);
    int wake;
    if (pollset < 0) return hl_linux_errno_result();
    wake = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (wake < 0) {
        close(pollset);
        return hl_linux_errno_result();
    }
    event.events = EPOLLIN;
    event.data.u64 = 0;
    if (epoll_ctl(pollset, EPOLL_CTL_ADD, wake, &event) != 0) {
        close(wake);
        close(pollset);
        return hl_linux_errno_result();
    }
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_POLLSET, pollset, NULL, NULL, 0, wake);
    if (result.status != HL_STATUS_OK) {
        close(wake);
        close(pollset);
    }
    return result;
}

static uint32_t hl_linux_epoll_events(uint32_t interests) {
    uint32_t events = 0;
    if ((interests & HL_HOST_READY_READ) != 0) events |= EPOLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) events |= EPOLLOUT;
    if ((interests & HL_HOST_READY_EDGE) != 0) events |= EPOLLET;
    if ((interests & HL_HOST_READY_ONESHOT) != 0) events |= EPOLLONESHOT;
    return events;
}

static hl_linux_timer_entry *hl_linux_event_timer(hl_host_linux *host, hl_host_handle pollset, uint64_t token);

static hl_host_result hl_linux_event_control(void *context, hl_host_handle pollset, uint32_t operation,
                                             hl_host_handle object, uint64_t token, uint32_t interests) {
    hl_host_linux *host = context;
    struct epoll_event event = {hl_linux_epoll_events(interests), {.u64 = token}};
    int pollset_fd;
    int object_fd;
    int native_operation;
    pthread_mutex_lock(&host->lock);
    pollset_fd = hl_linux_descriptor(host, pollset, HL_LINUX_HANDLE_POLLSET, HL_LINUX_HANDLE_POLLSET);
    object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SOCKET);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_COUNTER, HL_LINUX_HANDLE_COUNTER);
    if (object_fd < 0)
        object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_DIRECTORY, HL_LINUX_HANDLE_DIRECTORY);
    if (object_fd < 0)
        object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_WATCH, HL_LINUX_HANDLE_WATCH);
    if (pollset_fd < 0 || object_fd < 0 || token == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (operation == HL_HOST_EVENT_ADD)
        native_operation = EPOLL_CTL_ADD;
    else if (operation == HL_HOST_EVENT_MODIFY)
        native_operation = EPOLL_CTL_MOD;
    else if (operation == HL_HOST_EVENT_DELETE)
        native_operation = EPOLL_CTL_DEL;
    else {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int result = epoll_ctl(pollset_fd, native_operation, object_fd, &event);
    hl_host_result output = result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
    pthread_mutex_unlock(&host->lock);
    return output;
}

static hl_host_result hl_linux_event_wait(void *context, hl_host_handle pollset, hl_host_event_record *events,
                                          size_t event_capacity, uint64_t deadline_ns) {
    hl_host_linux *host = context;
    struct epoll_event native_events[64];
    int pollset_fd;
    int wake_descriptor;
    int timeout;
    int count;
    size_t i;
    if (events == NULL || event_capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (event_capacity > HL_ARRAY_COUNT(native_events)) event_capacity = HL_ARRAY_COUNT(native_events);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    pollset_fd = pollset_entry == NULL ? -1 : pollset_entry->descriptor;
    wake_descriptor = pollset_entry == NULL ? -1 : pollset_entry->wake_descriptor;
    pthread_mutex_unlock(&host->lock);
    if (pollset_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (deadline_ns == HL_HOST_DEADLINE_INFINITE)
        timeout = -1;
    else {
        uint64_t now = hl_linux_monotonic_value();
        uint64_t remaining = deadline_ns > now ? deadline_ns - now : 0;
        uint64_t milliseconds = (remaining + UINT64_C(999999)) / UINT64_C(1000000);
        timeout = milliseconds > INT_MAX ? INT_MAX : (int)milliseconds;
    }
    count = epoll_wait(pollset_fd, native_events, (int)event_capacity, timeout);
    if (count < 0) return hl_linux_errno_result();
    size_t output_count = 0;
    for (i = 0; i < (size_t)count; ++i) {
        uint32_t ready = 0;
        if (native_events[i].data.u64 == 0) {
            uint64_t ignored;
            while (read(wake_descriptor, &ignored, sizeof(ignored)) == (ssize_t)sizeof(ignored)) {}
            continue;
        }
        pthread_mutex_lock(&host->lock);
        hl_linux_timer_entry *timer = hl_linux_event_timer(host, pollset, native_events[i].data.u64);
        int timer_descriptor = timer == NULL ? -1 : timer->descriptor;
        pthread_mutex_unlock(&host->lock);
        if (timer_descriptor >= 0) {
            uint64_t expirations;
            ssize_t consumed = read(timer_descriptor, &expirations, sizeof(expirations));
            (void)consumed;
            ready |= HL_HOST_READY_TIMER;
        }
        if ((native_events[i].events & EPOLLIN) != 0) ready |= HL_HOST_READY_READ;
        if ((native_events[i].events & EPOLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
        if ((native_events[i].events & EPOLLERR) != 0) ready |= HL_HOST_READY_ERROR;
        if ((native_events[i].events & EPOLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
        events[output_count++] = (hl_host_event_record){native_events[i].data.u64, ready, 0};
    }
    return hl_linux_result(HL_STATUS_OK, output_count, 0);
}

static hl_host_result hl_linux_event_wake(void *context, hl_host_handle pollset) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    uint64_t one = 1;
    ssize_t result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = write(entry->wake_descriptor, &one, sizeof(one));
    pthread_mutex_unlock(&host->lock);
    return result == (ssize_t)sizeof(one) ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_linux_timer_entry *hl_linux_event_timer(hl_host_linux *host, hl_host_handle pollset, uint64_t token) {
    uint32_t index;
    for (index = 0; index < host->timer_capacity; ++index)
        if (host->timers[index].descriptor >= 0 && host->timers[index].pollset == pollset &&
            host->timers[index].token == token)
            return &host->timers[index];
    return NULL;
}

static hl_host_result hl_linux_event_arm_timer(void *context, hl_host_handle pollset, uint64_t token,
                                               uint64_t deadline_ns, uint64_t interval_ns) {
    hl_host_linux *host = context;
    hl_linux_timer_entry *timer;
    hl_linux_handle_entry *pollset_entry;
    struct itimerspec setting = {0};
    struct epoll_event event = {EPOLLIN, {.u64 = token}};
    int descriptor;
    uint32_t index;
    if (token == 0 || deadline_ns == HL_HOST_DEADLINE_INFINITE)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    if (pollset_entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    timer = hl_linux_event_timer(host, pollset, token);
    if (timer == NULL) {
        for (index = 0; index < host->timer_capacity; ++index)
            if (host->timers[index].descriptor < 0) {
                timer = &host->timers[index];
                break;
            }
        if (timer == NULL) {
            uint32_t capacity;
            hl_linux_timer_entry *grown = hl_linux_grow_capacity(host->timer_capacity, HL_LINUX_TIMER_CAPACITY,
                                                                 sizeof(*host->timers), &capacity) == 0
                                              ? realloc(host->timers, (size_t)capacity * sizeof(*grown))
                                              : NULL;
            if (grown != NULL) {
                for (index = host->timer_capacity; index < capacity; ++index) {
                    grown[index] = (hl_linux_timer_entry){0};
                    grown[index].descriptor = -1;
                }
                timer = &grown[host->timer_capacity];
                host->timers = grown;
                host->timer_capacity = capacity;
            }
        }
        if (timer == NULL) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        descriptor = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
        if (descriptor < 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_errno_result();
        }
        if (epoll_ctl(pollset_entry->descriptor, EPOLL_CTL_ADD, descriptor, &event) != 0) {
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            pthread_mutex_unlock(&host->lock);
            return error;
        }
        timer->pollset = pollset;
        timer->token = token;
        timer->descriptor = descriptor;
    }
    if (deadline_ns <= hl_linux_monotonic_value()) deadline_ns = hl_linux_monotonic_value() + 1;
    setting.it_value.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    setting.it_value.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    setting.it_interval.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    setting.it_interval.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    descriptor = timer->descriptor;
    int configured = timerfd_settime(descriptor, TFD_TIMER_ABSTIME, &setting, NULL);
    pthread_mutex_unlock(&host->lock);
    return configured == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_event_disarm_timer(void *context, hl_host_handle pollset, uint64_t token) {
    hl_host_linux *host = context;
    hl_linux_timer_entry *timer;
    hl_linux_handle_entry *pollset_entry;
    int descriptor;
    if (token == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    timer = pollset_entry == NULL ? NULL : hl_linux_event_timer(host, pollset, token);
    if (timer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    descriptor = timer->descriptor;
    timer->descriptor = -1;
    timer->pollset = HL_HOST_HANDLE_INVALID;
    timer->token = 0;
    (void)epoll_ctl(pollset_entry->descriptor, EPOLL_CTL_DEL, descriptor, NULL);
    pthread_mutex_unlock(&host->lock);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_event_close(void *context, hl_host_handle pollset) {
    hl_host_linux *host = context;
    uint32_t index;
    pthread_mutex_lock(&host->lock);
    if (hl_linux_descriptor(host, pollset, HL_LINUX_HANDLE_POLLSET, HL_LINUX_HANDLE_POLLSET) < 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (index = 0; index < host->timer_capacity; ++index) {
        hl_linux_timer_entry *timer = &host->timers[index];
        if (timer->descriptor < 0 || timer->pollset != pollset) continue;
        close(timer->descriptor);
        timer->descriptor = -1;
        timer->pollset = HL_HOST_HANDLE_INVALID;
        timer->token = 0;
    }
    pthread_mutex_unlock(&host->lock);
    return hl_linux_close_descriptor(context, pollset);
}

static hl_host_result hl_linux_counter_create(void *context, uint64_t initial, uint32_t flags) {
    hl_host_linux *host = context;
    int native_flags = EFD_CLOEXEC;
    int descriptor;
    hl_host_result result;
    if (initial == UINT64_MAX || (flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((flags & HL_HOST_COUNTER_SEMAPHORE) != 0) native_flags |= EFD_SEMAPHORE;
    if ((flags & HL_HOST_COUNTER_NONBLOCK) != 0) native_flags |= EFD_NONBLOCK;
    descriptor = eventfd(0, native_flags);
    if (descriptor < 0) return hl_linux_errno_result();
    if (initial != 0 && write(descriptor, &initial, sizeof(initial)) != (ssize_t)sizeof(initial)) {
        hl_host_result error = hl_linux_errno_result();
        close(descriptor);
        return error;
    }
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_COUNTER, descriptor, NULL, NULL, flags, -1);
    if (result.status == HL_STATUS_OK) {
        pthread_mutex_lock(&host->lock);
        hl_linux_lookup_locked(host, result.value, HL_LINUX_HANDLE_COUNTER)->reserved =
            (uint16_t)(HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT |
                       HL_HOST_TRANSFER_CONTROL);
        pthread_mutex_unlock(&host->lock);
    }
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static hl_host_result hl_linux_counter_read(void *context, hl_host_handle counter) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    uint64_t value;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_READ) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = entry == NULL ? -1 : entry->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return read(descriptor, &value, sizeof(value)) == (ssize_t)sizeof(value) ? hl_linux_result(HL_STATUS_OK, value, 0)
                                                                             : hl_linux_errno_result();
}

static hl_host_result hl_linux_counter_write(void *context, hl_host_handle counter, uint64_t value) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int descriptor;
    if (value == UINT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_WRITE) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = entry == NULL ? -1 : entry->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return write(descriptor, &value, sizeof(value)) == (ssize_t)sizeof(value) ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                              : hl_linux_errno_result();
}

static hl_host_result hl_linux_counter_get_flags(void *context, hl_host_handle counter) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    uint64_t flags;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_CONTROL) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    flags = entry == NULL ? UINT64_MAX : entry->size;
    pthread_mutex_unlock(&host->lock);
    return flags == UINT64_MAX ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0)
                               : hl_linux_result(HL_STATUS_OK, flags, 0);
}

static hl_host_result hl_linux_counter_set_flags(void *context, hl_host_handle counter, uint32_t flags) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int descriptor;
    int native;
    if ((flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_CONTROL) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    if (entry == NULL || ((uint32_t)entry->size & HL_HOST_COUNTER_SEMAPHORE) != (flags & HL_HOST_COUNTER_SEMAPHORE)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = entry->descriptor;
    native = fcntl(descriptor, F_GETFL);
    if (native >= 0 &&
        fcntl(descriptor, F_SETFL, (native & ~O_NONBLOCK) | ((flags & HL_HOST_COUNTER_NONBLOCK) ? O_NONBLOCK : 0)) == 0)
        entry->size = flags;
    else
        descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    return descriptor < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_counter_duplicate(void *context, hl_host_handle counter) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int descriptor;
    uint64_t flags;
    uint16_t rights;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    descriptor = entry == NULL ? -1 : dup(entry->descriptor);
    flags = entry == NULL ? 0 : entry->size;
    rights = entry == NULL ? 0 : entry->reserved;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_errno_result();
    {
        hl_host_result result =
            hl_linux_allocate_handle(host, HL_LINUX_HANDLE_COUNTER, descriptor, NULL, NULL, flags, -1);
        if (result.status != HL_STATUS_OK) close(descriptor);
        if (result.status == HL_STATUS_OK) {
            pthread_mutex_lock(&host->lock);
            hl_linux_lookup_locked(host, result.value, HL_LINUX_HANDLE_COUNTER)->reserved = rights;
            pthread_mutex_unlock(&host->lock);
        }
        return result;
    }
}

static hl_host_result hl_linux_counter_readiness(void *context, hl_host_handle counter, uint32_t interests) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    struct pollfd descriptor;
    int result;
    if ((interests & ~(uint32_t)HL_HOST_READY_READ) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_WAIT) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = (struct pollfd){entry == NULL ? -1 : entry->descriptor, POLLIN, 0};
    pthread_mutex_unlock(&host->lock);
    if (descriptor.fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = poll(&descriptor, 1, 0);
    return result < 0 ? hl_linux_errno_result()
                      : hl_linux_result(HL_STATUS_OK, result == 1 ? HL_HOST_READY_READ & interests : 0, 0);
}

static void *hl_linux_counter_subscription_main(void *opaque) {
    hl_linux_counter_subscription *subscription = opaque;
    int notified = 0;
    for (;;) {
        struct pollfd descriptors[2] = {{subscription->descriptor, POLLIN, 0}, {subscription->wake[0], POLLIN, 0}};
        int result = poll(descriptors, 2, notified ? 1 : -1);
        if (result < 0 && errno == EINTR) continue;
        if (descriptors[1].revents != 0) break;
        if (descriptors[0].revents & POLLIN) {
            if (!notified) subscription->notify(subscription->observer, subscription->token);
            notified = 1;
        } else if (result == 0) {
            struct pollfd probe = {subscription->descriptor, POLLIN, 0};
            notified = poll(&probe, 1, 0) == 1;
        }
    }
    return NULL;
}

static hl_host_result hl_linux_counter_subscribe(void *context, hl_host_handle counter,
                                                 void (*notify)(void *, uint64_t), void *observer, uint64_t token) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_counter_subscription *subscription = NULL;
    uint32_t index;
    int descriptor;
    if (notify == NULL || token == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, counter, HL_LINUX_HANDLE_COUNTER);
    if (entry != NULL && (entry->reserved & HL_HOST_TRANSFER_WAIT) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = entry == NULL ? -1 : dup(entry->descriptor);
    for (index = 0; descriptor >= 0 && index < host->counter_subscription_capacity; ++index)
        if (host->counter_subscriptions[index] == NULL ||
            (!host->counter_subscriptions[index]->active && !host->counter_subscriptions[index]->retiring)) {
            subscription = host->counter_subscriptions[index];
            break;
        }
    if (descriptor >= 0 && index == host->counter_subscription_capacity) {
        uint32_t capacity;
        void *grown =
            hl_linux_grow_capacity(host->counter_subscription_capacity, HL_LINUX_COUNTER_SUBSCRIPTIONS_INITIAL,
                                   sizeof(*host->counter_subscriptions), &capacity) == 0
                ? realloc(host->counter_subscriptions, (size_t)capacity * sizeof(*host->counter_subscriptions))
                : NULL;
        if (grown != NULL) {
            host->counter_subscriptions = grown;
            memset(host->counter_subscriptions + host->counter_subscription_capacity, 0,
                   (size_t)(capacity - host->counter_subscription_capacity) * sizeof(*host->counter_subscriptions));
            host->counter_subscription_capacity = capacity;
            subscription = calloc(1, sizeof(*subscription));
            host->counter_subscriptions[index] = subscription;
        }
    } else if (descriptor >= 0 && subscription == NULL) {
        subscription = calloc(1, sizeof(*subscription));
        host->counter_subscriptions[index] = subscription;
    }
    if (subscription != NULL) {
        subscription->generation++;
        if (subscription->generation == 0) subscription->generation = 1;
        subscription->active = 1;
        subscription->host = host;
        subscription->counter = counter;
        subscription->descriptor = descriptor;
        subscription->notify = notify;
        subscription->observer = observer;
        subscription->token = token;
        subscription->wake[0] = -1;
        subscription->wake[1] = -1;
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (subscription == NULL) {
        close(descriptor);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    if (pipe2(subscription->wake, O_CLOEXEC | O_NONBLOCK) != 0 ||
        pthread_create(&subscription->thread, NULL, hl_linux_counter_subscription_main, subscription) != 0) {
        if (subscription->wake[0] >= 0) close(subscription->wake[0]);
        if (subscription->wake[1] >= 0) close(subscription->wake[1]);
        close(descriptor);
        pthread_mutex_lock(&host->lock);
        subscription->active = 0;
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    return hl_linux_result(HL_STATUS_OK, ((uint64_t)subscription->generation << 32) | (uint64_t)(index + 1u), 0);
}

static hl_host_result hl_linux_counter_unsubscribe(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)handle;
    hl_linux_counter_subscription *subscription;
    uint8_t byte = 1;
    ssize_t ignored;
    if (low == 0 || low > host->counter_subscription_capacity) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    subscription = host->counter_subscriptions[low - 1u];
    if (subscription == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (!subscription->active || subscription->generation != (uint32_t)(handle >> 32)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    subscription->active = 0;
    subscription->retiring = 1;
    pthread_mutex_unlock(&host->lock);
    ignored = write(subscription->wake[1], &byte, 1);
    (void)ignored;
    (void)pthread_join(subscription->thread, NULL);
    close(subscription->wake[0]);
    close(subscription->wake[1]);
    close(subscription->descriptor);
    pthread_mutex_lock(&host->lock);
    subscription->counter = HL_HOST_HANDLE_INVALID;
    subscription->notify = NULL;
    subscription->observer = NULL;
    subscription->retiring = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static void hl_linux_counter_unsubscribe_all(hl_host_linux *host, hl_host_handle counter) {
    uint32_t index;
    for (index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_host_handle subscription = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[index] != NULL && host->counter_subscriptions[index]->active &&
            host->counter_subscriptions[index]->counter == counter)
            subscription = ((uint64_t)host->counter_subscriptions[index]->generation << 32) | (uint64_t)(index + 1u);
        pthread_mutex_unlock(&host->lock);
        if (subscription != HL_HOST_HANDLE_INVALID) (void)hl_linux_counter_unsubscribe(host, subscription);
    }
}

typedef struct hl_linux_transfer_wire {
    uint32_t data_size;
    uint32_t attachment_count;
    uint32_t flags[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t rights[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint8_t data[HL_HOST_TRANSFER_MAX_DATA];
} hl_linux_transfer_wire;

static hl_host_result hl_linux_transfer_channel_pair(void *context) {
    hl_host_linux *host = context;
    int pair[2];
    hl_host_result first;
    hl_host_result second;
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, pair) != 0) return hl_linux_errno_result();
    first = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, pair[0], NULL, NULL, 0, -1);
    if (first.status != HL_STATUS_OK) {
        close(pair[0]);
        close(pair[1]);
        return first;
    }
    second = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, pair[1], NULL, NULL, 0, -1);
    if (second.status != HL_STATUS_OK) {
        (void)hl_linux_close_descriptor(host, first.value);
        close(pair[1]);
        return second;
    }
    return hl_linux_result(HL_STATUS_OK, first.value, second.value);
}

static hl_host_result hl_linux_transfer_send(void *context, hl_host_handle channel, hl_host_const_bytes data,
                                             const hl_host_transfer_attachment *attachments, uint32_t count) {
    hl_host_linux *host = context;
    hl_linux_transfer_wire wire = {0};
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int descriptors[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    int channel_fd;
    uint32_t index;
    if (data.size > HL_HOST_TRANSFER_MAX_DATA || (data.size != 0 && data.data == NULL) ||
        count > HL_HOST_TRANSFER_MAX_ATTACHMENTS || (count != 0 && attachments == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    channel_fd = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    for (index = 0; index < count && channel_fd >= 0; ++index) {
        hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, attachments[index].object, HL_LINUX_HANDLE_COUNTER);
        uint32_t valid =
            HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
        if (entry == NULL || attachments[index].kind != HL_HOST_TRANSFER_KIND_COUNTER ||
            (attachments[index].rights & ~valid) != 0 ||
            (attachments[index].rights & entry->reserved) != attachments[index].rights) {
            channel_fd = -1;
            break;
        }
        descriptors[index] = entry->descriptor;
        wire.flags[index] = (uint32_t)entry->size;
        wire.rights[index] = attachments[index].rights;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    wire.data_size = (uint32_t)data.size;
    wire.attachment_count = count;
    if (data.size != 0) memcpy(wire.data, data.data, data.size);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (count != 0) {
        struct cmsghdr *header;
        message.msg_control = control;
        message.msg_controllen = CMSG_SPACE(sizeof(int) * count);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = CMSG_LEN(sizeof(int) * count);
        memcpy(CMSG_DATA(header), descriptors, sizeof(int) * count);
    }
    return sendmsg(channel_fd, &message, 0) == (ssize_t)sizeof(wire) ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                     : hl_linux_errno_result();
}

static hl_host_result hl_linux_transfer_receive(void *context, hl_host_handle channel, hl_host_bytes data,
                                                hl_host_transfer_attachment *attachments, uint32_t capacity) {
    hl_host_linux *host = context;
    hl_linux_transfer_wire wire;
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int channel_fd;
    int received[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t index;
    ssize_t bytes;
    pthread_mutex_lock(&host->lock);
    channel_fd = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0 || (data.size != 0 && data.data == NULL) || (capacity != 0 && attachments == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    bytes = recv(channel_fd, &wire, sizeof(wire), MSG_PEEK);
    if (bytes < 0) return hl_linux_errno_result();
    if (bytes != (ssize_t)sizeof(wire) || wire.data_size > HL_HOST_TRANSFER_MAX_DATA ||
        wire.attachment_count > HL_HOST_TRANSFER_MAX_ATTACHMENTS)
        return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.data_size > data.size || wire.attachment_count > capacity)
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, wire.data_size, wire.attachment_count);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    bytes = recvmsg(channel_fd, &message, MSG_CMSG_CLOEXEC);
    if (bytes != (ssize_t)sizeof(wire))
        return bytes < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.attachment_count != 0) {
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
            header->cmsg_len != CMSG_LEN(sizeof(int) * wire.attachment_count))
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        memcpy(received, CMSG_DATA(header), sizeof(int) * wire.attachment_count);
    }
    for (index = 0; index < wire.attachment_count; ++index) {
        hl_host_result installed =
            hl_linux_allocate_handle(host, HL_LINUX_HANDLE_COUNTER, received[index], NULL, NULL, wire.flags[index], -1);
        if (installed.status != HL_STATUS_OK) {
            uint32_t rest;
            close(received[index]);
            for (rest = index + 1; rest < wire.attachment_count; ++rest)
                close(received[rest]);
            for (rest = 0; rest < index; ++rest)
                (void)hl_linux_close_descriptor(host, attachments[rest].object);
            return installed;
        }
        pthread_mutex_lock(&host->lock);
        hl_linux_lookup_locked(host, installed.value, HL_LINUX_HANDLE_COUNTER)->reserved = (uint16_t)wire.rights[index];
        pthread_mutex_unlock(&host->lock);
        attachments[index] =
            (hl_host_transfer_attachment){installed.value, HL_HOST_TRANSFER_KIND_COUNTER, wire.rights[index]};
    }
    if (wire.data_size != 0) memcpy(data.data, wire.data, wire.data_size);
    return hl_linux_result(HL_STATUS_OK, wire.data_size, wire.attachment_count);
}

static hl_host_result hl_linux_transfer_duplicate(void *context, hl_host_handle channel) {
    hl_host_linux *host = context;
    int descriptor;
    hl_host_result result;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    descriptor = descriptor < 0 ? -1 : dup(descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_errno_result();
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static hl_host_result hl_linux_network_socket(void *context, uint32_t family, uint32_t type, uint32_t protocol) {
    hl_host_linux *host = context;
    int native_family;
    int native_type;
    int descriptor;
    if (family == HL_HOST_NETWORK_IPV4)
        native_family = AF_INET;
    else if (family == HL_HOST_NETWORK_IPV6)
        native_family = AF_INET6;
    else if (family == HL_HOST_NETWORK_LOCAL)
        native_family = AF_UNIX;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (type == HL_HOST_NETWORK_STREAM)
        native_type = SOCK_STREAM;
    else if (type == HL_HOST_NETWORK_DATAGRAM)
        native_type = SOCK_DGRAM;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = socket(native_family, native_type | SOCK_CLOEXEC, (int)protocol);
    if (descriptor < 0) return hl_linux_errno_result();
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static int hl_linux_socket_descriptor(hl_host_linux *host, hl_host_handle socket_handle) {
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, socket_handle, HL_LINUX_HANDLE_SOCKET, HL_LINUX_HANDLE_SOCKET);
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_status hl_linux_network_address(const hl_host_network_address *address, struct sockaddr_storage *storage,
                                          socklen_t *size) {
    memset(storage, 0, sizeof(*storage));
    if (address == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (address->family == HL_HOST_NETWORK_IPV4 && address->size == 4) {
        struct sockaddr_in *ipv4 = (struct sockaddr_in *)storage;
        ipv4->sin_family = AF_INET;
        ipv4->sin_port = htons(address->port);
        memcpy(&ipv4->sin_addr, address->address, 4);
        *size = sizeof(*ipv4);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_IPV6 && address->size == 16) {
        struct sockaddr_in6 *ipv6 = (struct sockaddr_in6 *)storage;
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = htons(address->port);
        memcpy(&ipv6->sin6_addr, address->address, 16);
        *size = sizeof(*ipv6);
        return HL_STATUS_OK;
    }
    if (address->family == HL_HOST_NETWORK_LOCAL && address->size > 0 && address->size < sizeof(address->local_path)) {
        struct sockaddr_un *local = (struct sockaddr_un *)storage;
        local->sun_family = AF_UNIX;
        memcpy(local->sun_path, address->local_path, address->size);
        local->sun_path[address->size] = '\0';
        *size = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + address->size + 1u);
        return HL_STATUS_OK;
    }
    return HL_STATUS_INVALID_ARGUMENT;
}

static hl_host_result hl_linux_network_bind(void *context, hl_host_handle socket_handle,
                                            const hl_host_network_address *address) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size;
    hl_status status = hl_linux_network_address(address, &storage, &size);
    if (descriptor < 0 || status != HL_STATUS_OK) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return bind(descriptor, (const struct sockaddr *)&storage, size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                          : hl_linux_errno_result();
}

static hl_host_result hl_linux_network_connect(void *context, hl_host_handle socket_handle,
                                               const hl_host_network_address *address) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size;
    hl_status status = hl_linux_network_address(address, &storage, &size);
    if (descriptor < 0 || status != HL_STATUS_OK) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return connect(descriptor, (const struct sockaddr *)&storage, size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                             : hl_linux_errno_result();
}

/* Defined with the rest of the ABI 2 arm below; declared here because the two
 * transfer callbacks predate it and must not keep casting the contract's flag
 * word to a native one. The two words are disjoint sets of bits and always
 * were: the contract's MSG_DONT_WAIT is bit 2 and Linux's is bit 6, so the old
 * cast asked for MSG_DONTROUTE whenever a caller asked not to block. */
static uint32_t hl_linux_network_flags(uint32_t flags);
static hl_host_result hl_linux_network_error(void);

static hl_host_result hl_linux_network_send(void *context, hl_host_handle socket_handle, hl_host_const_bytes data,
                                            uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    ssize_t count;
    if (descriptor < 0 || (data.size != 0 && data.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = send(descriptor, data.data, data.size, (int)hl_linux_network_flags(flags));
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_receive(void *context, hl_host_handle socket_handle, hl_host_bytes data,
                                               uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    ssize_t count;
    if (descriptor < 0 || (data.size != 0 && data.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = recv(descriptor, data.data, data.size, (int)hl_linux_network_flags(flags));
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_network_error();
}

/* --- network, ABI 2 --------------------------------------------------------
 *
 * The Linux arm of the socket contract. Everything here is a thin translation
 * because the host IS the guest's own kernel; the value of the layer on this
 * host is not that it hides a difference but that it names one interface both
 * hosts can be held to. Two things are still translated rather than passed
 * through, and both are defects the previous six-callback group had:
 *
 *   - the transfer FLAGS word. It used to be cast straight to int, so a caller
 *     that had translated its guest flags into the contract's would have set
 *     MSG_OOB where it meant MSG_PEEK. Contract bits and Linux bits do not
 *     agree and were never specified to.
 *   - the PORT. hl_host_network_address carries host byte order on both sides
 *     of the seam, and the provider owns the swap.
 */

static uint32_t hl_linux_network_flags(uint32_t flags) {
    uint32_t native = 0;
    if ((flags & HL_HOST_MSG_PEEK) != 0) native |= MSG_PEEK;
    if ((flags & HL_HOST_MSG_OUT_OF_BAND) != 0) native |= MSG_OOB;
    if ((flags & HL_HOST_MSG_DONT_WAIT) != 0) native |= MSG_DONTWAIT;
    if ((flags & HL_HOST_MSG_WAIT_ALL) != 0) native |= MSG_WAITALL;
    if ((flags & HL_HOST_MSG_DONT_ROUTE) != 0) native |= MSG_DONTROUTE;
    if ((flags & HL_HOST_MSG_NO_SIGNAL) != 0) native |= MSG_NOSIGNAL;
    if ((flags & HL_HOST_MSG_END_OF_RECORD) != 0) native |= MSG_EOR;
    if ((flags & HL_HOST_MSG_MORE) != 0) native |= MSG_MORE;
    return native;
}

static uint32_t hl_linux_network_flags_back(int flags) {
    uint32_t out = 0;
    if ((flags & MSG_TRUNC) != 0) out |= HL_HOST_MSG_TRUNCATED;
    if ((flags & MSG_CTRUNC) != 0) out |= HL_HOST_MSG_CONTROL_TRUNCATED;
    if ((flags & MSG_OOB) != 0) out |= HL_HOST_MSG_RECEIVED_OUT_OF_BAND;
    if ((flags & MSG_EOR) != 0) out |= HL_HOST_MSG_RECEIVED_END_OF_RECORD;
    return out;
}

/* The neutral condition that goes with an errno, so a caller gets the precise
 * answer without the host's own numbering. Zero means "the coarse status says
 * everything there is to say". */
static uint32_t hl_linux_network_condition(int code) {
    switch (code) {
    case EAGAIN: return HL_HOST_NETWORK_CONDITION_WOULD_BLOCK;
    case EINPROGRESS: return HL_HOST_NETWORK_CONDITION_CONNECT_IN_PROGRESS;
    case EALREADY: return HL_HOST_NETWORK_CONDITION_CONNECT_PENDING;
    case EISCONN: return HL_HOST_NETWORK_CONDITION_ALREADY_CONNECTED;
    case ENOTCONN: return HL_HOST_NETWORK_CONDITION_NOT_CONNECTED;
    case EADDRINUSE: return HL_HOST_NETWORK_CONDITION_ADDRESS_IN_USE;
    case EADDRNOTAVAIL: return HL_HOST_NETWORK_CONDITION_ADDRESS_NOT_AVAILABLE;
    case ECONNREFUSED: return HL_HOST_NETWORK_CONDITION_CONNECTION_REFUSED;
    case ECONNRESET: return HL_HOST_NETWORK_CONDITION_CONNECTION_RESET;
    case ECONNABORTED: return HL_HOST_NETWORK_CONDITION_CONNECTION_ABORTED;
    case EDESTADDRREQ: return HL_HOST_NETWORK_CONDITION_DESTINATION_REQUIRED;
    case EMSGSIZE: return HL_HOST_NETWORK_CONDITION_MESSAGE_TOO_LARGE;
    case EAFNOSUPPORT: return HL_HOST_NETWORK_CONDITION_FAMILY_NOT_SUPPORTED;
    case EPROTONOSUPPORT: return HL_HOST_NETWORK_CONDITION_PROTOCOL_NOT_SUPPORTED;
    case ESOCKTNOSUPPORT: return HL_HOST_NETWORK_CONDITION_TYPE_NOT_SUPPORTED;
    case ENOPROTOOPT: return HL_HOST_NETWORK_CONDITION_OPTION_NOT_SUPPORTED;
    case EPROTOTYPE: return HL_HOST_NETWORK_CONDITION_WRONG_PROTOCOL;
    case ENOTSOCK: return HL_HOST_NETWORK_CONDITION_NOT_A_SOCKET;
    case EHOSTUNREACH: return HL_HOST_NETWORK_CONDITION_HOST_UNREACHABLE;
    case ENETUNREACH: return HL_HOST_NETWORK_CONDITION_NETWORK_UNREACHABLE;
    case ENETDOWN: return HL_HOST_NETWORK_CONDITION_NETWORK_DOWN;
    case ENETRESET: return HL_HOST_NETWORK_CONDITION_NETWORK_RESET;
    case ENOBUFS: return HL_HOST_NETWORK_CONDITION_BUFFER_EXHAUSTED;
    case ESHUTDOWN: return HL_HOST_NETWORK_CONDITION_SHUT_DOWN;
    case EPIPE: return HL_HOST_NETWORK_CONDITION_BROKEN_PIPE;
    case EOPNOTSUPP: return HL_HOST_NETWORK_CONDITION_OPERATION_NOT_SUPPORTED;
    case ETIMEDOUT: return HL_HOST_NETWORK_CONDITION_TIMED_OUT;
    case EINTR: return HL_HOST_NETWORK_CONDITION_INTERRUPTED;
    default: return HL_HOST_NETWORK_CONDITION_NONE;
    }
}

static hl_host_result hl_linux_network_error(void) {
    const int code = errno;
    const uint32_t condition = hl_linux_network_condition(code);
    hl_host_result result = hl_linux_errno_result();
    if (condition != HL_HOST_NETWORK_CONDITION_NONE) {
        result.detail_domain = HL_HOST_DETAIL_NETWORK;
        result.detail = condition;
    }
    return result;
}

static hl_status hl_linux_network_decode(const struct sockaddr_storage *storage, socklen_t size,
                                         hl_host_network_address *out) {
    memset(out, 0, sizeof(*out));
    if (size < (socklen_t)sizeof(sa_family_t)) return HL_STATUS_INVALID_ARGUMENT;
    if (storage->ss_family == AF_INET && size >= (socklen_t)sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *ipv4 = (const struct sockaddr_in *)storage;
        out->family = HL_HOST_NETWORK_IPV4;
        out->port = ntohs(ipv4->sin_port);
        out->size = 4;
        memcpy(out->address, &ipv4->sin_addr, 4);
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_INET6 && size >= (socklen_t)sizeof(struct sockaddr_in6)) {
        const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)storage;
        out->family = HL_HOST_NETWORK_IPV6;
        out->port = ntohs(ipv6->sin6_port);
        out->size = 16;
        memcpy(out->address, &ipv6->sin6_addr, 16);
        out->scope_id = ipv6->sin6_scope_id;
        out->flow_info = ipv6->sin6_flowinfo;
        return HL_STATUS_OK;
    }
    if (storage->ss_family == AF_UNIX) {
        const struct sockaddr_un *local = (const struct sockaddr_un *)storage;
        size_t capacity = size > (socklen_t)offsetof(struct sockaddr_un, sun_path)
                              ? (size_t)size - offsetof(struct sockaddr_un, sun_path)
                              : 0;
        size_t length = 0;
        if (capacity > sizeof(local->sun_path)) capacity = sizeof(local->sun_path);
        while (length < capacity && local->sun_path[length] != '\0')
            length++;
        out->family = HL_HOST_NETWORK_LOCAL;
        out->size = (uint16_t)length;
        if (length != 0) memcpy(out->local_path, local->sun_path, length);
        return HL_STATUS_OK;
    }
    return HL_STATUS_NOT_SUPPORTED;
}

static int hl_linux_network_type_native(uint32_t type) {
    switch (type) {
    case HL_HOST_NETWORK_STREAM: return SOCK_STREAM;
    case HL_HOST_NETWORK_DATAGRAM: return SOCK_DGRAM;
    case HL_HOST_NETWORK_SEQPACKET: return SOCK_SEQPACKET;
    case HL_HOST_NETWORK_RAW: return SOCK_RAW;
    default: return -1;
    }
}

static int hl_linux_network_family_native(uint32_t family) {
    switch (family) {
    case HL_HOST_NETWORK_IPV4: return AF_INET;
    case HL_HOST_NETWORK_IPV6: return AF_INET6;
    case HL_HOST_NETWORK_LOCAL: return AF_UNIX;
    default: return -1;
    }
}

static hl_host_result hl_linux_network_listen(void *context, hl_host_handle socket_handle, uint32_t backlog) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (backlog > (uint32_t)INT_MAX) backlog = (uint32_t)INT_MAX;
    return listen(descriptor, (int)backlog) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_accept(void *context, hl_host_handle socket_handle,
                                              hl_host_network_address *peer, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size = (socklen_t)sizeof(storage);
    hl_host_result result;
    int accepted;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&storage, 0, sizeof(storage));
    accepted = accept4(descriptor, (struct sockaddr *)&storage, &size,
                       SOCK_CLOEXEC | ((flags & HL_HOST_SOCKET_NONBLOCK) != 0 ? SOCK_NONBLOCK : 0));
    if (accepted < 0) return hl_linux_network_error();
    if (peer != NULL && hl_linux_network_decode(&storage, size, peer) != HL_STATUS_OK) {
        memset(peer, 0, sizeof(*peer));
        peer->family = HL_HOST_NETWORK_LOCAL;
    }
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, accepted, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(accepted);
    return result;
}

static hl_host_result hl_linux_network_pair(void *context, uint32_t family, uint32_t type, uint32_t protocol,
                                            hl_host_handle ends[2]) {
    hl_host_linux *host = context;
    int native_family = hl_linux_network_family_native(family);
    int native_type = hl_linux_network_type_native(type);
    int descriptors[2];
    hl_host_result first;
    hl_host_result second;
    if (ends == NULL || native_family < 0 || native_type < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (socketpair(native_family, native_type | SOCK_CLOEXEC, (int)protocol, descriptors) != 0)
        return hl_linux_network_error();
    first = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptors[0], NULL, NULL, 0, -1);
    if (first.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        return first;
    }
    second = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, descriptors[1], NULL, NULL, 0, -1);
    if (second.status != HL_STATUS_OK) {
        (void)hl_linux_network_close(host, first.value);
        close(descriptors[1]);
        return second;
    }
    ends[0] = first.value;
    ends[1] = second.value;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_network_shutdown(void *context, hl_host_handle socket_handle, uint32_t direction) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    int native;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    switch (direction) {
    case HL_HOST_SHUTDOWN_READ: native = SHUT_RD; break;
    case HL_HOST_SHUTDOWN_WRITE: native = SHUT_WR; break;
    case HL_HOST_SHUTDOWN_BOTH: native = SHUT_RDWR; break;
    default: return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    return shutdown(descriptor, native) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_name(void *context, hl_host_handle socket_handle,
                                            hl_host_network_address *address, int peer) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    socklen_t size = (socklen_t)sizeof(storage);
    hl_status status;
    if (descriptor < 0 || address == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&storage, 0, sizeof(storage));
    if ((peer ? getpeername(descriptor, (struct sockaddr *)&storage, &size)
              : getsockname(descriptor, (struct sockaddr *)&storage, &size)) != 0)
        return hl_linux_network_error();
    status = hl_linux_network_decode(&storage, size, address);
    return status == HL_STATUS_OK ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_result(status, 0, 0);
}

static hl_host_result hl_linux_network_local_address(void *context, hl_host_handle socket_handle,
                                                     hl_host_network_address *address) {
    return hl_linux_network_name(context, socket_handle, address, 0);
}

static hl_host_result hl_linux_network_peer_address(void *context, hl_host_handle socket_handle,
                                                    hl_host_network_address *address) {
    return hl_linux_network_name(context, socket_handle, address, 1);
}

/* The flat neutral option -> (level, name). One table, and no path from a
 * caller's number to a native number that does not pass through it -- which is
 * the property the flat enum exists to give, and the reason there is no
 * `default: return option;` anywhere below. */
typedef struct hl_linux_network_option {
    uint32_t option;
    int level;
    int name;
} hl_linux_network_option;

static const hl_linux_network_option hl_linux_network_options[] = {
    {HL_HOST_SOCKOPT_REUSE_ADDRESS, SOL_SOCKET, SO_REUSEADDR},
    {HL_HOST_SOCKOPT_REUSE_PORT, SOL_SOCKET, SO_REUSEPORT},
    {HL_HOST_SOCKOPT_KEEP_ALIVE, SOL_SOCKET, SO_KEEPALIVE},
    {HL_HOST_SOCKOPT_BROADCAST, SOL_SOCKET, SO_BROADCAST},
    {HL_HOST_SOCKOPT_DONT_ROUTE, SOL_SOCKET, SO_DONTROUTE},
    {HL_HOST_SOCKOPT_OUT_OF_BAND_INLINE, SOL_SOCKET, SO_OOBINLINE},
    {HL_HOST_SOCKOPT_SEND_BUFFER, SOL_SOCKET, SO_SNDBUF},
    {HL_HOST_SOCKOPT_RECEIVE_BUFFER, SOL_SOCKET, SO_RCVBUF},
    {HL_HOST_SOCKOPT_SEND_LOW_WATER, SOL_SOCKET, SO_SNDLOWAT},
    {HL_HOST_SOCKOPT_RECEIVE_LOW_WATER, SOL_SOCKET, SO_RCVLOWAT},
    {HL_HOST_SOCKOPT_TYPE, SOL_SOCKET, SO_TYPE},
    {HL_HOST_SOCKOPT_PROTOCOL, SOL_SOCKET, SO_PROTOCOL},
    {HL_HOST_SOCKOPT_DOMAIN, SOL_SOCKET, SO_DOMAIN},
    {HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS, SOL_SOCKET, SO_ACCEPTCONN},
    {HL_HOST_SOCKOPT_PASS_CREDENTIALS, SOL_SOCKET, SO_PASSCRED},
    {HL_HOST_SOCKOPT_TCP_NO_DELAY, IPPROTO_TCP, TCP_NODELAY},
    {HL_HOST_SOCKOPT_TCP_KEEP_IDLE, IPPROTO_TCP, TCP_KEEPIDLE},
    {HL_HOST_SOCKOPT_TCP_KEEP_INTERVAL, IPPROTO_TCP, TCP_KEEPINTVL},
    {HL_HOST_SOCKOPT_TCP_KEEP_COUNT, IPPROTO_TCP, TCP_KEEPCNT},
    {HL_HOST_SOCKOPT_TCP_MAX_SEGMENT, IPPROTO_TCP, TCP_MAXSEG},
    {HL_HOST_SOCKOPT_TCP_CORK, IPPROTO_TCP, TCP_CORK},
    {HL_HOST_SOCKOPT_TCP_QUICK_ACK, IPPROTO_TCP, TCP_QUICKACK},
    {HL_HOST_SOCKOPT_TCP_USER_TIMEOUT, IPPROTO_TCP, TCP_USER_TIMEOUT},
    {HL_HOST_SOCKOPT_IP_TIME_TO_LIVE, IPPROTO_IP, IP_TTL},
    {HL_HOST_SOCKOPT_IP_TYPE_OF_SERVICE, IPPROTO_IP, IP_TOS},
    {HL_HOST_SOCKOPT_IP_HEADER_INCLUDED, IPPROTO_IP, IP_HDRINCL},
    {HL_HOST_SOCKOPT_IP_MULTICAST_TTL, IPPROTO_IP, IP_MULTICAST_TTL},
    {HL_HOST_SOCKOPT_IP_MULTICAST_LOOP, IPPROTO_IP, IP_MULTICAST_LOOP},
    {HL_HOST_SOCKOPT_IP_PACKET_INFO, IPPROTO_IP, IP_PKTINFO},
    {HL_HOST_SOCKOPT_IPV6_ONLY, IPPROTO_IPV6, IPV6_V6ONLY},
    {HL_HOST_SOCKOPT_IPV6_UNICAST_HOPS, IPPROTO_IPV6, IPV6_UNICAST_HOPS},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_HOPS, IPPROTO_IPV6, IPV6_MULTICAST_HOPS},
    {HL_HOST_SOCKOPT_IPV6_MULTICAST_LOOP, IPPROTO_IPV6, IPV6_MULTICAST_LOOP},
    {HL_HOST_SOCKOPT_IPV6_PACKET_INFO, IPPROTO_IPV6, IPV6_RECVPKTINFO}};

static const hl_linux_network_option *hl_linux_network_option_find(uint32_t option) {
    size_t index;
    for (index = 0; index < sizeof(hl_linux_network_options) / sizeof(hl_linux_network_options[0]); ++index)
        if (hl_linux_network_options[index].option == option) return &hl_linux_network_options[index];
    return NULL;
}

static hl_host_result hl_linux_network_get_option(void *context, hl_host_handle socket_handle, uint32_t option,
                                                  hl_host_bytes value) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    const hl_linux_network_option *entry;
    if (descriptor < 0 || value.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (option == HL_HOST_SOCKOPT_ERROR) {
        /* Reported as an hl_status and never as an errno, so a caller need not
         * know whose numbering it is holding. The host number stays in detail. */
        int code = 0;
        socklen_t size = (socklen_t)sizeof(code);
        uint32_t status;
        if (value.size < sizeof(uint32_t)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (getsockopt(descriptor, SOL_SOCKET, SO_ERROR, &code, &size) != 0) return hl_linux_network_error();
        status = code == 0 ? (uint32_t)HL_STATUS_OK : (uint32_t)hl_linux_status_from_errno(code);
        memcpy(value.data, &status, sizeof(status));
        return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_ERRNO, sizeof(status), (uint64_t)(uint32_t)code};
    }
    if (option == HL_HOST_SOCKOPT_LINGER) {
        struct linger native;
        hl_host_network_linger out;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(out)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        if (getsockopt(descriptor, SOL_SOCKET, SO_LINGER, &native, &size) != 0) return hl_linux_network_error();
        out.enabled = native.l_onoff != 0 ? 1u : 0u;
        out.seconds = (uint32_t)(native.l_linger < 0 ? 0 : native.l_linger);
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
    if (option == HL_HOST_SOCKOPT_SEND_TIMEOUT || option == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        struct timeval native;
        uint64_t nanoseconds;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(nanoseconds)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        if (getsockopt(descriptor, SOL_SOCKET, option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO,
                       &native, &size) != 0)
            return hl_linux_network_error();
        nanoseconds = (uint64_t)native.tv_sec * UINT64_C(1000000000) + (uint64_t)native.tv_usec * UINT64_C(1000);
        memcpy(value.data, &nanoseconds, sizeof(nanoseconds));
        return hl_linux_result(HL_STATUS_OK, sizeof(nanoseconds), 0);
    }
    if (option == HL_HOST_SOCKOPT_PEER_CREDENTIALS) {
        struct ucred native;
        hl_host_network_credentials out;
        socklen_t size = (socklen_t)sizeof(native);
        if (value.size < sizeof(out)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memset(&native, 0, sizeof(native));
        memset(&out, 0, sizeof(out));
        if (getsockopt(descriptor, SOL_SOCKET, SO_PEERCRED, &native, &size) != 0) return hl_linux_network_error();
        out.process = (int32_t)native.pid;
        out.user = (uint32_t)native.uid;
        out.group = (uint32_t)native.gid;
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
    entry = hl_linux_network_option_find(option);
    if (entry == NULL) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    {
        int scalar = 0;
        uint32_t out;
        socklen_t size = (socklen_t)sizeof(scalar);
        if (value.size < sizeof(uint32_t)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (getsockopt(descriptor, entry->level, entry->name, &scalar, &size) != 0) return hl_linux_network_error();
        if (option == HL_HOST_SOCKOPT_TYPE) {
            /* SO_TYPE answers in the HOST's numbering; the caller asked in the
             * contract's, and those are two different questions on some host. */
            out = scalar == SOCK_STREAM      ? HL_HOST_NETWORK_STREAM
                  : scalar == SOCK_DGRAM     ? HL_HOST_NETWORK_DATAGRAM
                  : scalar == SOCK_SEQPACKET ? HL_HOST_NETWORK_SEQPACKET
                  : scalar == SOCK_RAW       ? HL_HOST_NETWORK_RAW
                                             : 0u;
        } else if (option == HL_HOST_SOCKOPT_DOMAIN) {
            out = scalar == AF_INET    ? HL_HOST_NETWORK_IPV4
                  : scalar == AF_INET6 ? HL_HOST_NETWORK_IPV6
                  : scalar == AF_UNIX  ? HL_HOST_NETWORK_LOCAL
                                       : 0u;
        } else {
            out = (uint32_t)scalar;
        }
        memcpy(value.data, &out, sizeof(out));
        return hl_linux_result(HL_STATUS_OK, sizeof(out), 0);
    }
}

static hl_host_result hl_linux_network_set_option(void *context, hl_host_handle socket_handle, uint32_t option,
                                                  const hl_host_const_bytes value) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    const hl_linux_network_option *entry;
    if (descriptor < 0 || value.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (option == HL_HOST_SOCKOPT_LINGER) {
        hl_host_network_linger in;
        struct linger native;
        if (value.size < sizeof(in)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&in, value.data, sizeof(in));
        native.l_onoff = in.enabled != 0 ? 1 : 0;
        native.l_linger = (int)in.seconds;
        return setsockopt(descriptor, SOL_SOCKET, SO_LINGER, &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
    if (option == HL_HOST_SOCKOPT_SEND_TIMEOUT || option == HL_HOST_SOCKOPT_RECEIVE_TIMEOUT) {
        uint64_t nanoseconds;
        struct timeval native;
        if (value.size < sizeof(nanoseconds)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&nanoseconds, value.data, sizeof(nanoseconds));
        native.tv_sec = (time_t)(nanoseconds / UINT64_C(1000000000));
        native.tv_usec = (suseconds_t)((nanoseconds % UINT64_C(1000000000)) / UINT64_C(1000));
        return setsockopt(descriptor, SOL_SOCKET, option == HL_HOST_SOCKOPT_SEND_TIMEOUT ? SO_SNDTIMEO : SO_RCVTIMEO,
                          &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
    if (option == HL_HOST_SOCKOPT_ERROR || option == HL_HOST_SOCKOPT_TYPE || option == HL_HOST_SOCKOPT_DOMAIN ||
        option == HL_HOST_SOCKOPT_PROTOCOL || option == HL_HOST_SOCKOPT_ACCEPT_CONNECTIONS ||
        option == HL_HOST_SOCKOPT_PEER_CREDENTIALS)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    entry = hl_linux_network_option_find(option);
    if (entry == NULL) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    {
        uint32_t scalar;
        int native;
        if (value.size < sizeof(scalar)) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        memcpy(&scalar, value.data, sizeof(scalar));
        native = (int)scalar;
        return setsockopt(descriptor, entry->level, entry->name, &native, (socklen_t)sizeof(native)) == 0
                   ? hl_linux_result(HL_STATUS_OK, 0, 0)
                   : hl_linux_network_error();
    }
}

enum { HL_LINUX_NETWORK_IOV_MAX = 64 };

static hl_host_result hl_linux_network_send_message(void *context, hl_host_handle socket_handle,
                                                    const hl_host_network_message *message, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    struct iovec vectors[HL_LINUX_NETWORK_IOV_MAX];
    struct msghdr header;
    socklen_t size = 0;
    uint32_t index;
    ssize_t sent;
    if (descriptor < 0 || message == NULL || message->buffer_count > HL_LINUX_NETWORK_IOV_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (message->buffer_count != 0 && message->buffers == NULL)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < message->buffer_count; ++index) {
        vectors[index].iov_base = (void *)(uintptr_t)message->buffers[index].address;
        vectors[index].iov_len = (size_t)message->buffers[index].size;
    }
    memset(&header, 0, sizeof(header));
    header.msg_iov = vectors;
    header.msg_iovlen = message->buffer_count;
    if (message->address != NULL) {
        if (hl_linux_network_address(message->address, &storage, &size) != HL_STATUS_OK)
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        header.msg_name = &storage;
        header.msg_namelen = size;
    }
    /* Ancillary data has no encoding on this seam yet. Refused rather than
     * dropped: a caller that passed control bytes and got a success would
     * believe the peer received them. */
    if (message->control != NULL && message->control_size != 0) return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    sent = sendmsg(descriptor, &header, (int)hl_linux_network_flags(flags));
    return sent >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)sent, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_receive_message(void *context, hl_host_handle socket_handle,
                                                       hl_host_network_message *message, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct sockaddr_storage storage;
    struct iovec vectors[HL_LINUX_NETWORK_IOV_MAX];
    struct msghdr header;
    uint32_t index;
    ssize_t received;
    if (descriptor < 0 || message == NULL || message->buffer_count > HL_LINUX_NETWORK_IOV_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (message->buffer_count != 0 && message->buffers == NULL)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < message->buffer_count; ++index) {
        vectors[index].iov_base = (void *)(uintptr_t)message->buffers[index].address;
        vectors[index].iov_len = (size_t)message->buffers[index].size;
    }
    memset(&storage, 0, sizeof(storage));
    memset(&header, 0, sizeof(header));
    header.msg_iov = vectors;
    header.msg_iovlen = message->buffer_count;
    if (message->address != NULL) {
        header.msg_name = &storage;
        header.msg_namelen = (socklen_t)sizeof(storage);
    }
    received = recvmsg(descriptor, &header, (int)hl_linux_network_flags(flags));
    if (received < 0) return hl_linux_network_error();
    message->flags = hl_linux_network_flags_back(header.msg_flags);
    message->control_size = 0;
    if (message->address != NULL &&
        hl_linux_network_decode(&storage, header.msg_namelen, message->address) != HL_STATUS_OK)
        memset(message->address, 0, sizeof(*message->address));
    return hl_linux_result(HL_STATUS_OK, (uint64_t)received, 0);
}

static hl_host_result hl_linux_network_readiness(void *context, hl_host_handle socket_handle, uint32_t interests) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    struct pollfd probe;
    uint32_t ready = 0;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    probe.fd = descriptor;
    probe.events = (short)(POLLIN | POLLOUT | POLLPRI);
    probe.revents = 0;
    if (poll(&probe, 1, 0) < 0) return hl_linux_network_error();
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    {
        /* The queued byte count rides in detail, so a caller never has to name
         * an ioctl to ask a question readiness already almost answers. */
        int queued = 0;
        uint64_t pending = ioctl(descriptor, FIONREAD, &queued) == 0 && queued > 0 ? (uint64_t)queued : 0;
        return (hl_host_result){(int32_t)HL_STATUS_OK, HL_HOST_DETAIL_NONE,
                                interests == 0 ? ready : (ready & interests), pending};
    }
}

/* A socket here IS a pollable kernel object, so the event group accepts it and
   the answer is yes. */
static hl_host_result hl_linux_network_wait_handle(void *context, hl_host_handle socket_handle) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_linux_result(HL_STATUS_OK, 1, 0);
}

static hl_host_result hl_linux_network_set_status_flags(void *context, hl_host_handle socket_handle, uint32_t flags) {
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    int current;
    if (descriptor < 0 || (flags & ~(uint32_t)HL_HOST_SOCKET_NONBLOCK) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    current = fcntl(descriptor, F_GETFL);
    if (current < 0) return hl_linux_network_error();
    current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_SOCKET_NONBLOCK) != 0 ? O_NONBLOCK : 0);
    return fcntl(descriptor, F_SETFL, current) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_network_error();
}

static hl_host_result hl_linux_network_duplicate(void *context, hl_host_handle socket_handle) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_socket_descriptor(context, socket_handle);
    hl_host_result result;
    int copy;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    copy = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (copy < 0) return hl_linux_network_error();
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SOCKET, copy, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(copy);
    return result;
}

static hl_host_result hl_linux_shared_create(void *context, uint64_t size, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor;
    if (size > INT64_MAX || flags != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = memfd_create("hl-engine", MFD_CLOEXEC);
    if (descriptor < 0) return hl_linux_errno_result();
    if (ftruncate(descriptor, (off_t)size) != 0) {
        close(descriptor);
        return hl_linux_errno_result();
    }
    hl_host_result result =
        hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SHARED_MEMORY, descriptor, NULL, NULL, size, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    if (result.status == HL_STATUS_OK) result.detail = result.value;
    return result;
}

static hl_host_result hl_linux_shared_open(void *context, uint64_t identity, uint32_t flags) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *source;
    int descriptor;
    uint64_t size;
    hl_host_result result;
    if (flags != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    source = hl_linux_lookup_locked(host, identity, HL_LINUX_HANDLE_SHARED_MEMORY);
    if (source == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = fcntl(source->descriptor, F_DUPFD_CLOEXEC, 0);
    size = source->size;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_errno_result();
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_SHARED_MEMORY, descriptor, NULL, NULL, size, -1);
    if (result.status != HL_STATUS_OK)
        close(descriptor);
    else
        result.detail = identity;
    return result;
}

static hl_host_result hl_linux_shared_resize(void *context, hl_host_handle object, uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int result;
    if (size > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, object, HL_LINUX_HANDLE_SHARED_MEMORY);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = ftruncate(entry->descriptor, (off_t)size);
    if (result == 0) entry->size = size;
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_process_spawn_mode(void *context, hl_host_process_entry entry, void *entry_context,
                                                  int prepared) {
    hl_host_linux *host = context;
    hl_host_result result;
    pid_t pid;
    int fork_error;
    int private_prepared = 0;
    if (entry == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!prepared && pthread_mutex_lock(&host->fork_gate) != 0)
        return hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (hl_host_process_fd_private_fork_prepare() != 0) {
        if (!prepared) (void)pthread_mutex_unlock(&host->fork_gate);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    private_prepared = 1;
    pid = fork();
    fork_error = errno;
    int private_status = private_prepared ? hl_host_process_fd_private_fork_complete(pid == 0) : 0;
    if (prepared) {
        /* The fork child has no watcher threads.  Reset inherited subscription
         * records there instead of applying the parent's completion path: a
         * later close must never pthread_join a thread that vanished at fork. */
        result = pid == 0 ? hl_linux_fork_child(host) : hl_linux_fork_complete(host);
    } else {
        result = pthread_mutex_unlock(&host->fork_gate) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                             : hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    if (pid < 0) {
        errno = fork_error;
        return result.status == HL_STATUS_OK ? hl_linux_errno_result() : result;
    }
    if (private_status != 0 && result.status == HL_STATUS_OK) result = hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    if (result.status != HL_STATUS_OK) {
        if (pid > 0) {
            int status;
            kill(pid, SIGKILL);
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        }
        if (pid == 0) _exit(255);
        return result;
    }
    if (pid == 0) _exit(entry(entry_context) & 255);
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_PROCESS, pid, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        int status;
        kill(pid, SIGKILL);
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
    }
    return result;
}

static hl_host_result hl_linux_process_spawn(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_linux_process_spawn_mode(context, entry, entry_context, 0);
}

static hl_host_result hl_linux_process_spawn_prepared(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_linux_process_spawn_mode(context, entry, entry_context, 1);
}

static hl_host_result hl_linux_process_wait(void *context, hl_host_handle handle, uint64_t deadline_ns) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pid_t pid;
    pid_t waited;
    int status;
    int options;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry == NULL || host->destroying) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    entry->process_waiters++;
    while (entry != NULL && entry->process_waiting && !entry->process_reaped) {
        if (deadline_ns == 0 ||
            (deadline_ns != HL_HOST_DEADLINE_INFINITE && hl_linux_monotonic_value() >= deadline_ns)) {
            entry->process_waiters--;
            pthread_cond_broadcast(&host->process_changed);
            pthread_mutex_unlock(&host->lock);
            return hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        hl_linux_process_changed_wait(host, deadline_ns);
        entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    }
    if (entry != NULL && entry->process_reaped) {
        hl_host_result result = hl_linux_result(HL_STATUS_OK, entry->process_exit_value, entry->process_exit_kind);
        entry->process_waiters--;
        pthread_cond_broadcast(&host->process_changed);
        pthread_mutex_unlock(&host->lock);
        return result;
    }
    pid = entry != NULL ? entry->descriptor : -1;
    if (entry != NULL) entry->process_waiting = 1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    options = deadline_ns == HL_HOST_DEADLINE_INFINITE ? 0 : WNOHANG;
    for (;;) {
        do {
            waited = waitpid(pid, &status, options);
        } while (waited < 0 && errno == EINTR);
        if (waited != 0) break;
        if (deadline_ns == 0 || hl_linux_monotonic_value() >= deadline_ns) break;
        hl_linux_sleep_until(deadline_ns);
    }
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry != NULL) {
        entry->process_waiting = 0;
        entry->process_waiters--;
    }
    if (waited > 0 && entry != NULL) {
        entry->process_reaped = 1;
        entry->process_exit_kind = WIFEXITED(status) ? HL_HOST_PROCESS_EXIT_CODE : HL_HOST_PROCESS_EXIT_SIGNAL;
        entry->process_exit_value = WIFEXITED(status) ? (uint32_t)WEXITSTATUS(status) : (uint32_t)WTERMSIG(status);
    }
    pthread_cond_broadcast(&host->process_changed);
    pthread_mutex_unlock(&host->lock);
    if (waited == 0) return hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    if (waited < 0) return hl_linux_errno_result();
    if (WIFEXITED(status))
        return hl_linux_result(HL_STATUS_OK, (uint64_t)WEXITSTATUS(status), HL_HOST_PROCESS_EXIT_CODE);
    if (WIFSIGNALED(status))
        return hl_linux_result(HL_STATUS_OK, (uint64_t)WTERMSIG(status), HL_HOST_PROCESS_EXIT_SIGNAL);
    return hl_linux_result(HL_STATUS_CORRUPT, 0, (uint64_t)(uint32_t)status);
}

static hl_host_result hl_linux_process_terminate(void *context, hl_host_handle handle, uint32_t reason) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pid_t pid;
    if (reason != HL_HOST_PROCESS_TERMINATE_INTERRUPT && reason != HL_HOST_PROCESS_TERMINATE_FORCE &&
        (reason <= HL_HOST_PROCESS_TERMINATE_SIGNAL || reason > HL_HOST_PROCESS_TERMINATE_SIGNAL + 64))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    pid = entry != NULL && !entry->process_reaped && !host->destroying ? entry->descriptor : -1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int signal_number = reason == HL_HOST_PROCESS_TERMINATE_INTERRUPT ? SIGINT
                        : reason == HL_HOST_PROCESS_TERMINATE_FORCE   ? SIGKILL
                                                                    : (int)(reason - HL_HOST_PROCESS_TERMINATE_SIGNAL);
    if (kill(pid, signal_number) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_process_close(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_PROCESS);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (host->destroying || !entry->process_reaped || entry->process_waiting || entry->process_waiters != 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->process_reaped = 0;
    entry->process_exit_kind = 0;
    entry->process_exit_value = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

/* --- terminal ---------------------------------------------------------------------------
 *
 * The device, and only the device. Everything the guest's terminal vocabulary is made of -- the
 * attribute structure, its control characters, canonical buffering, line editing, signal
 * generation, minimum-and-timeout reads, output post-processing -- is above this and is not
 * reachable from here on purpose.
 *
 * The five mode bits are the abstract capabilities the contract names, mapped onto the five native
 * flags that carry the same meaning. Nothing else in the native attributes is read or written, so
 * a caller that sets a mode gets exactly the change it named and no adjacent policy it did not.
 */
static int hl_linux_terminal_descriptor(hl_host_linux *host, hl_host_handle handle) {
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, handle, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_host_result hl_linux_terminal_probe(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* This host answers from the terminal line discipline itself rather than from the object type,
     * which is the distinction the contract exists for: a character device is not a terminal. */
    return hl_linux_result(HL_STATUS_OK, isatty(descriptor) != 0 ? 1u : 0u, 0);
}

static hl_host_result hl_linux_terminal_get_mode(void *context, hl_host_handle handle, uint32_t *mode) {
    hl_host_linux *host = context;
    struct termios attributes;
    int descriptor;
    uint32_t value = 0;
    if (mode == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_linux_errno_result();
    if ((attributes.c_lflag & ICANON) == 0) value |= HL_HOST_TERMINAL_RAW_INPUT;
    if ((attributes.c_lflag & ECHO) != 0) value |= HL_HOST_TERMINAL_ECHO;
    if ((attributes.c_lflag & ISIG) != 0) value |= HL_HOST_TERMINAL_SIGNALS;
    if ((attributes.c_iflag & IXON) != 0) value |= HL_HOST_TERMINAL_FLOW_CONTROL;
    if ((attributes.c_oflag & OPOST) != 0) value |= HL_HOST_TERMINAL_OUTPUT_PROCESSING;
    *mode = value;
    return hl_linux_result(HL_STATUS_OK, value, 0);
}

static hl_host_result hl_linux_terminal_set_mode(void *context, hl_host_handle handle, uint32_t mode) {
    hl_host_linux *host = context;
    struct termios attributes;
    int descriptor;
    if ((mode & ~(uint32_t)(HL_HOST_TERMINAL_RAW_INPUT | HL_HOST_TERMINAL_ECHO | HL_HOST_TERMINAL_SIGNALS |
                            HL_HOST_TERMINAL_FLOW_CONTROL | HL_HOST_TERMINAL_OUTPUT_PROCESSING)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_linux_errno_result();
    if ((mode & HL_HOST_TERMINAL_RAW_INPUT) != 0)
        attributes.c_lflag &= (tcflag_t)~ICANON;
    else
        attributes.c_lflag |= (tcflag_t)ICANON;
    if ((mode & HL_HOST_TERMINAL_ECHO) != 0)
        attributes.c_lflag |= (tcflag_t)ECHO;
    else
        attributes.c_lflag &= (tcflag_t)~ECHO;
    if ((mode & HL_HOST_TERMINAL_SIGNALS) != 0)
        attributes.c_lflag |= (tcflag_t)ISIG;
    else
        attributes.c_lflag &= (tcflag_t)~ISIG;
    if ((mode & HL_HOST_TERMINAL_FLOW_CONTROL) != 0)
        attributes.c_iflag |= (tcflag_t)IXON;
    else
        attributes.c_iflag &= (tcflag_t)~IXON;
    if ((mode & HL_HOST_TERMINAL_OUTPUT_PROCESSING) != 0)
        attributes.c_oflag |= (tcflag_t)OPOST;
    else
        attributes.c_oflag &= (tcflag_t)~OPOST;
    /* Applied now rather than after the queued output drains: a caller turning echo off before
     * asking for a secret cannot be made to wait on a writer it does not control. */
    if (tcsetattr(descriptor, TCSANOW, &attributes) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, mode, 0);
}

static hl_host_result hl_linux_terminal_get_size(void *context, hl_host_handle handle, hl_host_terminal_size *size) {
    hl_host_linux *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    if (ioctl(descriptor, TIOCGWINSZ, &window) != 0) return hl_linux_errno_result();
    size->columns = window.ws_col;
    size->rows = window.ws_row;
    size->pixel_width = window.ws_xpixel;
    size->pixel_height = window.ws_ypixel;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_terminal_set_size(void *context, hl_host_handle handle,
                                                 const hl_host_terminal_size *size) {
    hl_host_linux *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL || size->columns > UINT16_MAX || size->rows > UINT16_MAX || size->pixel_width > UINT16_MAX ||
        size->pixel_height > UINT16_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    window.ws_col = (unsigned short)size->columns;
    window.ws_row = (unsigned short)size->rows;
    window.ws_xpixel = (unsigned short)size->pixel_width;
    window.ws_ypixel = (unsigned short)size->pixel_height;
    if (ioctl(descriptor, TIOCSWINSZ, &window) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_terminal_read(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output.data, output.size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_terminal_write(void *context, hl_host_handle handle, hl_host_const_bytes input) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || input.size > SSIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_terminal_descriptor(host, handle);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

/*
 * Typed absence, not an oversight. On this host a resize is delivered as a process-directed signal
 * that the engine's own signal machinery already owns end to end, so there is no separate object
 * for this to hand back -- and manufacturing one would mean installing a process-wide handler
 * underneath the layer that is already handling that signal, which is a worse bargain than saying
 * so. The operation exists in the contract for a host where the resize arrives in the input stream
 * instead, where waiting for input and then reading it is a deadlock rather than a composition.
 */
static hl_host_result hl_linux_terminal_size_change_event(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    if (hl_linux_terminal_descriptor(host, handle) < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_linux_mutex_create(void *context) {
    hl_host_linux *host = context;
    return hl_host_sync_mutex_create(host->sync);
}

static hl_host_result hl_linux_mutex_lock(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    return hl_host_sync_mutex_lock(host->sync, handle);
}

static hl_host_result hl_linux_mutex_unlock(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    return hl_host_sync_mutex_unlock(host->sync, handle);
}

static hl_host_result hl_linux_mutex_close(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    return hl_host_sync_mutex_close(host->sync, handle);
}

static hl_host_result hl_linux_park(void *context, uint64_t waiter, uint32_t scope, uint64_t key, const void *address,
                                    uint64_t expected, uint32_t compare_size, uint64_t deadline_ns) {
    hl_host_linux *host = context;
    return hl_host_sync_park(host->sync, waiter, scope, key, address, expected, compare_size, deadline_ns);
}

static hl_host_result hl_linux_unpark(void *context, uint32_t scope, uint64_t key, const void *address,
                                      uint32_t count) {
    hl_host_linux *host = context;
    return hl_host_sync_unpark(host->sync, scope, key, address, count);
}

static hl_host_result hl_linux_interrupt_park(void *context, uint64_t waiter) {
    hl_host_linux *host = context;
    return hl_host_sync_interrupt_park(host->sync, waiter);
}

static hl_host_result hl_linux_fork_prepare(void *context) {
    hl_host_linux *host = context;
    hl_host_result result;
    if (pthread_mutex_lock(&host->fork_gate) != 0) return hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_lock(&host->lock) != 0) {
        pthread_mutex_unlock(&host->fork_gate);
        return hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    result = hl_host_sync_fork_prepare(host->sync);
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_unlock(&host->lock);
        pthread_mutex_unlock(&host->fork_gate);
    }
    return result;
}

static hl_host_result hl_linux_fork_complete(void *context) {
    hl_host_linux *host = context;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

static hl_host_result hl_linux_fork_child(void *context) {
    hl_host_linux *host = context;
    uint32_t index;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    /* Only the forking thread exists here, so every waiter record inherited from the parent is about
     * a thread this process does not have. Left in place they would hand a reused waiter identity
     * someone else's outstanding interruption. */
    hl_host_sync_park_reset(host->sync);
    for (index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_linux_counter_subscription *subscription = host->counter_subscriptions[index];
        if (subscription == NULL) continue;
        if (!subscription->active) continue;
        close(subscription->descriptor);
        close(subscription->wake[0]);
        close(subscription->wake[1]);
        subscription->active = 0;
        subscription->counter = HL_HOST_HANDLE_INVALID;
        subscription->notify = NULL;
        subscription->observer = NULL;
    }
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_linux_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

hl_status hl_host_linux_create(hl_host_linux **out_host, hl_host_services *out_services) {
    static const hl_host_memory_services memory = {HL_HOST_MEMORY_ABI,
                                                   sizeof(memory),
                                                   hl_linux_memory_reserve,
                                                   hl_linux_memory_protect,
                                                   hl_linux_memory_release,
                                                   hl_linux_memory_publish,
                                                   hl_linux_memory_reserve_code,
                                                   hl_linux_memory_repair_code,
                                                   hl_linux_memory_code_write,
                                                   hl_linux_memory_code_write,
                                                   hl_linux_memory_map_file,
                                                   hl_linux_memory_sync,
                                                   hl_linux_memory_unmap_range,
                                                   hl_linux_memory_map_anonymous,
                                                   hl_linux_memory_discard,
                                                   hl_linux_memory_repair_signal_page,
                                                   hl_linux_memory_unmap_address,
                                                   hl_linux_memory_wire_range,
                                                   hl_linux_memory_unwire_range,
                                                   hl_linux_memory_protect_address,
                                                   hl_linux_memory_sync_address};
    static const hl_host_clock_services clock = {.abi = HL_HOST_CLOCK_ABI,
                                                 .size = sizeof(clock),
                                                 .monotonic_ns = hl_linux_monotonic,
                                                 .realtime_ns = hl_linux_realtime,
                                                 .raw_monotonic_ns = hl_linux_raw_monotonic,
                                                 .process_cpu_ns = hl_linux_process_cpu,
                                                 .thread_cpu_ns = hl_linux_thread_cpu,
                                                 .sleep_until = hl_linux_clock_sleep_until,
                                                 .architectural_counter_hz = hl_linux_architectural_counter,
                                                 .backoff_ns = hl_linux_backoff};
    static const hl_host_log_services log = {HL_HOST_LOG_ABI, sizeof(log), hl_linux_log};
    static const hl_host_file_services file = {HL_HOST_FILE_ABI,
                                               sizeof(file),
                                               hl_linux_file_open,
                                               hl_linux_file_read,
                                               hl_linux_file_write,
                                               hl_linux_file_append,
                                               hl_linux_file_metadata_get,
                                               hl_linux_file_close,
                                               hl_linux_file_read_sequential,
                                               hl_linux_file_write_sequential,
                                               hl_linux_file_clone_for_fork,
                                               hl_linux_file_seek,
                                               hl_linux_file_readv,
                                               hl_linux_file_writev,
                                               hl_linux_file_readv_at,
                                               hl_linux_file_writev_at,
                                               hl_linux_file_appendv,
                                               hl_linux_file_truncate,
                                               hl_linux_file_sync,
                                               hl_linux_file_data_sync,
                                               hl_linux_file_rename,
                                               hl_linux_file_unlink,
                                               hl_linux_file_path,
                                               hl_linux_file_standard_stream,
                                               hl_linux_file_readlink,
                                               hl_linux_file_set_owner,
                                               hl_linux_file_resolve_beneath,
                                               hl_linux_file_sync_range,
                                               hl_linux_file_sync_filesystem,
                                               hl_linux_file_open_beneath,
                                               hl_linux_file_allocate_range,
                                               hl_linux_file_filesystem_metadata,
                                               hl_linux_file_set_permissions,
                                               hl_linux_file_set_times,
                                               hl_linux_file_read_directory,
                                               hl_linux_file_mkdir,
                                               hl_linux_file_symlink,
                                               hl_linux_file_link,
                                               hl_linux_file_fifo,
                                               hl_linux_file_validate_private_regular,
                                               hl_linux_file_store_private_atomic,
                                               hl_linux_file_validate_private_directory,
                                               hl_linux_file_rmdir};
    static const hl_host_event_services event = {
        HL_HOST_EVENT_ABI,          sizeof(event),       hl_linux_event_create, hl_linux_event_control,
        hl_linux_event_wait,        hl_linux_event_wake, hl_linux_event_close,  hl_linux_event_arm_timer,
        hl_linux_event_disarm_timer};
    static const hl_host_network_services network = {HL_HOST_NETWORK_ABI,
                                                     sizeof(network),
                                                     hl_linux_network_socket,
                                                     hl_linux_network_bind,
                                                     hl_linux_network_connect,
                                                     hl_linux_network_send,
                                                     hl_linux_network_receive,
                                                     hl_linux_network_close,
                                                     hl_linux_network_listen,
                                                     hl_linux_network_accept,
                                                     hl_linux_network_pair,
                                                     hl_linux_network_shutdown,
                                                     hl_linux_network_local_address,
                                                     hl_linux_network_peer_address,
                                                     hl_linux_network_get_option,
                                                     hl_linux_network_set_option,
                                                     hl_linux_network_send_message,
                                                     hl_linux_network_receive_message,
                                                     hl_linux_network_readiness,
                                                     hl_linux_network_wait_handle,
                                                     hl_linux_network_set_status_flags,
                                                     hl_linux_network_duplicate};
    static const hl_host_shared_memory_services shared_memory = {HL_HOST_SHARED_MEMORY_ABI, sizeof(shared_memory),
                                                                 hl_linux_shared_create,    hl_linux_shared_open,
                                                                 hl_linux_shared_resize,    hl_linux_shared_close};
    static const hl_host_counter_services counter = {
        HL_HOST_COUNTER_ABI,          sizeof(counter),
        hl_linux_counter_create,      hl_linux_counter_read,
        hl_linux_counter_write,       hl_linux_counter_get_flags,
        hl_linux_counter_set_flags,   hl_linux_counter_duplicate,
        hl_linux_counter_readiness,   hl_linux_counter_subscribe,
        hl_linux_counter_unsubscribe, hl_linux_counter_close,
    };
    static const hl_host_transfer_services transfer = {
        HL_HOST_TRANSFER_ABI,    sizeof(transfer),          hl_linux_transfer_channel_pair,
        hl_linux_transfer_send,  hl_linux_transfer_receive, hl_linux_transfer_duplicate,
        hl_linux_transfer_close,
    };
    static const hl_host_directory_services directory = {
        HL_HOST_DIRECTORY_ABI,     sizeof(directory),         hl_linux_directory_create, hl_linux_directory_add,
        hl_linux_directory_modify, hl_linux_directory_remove, hl_linux_directory_read,   hl_linux_directory_duplicate,
        hl_linux_directory_close};
    static const hl_host_watch_services watch = {HL_HOST_WATCH_ABI,    sizeof(watch),        hl_linux_watch_open,
                                                 hl_linux_watch_query, hl_linux_watch_drain, hl_linux_watch_close};
    static const hl_host_stream_services stream = {HL_HOST_STREAM_ABI,        sizeof(stream),
                                                   hl_linux_stream_pipe_pair, hl_linux_stream_read,
                                                   hl_linux_stream_write,     hl_linux_stream_duplicate,
                                                   hl_linux_stream_close,     hl_linux_stream_set_status_flags,
                                                   hl_linux_stream_readiness, hl_linux_stream_move};
    static const hl_host_posix_attachment_services posix_attachment = {
        HL_HOST_POSIX_ATTACHMENT_ABI, sizeof(posix_attachment), hl_linux_attachment_borrow_file,
        hl_linux_attachment_borrow_file_at_least, hl_linux_attachment_release};
    static const hl_host_process_services process = {
        HL_HOST_PROCESS_ABI,        sizeof(process),        hl_linux_process_spawn,         hl_linux_process_wait,
        hl_linux_process_terminate, hl_linux_process_close, hl_linux_process_spawn_prepared};
    static const hl_host_sync_services sync = {HL_HOST_SYNC_ABI,      sizeof(sync),           hl_linux_mutex_create,
                                               hl_linux_mutex_lock,   hl_linux_mutex_unlock,  hl_linux_mutex_close,
                                               hl_linux_fork_prepare, hl_linux_fork_complete, hl_linux_fork_child,
                                               hl_linux_park,         hl_linux_unpark,        hl_linux_interrupt_park};
    static const hl_host_terminal_services terminal = {HL_HOST_TERMINAL_ABI,       sizeof(terminal),
                                                       hl_linux_terminal_probe,    hl_linux_terminal_get_mode,
                                                       hl_linux_terminal_set_mode, hl_linux_terminal_get_size,
                                                       hl_linux_terminal_set_size, hl_linux_terminal_read,
                                                       hl_linux_terminal_write,    hl_linux_terminal_size_change_event};
    hl_host_linux *host;
    uint32_t i;
    if (out_host == NULL || out_services == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_host = NULL;
    memset(out_services, 0, sizeof(*out_services));
    host = calloc(1, sizeof(*host));
    if (host == NULL) return HL_STATUS_OUT_OF_MEMORY;
    host->handles = calloc(HL_LINUX_HANDLE_CAPACITY, sizeof(*host->handles));
    host->timers = calloc(HL_LINUX_TIMER_CAPACITY, sizeof(*host->timers));
    if (host->handles == NULL || host->timers == NULL) {
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    host->handle_capacity = HL_LINUX_HANDLE_CAPACITY;
    host->timer_capacity = HL_LINUX_TIMER_CAPACITY;
    if (pthread_mutex_init(&host->lock, NULL) != 0) {
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_mutex_init(&host->fork_gate, NULL) != 0) {
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_cond_init(&host->process_changed, NULL) != 0) {
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (hl_host_sync_registry_create(&host->sync) != HL_STATUS_OK) {
        pthread_cond_destroy(&host->process_changed);
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    for (i = 0; i < host->handle_capacity; ++i) {
        host->handles[i].descriptor = -1;
        host->handles[i].wake_descriptor = -1;
    }
    for (i = 0; i < host->timer_capacity; ++i)
        host->timers[i].descriptor = -1;
    out_services->abi = HL_HOST_SERVICES_ABI;
    out_services->size = sizeof(*out_services);
    out_services->capabilities = HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_LOG | HL_HOST_CAP_FILE |
                                 HL_HOST_CAP_EVENT | HL_HOST_CAP_EVENT_TIMER | HL_HOST_CAP_NETWORK |
                                 HL_HOST_CAP_SHARED_MEMORY | HL_HOST_CAP_PROCESS | HL_HOST_CAP_CODE_MAPPING |
                                 HL_HOST_CAP_SYNC | HL_HOST_CAP_COUNTER | HL_HOST_CAP_TRANSFER | HL_HOST_CAP_DIRECTORY |
                                 HL_HOST_CAP_WATCH | HL_HOST_CAP_STREAM | HL_HOST_CAP_POSIX_ATTACHMENT |
                                 HL_HOST_CAP_TERMINAL;
    out_services->context = host;
    out_services->memory = &memory;
    out_services->clock = &clock;
    out_services->log = &log;
    out_services->file = &file;
    out_services->event = &event;
    out_services->network = &network;
    out_services->shared_memory = &shared_memory;
    out_services->process = &process;
    out_services->sync = &sync;
    out_services->counter = &counter;
    out_services->transfer = &transfer;
    out_services->directory = &directory;
    out_services->watch = &watch;
    out_services->stream = &stream;
    out_services->posix_attachment = &posix_attachment;
    out_services->terminal = &terminal;
    *out_host = host;
    return HL_STATUS_OK;
}

void hl_host_linux_destroy(hl_host_linux *host) {
    uint32_t i;
    if (host == NULL) return;
    pthread_mutex_lock(&host->lock);
    host->destroying = 1;
    for (i = 0; i < host->handle_capacity; ++i) {
        hl_linux_handle_entry *entry = &host->handles[i];
        if (entry->kind == HL_LINUX_HANDLE_PROCESS && !entry->process_reaped) kill(entry->descriptor, SIGKILL);
    }
    for (;;) {
        uint32_t waiters = 0;
        for (i = 0; i < host->handle_capacity; ++i)
            waiters += host->handles[i].process_waiters;
        if (waiters == 0) break;
        pthread_cond_wait(&host->process_changed, &host->lock);
    }
    pthread_mutex_unlock(&host->lock);
    for (i = 0; i < host->counter_subscription_capacity; ++i) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[i] != NULL && host->counter_subscriptions[i]->active)
            handle = ((uint64_t)host->counter_subscriptions[i]->generation << 32) | (uint64_t)(i + 1u);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_linux_counter_unsubscribe(host, handle);
    }
    for (i = 0; i < host->handle_capacity; ++i) {
        hl_linux_handle_entry *entry = &host->handles[i];
        if (entry->kind == HL_LINUX_HANDLE_MAPPING) {
            /* Teardown gives back only what is still held, for the same reason release does. */
            uint64_t held_offset;
            uint64_t held_size;
            for (uint32_t part = 0;
                 hl_host_hole_set_held_range(&entry->retired, entry->size, part, &held_offset, &held_size); ++part)
                munmap((char *)entry->address + held_offset, (size_t)held_size);
            if (entry->executable_address != NULL && entry->executable_address != entry->address)
                munmap(entry->executable_address, (size_t)entry->size);
            if (entry->descriptor >= 0) close(entry->descriptor);
            hl_host_hole_set_release(&entry->retired);
        } else if (entry->kind == HL_LINUX_HANDLE_PROCESS) {
            int status;
            if (entry->process_reaped) continue;
            kill(entry->descriptor, SIGKILL);
            while (waitpid(entry->descriptor, &status, 0) < 0 && errno == EINTR) {}
        } else if (entry->kind == HL_LINUX_HANDLE_DIRECTORY) {
            hl_linux_directory_object *object = entry->address;
            close(entry->descriptor);
            if (--object->references == 0) {
                free(object->pending);
                free(object->watches);
                free(object);
            }
        } else if (entry->kind == HL_LINUX_HANDLE_WATCH) {
            close(entry->descriptor);
            close(entry->wake_descriptor);
            free(entry->address);
        } else if (entry->kind != HL_LINUX_HANDLE_NONE) {
            if (entry->descriptor >= 0) close(entry->descriptor);
            if (entry->wake_descriptor >= 0) close(entry->wake_descriptor);
        }
    }
    hl_host_sync_registry_destroy(host->sync);
    pthread_cond_destroy(&host->process_changed);
    pthread_mutex_destroy(&host->fork_gate);
    pthread_mutex_destroy(&host->lock);
    for (i = 0; i < host->counter_subscription_capacity; ++i)
        free(host->counter_subscriptions[i]);
    free(host->counter_subscriptions);
    free(host->handles);
    free(host->timers);
    free(host);
}
