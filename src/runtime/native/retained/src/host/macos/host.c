#define _DARWIN_C_SOURCE

#include "hl/macos.h"
#include "probe.h"
#include "../range.h"
#include "../system.h"
#include "../resolve.h"
#include "../sync.h"

#include <errno.h>
#include <sys/resource.h>
#include <dirent.h>
#include <fcntl.h>
#include <libkern/OSCacheControl.h>
#include <limits.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <mach/mach_vm.h>
#include <mach/thread_policy.h>
#include <pthread.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/event.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/socket.h>
#include <sys/sem.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#define HL_MACOS_MAPPING_CAPACITY 4096u
#define HL_MACOS_LINUX_PAGE_SIZE 4096u
#define HL_MACOS_FILE_CAPACITY 1024u
#define HL_MACOS_PROCESS_CAPACITY 1024u
#define HL_MACOS_EVENT_CAPACITY 64u
#define HL_MACOS_TIMER_CAPACITY 32u
#define HL_MACOS_COUNTER_CAPACITY 128u
#define HL_MACOS_TRANSFER_CAPACITY 64u
#define HL_MACOS_DIRECTORY_CAPACITY 128u
#define HL_MACOS_DIRECTORY_WATCH_CAPACITY 256u
#define HL_MACOS_WATCH_CAPACITY 128u
#define HL_MACOS_COUNTER_SUBSCRIPTIONS_INITIAL 128u

typedef struct hl_macos_mapping {
    uint32_t generation;
    uint32_t active;
    void *writable;
    void *executable;
    uint64_t size;
    /* The subranges of [writable, writable + size) a partial unmap gave back. writable and size
     * stay the addressing frame every offset-keyed call is measured against, so what the handle
     * still holds has to be recorded beside them rather than folded into them. */
    hl_host_hole_set retired;
} hl_macos_mapping;

typedef struct hl_macos_stream_shared {
    int semaphore;
    uint32_t references;
} hl_macos_stream_shared;

typedef struct hl_macos_directory_shared {
    pthread_mutex_t lock;
    uint32_t references;
    uint64_t position;
} hl_macos_directory_shared;

typedef struct hl_macos_file {
    uint32_t generation;
    uint32_t active;
    uint32_t shared;
    int descriptor;
    int append_descriptor;
    hl_macos_stream_shared *stream;
    uint32_t stream_endpoint;
    DIR *directory;
    uint64_t directory_position;
    hl_macos_directory_shared *directory_shared;
} hl_macos_file;

typedef struct hl_macos_process {
    uint32_t generation;
    uint32_t active;
    pid_t pid;
    uint32_t reaped;
    uint32_t waiting;
    uint32_t waiters;
    uint32_t exit_kind;
    uint32_t exit_value;
} hl_macos_process;

typedef struct hl_macos_timer {
    uint64_t token;
    uint64_t interval_ns;
    uint32_t active;
} hl_macos_timer;

typedef struct hl_macos_event {
    uint32_t generation;
    uint32_t active;
    int descriptor;
    hl_macos_timer *timers;
    uint32_t timer_capacity;
} hl_macos_event;

typedef struct hl_macos_watch {
    uint32_t generation;
    uint32_t active;
    int descriptor;
    uint64_t delivered_generation;
    uint64_t modified_ns;
    uint64_t changed_ns;
    nlink_t links;
    hl_host_watch_record record;
} hl_macos_watch;

typedef struct hl_macos_counter_shared {
    pthread_mutex_t lock;
    uint64_t value;
    uint32_t flags;
    uint32_t references;
} hl_macos_counter_shared;

typedef struct hl_macos_counter_object {
    hl_macos_counter_shared *shared;
    int backing;
    int readable;
    int signal;
} hl_macos_counter_object;

typedef struct hl_macos_counter {
    uint32_t generation;
    uint32_t active;
    hl_macos_counter_object *object;
    uint32_t rights;
} hl_macos_counter;

typedef struct hl_macos_counter_subscription {
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
} hl_macos_counter_subscription;

typedef struct hl_macos_transfer {
    uint32_t generation;
    uint32_t active;
    int descriptor;
} hl_macos_transfer;

typedef struct hl_macos_directory_watch {
    uint64_t token;
    uint32_t interests;
    int descriptor;
    uint32_t active;
} hl_macos_directory_watch;

typedef struct hl_macos_directory_object {
    uint32_t references;
    int descriptor;
    hl_macos_directory_watch *watches;
    uint32_t watch_capacity;
} hl_macos_directory_object;

typedef struct hl_macos_directory {
    uint32_t generation;
    uint32_t active;
    hl_macos_directory_object *object;
} hl_macos_directory;

struct hl_host_macos {
    hl_host_sync_registry *sync;
    hl_macos_mapping *mappings;
    hl_macos_file *files;
    hl_macos_process *processes;
    hl_macos_event *events;
    hl_macos_counter *counters;
    hl_macos_counter_subscription **counter_subscriptions;
    hl_macos_transfer *transfers;
    hl_macos_directory *directories;
    hl_macos_watch *watches;
    pthread_cond_t process_changed;
    pthread_mutex_t lock;
    pthread_mutex_t fork_gate;
    uint32_t destroying;
    uint32_t mapping_capacity;
    uint32_t file_capacity;
    uint32_t process_capacity;
    uint32_t event_capacity;
    uint32_t counter_capacity;
    uint32_t counter_subscription_capacity;
    uint32_t transfer_capacity;
    uint32_t directory_capacity;
    uint32_t watch_capacity;
};

static uint32_t hl_macos_grow_capacity(uint32_t current, uint32_t initial, size_t element_size) {
    uint32_t capacity = current == 0 ? initial : (current <= UINT32_MAX / 2u ? current * 2u : 0);
    return capacity != 0 && (size_t)capacity <= SIZE_MAX / element_size ? capacity : 0;
}

uint32_t hl_host_macos_active_mappings(hl_host_macos *host) {
    uint32_t active = 0;
    uint32_t index;
    if (host == NULL) return 0;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->mapping_capacity; ++index)
        if (host->mappings[index].active) ++active;
    pthread_mutex_unlock(&host->lock);
    return active;
}

static int hl_macos_file_descriptor(hl_host_macos *host, hl_host_handle handle, int append);

static int hl_macos_stream_lock(hl_macos_stream_shared *stream, uint32_t endpoint) {
    struct sembuf operation = {(unsigned short)endpoint, -1, SEM_UNDO};
    int result;
    do
        result = semop(stream->semaphore, &operation, 1);
    while (result != 0 && errno == EINTR);
    return result;
}

static void hl_macos_stream_unlock(hl_macos_stream_shared *stream, uint32_t endpoint) {
    struct sembuf operation = {(unsigned short)endpoint, 1, SEM_UNDO};
    while (semop(stream->semaphore, &operation, 1) != 0 && errno == EINTR) {}
}

static void hl_macos_stream_release(hl_macos_stream_shared *stream) {
    if (stream != NULL && __atomic_sub_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL) == 0) {
        (void)semctl(stream->semaphore, 0, IPC_RMID);
        (void)munmap(stream, sizeof(*stream));
    }
}

static hl_macos_directory_shared *hl_macos_directory_shared_create(void) {
    pthread_mutexattr_t attributes;
    hl_macos_directory_shared *shared =
        mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
    if (shared == MAP_FAILED) return NULL;
    memset(shared, 0, sizeof(*shared));
    if (pthread_mutexattr_init(&attributes) != 0) {
        munmap(shared, sizeof(*shared));
        return NULL;
    }
    int initialized = pthread_mutexattr_setpshared(&attributes, PTHREAD_PROCESS_SHARED) == 0 &&
                      pthread_mutex_init(&shared->lock, &attributes) == 0;
    pthread_mutexattr_destroy(&attributes);
    if (!initialized) {
        munmap(shared, sizeof(*shared));
        return NULL;
    }
    shared->references = 1;
    return shared;
}

static void hl_macos_directory_shared_release(hl_macos_directory_shared *shared) {
    if (shared != NULL && __atomic_sub_fetch(&shared->references, 1u, __ATOMIC_ACQ_REL) == 0) {
        pthread_mutex_destroy(&shared->lock);
        munmap(shared, sizeof(*shared));
    }
}

static hl_host_result hl_macos_fork_complete(void *context);
static hl_host_result hl_macos_fork_child(void *context);
static hl_host_result hl_macos_counter_unsubscribe(void *context, hl_host_handle subscription);
static hl_host_result hl_macos_file_close(void *context, hl_host_handle handle);
static void hl_macos_counter_unsubscribe_all(hl_host_macos *host, hl_host_handle counter);

static uint64_t hl_macos_monotonic_value(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static void hl_macos_sleep_until(uint64_t deadline_ns) {
    uint64_t now = hl_macos_monotonic_value();
    uint64_t remaining;
    struct timespec delay;
    if (now >= deadline_ns) return;
    remaining = deadline_ns - now;
    if (remaining > UINT64_C(1000000)) remaining = UINT64_C(1000000);
    delay.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
    delay.tv_nsec = (long)(remaining % UINT64_C(1000000000));
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {}
}

static void hl_macos_process_changed_wait(hl_host_macos *host, uint64_t deadline_ns) {
    struct timespec realtime;
    uint64_t now;
    uint64_t remaining;
    uint64_t absolute;
    if (deadline_ns == HL_HOST_DEADLINE_INFINITE) {
        pthread_cond_wait(&host->process_changed, &host->lock);
        return;
    }
    now = hl_macos_monotonic_value();
    if (now >= deadline_ns) return;
    remaining = deadline_ns - now;
    clock_gettime(CLOCK_REALTIME, &realtime);
    absolute = (uint64_t)realtime.tv_sec * UINT64_C(1000000000) + (uint64_t)realtime.tv_nsec + remaining;
    realtime.tv_sec = (time_t)(absolute / UINT64_C(1000000000));
    realtime.tv_nsec = (long)(absolute % UINT64_C(1000000000));
    pthread_cond_timedwait(&host->process_changed, &host->lock, &realtime);
}

static hl_host_result hl_macos_result(hl_status status, uint64_t value, uint64_t detail) {
    return (hl_host_result){(int32_t)status, 2, value, detail};
}

static int hl_macos_private_add_many(int *descriptors, uint32_t count) {
    uint32_t index;
    for (index = 0; index < count; ++index) {
        if (descriptors[index] < 0) continue;
        int adopted = hl_host_process_fd_private_adopt(descriptors[index]);
        if (adopted >= 0) {
            descriptors[index] = adopted;
            continue;
        }
        while (index != 0) {
            --index;
            if (descriptors[index] >= 0) hl_host_process_fd_private_remove(descriptors[index]);
        }
        return -1;
    }
    return 0;
}

static hl_status hl_macos_status(int error) {
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
    default: return HL_STATUS_PLATFORM_FAILURE;
    }
}

static hl_host_result hl_macos_errno(void) {
    int error = errno;
    return hl_macos_result(hl_macos_status(error), 0, (uint64_t)(unsigned int)error);
}

typedef enum hl_macos_handle_kind {
    HL_MACOS_HANDLE_MAPPING = 1,
    HL_MACOS_HANDLE_FILE = 2,
    HL_MACOS_HANDLE_EVENT = 3,
    HL_MACOS_HANDLE_COUNTER = 4,
    HL_MACOS_HANDLE_DIRECTORY = 5,
    HL_MACOS_HANDLE_TRANSFER = 6,
    HL_MACOS_HANDLE_WATCH = 7,
    HL_MACOS_HANDLE_PROCESS = 8,
    HL_MACOS_HANDLE_SUBSCRIPTION = 9
} hl_macos_handle_kind;

static hl_host_handle hl_macos_handle(hl_macos_handle_kind kind, uint32_t index, uint32_t generation) {
    return ((uint64_t)generation << 32) | ((uint64_t)kind << 28) | (uint64_t)(index + 1u);
}

static int hl_macos_handle_index(hl_host_handle handle, hl_macos_handle_kind kind, uint32_t capacity, uint32_t *index) {
    uint32_t low = (uint32_t)handle;
    uint32_t value = low & UINT32_C(0x0fffffff);
    if ((low >> 28) != (uint32_t)kind || value == 0 || value - 1u >= capacity) return 0;
    *index = value - 1u;
    return 1;
}

static hl_macos_mapping *hl_macos_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_MAPPING, host->mapping_capacity, &index) ||
        !host->mappings[index].active || host->mappings[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->mappings[index];
}

/* Retire a mapping slot. The frame, the record of what it gave back, and the active bit go
 * together: keeping them apart is what once left a handle claiming a hole it had already unmapped. */
static void hl_macos_retire_mapping_locked(hl_macos_mapping *mapping) {
    hl_host_hole_set_release(&mapping->retired);
    mapping->active = 0;
    mapping->writable = NULL;
    mapping->executable = NULL;
    mapping->size = 0;
}

/* True when [low, high) touches a byte this mapping still holds. The frame alone is not the answer,
 * because a partial unmap gives bytes back without consuming the handle. Both aliases of a code
 * mapping count, because releasing either one out from under the owner is the failure the callers
 * of this exist to prevent; only the writable alias is reachable by a subrange unmap, so only it
 * carries holes. */
static inline int hl_macos_mapping_holds_locked(const hl_macos_mapping *mapping, uintptr_t low, uintptr_t high) {
    if (!mapping->active || mapping->size == 0) return 0;
    if (mapping->writable != NULL) {
        uintptr_t base = (uintptr_t)mapping->writable;
        uintptr_t end = base + (uintptr_t)mapping->size;
        if (low < end && base < high) {
            uint64_t from = low > base ? (uint64_t)(low - base) : 0;
            uint64_t to = high < end ? (uint64_t)(high - base) : mapping->size;
            if (hl_host_hole_set_holds(&mapping->retired, from, to - from)) return 1;
        }
    }
    if (mapping->executable != NULL) {
        uintptr_t base = (uintptr_t)mapping->executable;
        if (low < base + (uintptr_t)mapping->size && base < high) return 1;
    }
    return 0;
}

static hl_host_result hl_macos_register(hl_host_macos *host, void *writable, void *executable, uint64_t size) {
    uint32_t index;
    hl_host_handle handle = 0;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->mapping_capacity; index++) {
        hl_macos_mapping *mapping = &host->mappings[index];
        if (!mapping->active) {
            /* A reused slot must not inherit the previous tenant's holes. Every mapping retirement
             * already drops them; this is the belt that makes that impossible to get wrong. */
            hl_host_hole_set_release(&mapping->retired);
            mapping->generation++;
            if (mapping->generation == 0) mapping->generation = 1;
            mapping->active = 1;
            mapping->writable = writable;
            mapping->executable = executable;
            mapping->size = size;
            handle = hl_macos_handle(HL_MACOS_HANDLE_MAPPING, index, mapping->generation);
            break;
        }
    }
    if (handle == 0) {
        uint32_t capacity =
            host->mapping_capacity > UINT32_C(0x0ffffffe) / 2u ? UINT32_C(0x0ffffffe) : host->mapping_capacity * 2u;
        hl_macos_mapping *grown =
            capacity > host->mapping_capacity ? realloc(host->mappings, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            memset(grown + host->mapping_capacity, 0, (size_t)(capacity - host->mapping_capacity) * sizeof(*grown));
            index = host->mapping_capacity;
            host->mappings = grown;
            host->mapping_capacity = capacity;
            hl_macos_mapping *mapping = &host->mappings[index];
            mapping->generation = 1;
            mapping->active = 1;
            mapping->writable = writable;
            mapping->executable = executable;
            mapping->size = size;
            handle = hl_macos_handle(HL_MACOS_HANDLE_MAPPING, index, mapping->generation);
        }
    }
    pthread_mutex_unlock(&host->lock);
    return handle != 0 ? hl_macos_result(HL_STATUS_OK, handle, 0) : hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

static int hl_macos_mapping_fill(hl_host_macos *host, hl_host_handle handle, void *address, uint64_t size) {
    int filled = 0;
    pthread_mutex_lock(&host->lock);
    hl_macos_mapping *mapping = hl_macos_lookup(host, handle);
    if (mapping != NULL) {
        mapping->writable = address;
        mapping->size = size;
        filled = 1;
    }
    pthread_mutex_unlock(&host->lock);
    return filled;
}

static int hl_macos_protection(uint32_t flags) {
    int protection = 0;
    if ((flags & HL_HOST_MEMORY_READ) != 0) protection |= PROT_READ;
    if ((flags & HL_HOST_MEMORY_WRITE) != 0) protection |= PROT_WRITE;
    if ((flags & HL_HOST_MEMORY_EXECUTE) != 0) protection |= PROT_EXEC;
    return protection;
}

static int hl_macos_dual_map(uint64_t size, vm_inherit_t inheritance, void **writable_out, void **executable_out) {
    void *writable = mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    mach_vm_address_t executable = 0;
    vm_prot_t current = 0;
    vm_prot_t maximum = 0;
    kern_return_t result;
    if (writable == MAP_FAILED) return -1;
    result = mach_vm_remap(mach_task_self(), &executable, size, 0, VM_FLAGS_ANYWHERE, mach_task_self(),
                           (mach_vm_address_t)writable, FALSE, &current, &maximum, inheritance);
    if (result == KERN_SUCCESS)
        result = mach_vm_protect(mach_task_self(), executable, size, FALSE, VM_PROT_READ | VM_PROT_EXECUTE);
    if (result != KERN_SUCCESS) {
        if (executable != 0) mach_vm_deallocate(mach_task_self(), executable, size);
        munmap(writable, (size_t)size);
        return -1;
    }
    *writable_out = writable;
    *executable_out = (void *)(uintptr_t)executable;
    return 0;
}

static hl_host_result hl_macos_reserve(void *context, uint64_t size, uint64_t alignment, uint32_t flags) {
    hl_host_macos *host = context;
    long page = sysconf(_SC_PAGESIZE);
    void *address;
    hl_host_result handle;
    if (size == 0 || size > SIZE_MAX || page <= 0 || alignment > (uint64_t)page)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    address = mmap(NULL, (size_t)size, hl_macos_protection(flags), MAP_PRIVATE | MAP_ANON, -1, 0);
    if (address == MAP_FAILED) return hl_macos_errno();
    handle = hl_macos_register(host, address, NULL, size);
    if (handle.status != HL_STATUS_OK) munmap(address, (size_t)size);
    return handle;
}

static hl_host_result hl_macos_protect(void *context, hl_host_handle handle, uint64_t offset, uint64_t size,
                                       uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    int result;
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL || offset > mapping->size || size > mapping->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = mprotect((char *)mapping->writable + offset, (size_t)size, hl_macos_protection(flags));
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_release(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    int result;
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
               hl_host_hole_set_held_range(&mapping->retired, mapping->size, part, &held_offset, &held_size)) {
            result = munmap((char *)mapping->writable + held_offset, (size_t)held_size);
            ++part;
        }
    }
    if (mapping->executable != NULL && mapping->executable != mapping->writable)
        (void)munmap(mapping->executable, (size_t)mapping->size);
    if (result == 0) hl_macos_retire_mapping_locked(mapping);
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_discard(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    hl_macos_retire_mapping_locked(mapping);
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static int hl_macos_repair_signal_page(void *context, uint64_t address, uint64_t size, uint32_t protection) {
    (void)context;
    if (address == 0 || address > UINTPTR_MAX || size != UINT64_C(4096) || (address & UINT64_C(4095)) != 0 ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0)
        return 0;
    void *page = (void *)(uintptr_t)address;
    int native_protection = hl_macos_protection(protection);
    if (mprotect(page, (size_t)size, native_protection) == 0) return 1;
    mach_vm_address_t exact = (mach_vm_address_t)address;
    kern_return_t allocated = mach_vm_allocate(mach_task_self(), &exact, (mach_vm_size_t)size, VM_FLAGS_FIXED);
    if (allocated == KERN_SUCCESS) {
        if (native_protection == (PROT_READ | PROT_WRITE)) return 1;
        vm_prot_t vm_protection = 0;
        if ((native_protection & PROT_READ) != 0) vm_protection |= VM_PROT_READ;
        if ((native_protection & PROT_WRITE) != 0) vm_protection |= VM_PROT_WRITE;
        if ((native_protection & PROT_EXEC) != 0) vm_protection |= VM_PROT_EXECUTE;
        if (mach_vm_protect(mach_task_self(), exact, (mach_vm_size_t)size, FALSE, vm_protection) == KERN_SUCCESS)
            return 1;
        (void)mach_vm_deallocate(mach_task_self(), exact, (mach_vm_size_t)size);
        return 0;
    }
    return mprotect(page, (size_t)size, native_protection) == 0;
}

static int hl_macos_pread_fill(int descriptor, void *buffer, size_t size, off_t offset) {
    size_t done = 0;
    while (done < size) {
        ssize_t count = pread(descriptor, (char *)buffer + done, size - done, offset + (off_t)done);
        if (count > 0) {
            done += (size_t)count;
            continue;
        }
        if (count == 0) break;
        if (errno == EINTR) continue;
        return -1;
    }
    if (done < size) memset((char *)buffer + done, 0, size - done);
    return 0;
}

/* Darwin has no portable mmap flag with Linux MAP_FIXED_NOREPLACE semantics.  Claim the
 * destination atomically with Mach first: VM_FLAGS_FIXED, unlike VM_FLAGS_OVERWRITE,
 * fails when any part is occupied.  Once claimed, MAP_FIXED can only replace our own
 * reservation; non-fixed mappings cannot race into it. */
static hl_host_result hl_macos_reserve_exact(uint64_t address, uint64_t size) {
    mach_vm_address_t reserved = (mach_vm_address_t)address;
    kern_return_t status = mach_vm_allocate(mach_task_self(), &reserved, (mach_vm_size_t)size, VM_FLAGS_FIXED);
    if (status == KERN_SUCCESS) return hl_macos_result(HL_STATUS_OK, 0, 0);
    if (status == KERN_NO_SPACE || status == KERN_MEMORY_PRESENT)
        return hl_macos_result(HL_STATUS_ALREADY_EXISTS, 0, 0);
    if (status == KERN_INVALID_ADDRESS || status == KERN_INVALID_ARGUMENT)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (status == KERN_RESOURCE_SHORTAGE) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, (uint32_t)status, 0);
}

static hl_host_result hl_macos_map_file(void *context, hl_host_handle file, uint64_t requested_address, uint64_t offset,
                                        uint64_t size, uint32_t protection, uint32_t flags,
                                        hl_host_file_mapping *output) {
    hl_host_macos *host = context;
    hl_host_result registered;
    void *address;
    int descriptor;
    long page = sysconf(_SC_PAGESIZE);
    int native_flags;
    uint32_t placement = flags & (HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    uint32_t sharing = flags & (HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_FILE_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || offset > INT64_MAX || page <= 0 || offset % HL_MACOS_LINUX_PAGE_SIZE != 0 ||
        requested_address > UINTPTR_MAX ||
        (requested_address != 0 && requested_address % HL_MACOS_LINUX_PAGE_SIZE != 0) ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_SHARED && sharing != HL_HOST_MEMORY_PRIVATE) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, file, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_macos_register(host, NULL, NULL, size);
    if (registered.status != HL_STATUS_OK) return registered;
    if (placement == 0 && sharing == HL_HOST_MEMORY_PRIVATE && (offset % (uint64_t)page) != 0) {
        address = mmap((void *)(uintptr_t)requested_address, (size_t)size, PROT_READ | PROT_WRITE,
                       MAP_ANON | MAP_PRIVATE, -1, 0);
        if (address == MAP_FAILED) {
            hl_host_result failure = hl_macos_errno();
            (void)hl_macos_discard(context, registered.value);
            return failure;
        }
        if (hl_macos_pread_fill(descriptor, address, (size_t)size, (off_t)offset) != 0) {
            int error = errno;
            munmap(address, (size_t)size);
            errno = error;
            hl_host_result failure = hl_macos_errno();
            (void)hl_macos_discard(context, registered.value);
            return failure;
        }
        if (!hl_macos_mapping_fill(host, registered.value, address, size)) abort();
        output->handle = registered.value;
        output->address = (uint64_t)(uintptr_t)address;
        output->mapped_size = size;
        output->reserved = 0;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if (placement == HL_HOST_MEMORY_FIXED && sharing == HL_HOST_MEMORY_PRIVATE &&
        ((requested_address % (uint64_t)page) != 0 || (offset % (uint64_t)page) != 0)) {
        uint64_t low = requested_address & ~((uint64_t)page - 1u);
        uint64_t head = requested_address - low;
        if (head > UINT64_MAX - size || head + size > UINT64_MAX - ((uint64_t)page - 1u)) {
            (void)hl_macos_discard(context, registered.value);
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
        uint64_t total = (head + size + (uint64_t)page - 1u) & ~((uint64_t)page - 1u);
        if (total > SIZE_MAX || low > (uint64_t)UINTPTR_MAX - total) {
            (void)hl_macos_discard(context, registered.value);
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
        uint8_t *head_copy = head != 0 ? malloc((size_t)head) : NULL;
        if (head != 0 && head_copy == NULL) {
            free(head_copy);
            (void)hl_macos_discard(context, registered.value);
            return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        }
        if (head != 0) memcpy(head_copy, (void *)(uintptr_t)low, (size_t)head);
        address = mmap((void *)(uintptr_t)low, (size_t)total, PROT_READ | PROT_WRITE,
                       MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
        if (address != MAP_FAILED) {
            if (hl_macos_pread_fill(descriptor, (void *)(uintptr_t)requested_address, (size_t)size, (off_t)offset) !=
                0) {
                int error = errno;
                munmap(address, (size_t)total);
                address = MAP_FAILED;
                errno = error;
            } else {
                if (head != 0) memcpy((void *)(uintptr_t)low, head_copy, (size_t)head);
            }
        }
        free(head_copy);
        if (address == MAP_FAILED) {
            hl_host_result failure = hl_macos_errno();
            (void)hl_macos_discard(context, registered.value);
            return failure;
        }
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->mapping_capacity; ++index) {
            hl_macos_mapping *old = &host->mappings[index];
            if (hl_macos_mapping_holds_locked(old, (uintptr_t)low, (uintptr_t)(low + total)))
                hl_macos_retire_mapping_locked(old);
        }
        pthread_mutex_unlock(&host->lock);
        if (!hl_macos_mapping_fill(host, registered.value, address, total)) abort();
        output->handle = registered.value;
        output->address = requested_address;
        output->mapped_size = size;
        output->reserved = head;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if ((requested_address % (uint64_t)page) != 0 || (offset % (uint64_t)page) != 0) {
        (void)hl_macos_discard(context, registered.value);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    native_flags = sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE;
    if (placement == HL_HOST_MEMORY_FIXED) native_flags |= MAP_FIXED;
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        hl_host_result reserved = hl_macos_reserve_exact(requested_address, size);
        if (reserved.status != HL_STATUS_OK) {
            (void)hl_macos_discard(context, registered.value);
            return reserved;
        }
        native_flags |= MAP_FIXED;
    }
    address = mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_macos_protection(protection), native_flags,
                   descriptor, (off_t)offset);
    if (address == MAP_FAILED) {
        int error = errno;
        if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE)
            (void)mach_vm_deallocate(mach_task_self(), (mach_vm_address_t)requested_address, (mach_vm_size_t)size);
        errno = error;
        hl_host_result failure = hl_macos_errno();
        (void)hl_macos_discard(context, registered.value);
        return failure;
    }
    if (sharing == HL_HOST_MEMORY_PRIVATE) {
        struct stat metadata;
        if (fstat(descriptor, &metadata) == 0) {
            uint64_t available = (uint64_t)metadata.st_size > offset ? (uint64_t)metadata.st_size - offset : 0;
            uint64_t quiet = available > UINT64_MAX - ((uint64_t)page - 1u)
                                 ? UINT64_MAX
                                 : (available + (uint64_t)page - 1u) & ~((uint64_t)page - 1u);
            if (quiet < size)
                (void)mmap((char *)address + quiet, (size_t)(size - quiet), PROT_READ | PROT_WRITE,
                           MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
        }
    }
    /* MAP_FIXED replaced these VMAs atomically. Retire stale handles without unmapping the
     * replacement -- but only the ones that still held a byte of it. A handle whose overlap with
     * this range is entirely inside a hole it already gave back kept nothing here to go stale. */
    if (placement == HL_HOST_MEMORY_FIXED) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->mapping_capacity; ++index) {
            hl_macos_mapping *old = &host->mappings[index];
            if (hl_macos_mapping_holds_locked(old, low, high)) hl_macos_retire_mapping_locked(old);
        }
        pthread_mutex_unlock(&host->lock);
    }
    if (!hl_macos_mapping_fill(host, registered.value, address, size)) abort();
    output->handle = registered.value;
    output->address = (uint64_t)(uintptr_t)address;
    output->mapped_size = size;
    output->reserved = 0;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_map_anonymous(void *context, uint64_t requested_address, uint64_t size,
                                             uint32_t protection, uint32_t flags, hl_host_memory_mapping *output) {
    hl_host_macos *host = context;
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
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_macos_register(host, NULL, NULL, size);
    if (registered.status != HL_STATUS_OK) return registered;
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        hl_host_result reserved = hl_macos_reserve_exact(requested_address, size);
        if (reserved.status != HL_STATUS_OK) {
            (void)hl_macos_discard(context, registered.value);
            return reserved;
        }
    }
    int native_flags = (sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE) | MAP_ANON;
    if (placement != 0) native_flags |= MAP_FIXED;
    address =
        mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_macos_protection(protection), native_flags, -1, 0);
    if (address == MAP_FAILED) {
        hl_host_result failure = hl_macos_errno();
        if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE)
            (void)mach_vm_deallocate(mach_task_self(), (mach_vm_address_t)requested_address, (mach_vm_size_t)size);
        (void)hl_macos_discard(context, registered.value);
        return failure;
    }
    if (placement != 0) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->mapping_capacity; ++index) {
            hl_macos_mapping *old = &host->mappings[index];
            if (hl_macos_mapping_holds_locked(old, low, high)) hl_macos_retire_mapping_locked(old);
        }
        pthread_mutex_unlock(&host->lock);
    }
    pthread_mutex_lock(&host->lock);
    hl_macos_mapping *owned = hl_macos_lookup(host, registered.value);
    if (owned != NULL) owned->writable = address;
    pthread_mutex_unlock(&host->lock);
    *output = (hl_host_memory_mapping){
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(*output), registered.value, (uint64_t)(uintptr_t)address, size, 0};
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_mapping_sync(void *context, hl_host_handle handle, uint64_t offset, uint64_t size) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    int status;
    if (size == 0 || size > SIZE_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL || offset > mapping->size || size > mapping->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = msync((char *)mapping->writable + offset, (size_t)size, MS_SYNC);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_unmap_range(void *context, hl_host_handle handle, uint64_t offset, uint64_t size) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    int status;
    long page = sysconf(_SC_PAGESIZE);
    if (size == 0 || size > SIZE_MAX || page <= 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL || offset > mapping->size || size > mapping->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((offset != 0 || size != mapping->size) && (offset % (uint64_t)page != 0 || size % (uint64_t)page != 0)) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = munmap((char *)mapping->writable + offset, (size_t)size);
    if (status == 0) {
        /* A full-range unmap consumes the handle. A partial one keeps it, so the subrange it just
         * gave back has to leave the handle's coverage too -- otherwise the handle goes on claiming
         * a hole the address space is free to hand to someone else. When repeated partial unmaps
         * finally leave nothing held, the handle is consumed exactly as a single full one would
         * have consumed it: a live mapping handle always holds at least one byte.
         *
         * If the record cannot grow, the subrange stays claimed. That refuses a reuse that would
         * have been legal, which is recoverable; the other direction is not. */
        if ((offset == 0 && size == mapping->size) || (hl_host_hole_set_retire(&mapping->retired, offset, size) &&
                                                       !hl_host_hole_set_holds(&mapping->retired, 0, mapping->size)))
            hl_macos_retire_mapping_locked(mapping);
    }
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

/* True while any live mapping handle still holds a byte of [low, high). */
static int hl_macos_range_owned_locked(hl_host_macos *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->mapping_capacity; ++index)
        if (hl_macos_mapping_holds_locked(&host->mappings[index], low, high)) return 1;
    return 0;
}

/* Reject before acting, so a refused range is left exactly as it was found. */
static hl_host_result hl_macos_unmap_address(void *context, uint64_t address, uint64_t size) {
    hl_host_macos *host = context;
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t low;
    int status;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    low = (uintptr_t)address;
    pthread_mutex_lock(&host->lock);
    if (hl_macos_range_owned_locked(host, low, low + (uintptr_t)size)) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_BUSY, 0, 0);
    }
    status = munmap((void *)low, (size_t)size);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

/* True while any live CODE mapping still holds a byte of [low, high). A code mapping is the one
 * whose protection an address-keyed caller must not touch: the writable and executable views are a
 * pair, the per-thread write gate flips between them, and a caller holding only an address cannot
 * put back what it changed because it does not hold the handle that knows the other view. */
static int hl_macos_code_range_owned_locked(hl_host_macos *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->mapping_capacity; ++index) {
        const hl_macos_mapping *mapping = &host->mappings[index];
        if (mapping->executable != NULL && hl_macos_mapping_holds_locked(mapping, low, high)) return 1;
    }
    return 0;
}

/* Page-align an address-keyed span the way mprotect(2) and msync(2) do: the address must already be
 * aligned and the length is rounded up. Returns zero when the request cannot be expressed. */
static int hl_macos_address_span(uint64_t address, uint64_t size, uintptr_t *low, size_t *span) {
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
static hl_host_result hl_macos_protect_address(void *context, uint64_t address, uint64_t size, uint32_t protection) {
    hl_host_macos *host = context;
    uintptr_t low;
    size_t span;
    int status;
    if ((protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        !hl_macos_address_span(address, size, &low, &span))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    if (hl_macos_code_range_owned_locked(host, low, low + (uintptr_t)span)) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_BUSY, 0, 0);
    }
    status = mprotect((void *)low, span, hl_macos_protection(protection));
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_sync_address(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    uintptr_t low;
    size_t span;
    int native_flags;
    (void)context;
    if ((flags & ~(uint32_t)(HL_HOST_MEMORY_SYNC_ASYNC | HL_HOST_MEMORY_SYNC_INVALIDATE)) != 0 ||
        !hl_macos_address_span(address, size, &low, &span))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    native_flags = (flags & HL_HOST_MEMORY_SYNC_ASYNC) != 0 ? MS_ASYNC : MS_SYNC;
    if ((flags & HL_HOST_MEMORY_SYNC_INVALIDATE) != 0) native_flags |= MS_INVALIDATE;
    /* No ownership question is asked. Flushing takes nothing away from a handle that covers the
     * range: the mapping, its protection and its contents are all exactly as they were. */
    return msync((void *)low, span, native_flags) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

/* Darwin mlock(2) pins against reclaim and charges RLIMIT_MEMLOCK exactly as Linux does, so this host
 * reports HL_HOST_WIRE_RESIDENT. The length follows mlock's own rule and is rounded up to whole pages. */
static hl_host_result hl_macos_wire_range(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX || flags != 0 ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (mlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_macos_unwire_range(void *context, uint64_t address, uint64_t size) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (munlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_macos_publish(void *context, hl_host_handle handle, uint64_t offset, uint64_t size) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    pthread_mutex_lock(&host->lock);
    mapping = hl_macos_lookup(host, handle);
    if (mapping == NULL || offset > mapping->size || size > mapping->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    sys_icache_invalidate((char *)(mapping->executable != NULL ? mapping->executable : mapping->writable) + offset,
                          (size_t)size);
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_begin_code_write(void *context) {
    (void)context;
    pthread_jit_write_protect_np(0);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_end_code_write(void *context) {
    (void)context;
    pthread_jit_write_protect_np(1);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_reserve_code(void *context, uint64_t size, uint64_t alignment, uint32_t flags,
                                            hl_host_code_mapping *output) {
    hl_host_macos *host = context;
    void *writable;
    void *executable;
    hl_host_result handle;
    long page = sysconf(_SC_PAGESIZE);
    if (output == NULL || size == 0 || size > SIZE_MAX || page <= 0 || alignment > (uint64_t)page)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    if ((flags & HL_HOST_CODE_DUAL_ALIAS) != 0) {
        if (hl_macos_dual_map(size, VM_INHERIT_NONE, &writable, &executable) != 0)
            return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    } else {
        writable =
            mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
        if (writable == MAP_FAILED) return hl_macos_errno();
        executable = writable;
    }
    handle = hl_macos_register(host, writable, executable, size);
    if (handle.status != HL_STATUS_OK) {
        if (executable != writable) munmap(executable, (size_t)size);
        munmap(writable, (size_t)size);
        return handle;
    }
    output->abi = 1;
    output->size = sizeof(*output);
    output->handle = handle.value;
    output->writable_address = (uint64_t)(uintptr_t)writable;
    output->executable_address = (uint64_t)(uintptr_t)executable;
    output->mapped_size = size;
    return handle;
}

static hl_host_result hl_macos_repair_code(void *context, hl_host_code_mapping *public_mapping, uint32_t preserve) {
    hl_host_macos *host = context;
    hl_macos_mapping *mapping;
    void *writable;
    void *executable;
    kern_return_t result = KERN_FAILURE;
    if (public_mapping == NULL || public_mapping->abi != 1 || public_mapping->size < sizeof(*public_mapping))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_init(&host->lock, NULL);
    mapping = hl_macos_lookup(host, public_mapping->handle);
    if (mapping == NULL || mapping->executable == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (mapping->executable == mapping->writable) return hl_macos_result(HL_STATUS_OK, public_mapping->handle, 0);
    if (preserve != 0) {
        mach_vm_address_t target = (mach_vm_address_t)mapping->executable;
        vm_prot_t current = 0;
        vm_prot_t maximum = 0;
        int remapped = 0;
        result = mach_vm_remap(mach_task_self(), &target, mapping->size, 0, VM_FLAGS_FIXED, mach_task_self(),
                               (mach_vm_address_t)mapping->writable, FALSE, &current, &maximum, VM_INHERIT_NONE);
        if (result == KERN_SUCCESS) {
            remapped = 1;
            result = mach_vm_protect(mach_task_self(), target, mapping->size, FALSE, VM_PROT_READ | VM_PROT_EXECUTE);
        }
        if (result == KERN_SUCCESS) return hl_macos_result(HL_STATUS_OK, public_mapping->handle, 0);
        if (remapped) mach_vm_deallocate(mach_task_self(), target, mapping->size);
    }
    if (hl_macos_dual_map(mapping->size, VM_INHERIT_NONE, &writable, &executable) != 0)
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, (uint64_t)result);
    munmap(mapping->writable, (size_t)mapping->size);
    mapping->writable = writable;
    mapping->executable = executable;
    public_mapping->writable_address = (uint64_t)(uintptr_t)writable;
    public_mapping->executable_address = (uint64_t)(uintptr_t)executable;
    return hl_macos_result(HL_STATUS_OK, public_mapping->handle, 0);
}

static hl_host_result hl_macos_clock(clockid_t clock_id) {
    struct timespec value;
    if (clock_gettime(clock_id, &value) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec, 0);
}

static hl_host_result hl_macos_monotonic(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_MONOTONIC);
}

static hl_host_result hl_macos_realtime(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_REALTIME);
}

static hl_host_result hl_macos_raw_monotonic(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_MONOTONIC_RAW);
}

static hl_host_result hl_macos_process_cpu(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_PROCESS_CPUTIME_ID);
}

static hl_host_result hl_macos_thread_cpu(void *context) {
    (void)context;
    return hl_macos_clock(CLOCK_THREAD_CPUTIME_ID);
}

static hl_host_result hl_macos_architectural_counter(void *context) {
    mach_timebase_info_data_t timebase = {0, 0};
    uint64_t frequency;
    (void)context;
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.numer == 0 || timebase.denom == 0)
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    frequency = (uint64_t)(((unsigned __int128)UINT64_C(1000000000) * timebase.denom) / timebase.numer);
    if (frequency == 0) return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return hl_macos_result(HL_STATUS_OK, frequency, 0);
}

static hl_host_result hl_macos_backoff(void *context, uint64_t interval_ns) {
    struct timespec remaining;
    (void)context;
    remaining.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    remaining.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR) return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static void hl_macos_precise_sleep_begin(void) {
    mach_timebase_info_data_t timebase;
    thread_time_constraint_policy_data_t policy;
    double nanoseconds_to_ticks;
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.numer == 0) return;
    nanoseconds_to_ticks = (double)timebase.denom / (double)timebase.numer;
    policy.period = (uint32_t)(500000.0 * nanoseconds_to_ticks);
    policy.computation = (uint32_t)(100000.0 * nanoseconds_to_ticks);
    policy.constraint = (uint32_t)(500000.0 * nanoseconds_to_ticks);
    policy.preemptible = 1;
    (void)thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&policy,
                            THREAD_TIME_CONSTRAINT_POLICY_COUNT);
}

static void hl_macos_precise_sleep_end(void) {
    thread_standard_policy_data_t policy = {0};
    (void)thread_policy_set(mach_thread_self(), THREAD_STANDARD_POLICY, (thread_policy_t)&policy,
                            THREAD_STANDARD_POLICY_COUNT);
}

static hl_host_result hl_macos_clock_sleep_until(void *context, uint32_t clock_kind, uint64_t deadline_ns) {
    clockid_t clock_id;
    struct timespec now, delay;
    uint64_t now_ns, remaining;
    (void)context;
    switch (clock_kind) {
    case HL_HOST_CLOCK_MONOTONIC: clock_id = CLOCK_MONOTONIC; break;
    case HL_HOST_CLOCK_REALTIME: clock_id = CLOCK_REALTIME; break;
    case HL_HOST_CLOCK_PROCESS_CPU: clock_id = CLOCK_PROCESS_CPUTIME_ID; break;
    default: return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
    for (;;) {
        if (clock_gettime(clock_id, &now) != 0) return hl_macos_errno();
        now_ns = (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
        if (now_ns >= deadline_ns) return hl_macos_result(HL_STATUS_OK, 0, 0);
        remaining = deadline_ns - now_ns;
        /* Recheck non-monotonic clocks periodically so realtime adjustments and process-CPU progress
         * change the effective absolute deadline instead of becoming one stale wall-clock delay. */
        if (clock_kind != HL_HOST_CLOCK_MONOTONIC && remaining > UINT64_C(10000000)) remaining = UINT64_C(10000000);
        delay.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
        delay.tv_nsec = (long)(remaining % UINT64_C(1000000000));
        /* Match Linux high-resolution timer wakeups without leaking a Darwin scheduler policy into linux_abi. */
        hl_macos_precise_sleep_begin();
        if (nanosleep(&delay, NULL) != 0) {
            hl_host_result result = hl_macos_errno();
            hl_macos_precise_sleep_end();
            return result;
        }
        hl_macos_precise_sleep_end();
        if (clock_kind == HL_HOST_CLOCK_MONOTONIC) return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
}

static hl_macos_file *hl_macos_file_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_FILE, host->file_capacity, &index) ||
        !host->files[index].active || host->files[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->files[index];
}

static hl_host_result hl_macos_file_register(hl_host_macos *host, int descriptor, int append_descriptor,
                                             uint32_t shared) {
    uint32_t index;
    hl_host_handle handle = 0;
    if (descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        descriptor = adopted;
    }
    if (append_descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(append_descriptor);
        if (adopted < 0) {
            hl_host_process_fd_private_remove(descriptor);
            close(descriptor);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        append_descriptor = adopted;
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->file_capacity; ++index) {
        hl_macos_file *file = &host->files[index];
        if (!file->active) {
            file->generation++;
            if (file->generation == 0) file->generation = 1;
            file->active = 1;
            file->shared = shared;
            file->descriptor = descriptor;
            file->append_descriptor = append_descriptor;
            file->stream = NULL;
            file->stream_endpoint = 0;
            file->directory = NULL;
            file->directory_position = 0;
            file->directory_shared = NULL;
            handle = hl_macos_handle(HL_MACOS_HANDLE_FILE, index, file->generation);
            break;
        }
    }
    if (handle == 0) {
        uint32_t capacity =
            host->file_capacity > UINT32_C(0x0ffffffe) / 2u ? UINT32_C(0x0ffffffe) : host->file_capacity * 2u;
        hl_macos_file *grown =
            capacity > host->file_capacity ? realloc(host->files, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            memset(grown + host->file_capacity, 0, (size_t)(capacity - host->file_capacity) * sizeof(*grown));
            index = host->file_capacity;
            host->files = grown;
            host->file_capacity = capacity;
            hl_macos_file *file = &host->files[index];
            file->generation = 1;
            file->active = 1;
            file->shared = shared;
            file->descriptor = descriptor;
            file->append_descriptor = append_descriptor;
            file->stream = NULL;
            file->stream_endpoint = 0;
            file->directory = NULL;
            file->directory_position = 0;
            file->directory_shared = NULL;
            handle = hl_macos_handle(HL_MACOS_HANDLE_FILE, index, file->generation);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == 0) {
        hl_host_process_fd_private_remove(descriptor);
        hl_host_process_fd_private_remove(append_descriptor);
    }
    return handle != 0 ? hl_macos_result(HL_STATUS_OK, handle, 0) : hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

static hl_host_result hl_macos_file_open(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t access, uint32_t creation, uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd = AT_FDCWD;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        pthread_mutex_lock(&host->lock);
        hl_macos_file *file = hl_macos_file_lookup(host, directory);
        directory_fd = file != NULL ? file->descriptor : -1;
        pthread_mutex_unlock(&host->lock);
        if (directory_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((access & HL_HOST_FILE_PATH_ONLY) != 0)
#ifdef O_SYMLINK
        flags = (access & HL_HOST_FILE_NOFOLLOW) != 0 ? O_SYMLINK : O_RDONLY | O_NONBLOCK;
#else
        flags = O_RDONLY | O_NONBLOCK;
#endif
    else if ((access & (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE)) == (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE))
        flags = O_RDWR;
    else if ((access & HL_HOST_FILE_WRITE) != 0)
        flags = O_WRONLY;
    else if ((access & HL_HOST_FILE_READ) != 0)
        flags = O_RDONLY;
    else
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
#ifdef O_NOFOLLOW
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0 && (access & HL_HOST_FILE_PATH_ONLY) == 0) flags |= O_NOFOLLOW;
#endif
#ifdef O_DIRECTORY
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) flags |= O_DIRECTORY;
#endif
    /* PATH_ONLY models Linux O_PATH. O_CREAT/O_EXCL/O_TRUNC are ignored by
       that API even though macOS has no native O_PATH flag. */
    if ((access & HL_HOST_FILE_PATH_ONLY) == 0) {
        if ((creation & HL_HOST_FILE_CREATE) != 0) flags |= O_CREAT;
        if ((creation & HL_HOST_FILE_EXCLUSIVE) != 0) flags |= O_EXCL;
        if ((creation & HL_HOST_FILE_TRUNCATE) != 0) flags |= O_TRUNC;
    }
    if ((access & HL_HOST_FILE_APPEND) != 0) flags |= O_APPEND;
    descriptor = openat(directory_fd, local, flags | O_CLOEXEC, (mode_t)(permissions & 07777u));
    if (descriptor < 0) return hl_macos_errno();
    if ((access & HL_HOST_FILE_APPEND) != 0) {
        append_descriptor = dup(descriptor);
        if (append_descriptor < 0) {
            hl_host_result error = hl_macos_errno();
            close(descriptor);
            return error;
        }
    }
    hl_host_result result = hl_macos_file_register(host, descriptor, append_descriptor, 0);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
    }
    return result;
}

static int hl_macos_file_descriptor(hl_host_macos *host, hl_host_handle handle, int append);

static hl_host_result hl_macos_file_standard_stream(void *context, uint32_t stream) {
    hl_host_macos *host = context;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    uint32_t detail = 0;
    hl_host_result result;
    if (stream > HL_HOST_STANDARD_ERROR) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    flags = fcntl((int)stream, F_GETFL);
    if (flags < 0) return hl_macos_errno();
    descriptor = fcntl((int)stream, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_errno();
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
            hl_host_result error = hl_macos_errno();
            close(descriptor);
            return error;
        }
    }
    if ((flags & O_NONBLOCK) != 0) detail |= HL_HOST_FILE_NONBLOCK;
    result = hl_macos_file_register(host, descriptor, append_descriptor, 0);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
        return result;
    }
    result.detail = detail;
    return result;
}

static hl_host_result hl_macos_file_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    char path[PATH_MAX];
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX || descriptor < 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fcntl(descriptor, F_GETPATH, path) != 0) return hl_macos_errno();
    do
        count = readlink(path, output.data, output.size);
    while (count < 0 && errno == EINTR);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_set_owner(void *context, hl_host_handle file, uint32_t uid, uint32_t gid) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    int status;
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchown(descriptor, (uid_t)uid, (gid_t)gid);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_set_permissions(void *context, hl_host_handle file, uint32_t permissions) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    int status;
    if (descriptor < 0 || (permissions & ~07777u) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchmod(descriptor, (mode_t)permissions);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_set_times(void *context, hl_host_handle file, const hl_host_file_time times[2]) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    struct timespec native[2];
    int status;
    if (descriptor < 0 || times == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
    }
    do
        status = futimens(descriptor, native);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static int hl_macos_file_descriptor(hl_host_macos *host, hl_host_handle handle, int append) {
    hl_macos_file *file;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    descriptor = file == NULL ? -1 : (append ? file->append_descriptor : file->descriptor);
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_host_result hl_macos_attachment_borrow_file_at_least(void *context, hl_host_handle handle,
                                                               uint32_t minimum_descriptor) {
    hl_host_macos *host = context;
    int descriptor = -1;
    int found;
    if (minimum_descriptor > INT_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, handle);
    found = file != NULL;
    if (found) descriptor = fcntl(file->descriptor, F_DUPFD_CLOEXEC, (int)minimum_descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return found ? hl_macos_errno() : hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    return hl_macos_result(HL_STATUS_OK, (uint64_t)(unsigned)descriptor, 0);
}

static hl_host_result hl_macos_attachment_borrow_file(void *context, hl_host_handle handle) {
    return hl_macos_attachment_borrow_file_at_least(context, handle, 0);
}

static hl_host_result hl_macos_attachment_release(void *context, uint64_t borrowed_descriptor) {
    (void)context;
    if (borrowed_descriptor > INT_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int descriptor = (int)borrowed_descriptor;
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_read(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || descriptor < 0 || offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pread(descriptor, output.data, output.size, (off_t)offset);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_write(void *context, hl_host_handle file, uint64_t offset,
                                          hl_host_const_bytes input) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || descriptor < 0 || offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pwrite(descriptor, input.data, input.size, (off_t)offset);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_read_sequential(void *context, hl_host_handle file, void *output,
                                                    uint64_t output_size) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *stream = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    ssize_t count;
    int saved_error;
    if ((output_size != 0 && output == NULL) || output_size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        descriptor = dup(entry->descriptor);
        stream = entry->stream;
        endpoint = entry->stream_endpoint;
        if (descriptor >= 0 && stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (stream != NULL && hl_macos_stream_lock(stream, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_errno();
    }
    count = read(descriptor, output, (size_t)output_size);
    saved_error = errno;
    if (stream != NULL) hl_macos_stream_unlock(stream, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(stream);
    errno = saved_error;
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_write_sequential(void *context, hl_host_handle file, const void *input,
                                                     uint64_t input_size) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *stream = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    ssize_t count;
    int saved_error;
    if ((input_size != 0 && input == NULL) || input_size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        descriptor = dup(entry->descriptor);
        stream = entry->stream;
        endpoint = entry->stream_endpoint;
        if (descriptor >= 0 && stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (stream != NULL && hl_macos_stream_lock(stream, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_errno();
    }
    count = write(descriptor, input, (size_t)input_size);
    saved_error = errno;
    if (stream != NULL) hl_macos_stream_unlock(stream, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(stream);
    errno = saved_error;
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_clone_for_fork(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    hl_macos_file *entry;
    int descriptor = -1;
    int append_descriptor = -1;
    int needs_append = 0;
    uint32_t shared = 0;
    hl_macos_stream_shared *stream = NULL;
    uint32_t stream_endpoint = 0;
    hl_macos_directory_shared *directory_shared = NULL;
    int directory_error = 0;
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        needs_append = entry->append_descriptor >= 0;
        shared = entry->shared;
        stream = entry->stream;
        stream_endpoint = entry->stream_endpoint;
        struct stat status;
        if (entry->directory_shared == NULL && fstat(entry->descriptor, &status) == 0 && S_ISDIR(status.st_mode)) {
            entry->directory_shared = hl_macos_directory_shared_create();
            if (entry->directory_shared == NULL) directory_error = 1;
        }
        directory_shared = entry->directory_shared;
        if (directory_shared != NULL) (void)__atomic_add_fetch(&directory_shared->references, 1u, __ATOMIC_ACQ_REL);
        if (stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
        descriptor = fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
        if (descriptor >= 0 && needs_append) append_descriptor = fcntl(entry->append_descriptor, F_DUPFD_CLOEXEC, 0);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || (needs_append && append_descriptor < 0) || directory_error) {
        hl_host_result error = directory_error ? hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0) : hl_macos_errno();
        if (stream != NULL) (void)__atomic_sub_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
        hl_macos_directory_shared_release(directory_shared);
        if (descriptor >= 0) close(descriptor);
        return error;
    }
    {
        hl_host_result result = hl_macos_file_register(host, descriptor, append_descriptor, shared);
        if (result.status != HL_STATUS_OK) {
            if (stream != NULL) (void)__atomic_sub_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
            hl_macos_directory_shared_release(directory_shared);
            close(descriptor);
            if (append_descriptor >= 0) close(append_descriptor);
        } else {
            pthread_mutex_lock(&host->lock);
            hl_macos_file *copy = hl_macos_file_lookup(host, result.value);
            if (copy != NULL) {
                copy->directory_shared = directory_shared;
                copy->stream = stream;
                copy->stream_endpoint = stream_endpoint;
            }
            pthread_mutex_unlock(&host->lock);
        }
        return result;
    }
}

/* Some virtual/shared macOS filesystems reject SEEK_DATA/SEEK_HOLE with ENOTTY even for regular files.
 * Preserve the Linux contract there by discovering logical zero/data runs. Native APFS/HFS extents remain
 * authoritative whenever the filesystem implements the operation. */
static off_t hl_macos_sparse_seek_fallback(int descriptor, off_t offset, uint32_t whence) {
    unsigned char bytes[16384];
    struct stat metadata;
    off_t cursor = offset;
    int want_data = whence == HL_HOST_FILE_SEEK_DATA;
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (fstat(descriptor, &metadata) != 0) return -1;
    if (offset >= metadata.st_size) {
        errno = ENXIO;
        return -1;
    }
    while (cursor < metadata.st_size) {
        size_t amount =
            (uint64_t)(metadata.st_size - cursor) < sizeof(bytes) ? (size_t)(metadata.st_size - cursor) : sizeof(bytes);
        ssize_t count = pread(descriptor, bytes, amount, cursor);
        if (count <= 0) {
            if (count == 0) errno = ENXIO;
            return -1;
        }
        for (ssize_t index = 0; index < count; ++index)
            if ((bytes[index] != 0) == want_data) return cursor + index;
        cursor += count;
    }
    if (!want_data) return metadata.st_size;
    errno = ENXIO;
    return -1;
}

static hl_host_result hl_macos_file_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_macos *host = context;
    int descriptor = -1;
    off_t result;
    if (whence > HL_HOST_FILE_SEEK_HOLE) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) descriptor = entry->descriptor;
    if (entry != NULL && entry->directory_shared != NULL) {
        hl_macos_directory_shared *shared = entry->directory_shared;
        pthread_mutex_lock(&shared->lock);
        if (whence == SEEK_SET)
            result = (off_t)offset;
        else if (whence == SEEK_CUR && shared->position <= INT64_MAX)
            result = (off_t)shared->position + (off_t)offset;
        else
            result = -1;
        if (result >= 0) {
            shared->position = (uint64_t)result;
            if (entry->directory != NULL) {
                rewinddir(entry->directory);
                entry->directory_position = 0;
            }
        } else {
            errno = EINVAL;
        }
        pthread_mutex_unlock(&shared->lock);
        pthread_mutex_unlock(&host->lock);
        return result < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)result, 0);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (whence == HL_HOST_FILE_SEEK_DATA)
        result = lseek(descriptor, (off_t)offset, SEEK_DATA);
    else if (whence == HL_HOST_FILE_SEEK_HOLE)
        result = lseek(descriptor, (off_t)offset, SEEK_HOLE);
    else
        result = lseek(descriptor, (off_t)offset, (int)whence);
    if (result < 0 && (whence == HL_HOST_FILE_SEEK_DATA || whence == HL_HOST_FILE_SEEK_HOLE) && errno != EBADF &&
        errno != ESPIPE) {
        result = hl_macos_sparse_seek_fallback(descriptor, (off_t)offset, whence);
        if (result >= 0 && lseek(descriptor, result, SEEK_SET) < 0) result = -1;
    }
    return result < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)result, 0);
}

static hl_host_result hl_macos_stream_pipe_pair(void *context, uint32_t flags) {
    hl_host_macos *host = context;
    int descriptors[2] = {-1, -1};
    int native_flags = (flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0;
    int no_sigpipe = 1;
    hl_host_result input, output;
    hl_macos_stream_shared *shared;
    unsigned short semaphore_values[2] = {1, 1};
    union semun semaphore_argument;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
    if (shared == MAP_FAILED) return hl_macos_errno();
    shared->semaphore = semget(IPC_PRIVATE, 2, IPC_CREAT | 0600);
    shared->references = 2;
    semaphore_argument.array = semaphore_values;
    if (shared->semaphore < 0 || semctl(shared->semaphore, 0, SETALL, semaphore_argument) != 0) {
        hl_host_result error = hl_macos_errno();
        if (shared->semaphore >= 0) (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return error;
    }
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, descriptors) != 0) {
        hl_host_result error = hl_macos_errno();
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return error;
    }
    (void)shutdown(descriptors[0], SHUT_WR);
    (void)shutdown(descriptors[1], SHUT_RD);
    (void)setsockopt(descriptors[1], SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
    if (native_flags != 0) {
        (void)fcntl(descriptors[0], F_SETFL, fcntl(descriptors[0], F_GETFL) | native_flags);
        (void)fcntl(descriptors[1], F_SETFL, fcntl(descriptors[1], F_GETFL) | native_flags);
    }
    /* Host descriptors never survive a native exec; guest CLOEXEC remains ABI-table state. */
    (void)fcntl(descriptors[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(descriptors[1], F_SETFD, FD_CLOEXEC);
    input = hl_macos_file_register(host, descriptors[0], -1, 0);
    if (input.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return input;
    }
    output = hl_macos_file_register(host, descriptors[1], -1, 0);
    if (output.status != HL_STATUS_OK) {
        (void)hl_macos_file_close(host, input.value);
        close(descriptors[1]);
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return output;
    }
    pthread_mutex_lock(&host->lock);
    hl_macos_file *input_file = hl_macos_file_lookup(host, input.value);
    hl_macos_file *output_file = hl_macos_file_lookup(host, output.value);
    input_file->stream = shared;
    input_file->stream_endpoint = 0;
    output_file->stream = shared;
    output_file->stream_endpoint = 1;
    pthread_mutex_unlock(&host->lock);
    input.detail = output.value;
    return input;
}

static hl_host_result hl_macos_stream_set_status_flags(void *context, hl_host_handle stream, uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *shared = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    int current;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, stream);
    if (file != NULL && file->stream != NULL) {
        descriptor = dup(file->descriptor);
        shared = file->stream;
        endpoint = file->stream_endpoint;
        if (descriptor >= 0) (void)__atomic_add_fetch(&shared->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(shared);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (hl_macos_stream_lock(shared, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(shared);
        return hl_macos_errno();
    }
    current = fcntl(descriptor, F_GETFL);
    if (current >= 0) {
        current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0);
        if (fcntl(descriptor, F_SETFL, current) != 0) current = -1;
    }
    hl_macos_stream_unlock(shared, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(shared);
    return current >= 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static int hl_macos_stream_handle(hl_host_macos *host, hl_host_handle handle) {
    int valid;
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, handle);
    valid = file != NULL && file->stream != NULL;
    pthread_mutex_unlock(&host->lock);
    return valid;
}

static hl_host_result hl_macos_stream_read(void *context, hl_host_handle stream, hl_host_bytes output) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_read_sequential(context, stream, output.data, output.size);
}

static hl_host_result hl_macos_stream_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_write_sequential(context, stream, input.data, input.size);
}

static hl_host_result hl_macos_stream_duplicate(void *context, hl_host_handle stream) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_clone_for_fork(context, stream);
}

static hl_host_result hl_macos_stream_close(void *context, hl_host_handle stream) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_close(context, stream);
}

static hl_host_result hl_macos_stream_readiness(void *context, hl_host_handle stream, uint32_t interests) {
    int descriptor;
    struct pollfd probe;
    descriptor = hl_macos_file_descriptor(context, stream, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    probe = (struct pollfd){descriptor, 0, 0};
    if ((interests & HL_HOST_READY_READ) != 0) probe.events |= POLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) probe.events |= POLLOUT;
    if (poll(&probe, 1, 0) < 0) return hl_macos_errno();
    uint32_t ready = 0;
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    return hl_macos_result(HL_STATUS_OK, ready & interests, 0);
}

static hl_host_result hl_macos_stream_move(void *context, hl_host_handle source, uint64_t source_offset,
                                           hl_host_handle destination, uint64_t destination_offset, uint64_t size,
                                           uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *locks[2] = {NULL, NULL};
    hl_macos_stream_shared *input_pin = NULL, *output_pin = NULL;
    uint32_t endpoints[2] = {0, 0};
    uint32_t locked = 0;
    int input = -1, output = -1;
    unsigned char buffer[65536];
    struct stat input_status, output_status;
    off_t input_position = 0, output_position = 0;
    size_t request;
    ssize_t read_count, written;
    int input_stream, output_stream;
    hl_host_result result;
    uint32_t allowed = HL_HOST_STREAM_SOURCE_POSITIONED | HL_HOST_STREAM_DESTINATION_POSITIONED;
    if ((flags & ~allowed) != 0 || source_offset > INT64_MAX || destination_offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (size == 0) return hl_macos_result(HL_STATUS_OK, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *input_file = hl_macos_file_lookup(host, source);
    hl_macos_file *output_file = hl_macos_file_lookup(host, destination);
    if (input_file != NULL && output_file != NULL) {
        input = dup(input_file->descriptor);
        output = dup(output_file->descriptor);
        locks[0] = input_file->stream;
        endpoints[0] = input_file->stream_endpoint;
        locks[1] = output_file->stream;
        endpoints[1] = output_file->stream_endpoint;
        if (input >= 0 && locks[0] != NULL) {
            input_pin = locks[0];
            (void)__atomic_add_fetch(&input_pin->references, 1u, __ATOMIC_ACQ_REL);
        }
        if (output >= 0 && locks[1] != NULL) {
            output_pin = locks[1];
            (void)__atomic_add_fetch(&output_pin->references, 1u, __ATOMIC_ACQ_REL);
        }
        if (input < 0) locks[0] = NULL;
        if (output < 0) locks[1] = NULL;
        if (locks[1] != NULL && (locks[0] == NULL || locks[1]->semaphore < locks[0]->semaphore ||
                                 (locks[1] == locks[0] && endpoints[1] < endpoints[0]))) {
            hl_macos_stream_shared *swap_lock = locks[0];
            uint32_t swap_endpoint = endpoints[0];
            locks[0] = locks[1];
            endpoints[0] = endpoints[1];
            locks[1] = swap_lock;
            endpoints[1] = swap_endpoint;
        }
        for (locked = 0; locked < 2 && locks[locked] != NULL; ++locked) {
            if (locked != 0 && locks[locked] == locks[locked - 1] && endpoints[locked] == endpoints[locked - 1])
                continue;
            if (hl_macos_stream_lock(locks[locked], endpoints[locked]) != 0) break;
        }
    }
    pthread_mutex_unlock(&host->lock);
    {
        int descriptors[2] = {input, output};
        if (hl_macos_private_add_many(descriptors, 2) != 0) {
            result = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
            goto done;
        }
        input = descriptors[0];
        output = descriptors[1];
    }
    if (input < 0 || output < 0 || (locked < 2 && locks[locked] != NULL)) {
        result = hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        goto done;
    }
    if (fstat(input, &input_status) != 0 || fstat(output, &output_status) != 0) {
        result = hl_macos_errno();
        goto done;
    }
    input_stream = S_ISSOCK(input_status.st_mode) || S_ISFIFO(input_status.st_mode);
    output_stream = S_ISSOCK(output_status.st_mode) || S_ISFIFO(output_status.st_mode);
    if ((!input_stream && !output_stream) || ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0 && input_stream) ||
        ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0 && output_stream) ||
        (!input_stream && (flags & HL_HOST_STREAM_SOURCE_POSITIONED) == 0) ||
        (!output_stream && (flags & HL_HOST_STREAM_DESTINATION_POSITIONED) == 0)) {
        result = hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        goto done;
    }
    request = size < sizeof(buffer) ? (size_t)size : sizeof(buffer);
    if (input_stream) {
        read_count = recv(input, buffer, request, MSG_PEEK);
    } else {
        input_position = (off_t)source_offset;
        read_count = pread(input, buffer, request, input_position);
    }
    if (read_count <= 0) {
        result = read_count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, 0, 0);
        goto done;
    }
    if (output_stream) {
        written = send(output, buffer, (size_t)read_count, 0);
    } else {
        output_position = (off_t)destination_offset;
        written = pwrite(output, buffer, (size_t)read_count, output_position);
    }
    if (written <= 0) {
        result = written < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, 0, 0);
        goto done;
    }
    if (input_stream) {
        ssize_t consumed = recv(input, buffer, (size_t)written, 0);
        if (consumed != written) {
            result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
            goto done;
        }
    }
    result = hl_macos_result(HL_STATUS_OK, (uint64_t)written, 0);
done:
    while (locked != 0) {
        --locked;
        if (locked != 0 && locks[locked] == locks[locked - 1] && endpoints[locked] == endpoints[locked - 1]) continue;
        hl_macos_stream_unlock(locks[locked], endpoints[locked]);
    }
    if (input >= 0) {
        hl_host_process_fd_private_remove(input);
        close(input);
    }
    if (output >= 0) {
        hl_host_process_fd_private_remove(output);
        close(output);
    }
    hl_macos_stream_release(input_pin);
    hl_macos_stream_release(output_pin);
    return result;
}

static hl_host_result hl_macos_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    int descriptor = hl_macos_file_descriptor(context, file, 1);
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || descriptor < 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    if (count < 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_vector(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                           uint32_t count, uint64_t offset, int operation) {
    struct iovec native[HL_HOST_FILE_IOV_MAX];
    int descriptor = hl_macos_file_descriptor(context, file, operation == 4);
    ssize_t transferred;
    uint32_t index;
    if ((count != 0 && vectors == NULL) || count > HL_HOST_FILE_IOV_MAX || descriptor < 0 ||
        ((operation == 2 || operation == 3) && offset > INT64_MAX))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > SIZE_MAX || vectors[index].address > UINTPTR_MAX)
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        native[index].iov_base = (void *)(uintptr_t)vectors[index].address;
        native[index].iov_len = (size_t)vectors[index].size;
    }
    /*
     * An empty vector transfers nothing and succeeds on Linux, but Darwin's readv/writev family
     * rejects iovcnt 0 outright with EINVAL. Present one zero-length segment instead of zero
     * segments so the descriptor's access-mode check still runs and the caller observes the Linux
     * result. The Linux backend needs no such adjustment.
     */
    if (count == 0) {
        static char empty_segment;
        native[0].iov_base = &empty_segment;
        native[0].iov_len = 0;
        count = 1;
    }
    switch (operation) {
    case 0: transferred = readv(descriptor, native, (int)count); break;
    case 1: transferred = writev(descriptor, native, (int)count); break;
    case 2: transferred = preadv(descriptor, native, (int)count, (off_t)offset); break;
    case 3: transferred = pwritev(descriptor, native, (int)count, (off_t)offset); break;
    default: transferred = writev(descriptor, native, (int)count); break;
    }
    if (transferred < 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)transferred, 0);
}

#define HL_MACOS_VECTOR_WRAPPER(name, operation)                                                                       \
    static hl_host_result name(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {     \
        return hl_macos_file_vector(context, file, vectors, count, 0, operation);                                      \
    }
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_readv, 0)
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_writev, 1)
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_appendv, 4)

static hl_host_result hl_macos_file_truncate(void *context, hl_host_handle file, uint64_t size) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0 || size > INT64_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return ftruncate(descriptor, (off_t)size) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_sync(void *context, hl_host_handle file) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fsync(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_sync_range(void *context, hl_host_handle file, uint64_t offset, uint64_t size,
                                               uint32_t flags) {
    if ((flags & ~7u) != 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* macOS has no range durability primitive; fsync is stronger and truthful. */
    return hl_macos_file_sync(context, file);
}

static hl_host_result hl_macos_file_sync_filesystem(void *context, hl_host_handle file) {
    /* fsync the selected filesystem object; macOS exposes no syncfs equivalent. */
    return hl_macos_file_sync(context, file);
}

static int hl_macos_write_zeros(int descriptor, off_t begin, off_t end) {
    static const unsigned char zeros[65536];
    while (begin < end) {
        size_t request = (uint64_t)(end - begin) < sizeof(zeros) ? (size_t)(end - begin) : sizeof(zeros);
        ssize_t count = pwrite(descriptor, zeros, request, begin);
        if (count < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (count == 0) {
            errno = EIO;
            return -1;
        }
        begin += count;
    }
    return 0;
}

static hl_host_result hl_macos_file_allocate_range(void *context, hl_host_handle file, uint32_t mode, uint64_t offset,
                                                   uint64_t size) {
    const uint32_t keep = HL_HOST_FILE_ALLOC_KEEP_SIZE;
    const uint32_t punch = HL_HOST_FILE_ALLOC_PUNCH_HOLE;
    const uint32_t collapse = HL_HOST_FILE_ALLOC_COLLAPSE_RANGE;
    const uint32_t zero = HL_HOST_FILE_ALLOC_ZERO_RANGE;
    const uint32_t insert = HL_HOST_FILE_ALLOC_INSERT_RANGE;
    const uint32_t unshare = HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    const uint32_t allowed = keep | punch | collapse | zero | insert | unshare;
    unsigned char buffer[65536];
    struct stat status;
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    off_t begin, length, end, current;
    if (descriptor < 0 || size == 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size ||
        (mode & ~allowed) != 0 || ((mode & punch) != 0 && (mode & keep) == 0) ||
        ((mode & collapse) != 0 && mode != collapse) || ((mode & insert) != 0 && mode != insert) ||
        ((mode & unshare) != 0 && (mode & ~(unshare | keep)) != 0))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    begin = (off_t)offset;
    length = (off_t)size;
    end = begin + length;
    if (fstat(descriptor, &status) != 0) return hl_macos_errno();
    if (!S_ISREG(status.st_mode)) return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    current = status.st_size;
    if ((mode & punch) != 0) {
#ifdef F_PUNCHHOLE
        struct fpunchhole hole = {.fp_offset = begin, .fp_length = length};
        if (fcntl(descriptor, F_PUNCHHOLE, &hole) == 0) return hl_macos_result(HL_STATUS_OK, 0, 0);
        if (errno != EINVAL) return hl_macos_errno();
#endif
        if (begin < current && hl_macos_write_zeros(descriptor, begin, end < current ? end : current) != 0)
            return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if ((mode & zero) != 0) {
        off_t zero_end = (mode & keep) != 0 && end > current ? current : end;
        if ((mode & keep) == 0 && end > current && ftruncate(descriptor, end) != 0) return hl_macos_errno();
        if (begin < zero_end && hl_macos_write_zeros(descriptor, begin, zero_end) != 0) return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if ((mode & collapse) != 0) {
        off_t read_position = end, write_position = begin;
        if (end >= current) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        while (read_position < current) {
            size_t request = (uint64_t)(current - read_position) < sizeof(buffer) ? (size_t)(current - read_position)
                                                                                  : sizeof(buffer);
            ssize_t count = pread(descriptor, buffer, request, read_position);
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) {
                if (count == 0) errno = EIO;
                return hl_macos_errno();
            }
            size_t done = 0;
            while (done < (size_t)count) {
                ssize_t written = pwrite(descriptor, buffer + done, (size_t)count - done, write_position + (off_t)done);
                if (written < 0 && errno == EINTR) continue;
                if (written <= 0) {
                    if (written == 0) errno = EIO;
                    return hl_macos_errno();
                }
                done += (size_t)written;
            }
            read_position += count;
            write_position += count;
        }
        return ftruncate(descriptor, current - length) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
    }
    if ((mode & insert) != 0) {
        if (begin >= current || current > INT64_MAX - length) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (ftruncate(descriptor, current + length) != 0) return hl_macos_errno();
        for (off_t remaining = current - begin; remaining != 0;) {
            size_t request = (uint64_t)remaining < sizeof(buffer) ? (size_t)remaining : sizeof(buffer);
            off_t source = begin + remaining - (off_t)request;
            ssize_t count = pread(descriptor, buffer, request, source);
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) {
                if (count == 0) errno = EIO;
                return hl_macos_errno();
            }
            size_t done = 0;
            while (done < (size_t)count) {
                ssize_t written =
                    pwrite(descriptor, buffer + done, (size_t)count - done, source + length + (off_t)done);
                if (written < 0 && errno == EINTR) continue;
                if (written <= 0) {
                    if (written == 0) errno = EIO;
                    return hl_macos_errno();
                }
                done += (size_t)written;
            }
            remaining -= count;
        }
        if (hl_macos_write_zeros(descriptor, begin, end) != 0) return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
#ifdef F_PREALLOCATE
    {
        fstore_t store = {.fst_flags = F_ALLOCATECONTIG,
                          .fst_posmode = F_PEOFPOSMODE,
                          .fst_offset = begin - current,
                          .fst_length = length,
                          .fst_bytesalloc = 0};
        if (fcntl(descriptor, F_PREALLOCATE, &store) != 0) {
            store.fst_flags = F_ALLOCATEALL;
            if (fcntl(descriptor, F_PREALLOCATE, &store) != 0 && errno != EINVAL && errno != ENOTSUP)
                return hl_macos_errno();
        }
    }
#endif
    if ((mode & keep) == 0 && end > current && ftruncate(descriptor, end) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_filesystem_metadata(void *context, hl_host_handle file,
                                                        hl_host_filesystem_metadata *output) {
    struct statfs status;
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0 || output == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstatfs(descriptor, &status) != 0) return hl_macos_errno();
    memset(output, 0, sizeof(*output));
    output->blocks = status.f_blocks;
    output->blocks_free = status.f_bfree;
    output->blocks_available = status.f_bavail;
    output->files = status.f_files;
    output->files_free = status.f_ffree;
    output->filesystem_id[0] = (uint32_t)status.f_fsid.val[0];
    output->filesystem_id[1] = (uint32_t)status.f_fsid.val[1];
    output->block_size = (uint64_t)status.f_bsize;
    output->fragment_size = (uint64_t)status.f_bsize;
    output->name_max = NAME_MAX;
    output->flags = (uint64_t)status.f_flags;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                                   uint32_t entry_capacity, uint32_t byte_capacity) {
    hl_host_macos *host = context;
    uint32_t produced = 0, used = 0;
    int saved_error = 0;
    if (entries == NULL || entry_capacity == 0 || byte_capacity < 24 || byte_capacity > UINT32_C(1 << 20))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (entry->directory_shared == NULL) {
        struct stat status;
        if (fstat(entry->descriptor, &status) != 0) {
            saved_error = errno;
        } else if (!S_ISDIR(status.st_mode)) {
            saved_error = ENOTDIR;
        } else {
            entry->directory_shared = hl_macos_directory_shared_create();
            if (entry->directory_shared == NULL) saved_error = ENOMEM;
        }
    }
    hl_macos_directory_shared *shared = entry->directory_shared;
    if (saved_error == 0 && shared == NULL) saved_error = EIO;
    if (shared != NULL) pthread_mutex_lock(&shared->lock);
    if (saved_error == 0 && entry->directory == NULL) {
        int duplicate = fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
        if (duplicate < 0) {
            saved_error = errno;
        } else {
            entry->directory = fdopendir(duplicate);
            if (entry->directory == NULL) {
                saved_error = errno;
                close(duplicate);
            } else {
                entry->directory_position = 0;
            }
        }
    }
    if (saved_error == 0 && entry->directory == NULL) saved_error = EIO;
    if (saved_error == 0 && entry->directory_position != shared->position) {
        rewinddir(entry->directory);
        entry->directory_position = 0;
        while (entry->directory_position < shared->position) {
            errno = 0;
            if (readdir(entry->directory) == NULL) {
                saved_error = errno != 0 ? errno : EINVAL;
                break;
            }
            entry->directory_position++;
        }
    }
    while (saved_error == 0 && produced < entry_capacity) {
        long before = telldir(entry->directory);
        errno = 0;
        struct dirent *native = readdir(entry->directory);
        if (native == NULL) {
            saved_error = errno;
            break;
        }
        size_t name_size = strnlen(native->d_name, sizeof(native->d_name));
        uint32_t record_size = (uint32_t)((19u + name_size + 1u + 7u) & ~(size_t)7u);
        if (name_size == sizeof(native->d_name)) {
            saved_error = EIO;
            break;
        }
        if (record_size > byte_capacity - used) {
            seekdir(entry->directory, before);
            if (produced == 0) saved_error = EINVAL;
            break;
        }
        entries[produced].object = native->d_ino;
        entries[produced].next_offset = entry->directory_position + 1;
        entries[produced].type = native->d_type;
        entries[produced].name_size = (uint32_t)name_size;
        memcpy(entries[produced].name, native->d_name, name_size + 1);
        used += record_size;
        produced++;
        entry->directory_position++;
    }
    if (saved_error == 0) shared->position = entry->directory_position;
    if (shared != NULL) pthread_mutex_unlock(&shared->lock);
    pthread_mutex_unlock(&host->lock);
    if (saved_error != 0) {
        errno = saved_error;
        return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, produced, used);
}

static int hl_macos_file_directory(hl_host_macos *host, hl_host_handle directory) {
    int descriptor = AT_FDCWD;
    if (directory == HL_HOST_HANDLE_CWD) return descriptor;
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, directory);
    descriptor = file != NULL ? file->descriptor : -1;
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_host_result hl_macos_file_rename(void *context, hl_host_handle old_directory, const char *old_path,
                                           size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                           size_t new_path_size) {
    hl_host_macos *host = context;
    char old_local[PATH_MAX];
    char new_local[PATH_MAX];
    int old_fd;
    int new_fd;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = '\0';
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = '\0';
    old_fd = hl_macos_file_directory(host, old_directory);
    new_fd = hl_macos_file_directory(host, new_directory);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (renameat(old_fd, old_local, new_fd, new_local) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_unlink(void *context, hl_host_handle directory, const char *path,
                                           size_t path_size) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, 0) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_rmdir(void *context, hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, AT_REMOVEDIR) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_mkdir(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                          uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkdirat(directory_fd, local, (mode_t)permissions) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_macos_errno();
}

static hl_host_result hl_macos_file_fifo(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkfifoat(directory_fd, local, (mode_t)permissions) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                   : hl_macos_errno();
}

static hl_host_result hl_macos_file_symlink(void *context, const char *target, size_t target_size,
                                            hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_macos *host = context;
    char target_local[PATH_MAX], path_local[PATH_MAX];
    if (target == NULL || path == NULL || target_size == 0 || path_size == 0 || target_size >= sizeof(target_local) ||
        path_size >= sizeof(path_local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(target_local, target, target_size);
    target_local[target_size] = 0;
    memcpy(path_local, path, path_size);
    path_local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return symlinkat(target_local, directory_fd, path_local) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_macos_errno();
}

static hl_host_result hl_macos_file_link(void *context, hl_host_handle old_directory, const char *old_path,
                                         size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                         size_t new_path_size, uint32_t flags) {
    hl_host_macos *host = context;
    char old_local[PATH_MAX], new_local[PATH_MAX];
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local) || (flags & ~1u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = 0;
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = 0;
    int old_fd = hl_macos_file_directory(host, old_directory);
    int new_fd = hl_macos_file_directory(host, new_directory);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int native_flags = (flags & 1u) != 0 ? AT_SYMLINK_FOLLOW : 0;
    return linkat(old_fd, old_local, new_fd, new_local, native_flags) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                           : hl_macos_errno();
}

static hl_host_result hl_macos_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset) {
    return hl_macos_file_vector(context, file, vectors, count, offset, 2);
}

static hl_host_result hl_macos_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count, uint64_t offset) {
    return hl_macos_file_vector(context, file, vectors, count, offset, 3);
}

static hl_host_result hl_macos_file_metadata_get(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    struct stat status;
    if (descriptor < 0 || output == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstat(descriptor, &status) != 0) return hl_macos_errno();
    memset(output, 0, sizeof(*output));
    output->stable_device = (uint64_t)status.st_dev;
    output->stable_object = (uint64_t)status.st_ino;
    output->size = (uint64_t)status.st_size;
    output->allocated_size = (uint64_t)status.st_blocks * 512u;
    output->modified_ns =
        (uint64_t)status.st_mtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtimespec.tv_nsec;
    output->accessed_ns =
        (uint64_t)status.st_atimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_atimespec.tv_nsec;
    output->changed_ns =
        (uint64_t)status.st_ctimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctimespec.tv_nsec;
    output->created_ns =
        (uint64_t)status.st_birthtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_birthtimespec.tv_nsec;
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
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_resolve_beneath(void *context, hl_host_handle root, const char *path,
                                                    size_t path_size, uint32_t policy,
                                                    hl_host_file_resolution *output) {
    hl_host_macos *host = context;
    hl_host_resolved_path resolved = {0};
    hl_host_result parent;
    hl_host_result target = {HL_STATUS_OK, 0, HL_HOST_HANDLE_INVALID, 0};
    char local[PATH_MAX];
    int root_fd = hl_macos_file_descriptor(host, root, 0);
    /* Resolution is a metadata probe, never an I/O open.  O_NONBLOCK prevents a FIFO/device final
     * component from stalling the resolver before the Linux ABI can apply its actual open flags. */
    int target_flags = O_RDONLY | O_NONBLOCK;
    if (root_fd < 0 || output == NULL || path == NULL || path_size == 0 || path_size >= sizeof(local) ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS |
                              HL_HOST_RESOLVE_ALLOW_MISSING)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
#ifdef O_SYMLINK
    if ((policy & HL_HOST_RESOLVE_NOFOLLOW_FINAL) != 0) target_flags = O_SYMLINK;
#endif
    if ((policy & HL_HOST_RESOLVE_ALLOW_MISSING) != 0) target_flags = -1;
    if (hl_host_resolve_beneath(root_fd, local, policy & (HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS),
                                target_flags, &resolved) != 0)
        return hl_macos_errno();
    if (strlen(resolved.leaf) >= sizeof(output->final)) {
        hl_host_resolved_path_destroy(&resolved);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    parent = hl_macos_file_register(host, resolved.parent_fd, -1, 0);
    if (parent.status != HL_STATUS_OK) {
        hl_host_resolved_path_destroy(&resolved);
        return parent;
    }
    resolved.parent_fd = -1;
    if (resolved.target_fd >= 0) {
        target = hl_macos_file_register(host, resolved.target_fd, -1, 0);
        if (target.status != HL_STATUS_OK) {
            (void)hl_macos_file_close(host, parent.value);
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
        hl_host_file_metadata metadata = {0};
        if (hl_macos_file_metadata_get(host, output->target, &metadata).status == HL_STATUS_OK)
            output->target_type = metadata.type;
    }
    hl_host_resolved_path_destroy(&resolved);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_open_beneath(void *context, hl_host_handle root, const char *path, size_t path_size,
                                                 uint32_t access, uint32_t creation, uint32_t permissions,
                                                 uint32_t policy) {
    hl_host_file_resolution resolved = {0};
    hl_host_result result;
    if (path == NULL || path_size == 0 || path[0] == '/' || memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    uint32_t resolve_policy = policy | HL_HOST_RESOLVE_ALLOW_MISSING;
    if ((creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        resolve_policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_macos_file_resolve_beneath(context, root, path, path_size, resolve_policy, &resolved);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_macos_file_open(context, resolved.parent, resolved.final, resolved.final_size,
                                access | HL_HOST_FILE_NOFOLLOW, creation, permissions);
    if (resolved.target != HL_HOST_HANDLE_INVALID) (void)hl_macos_file_close(context, resolved.target);
    (void)hl_macos_file_close(context, resolved.parent);
    return result;
}

static hl_host_result hl_macos_file_path(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_macos *host = context;
    char path[PATH_MAX];
    hl_macos_file *file;
    int error = 0;
    size_t length;
    if ((output.size != 0 && output.data == 0) || output.size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    if (file == NULL)
        error = EBADF;
    else if (fcntl(file->descriptor, F_GETPATH, path) != 0)
        error = errno;
    pthread_mutex_unlock(&host->lock);
    if (error != 0) {
        errno = error;
        return hl_macos_errno();
    }
    length = strnlen(path, sizeof path);
    if (length > output.size) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, length, 0);
    if (length != 0) memcpy(output.data, path, length);
    return hl_macos_result(HL_STATUS_OK, length, 0);
}

static hl_host_result hl_macos_file_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_file *file;
    int descriptor;
    int append_descriptor;
    hl_macos_stream_shared *stream;
    DIR *directory;
    hl_macos_directory_shared *directory_shared;
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    if (file == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = file->descriptor;
    append_descriptor = file->append_descriptor;
    stream = file->stream;
    directory = file->directory;
    directory_shared = file->directory_shared;
    hl_host_process_fd_private_remove(descriptor);
    hl_host_process_fd_private_remove(append_descriptor);
    file->active = 0;
    file->shared = 0;
    file->descriptor = -1;
    file->append_descriptor = -1;
    file->stream = NULL;
    file->stream_endpoint = 0;
    file->directory = NULL;
    file->directory_position = 0;
    file->directory_shared = NULL;
    pthread_mutex_unlock(&host->lock);
    int saved_error = close(descriptor) != 0 ? errno : 0;
    if (directory != NULL && closedir(directory) != 0 && saved_error == 0) saved_error = errno;
    if (append_descriptor >= 0 && close(append_descriptor) != 0 && saved_error == 0) saved_error = errno;
    hl_macos_stream_release(stream);
    hl_macos_directory_shared_release(directory_shared);
    if (saved_error != 0) {
        errno = saved_error;
        return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_shared_create(void *context, uint64_t size, uint32_t flags) {
    hl_host_macos *host = context;
    char path[] = "/tmp/hl-engine-shared-XXXXXX";
    int descriptor;
    hl_host_result result;
    if (size > INT64_MAX || flags != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = mkstemp(path);
    if (descriptor < 0) return hl_macos_errno();
    if (unlink(path) != 0 || fcntl(descriptor, F_SETFD, FD_CLOEXEC) != 0 || ftruncate(descriptor, (off_t)size) != 0) {
        hl_host_result error = hl_macos_errno();
        close(descriptor);
        return error;
    }
    result = hl_macos_file_register(host, descriptor, -1, 1);
    if (result.status != HL_STATUS_OK)
        close(descriptor);
    else
        result.detail = result.value;
    return result;
}

static hl_host_result hl_macos_shared_open(void *context, uint64_t identity, uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_file *source;
    int descriptor;
    int valid;
    hl_host_result result;
    if (flags != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    source = hl_macos_file_lookup(host, identity);
    valid = source != NULL && source->shared;
    descriptor = valid ? fcntl(source->descriptor, F_DUPFD_CLOEXEC, 0) : -1;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return valid ? hl_macos_errno() : hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_macos_file_register(host, descriptor, -1, 1);
    if (result.status != HL_STATUS_OK)
        close(descriptor);
    else
        result.detail = identity;
    return result;
}

static hl_host_result hl_macos_shared_resize(void *context, hl_host_handle object, uint64_t size) {
    hl_host_macos *host = context;
    hl_macos_file *entry;
    int result;
    if (size > INT64_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_file_lookup(host, object);
    if (entry == NULL || !entry->shared) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = ftruncate(entry->descriptor, (off_t)size);
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_macos_event *hl_macos_event_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_EVENT, host->event_capacity, &index) ||
        !host->events[index].active || host->events[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->events[index];
}

static hl_macos_counter *hl_macos_counter_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_COUNTER, host->counter_capacity, &index) ||
        !host->counters[index].active || host->counters[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->counters[index];
}

static hl_macos_directory *hl_macos_directory_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_DIRECTORY, host->directory_capacity, &index) ||
        !host->directories[index].active || host->directories[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->directories[index];
}

static hl_host_result hl_macos_directory_register(hl_host_macos *host, hl_macos_directory_object *object) {
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    pthread_mutex_lock(&host->lock);
    for (uint32_t index = 0; index < host->directory_capacity; ++index) {
        hl_macos_directory *directory = &host->directories[index];
        if (directory->active) continue;
        directory->generation++;
        if (directory->generation == 0) directory->generation = 1;
        directory->active = 1;
        directory->object = object;
        object->references++;
        handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, directory->generation);
        break;
    }
    if (handle == HL_HOST_HANDLE_INVALID) {
        uint32_t capacity =
            hl_macos_grow_capacity(host->directory_capacity, HL_MACOS_DIRECTORY_CAPACITY, sizeof(*host->directories));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_directory *grown = realloc(host->directories, (size_t)capacity * sizeof(*grown));
        if (grown != NULL) {
            memset(grown + host->directory_capacity, 0, (size_t)(capacity - host->directory_capacity) * sizeof(*grown));
            uint32_t index = host->directory_capacity;
            host->directories = grown;
            host->directory_capacity = capacity;
            grown[index].generation = 1;
            grown[index].active = 1;
            grown[index].object = object;
            object->references++;
            handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, 1);
        }
    }
    pthread_mutex_unlock(&host->lock);
    return handle == HL_HOST_HANDLE_INVALID ? hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0)
                                            : hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_directory_create(void *context) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = calloc(1, sizeof(*object));
    if (object == NULL) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    object->descriptor = kqueue();
    if (object->descriptor < 0) {
        free(object);
        return hl_macos_errno();
    }
    (void)fcntl(object->descriptor, F_SETFD, FD_CLOEXEC);
    int adopted = hl_host_process_fd_private_adopt(object->descriptor);
    if (adopted < 0) {
        close(object->descriptor);
        free(object);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    object->descriptor = adopted;
    hl_host_result result = hl_macos_directory_register(host, object);
    if (result.status != HL_STATUS_OK) {
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        free(object);
    }
    return result;
}

static hl_macos_directory_object *hl_macos_directory_object_get(hl_host_macos *host, hl_host_handle handle) {
    pthread_mutex_lock(&host->lock);
    hl_macos_directory *directory = hl_macos_directory_lookup(host, handle);
    hl_macos_directory_object *object = directory == NULL ? NULL : directory->object;
    pthread_mutex_unlock(&host->lock);
    return object;
}

static uint32_t hl_macos_directory_native(uint32_t interests) {
    uint32_t flags = 0;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) flags |= NOTE_ATTRIB;
    if ((interests & HL_HOST_DIRECTORY_MODIFY) != 0) flags |= NOTE_WRITE | NOTE_EXTEND;
    if ((interests & (HL_HOST_DIRECTORY_CREATE | HL_HOST_DIRECTORY_DELETE)) != 0) flags |= NOTE_WRITE | NOTE_LINK;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) flags |= NOTE_RENAME;
    if ((interests & HL_HOST_DIRECTORY_ATTRIB) != 0) flags |= NOTE_ATTRIB;
    return flags == 0 ? NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND | NOTE_LINK : flags;
}

static void hl_macos_directory_descriptor_close(int descriptor) {
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
}

static hl_host_result hl_macos_directory_add(void *context, hl_host_handle instance, hl_host_handle file,
                                             uint64_t token, uint32_t interests) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = hl_macos_directory_object_get(host, instance);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file_entry = hl_macos_file_lookup(host, file);
    int descriptor = file_entry == NULL ? -1 : fcntl(file_entry->descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) {
            close(descriptor);
            descriptor = -1;
        } else {
            descriptor = adopted;
        }
    }
    if (object == NULL || descriptor < 0 || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (watch->active && watch->token != token) continue;
        if (!watch->active || watch->token == token) {
            struct kevent change;
            uint16_t flags =
                (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
            EV_SET(&change, descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0,
                   (void *)(uintptr_t)token);
            if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) {
                hl_macos_directory_descriptor_close(descriptor);
                return hl_macos_errno();
            }
            if (watch->active) hl_macos_directory_descriptor_close(watch->descriptor);
            *watch = (hl_macos_directory_watch){token, interests, descriptor, 1};
            return hl_macos_result(HL_STATUS_OK, 0, 0);
        }
    }
    uint32_t capacity = object->watch_capacity == 0 ? HL_MACOS_DIRECTORY_WATCH_CAPACITY : object->watch_capacity * 2u;
    hl_macos_directory_watch *grown = realloc(object->watches, (size_t)capacity * sizeof(*grown));
    if (grown == NULL) {
        hl_macos_directory_descriptor_close(descriptor);
        return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    memset(grown + object->watch_capacity, 0, (size_t)(capacity - object->watch_capacity) * sizeof(*grown));
    uint32_t index = object->watch_capacity;
    object->watches = grown;
    object->watch_capacity = capacity;
    struct kevent change;
    uint16_t flags = (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
    EV_SET(&change, descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0, (void *)(uintptr_t)token);
    if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) {
        hl_macos_directory_descriptor_close(descriptor);
        return hl_macos_errno();
    }
    object->watches[index] = (hl_macos_directory_watch){token, interests, descriptor, 1};
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_directory_modify(void *context, hl_host_handle instance, uint64_t token,
                                                uint32_t interests) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    if (object == NULL || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (!watch->active || watch->token != token) continue;
        struct kevent change;
        uint16_t flags =
            (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
        EV_SET(&change, watch->descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0,
               (void *)(uintptr_t)token);
        if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) return hl_macos_errno();
        watch->interests = interests;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
}

static hl_host_result hl_macos_directory_remove(void *context, hl_host_handle instance, uint64_t token) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    if (object == NULL || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (!watch->active || watch->token != token) continue;
        struct kevent change;
        EV_SET(&change, watch->descriptor, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0 && errno != ENOENT) return hl_macos_errno();
        watch->active = 0;
        hl_macos_directory_descriptor_close(watch->descriptor);
        watch->descriptor = -1;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
}

static uint32_t hl_macos_directory_changes(uint32_t flags, uint32_t interests) {
    uint32_t changes = 0;
    if ((flags & (NOTE_WRITE | NOTE_EXTEND | NOTE_LINK)) != 0)
        changes |= interests & (HL_HOST_DIRECTORY_MODIFY | HL_HOST_DIRECTORY_CREATE | HL_HOST_DIRECTORY_DELETE);
    if ((flags & NOTE_ATTRIB) != 0) changes |= HL_HOST_DIRECTORY_ATTRIB;
    if ((flags & NOTE_DELETE) != 0) changes |= HL_HOST_DIRECTORY_DELETE;
    if ((flags & NOTE_RENAME) != 0) changes |= HL_HOST_DIRECTORY_RENAME;
    return changes;
}

static hl_host_result hl_macos_directory_read(void *context, hl_host_handle instance, hl_host_directory_record *records,
                                              uint32_t capacity) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    struct kevent native[64];
    struct timespec zero = {0, 0};
    if (object == NULL || records == NULL || capacity == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (capacity > 64) capacity = 64;
    int count = kevent(object->descriptor, NULL, 0, native, (int)capacity, &zero);
    if (count < 0) return hl_macos_errno();
    if (count == 0) return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    for (int index = 0; index < count; ++index) {
        uint64_t token = (uint64_t)(uintptr_t)native[index].udata;
        uint32_t interests = 0;
        for (uint32_t watch_index = 0; watch_index < object->watch_capacity; ++watch_index) {
            hl_macos_directory_watch *watch = &object->watches[watch_index];
            if (watch->active && watch->token == token) interests = watch->interests;
            if (watch->active && watch->token == token && (watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0) {
                watch->active = 0;
                hl_macos_directory_descriptor_close(watch->descriptor);
                watch->descriptor = -1;
                interests |= HL_HOST_DIRECTORY_IGNORED;
            }
        }
        records[index] =
            (hl_host_directory_record){token, hl_macos_directory_changes(native[index].fflags, interests), 0};
        if ((interests & HL_HOST_DIRECTORY_IGNORED) != 0) records[index].changes |= HL_HOST_DIRECTORY_IGNORED;
    }
    return hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_directory_duplicate(void *context, hl_host_handle instance) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = hl_macos_directory_object_get(host, instance);
    if (object == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_directory_register(host, object);
}

static hl_host_result hl_macos_directory_close(void *context, hl_host_handle instance) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object;
    int final = 0;
    pthread_mutex_lock(&host->lock);
    hl_macos_directory *directory = hl_macos_directory_lookup(host, instance);
    if (directory == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = directory->object;
    directory->active = 0;
    directory->object = NULL;
    final = --object->references == 0;
    pthread_mutex_unlock(&host->lock);
    if (final) {
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        for (uint32_t index = 0; index < object->watch_capacity; ++index)
            if (object->watches[index].active) hl_macos_directory_descriptor_close(object->watches[index].descriptor);
        free(object->watches);
        free(object);
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_macos_transfer *hl_macos_transfer_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_TRANSFER, host->transfer_capacity, &index) ||
        !host->transfers[index].active || host->transfers[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->transfers[index];
}

static hl_host_result hl_macos_transfer_register(hl_host_macos *host, int descriptor) {
    uint32_t index;
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    descriptor = adopted;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->transfer_capacity; ++index) {
        hl_macos_transfer *transfer = &host->transfers[index];
        if (transfer->active) continue;
        transfer->generation++;
        if (transfer->generation == 0) transfer->generation = 1;
        transfer->active = 1;
        transfer->descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, transfer->generation), 0);
    }
    uint32_t capacity =
        hl_macos_grow_capacity(host->transfer_capacity, HL_MACOS_TRANSFER_CAPACITY, sizeof(*host->transfers));
    if (capacity == 0) {
        pthread_mutex_unlock(&host->lock);
        hl_host_process_fd_private_remove(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    hl_macos_transfer *grown = realloc(host->transfers, (size_t)capacity * sizeof(*grown));
    if (grown != NULL) {
        memset(grown + host->transfer_capacity, 0, (size_t)(capacity - host->transfer_capacity) * sizeof(*grown));
        index = host->transfer_capacity;
        host->transfers = grown;
        host->transfer_capacity = capacity;
        grown[index].generation = 1;
        grown[index].active = 1;
        grown[index].descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, 1), 0);
    }
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

typedef struct hl_macos_transfer_wire {
    uint32_t data_size;
    uint32_t attachment_count;
    uint32_t flags[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t rights[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint8_t data[HL_HOST_TRANSFER_MAX_DATA];
} hl_macos_transfer_wire;

static hl_host_result hl_macos_counter_register(hl_host_macos *host, hl_macos_counter_object *object, uint32_t rights);
static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle);

static hl_host_result hl_macos_transfer_channel_pair(void *context) {
    hl_host_macos *host = context;
    int pair[2];
    hl_host_result first;
    hl_host_result second;
    /* Darwin does not provide AF_UNIX SOCK_SEQPACKET; datagrams retain message boundaries. */
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) return hl_macos_errno();
    (void)fcntl(pair[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(pair[1], F_SETFD, FD_CLOEXEC);
    first = hl_macos_transfer_register(host, pair[0]);
    if (first.status != HL_STATUS_OK) {
        close(pair[0]);
        close(pair[1]);
        return first;
    }
    second = hl_macos_transfer_register(host, pair[1]);
    if (second.status != HL_STATUS_OK) {
        close(pair[1]);
        (void)hl_macos_transfer_close(host, first.value);
        return second;
    }
    return hl_macos_result(HL_STATUS_OK, first.value, second.value);
}

static hl_host_result hl_macos_transfer_send(void *context, hl_host_handle channel, hl_host_const_bytes data,
                                             const hl_host_transfer_attachment *attachments, uint32_t count) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire = {0};
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int descriptors[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    if (data.size > HL_HOST_TRANSFER_MAX_DATA || (data.size != 0 && data.data == NULL) ||
        count > HL_HOST_TRANSFER_MAX_ATTACHMENTS || (count != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    for (index = 0; index < count && channel_fd >= 0; ++index) {
        hl_macos_counter *counter = hl_macos_counter_lookup(host, attachments[index].object);
        uint32_t valid =
            HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
        if (counter == NULL || attachments[index].kind != HL_HOST_TRANSFER_KIND_COUNTER ||
            (attachments[index].rights & ~valid) != 0 ||
            (attachments[index].rights & counter->rights) != attachments[index].rights) {
            channel_fd = -1;
            break;
        }
        descriptors[index * 3u] = counter->object->backing;
        descriptors[index * 3u + 1u] = counter->object->readable;
        descriptors[index * 3u + 2u] = counter->object->signal;
        wire.flags[index] = counter->object->shared->flags;
        wire.rights[index] = attachments[index].rights;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    wire.data_size = (uint32_t)data.size;
    wire.attachment_count = count;
    if (data.size != 0) memcpy(wire.data, data.data, data.size);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (count != 0) {
        struct cmsghdr *header;
        size_t descriptor_bytes = sizeof(int) * count * 3u;
        message.msg_control = control;
        message.msg_controllen = (socklen_t)CMSG_SPACE(descriptor_bytes);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = (socklen_t)CMSG_LEN(descriptor_bytes);
        memcpy(CMSG_DATA(header), descriptors, descriptor_bytes);
    }
    return sendmsg(channel_fd, &message, 0) == (ssize_t)sizeof(wire) ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                     : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_receive(void *context, hl_host_handle channel, hl_host_bytes data,
                                                hl_host_transfer_attachment *attachments, uint32_t capacity) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire;
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int received[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    ssize_t bytes;
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0 || (data.size != 0 && data.data == NULL) || (capacity != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    bytes = recv(channel_fd, &wire, sizeof(wire), MSG_PEEK);
    if (bytes < 0) return hl_macos_errno();
    if (bytes != (ssize_t)sizeof(wire) || wire.data_size > HL_HOST_TRANSFER_MAX_DATA ||
        wire.attachment_count > HL_HOST_TRANSFER_MAX_ATTACHMENTS)
        return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.data_size > data.size || wire.attachment_count > capacity)
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, wire.data_size, wire.attachment_count);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    bytes = recvmsg(channel_fd, &message, 0);
    if (bytes != (ssize_t)sizeof(wire)) return bytes < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.attachment_count != 0) {
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        size_t descriptor_bytes = sizeof(int) * wire.attachment_count * 3u;
        if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
            header->cmsg_len != CMSG_LEN(descriptor_bytes))
            return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
        memcpy(received, CMSG_DATA(header), descriptor_bytes);
    }
    for (index = 0; index < wire.attachment_count; ++index) {
        hl_macos_counter_object *object = calloc(1, sizeof(*object));
        hl_host_result installed;
        if (object == NULL) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        object->backing = received[index * 3u];
        object->readable = received[index * 3u + 1u];
        object->signal = received[index * 3u + 2u];
        object->shared = mmap(NULL, sizeof(*object->shared), PROT_READ | PROT_WRITE, MAP_SHARED, object->backing, 0);
        if (object->shared == MAP_FAILED) {
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return hl_macos_errno();
        }
        installed = hl_macos_counter_register(host, object, wire.rights[index]);
        if (installed.status != HL_STATUS_OK) {
            munmap(object->shared, sizeof(*object->shared));
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return installed;
        }
        attachments[index] =
            (hl_host_transfer_attachment){installed.value, HL_HOST_TRANSFER_KIND_COUNTER, wire.rights[index]};
    }
    if (wire.data_size != 0) memcpy(data.data, wire.data, wire.data_size);
    return hl_macos_result(HL_STATUS_OK, wire.data_size, wire.attachment_count);
}

static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    if (transfer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = transfer->descriptor;
    transfer->active = 0;
    transfer->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_duplicate(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    descriptor = transfer == NULL ? -1 : dup(transfer->descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_errno();
    {
        hl_host_result result = hl_macos_transfer_register(host, descriptor);
        if (result.status != HL_STATUS_OK) close(descriptor);
        return result;
    }
}

static hl_host_result hl_macos_counter_register(hl_host_macos *host, hl_macos_counter_object *object, uint32_t rights) {
    uint32_t index;
    int registered = 0;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    pthread_mutex_lock(&host->lock);
    if (object->shared->references == 0) {
        int descriptors[3] = {object->readable, object->signal, object->backing};
        if (hl_macos_private_add_many(descriptors, 3) != 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        object->readable = descriptors[0];
        object->signal = descriptors[1];
        object->backing = descriptors[2];
        registered = 1;
    }
    for (index = 0; index < host->counter_capacity; ++index) {
        hl_macos_counter *counter = &host->counters[index];
        if (counter->active) continue;
        counter->generation++;
        if (counter->generation == 0) counter->generation = 1;
        counter->active = 1;
        counter->object = object;
        counter->rights = rights;
        object->shared->references++;
        handle = hl_macos_handle(HL_MACOS_HANDLE_COUNTER, index, counter->generation);
        break;
    }
    if (handle == HL_HOST_HANDLE_INVALID) {
        uint32_t capacity =
            hl_macos_grow_capacity(host->counter_capacity, HL_MACOS_COUNTER_CAPACITY, sizeof(*host->counters));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            if (registered) {
                hl_host_process_fd_private_remove(object->readable);
                hl_host_process_fd_private_remove(object->signal);
                hl_host_process_fd_private_remove(object->backing);
            }
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_counter *grown = realloc(host->counters, (size_t)capacity * sizeof(*grown));
        if (grown != NULL) {
            memset(grown + host->counter_capacity, 0, (size_t)(capacity - host->counter_capacity) * sizeof(*grown));
            index = host->counter_capacity;
            host->counters = grown;
            host->counter_capacity = capacity;
            grown[index].generation = 1;
            grown[index].active = 1;
            grown[index].object = object;
            grown[index].rights = rights;
            object->shared->references++;
            handle = hl_macos_handle(HL_MACOS_HANDLE_COUNTER, index, 1);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == HL_HOST_HANDLE_INVALID && registered) {
        hl_host_process_fd_private_remove(object->readable);
        hl_host_process_fd_private_remove(object->signal);
        hl_host_process_fd_private_remove(object->backing);
    }
    return handle == HL_HOST_HANDLE_INVALID ? hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0)
                                            : hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_counter_create(void *context, uint64_t initial, uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_counter_object *object;
    int descriptors[2];
    hl_host_result result;
    if (initial == UINT64_MAX || (flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (pipe(descriptors) != 0) return hl_macos_errno();
    (void)fcntl(descriptors[0], F_SETFL, O_NONBLOCK);
    (void)fcntl(descriptors[1], F_SETFL, O_NONBLOCK);
    (void)fcntl(descriptors[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(descriptors[1], F_SETFD, FD_CLOEXEC);
    object = calloc(1, sizeof(*object));
    if (object == NULL) {
        close(descriptors[0]);
        close(descriptors[1]);
        return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    {
        char name[64];
        static uint32_t sequence;
        snprintf(name, sizeof(name), "/hl-counter-%ld-%u", (long)getpid(), ++sequence);
        object->backing = shm_open(name, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (object->backing >= 0) shm_unlink(name);
        if (object->backing < 0 || ftruncate(object->backing, (off_t)sizeof(*object->shared)) != 0) {
            if (object->backing >= 0) close(object->backing);
            close(descriptors[0]);
            close(descriptors[1]);
            free(object);
            return hl_macos_errno();
        }
        object->shared = mmap(NULL, sizeof(*object->shared), PROT_READ | PROT_WRITE, MAP_SHARED, object->backing, 0);
        if (object->shared == MAP_FAILED) {
            close(object->backing);
            close(descriptors[0]);
            close(descriptors[1]);
            free(object);
            return hl_macos_errno();
        }
    }
    {
        pthread_mutexattr_t attributes;
        int initialized = pthread_mutexattr_init(&attributes) == 0;
        if (!initialized || pthread_mutexattr_setpshared(&attributes, PTHREAD_PROCESS_SHARED) != 0 ||
            pthread_mutex_init(&object->shared->lock, &attributes) != 0) {
            if (initialized) pthread_mutexattr_destroy(&attributes);
            close(descriptors[0]);
            close(descriptors[1]);
            munmap(object->shared, sizeof(*object->shared));
            close(object->backing);
            free(object);
            return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
        }
        pthread_mutexattr_destroy(&attributes);
    }
    object->shared->value = initial;
    object->shared->flags = flags;
    object->readable = descriptors[0];
    object->signal = descriptors[1];
    if (initial != 0) {
        const uint8_t byte = 1;
        (void)write(object->signal, &byte, 1);
    }
    result = hl_macos_counter_register(host, object,
                                       HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT |
                                           HL_HOST_TRANSFER_CONTROL);
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_destroy(&object->shared->lock);
        close(object->readable);
        close(object->signal);
        munmap(object->shared, sizeof(*object->shared));
        close(object->backing);
        free(object);
    }
    return result;
}

static hl_macos_counter_object *hl_macos_counter_object_get(hl_host_macos *host, hl_host_handle handle) {
    hl_macos_counter *counter;
    hl_macos_counter_object *object;
    pthread_mutex_lock(&host->lock);
    counter = hl_macos_counter_lookup(host, handle);
    object = counter == NULL ? NULL : counter->object;
    pthread_mutex_unlock(&host->lock);
    return object;
}

static hl_macos_counter_object *hl_macos_counter_object_with_right(void *context, hl_host_handle handle, uint32_t right,
                                                                   hl_status *status) {
    hl_host_macos *host = context;
    hl_macos_counter *counter;
    hl_macos_counter_object *object = NULL;
    pthread_mutex_lock(&host->lock);
    counter = hl_macos_counter_lookup(host, handle);
    if (counter == NULL)
        *status = HL_STATUS_INVALID_ARGUMENT;
    else if ((counter->rights & right) == 0)
        *status = HL_STATUS_PERMISSION_DENIED;
    else {
        object = counter->object;
        *status = HL_STATUS_OK;
    }
    pthread_mutex_unlock(&host->lock);
    return object;
}

static hl_host_result hl_macos_counter_read(void *context, hl_host_handle counter) {
    hl_status status;
    hl_macos_counter_object *object =
        hl_macos_counter_object_with_right(context, counter, HL_HOST_TRANSFER_READ, &status);
    uint64_t value;
    uint8_t bytes[32];
    if (object == NULL) return hl_macos_result(status, 0, 0);
    for (;;) {
        pthread_mutex_lock(&object->shared->lock);
        if (object->shared->value != 0) break;
        if ((object->shared->flags & HL_HOST_COUNTER_NONBLOCK) != 0) {
            pthread_mutex_unlock(&object->shared->lock);
            return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        pthread_mutex_unlock(&object->shared->lock);
        poll(&(struct pollfd){object->readable, POLLIN, 0}, 1, -1);
    }
    value = (object->shared->flags & HL_HOST_COUNTER_SEMAPHORE) != 0 ? 1 : object->shared->value;
    object->shared->value -= value;
    if (object->shared->value == 0)
        while (read(object->readable, bytes, sizeof(bytes)) > 0) {}
    pthread_mutex_unlock(&object->shared->lock);
    return hl_macos_result(HL_STATUS_OK, value, 0);
}

static hl_host_result hl_macos_counter_write(void *context, hl_host_handle counter, uint64_t value) {
    hl_status status;
    hl_macos_counter_object *object =
        hl_macos_counter_object_with_right(context, counter, HL_HOST_TRANSFER_WRITE, &status);
    uint8_t byte = 1;
    if (object == NULL) return hl_macos_result(status, 0, 0);
    if (value == UINT64_MAX || value == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&object->shared->lock);
    if (value > UINT64_MAX - 1u - object->shared->value) {
        pthread_mutex_unlock(&object->shared->lock);
        return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    }
    if (object->shared->value == 0) (void)write(object->signal, &byte, 1);
    object->shared->value += value;
    pthread_mutex_unlock(&object->shared->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_counter_get_flags(void *context, hl_host_handle counter) {
    hl_status status;
    hl_macos_counter_object *object =
        hl_macos_counter_object_with_right(context, counter, HL_HOST_TRANSFER_CONTROL, &status);
    uint32_t flags;
    if (object == NULL) return hl_macos_result(status, 0, 0);
    pthread_mutex_lock(&object->shared->lock);
    flags = object->shared->flags;
    pthread_mutex_unlock(&object->shared->lock);
    return hl_macos_result(HL_STATUS_OK, flags, 0);
}

static hl_host_result hl_macos_counter_set_flags(void *context, hl_host_handle counter, uint32_t flags) {
    hl_status status;
    hl_macos_counter_object *object =
        hl_macos_counter_object_with_right(context, counter, HL_HOST_TRANSFER_CONTROL, &status);
    if (object == NULL) return hl_macos_result(status, 0, 0);
    if ((flags & ~(uint32_t)(HL_HOST_COUNTER_SEMAPHORE | HL_HOST_COUNTER_NONBLOCK)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&object->shared->lock);
    if ((object->shared->flags & HL_HOST_COUNTER_SEMAPHORE) != (flags & HL_HOST_COUNTER_SEMAPHORE)) {
        pthread_mutex_unlock(&object->shared->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object->shared->flags = flags;
    pthread_mutex_unlock(&object->shared->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_counter_duplicate(void *context, hl_host_handle counter) {
    hl_host_macos *host = context;
    hl_macos_counter_object *object = hl_macos_counter_object_get(host, counter);
    if (object == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    {
        uint32_t rights;
        pthread_mutex_lock(&host->lock);
        rights = hl_macos_counter_lookup(host, counter)->rights;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_counter_register(host, object, rights);
    }
}

static hl_host_result hl_macos_counter_readiness(void *context, hl_host_handle counter, uint32_t interests) {
    hl_host_macos *host = context;
    hl_macos_counter *entry;
    struct pollfd descriptor;
    int result;
    if ((interests & ~(uint32_t)HL_HOST_READY_READ) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_counter_lookup(host, counter);
    if (entry != NULL && (entry->rights & HL_HOST_TRANSFER_WAIT) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = (struct pollfd){entry == NULL ? -1 : entry->object->readable, POLLIN, 0};
    pthread_mutex_unlock(&host->lock);
    if (descriptor.fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = poll(&descriptor, 1, 0);
    return result < 0 ? hl_macos_errno()
                      : hl_macos_result(HL_STATUS_OK, result == 1 ? HL_HOST_READY_READ & interests : 0, 0);
}

static void *hl_macos_counter_subscription_main(void *opaque) {
    hl_macos_counter_subscription *subscription = opaque;
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

static hl_host_result hl_macos_counter_subscribe(void *context, hl_host_handle counter,
                                                 void (*notify)(void *, uint64_t), void *observer, uint64_t token) {
    hl_host_macos *host = context;
    hl_macos_counter *entry;
    hl_macos_counter_subscription *subscription = NULL;
    uint32_t index;
    int descriptor;
    if (notify == NULL || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_counter_lookup(host, counter);
    if (entry != NULL && (entry->rights & HL_HOST_TRANSFER_WAIT) == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
    }
    descriptor = entry == NULL ? -1 : dup(entry->object->readable);
    for (index = 0; descriptor >= 0 && index < host->counter_subscription_capacity; ++index)
        if (host->counter_subscriptions[index] == NULL ||
            (!host->counter_subscriptions[index]->active && !host->counter_subscriptions[index]->retiring)) {
            subscription = host->counter_subscriptions[index];
            break;
        }
    if (descriptor >= 0 && index == host->counter_subscription_capacity) {
        uint32_t capacity = host->counter_subscription_capacity ? host->counter_subscription_capacity * 2u
                                                                : HL_MACOS_COUNTER_SUBSCRIPTIONS_INITIAL;
        void *grown = realloc(host->counter_subscriptions, (size_t)capacity * sizeof(*host->counter_subscriptions));
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
        subscription->counter = counter;
        subscription->descriptor = descriptor;
        subscription->notify = notify;
        subscription->observer = observer;
        subscription->token = token;
        subscription->wake[0] = -1;
        subscription->wake[1] = -1;
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (subscription == NULL) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    if (pipe(subscription->wake) != 0) {
        if (subscription->wake[0] >= 0) close(subscription->wake[0]);
        if (subscription->wake[1] >= 0) close(subscription->wake[1]);
        close(descriptor);
        pthread_mutex_lock(&host->lock);
        subscription->active = 0;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    {
        int descriptors[3] = {subscription->descriptor, subscription->wake[0], subscription->wake[1]};
        if (hl_macos_private_add_many(descriptors, 3) != 0) {
            close(subscription->wake[0]);
            close(subscription->wake[1]);
            close(descriptor);
            pthread_mutex_lock(&host->lock);
            subscription->active = 0;
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        subscription->descriptor = descriptors[0];
        subscription->wake[0] = descriptors[1];
        subscription->wake[1] = descriptors[2];
        descriptor = descriptors[0];
    }
    if (pthread_create(&subscription->thread, NULL, hl_macos_counter_subscription_main, subscription) != 0) {
        hl_host_process_fd_private_remove(subscription->descriptor);
        hl_host_process_fd_private_remove(subscription->wake[0]);
        hl_host_process_fd_private_remove(subscription->wake[1]);
        close(subscription->wake[0]);
        close(subscription->wake[1]);
        close(descriptor);
        pthread_mutex_lock(&host->lock);
        subscription->active = 0;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_SUBSCRIPTION, index, subscription->generation),
                           0);
}

static hl_host_result hl_macos_counter_unsubscribe(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    uint32_t index;
    hl_macos_counter_subscription *subscription;
    uint8_t byte = 1;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_SUBSCRIPTION, host->counter_subscription_capacity, &index))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    subscription = host->counter_subscriptions[index];
    if (subscription == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (!subscription->active || subscription->generation != (uint32_t)(handle >> 32)) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    subscription->active = 0;
    subscription->retiring = 1;
    pthread_mutex_unlock(&host->lock);
    (void)write(subscription->wake[1], &byte, 1);
    (void)pthread_join(subscription->thread, NULL);
    hl_host_process_fd_private_remove(subscription->wake[0]);
    hl_host_process_fd_private_remove(subscription->wake[1]);
    hl_host_process_fd_private_remove(subscription->descriptor);
    close(subscription->wake[0]);
    close(subscription->wake[1]);
    close(subscription->descriptor);
    pthread_mutex_lock(&host->lock);
    subscription->counter = HL_HOST_HANDLE_INVALID;
    subscription->notify = NULL;
    subscription->observer = NULL;
    subscription->retiring = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static void hl_macos_counter_unsubscribe_all(hl_host_macos *host, hl_host_handle counter) {
    uint32_t index;
    for (index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_host_handle subscription = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[index] != NULL && host->counter_subscriptions[index]->active &&
            host->counter_subscriptions[index]->counter == counter)
            subscription =
                hl_macos_handle(HL_MACOS_HANDLE_SUBSCRIPTION, index, host->counter_subscriptions[index]->generation);
        pthread_mutex_unlock(&host->lock);
        if (subscription != HL_HOST_HANDLE_INVALID) (void)hl_macos_counter_unsubscribe(host, subscription);
    }
}

static hl_host_result hl_macos_counter_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_counter *counter;
    hl_macos_counter_object *object;
    int final;
    hl_macos_counter_unsubscribe_all(host, handle);
    pthread_mutex_lock(&host->lock);
    counter = hl_macos_counter_lookup(host, handle);
    if (counter == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = counter->object;
    counter->active = 0;
    counter->object = NULL;
    counter->rights = 0;
    final = --object->shared->references == 0;
    pthread_mutex_unlock(&host->lock);
    if (final) {
        hl_host_process_fd_private_remove(object->readable);
        hl_host_process_fd_private_remove(object->signal);
        hl_host_process_fd_private_remove(object->backing);
        close(object->readable);
        close(object->signal);
        /* A descriptor already queued through SCM_RIGHTS may still map this object. */
        munmap(object->shared, sizeof(*object->shared));
        close(object->backing);
        free(object);
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_handle hl_macos_watch_handle(uint32_t index, uint32_t generation) {
    return hl_macos_handle(HL_MACOS_HANDLE_WATCH, index, generation);
}

static hl_macos_watch *hl_macos_watch_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_WATCH, host->watch_capacity, &index) ||
        !host->watches[index].active || host->watches[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->watches[index];
}

static int hl_macos_watch_refresh(hl_macos_watch *watch) {
    struct stat status;
    hl_host_watch_record next = watch->record;
    uint32_t changes = 0;
    if (fstat(watch->descriptor, &status) != 0) {
        if (errno != ENOENT) return -1;
        changes = HL_HOST_WATCH_DELETED;
    } else {
        uint64_t device = (uint64_t)status.st_dev, object = (uint64_t)status.st_ino;
        uint64_t size = status.st_size < 0 ? 0 : (uint64_t)status.st_size;
        uint64_t modified =
            (uint64_t)status.st_mtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtimespec.tv_nsec;
        uint64_t changed =
            (uint64_t)status.st_ctimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctimespec.tv_nsec;
        if (device != next.stable_device || object != next.stable_object) changes |= HL_HOST_WATCH_IDENTITY;
        if (size != next.size) changes |= HL_HOST_WATCH_SIZE;
        if (modified != watch->modified_ns) changes |= HL_HOST_WATCH_DATA;
        if (changed != watch->changed_ns) changes |= HL_HOST_WATCH_DATA;
        if (status.st_nlink == 0 && watch->links != 0) changes |= HL_HOST_WATCH_DELETED;
        next.stable_device = device;
        next.stable_object = object;
        next.size = size;
        watch->modified_ns = modified;
        watch->changed_ns = changed;
        watch->links = status.st_nlink;
    }
    if (changes != 0) {
        if (watch->record.generation != watch->delivered_generation) changes |= next.changes;
        next.generation++;
        if (next.generation == 0) next.generation = 1;
        next.changes = changes;
        watch->record = next;
    }
    return 0;
}

static hl_host_result hl_macos_watch_open(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    hl_macos_file *entry;
    int descriptor = -1;
    uint32_t index;
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) descriptor = fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return entry == NULL ? hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_macos_errno();
    struct stat status;
    if (fstat(descriptor, &status) != 0) {
        hl_host_result error = hl_macos_errno();
        close(descriptor);
        return error;
    }
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->watch_capacity && host->watches[index].active; ++index) {}
    if (index == host->watch_capacity) {
        uint32_t capacity =
            host->watch_capacity > UINT32_C(0x7fffffff) / 2u ? UINT32_C(0x7fffffff) : host->watch_capacity * 2u;
        hl_macos_watch *grown =
            capacity > host->watch_capacity ? realloc(host->watches, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown == NULL) {
            pthread_mutex_unlock(&host->lock);
            hl_host_process_fd_private_remove(descriptor);
            close(descriptor);
            return hl_macos_result(capacity > host->watch_capacity ? HL_STATUS_OUT_OF_MEMORY : HL_STATUS_RESOURCE_LIMIT,
                                   0, 0);
        }
        memset(grown + host->watch_capacity, 0, (size_t)(capacity - host->watch_capacity) * sizeof(*grown));
        host->watches = grown;
        host->watch_capacity = capacity;
    }
    hl_macos_watch *watch = &host->watches[index];
    watch->generation++;
    if (watch->generation == 0) watch->generation = 1;
    watch->active = 1;
    watch->descriptor = descriptor;
    watch->record = (hl_host_watch_record){
        1, (uint64_t)status.st_dev, (uint64_t)status.st_ino, status.st_size < 0 ? 0 : (uint64_t)status.st_size, 0, 0};
    watch->modified_ns =
        (uint64_t)status.st_mtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtimespec.tv_nsec;
    watch->changed_ns =
        (uint64_t)status.st_ctimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctimespec.tv_nsec;
    watch->links = status.st_nlink;
    watch->delivered_generation = 1;
    hl_host_handle handle = hl_macos_watch_handle(index, watch->generation);
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_watch_query(void *context, hl_host_handle handle, hl_host_watch_record *record) {
    hl_host_macos *host = context;
    int result;
    if (record == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_watch *watch = hl_macos_watch_lookup(host, handle);
    result = watch == NULL ? -2 : hl_macos_watch_refresh(watch);
    if (result == 0) *record = watch->record;
    pthread_mutex_unlock(&host->lock);
    if (result == -2) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_watch_drain(void *context, hl_host_handle handle, hl_host_watch_record *records,
                                           size_t capacity) {
    hl_host_macos *host = context;
    int result;
    if (records == NULL || capacity == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_watch *watch = hl_macos_watch_lookup(host, handle);
    result = watch == NULL ? -2 : hl_macos_watch_refresh(watch);
    if (result == 0 && watch->record.generation != watch->delivered_generation) {
        records[0] = watch->record;
        watch->delivered_generation = watch->record.generation;
        result = 1;
    }
    pthread_mutex_unlock(&host->lock);
    if (result == -2) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (result < 0) return hl_macos_errno();
    return result == 0 ? hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0) : hl_macos_result(HL_STATUS_OK, 1, 0);
}

static hl_host_result hl_macos_watch_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    hl_macos_watch *watch = hl_macos_watch_lookup(host, handle);
    if (watch == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = watch->descriptor;
    watch->active = 0;
    watch->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static void hl_macos_watch_note(hl_host_macos *host, uintptr_t descriptor, uint32_t native_changes) {
    for (uint32_t index = 0; index < host->watch_capacity; ++index) {
        hl_macos_watch *watch = &host->watches[index];
        if (!watch->active || (uintptr_t)watch->descriptor != descriptor) continue;
        uint32_t changes = 0;
        if ((native_changes & (NOTE_WRITE | NOTE_EXTEND)) != 0) changes |= HL_HOST_WATCH_DATA;
        if ((native_changes & NOTE_EXTEND) != 0) changes |= HL_HOST_WATCH_SIZE;
        if ((native_changes & NOTE_DELETE) != 0) changes |= HL_HOST_WATCH_DELETED;
        if ((native_changes & NOTE_RENAME) != 0) changes |= HL_HOST_WATCH_IDENTITY;
        if (changes != 0) {
            watch->record.generation++;
            if (watch->record.generation == 0) watch->record.generation = 1;
            watch->record.changes = changes;
        }
        break;
    }
}

static hl_host_result hl_macos_event_create(void *context) {
    hl_host_macos *host = context;
    struct kevent wake;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status exhausted = HL_STATUS_RESOURCE_LIMIT;
    uint32_t index;
    int descriptor = kqueue();
    if (descriptor < 0) return hl_macos_errno();
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    EV_SET(&wake, 0, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
    if (kevent(descriptor, &wake, 1, NULL, 0, NULL) != 0) {
        hl_host_result error = hl_macos_errno();
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return error;
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->event_capacity; ++index) {
        hl_macos_event *event = &host->events[index];
        if (event->active) continue;
        event->generation++;
        if (event->generation == 0) event->generation = 1;
        event->active = 1;
        event->descriptor = descriptor;
        event->timers = calloc(HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
        if (event->timers == NULL) continue;
        event->timer_capacity = HL_MACOS_TIMER_CAPACITY;
        handle = hl_macos_handle(HL_MACOS_HANDLE_EVENT, index, event->generation);
        break;
    }
    if (handle == HL_HOST_HANDLE_INVALID) {
        uint32_t capacity = host->event_capacity > UINT32_MAX / 2u ? UINT32_MAX : host->event_capacity * 2u;
        hl_macos_event *grown =
            capacity > host->event_capacity ? realloc(host->events, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            memset(grown + host->event_capacity, 0, (size_t)(capacity - host->event_capacity) * sizeof(*grown));
            index = host->event_capacity;
            host->events = grown;
            host->event_capacity = capacity;
            hl_macos_event *event = &host->events[index];
            event->generation = 1;
            event->active = 1;
            event->descriptor = descriptor;
            event->timers = calloc(HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
            if (event->timers == NULL) {
                event->active = 0;
                exhausted = HL_STATUS_OUT_OF_MEMORY;
            } else {
                event->timer_capacity = HL_MACOS_TIMER_CAPACITY;
                handle = hl_macos_handle(HL_MACOS_HANDLE_EVENT, index, event->generation);
            }
        } else if (capacity > host->event_capacity)
            exhausted = HL_STATUS_OUT_OF_MEMORY;
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == HL_HOST_HANDLE_INVALID) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return hl_macos_result(exhausted, 0, 0);
    }
    return hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_macos_timer *hl_macos_event_timer(hl_macos_event *event, uint64_t token) {
    uint32_t index;
    for (index = 0; index < event->timer_capacity; ++index)
        if (event->timers[index].active && event->timers[index].token == token) return &event->timers[index];
    return NULL;
}

static hl_host_result hl_macos_event_control(void *context, hl_host_handle pollset, uint32_t operation,
                                             hl_host_handle object_handle, uint64_t token, uint32_t interests) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_counter *counter;
    hl_macos_directory *directory;
    hl_macos_transfer *transfer;
    hl_macos_watch *watch;
    hl_macos_file *stream;
    struct kevent changes[2];
    int count = 0;
    int descriptor;
    uint16_t flags;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    counter = hl_macos_counter_lookup(host, object_handle);
    directory = hl_macos_directory_lookup(host, object_handle);
    transfer = hl_macos_transfer_lookup(host, object_handle);
    watch = hl_macos_watch_lookup(host, object_handle);
    stream = hl_macos_file_lookup(host, object_handle);
    if (stream != NULL && stream->stream == NULL) stream = NULL;
    descriptor =
        event == NULL || (counter == NULL && directory == NULL && transfer == NULL && watch == NULL && stream == NULL)
            ? -1
            : event->descriptor;
    if (descriptor >= 0 && counter != NULL) object_handle = (hl_host_handle)counter->object->readable;
    if (descriptor >= 0 && directory != NULL) object_handle = (hl_host_handle)directory->object->descriptor;
    if (descriptor >= 0 && transfer != NULL) object_handle = (hl_host_handle)transfer->descriptor;
    if (descriptor >= 0 && watch != NULL) object_handle = (hl_host_handle)watch->descriptor;
    if (descriptor >= 0 && stream != NULL) object_handle = (hl_host_handle)stream->descriptor;
    if (descriptor < 0 || token == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (operation == HL_HOST_EVENT_DELETE)
        flags = EV_DELETE;
    else if (operation == HL_HOST_EVENT_ADD || operation == HL_HOST_EVENT_MODIFY)
        flags = (uint16_t)(EV_ADD | EV_ENABLE | ((interests & HL_HOST_READY_EDGE) ? EV_CLEAR : 0) |
                           ((interests & HL_HOST_READY_ONESHOT) ? EV_ONESHOT : 0));
    else {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((interests & HL_HOST_READY_READ) != 0) {
        if (watch != NULL)
            EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_VNODE, flags,
                   NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_DELETE | NOTE_RENAME, 0, (void *)(uintptr_t)token);
        else
            EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_READ, flags, 0, 0, (void *)(uintptr_t)token);
    }
    if (stream != NULL && (interests & HL_HOST_READY_WRITE) != 0)
        EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_WRITE, flags, 0, 0, (void *)(uintptr_t)token);
    if (count == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int result = kevent(descriptor, changes, count, NULL, 0, NULL);
    hl_host_result output = result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
    pthread_mutex_unlock(&host->lock);
    return output;
}

static int hl_macos_event_submit_timer(int descriptor, uint64_t token, uint64_t delay_ns) {
    struct kevent change;
    if (delay_ns == 0) delay_ns = 1;
    if (delay_ns > INT64_MAX) delay_ns = INT64_MAX;
    // NOTE_CRITICAL: opt out of macOS power-aware timer coalescing. These kqueue timers back guest
    // timerfd/POSIX-timer/itimer expiries, which on Linux are hrtimers with no coalescing slop; the default
    // leeway lets the kernel slide an expiry by tens of ms (much more under a background QoS band), which
    // reorders or merges guest expiries that Linux keeps distinct.
    EV_SET(&change, (uintptr_t)token, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL,
           (intptr_t)delay_ns, (void *)(uintptr_t)token);
    return kevent(descriptor, &change, 1, NULL, 0, NULL);
}

static hl_host_result hl_macos_event_arm_timer(void *context, hl_host_handle pollset, uint64_t token,
                                               uint64_t deadline_ns, uint64_t interval_ns) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_timer *timer;
    uint32_t index;
    uint64_t now;
    int result;
    if (token == 0 || deadline_ns == HL_HOST_DEADLINE_INFINITE)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    if (event == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    timer = hl_macos_event_timer(event, token);
    if (timer == NULL) {
        for (index = 0; index < event->timer_capacity; ++index)
            if (!event->timers[index].active) {
                timer = &event->timers[index];
                break;
            }
    }
    if (timer == NULL) {
        uint32_t capacity =
            hl_macos_grow_capacity(event->timer_capacity, HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_timer *grown = realloc(event->timers, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        }
        memset(grown + event->timer_capacity, 0, (size_t)(capacity - event->timer_capacity) * sizeof(*grown));
        timer = &grown[event->timer_capacity];
        event->timers = grown;
        event->timer_capacity = capacity;
    }
    now = hl_macos_monotonic_value();
    result = hl_macos_event_submit_timer(event->descriptor, token, deadline_ns > now ? deadline_ns - now : 1);
    if (result == 0) {
        timer->active = 1;
        timer->token = token;
        timer->interval_ns = interval_ns;
    }
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_event_disarm_timer(void *context, hl_host_handle pollset, uint64_t token) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_timer *timer;
    struct kevent change;
    if (token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    timer = event == NULL ? NULL : hl_macos_event_timer(event, token);
    if (timer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    EV_SET(&change, (uintptr_t)token, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
    (void)kevent(event->descriptor, &change, 1, NULL, 0, NULL);
    memset(timer, 0, sizeof(*timer));
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_event_wait(void *context, hl_host_handle pollset, hl_host_event_record *events,
                                          size_t event_capacity, uint64_t deadline_ns) {
    hl_host_macos *host = context;
    struct kevent native[64];
    struct timespec timeout;
    struct timespec *timeout_pointer = NULL;
    hl_macos_event *event;
    int descriptor;
    int count;
    int index;
    size_t output_count = 0;
    if (events == NULL || event_capacity == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (event_capacity > HL_ARRAY_COUNT(native)) event_capacity = HL_ARRAY_COUNT(native);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    descriptor = event == NULL ? -1 : event->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (deadline_ns != HL_HOST_DEADLINE_INFINITE) {
        uint64_t now = hl_macos_monotonic_value();
        uint64_t remaining = deadline_ns > now ? deadline_ns - now : 0;
        timeout.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
        timeout.tv_nsec = (long)(remaining % UINT64_C(1000000000));
        timeout_pointer = &timeout;
    }
    count = kevent(descriptor, NULL, 0, native, (int)event_capacity, timeout_pointer);
    if (count < 0) return hl_macos_errno();
    for (index = 0; index < count; ++index) {
        uint32_t readiness = 0;
        uint64_t token = (uint64_t)(uintptr_t)native[index].udata;
        if (native[index].filter == EVFILT_USER) continue;
        if (native[index].filter == EVFILT_READ) readiness |= HL_HOST_READY_READ;
        if (native[index].filter == EVFILT_VNODE) readiness |= HL_HOST_READY_READ;
        if (native[index].filter == EVFILT_WRITE) readiness |= HL_HOST_READY_WRITE;
        if ((native[index].flags & EV_ERROR) != 0) readiness |= HL_HOST_READY_ERROR;
        if ((native[index].flags & EV_EOF) != 0) readiness |= HL_HOST_READY_HANGUP;
        if (native[index].filter == EVFILT_VNODE) {
            pthread_mutex_lock(&host->lock);
            hl_macos_watch_note(host, native[index].ident, native[index].fflags);
            pthread_mutex_unlock(&host->lock);
        }
        if (native[index].filter == EVFILT_TIMER) {
            readiness |= HL_HOST_READY_TIMER;
            token = (uint64_t)native[index].ident;
            pthread_mutex_lock(&host->lock);
            event = hl_macos_event_lookup(host, pollset);
            hl_macos_timer *timer = event == NULL ? NULL : hl_macos_event_timer(event, token);
            if (timer != NULL && timer->interval_ns != 0)
                (void)hl_macos_event_submit_timer(event->descriptor, token, timer->interval_ns);
            else if (timer != NULL)
                memset(timer, 0, sizeof(*timer));
            pthread_mutex_unlock(&host->lock);
        }
        events[output_count++] = (hl_host_event_record){token, readiness, 0};
    }
    return hl_macos_result(HL_STATUS_OK, output_count, 0);
}

static hl_host_result hl_macos_event_wake(void *context, hl_host_handle pollset) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    struct kevent trigger;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    descriptor = event == NULL ? -1 : event->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EV_SET(&trigger, 0, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    return kevent(descriptor, &trigger, 1, NULL, 0, NULL) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_event_close(void *context, hl_host_handle pollset) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    if (event == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = event->descriptor;
    event->active = 0;
    event->descriptor = -1;
    free(event->timers);
    event->timers = NULL;
    event->timer_capacity = 0;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_macos_process *hl_macos_process_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_PROCESS, host->process_capacity, &index) ||
        !host->processes[index].active || host->processes[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->processes[index];
}

static hl_host_result hl_macos_process_spawn_mode(void *context, hl_host_process_entry entry, void *entry_context,
                                                  int prepared) {
    hl_host_macos *host = context;
    hl_host_handle handle = 0;
    uint32_t index;
    pid_t pid;
    int fork_error;
    int private_prepared = 0;
    if (entry == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!prepared && pthread_mutex_lock(&host->fork_gate) != 0)
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (hl_host_process_fd_private_fork_prepare() != 0) {
        if (!prepared) (void)pthread_mutex_unlock(&host->fork_gate);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    private_prepared = 1;
    pid = fork();
    fork_error = errno;
    int private_status = private_prepared ? hl_host_process_fd_private_fork_complete(pid == 0) : 0;
    if (prepared) {
        /* Only the parent retains its watcher threads.  The child must discard
         * inherited subscription slots before object teardown can join them. */
        hl_host_result completed = pid == 0 ? hl_macos_fork_child(host) : hl_macos_fork_complete(host);
        if (private_status != 0 && completed.status == HL_STATUS_OK)
            completed = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        if (completed.status != HL_STATUS_OK) {
            if (pid > 0) {
                int status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            }
            if (pid == 0) _exit(255);
            return completed;
        }
    } else {
        int unlock_status = pthread_mutex_unlock(&host->fork_gate);
        if (private_status != 0 || unlock_status != 0) {
            if (pid > 0) {
                int status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            }
            if (pid == 0) _exit(255);
            return hl_macos_result(private_status != 0 ? HL_STATUS_RESOURCE_LIMIT : HL_STATUS_PLATFORM_FAILURE, 0, 0);
        }
    }
    if (pid < 0) {
        errno = fork_error;
        return hl_macos_errno();
    }
    if (pid == 0) _exit(entry(entry_context) & 255);
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        if (process->active) continue;
        process->generation++;
        if (process->generation == 0) process->generation = 1;
        process->active = 1;
        process->pid = pid;
        process->reaped = 0;
        process->waiting = 0;
        process->waiters = 0;
        process->exit_kind = 0;
        process->exit_value = 0;
        handle = hl_macos_handle(HL_MACOS_HANDLE_PROCESS, index, process->generation);
        break;
    }
    if (handle == 0) {
        uint32_t capacity =
            hl_macos_grow_capacity(host->process_capacity, HL_MACOS_PROCESS_CAPACITY, sizeof(*host->processes));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            int status;
            kill(pid, SIGKILL);
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_process *grown = realloc(host->processes, (size_t)capacity * sizeof(*grown));
        if (grown != NULL) {
            memset(grown + host->process_capacity, 0, (size_t)(capacity - host->process_capacity) * sizeof(*grown));
            index = host->process_capacity;
            host->processes = grown;
            host->process_capacity = capacity;
            grown[index].generation = 1;
            grown[index].active = 1;
            grown[index].pid = pid;
            handle = hl_macos_handle(HL_MACOS_HANDLE_PROCESS, index, 1);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == 0) {
        int status;
        kill(pid, SIGKILL);
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    return hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_process_spawn(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_macos_process_spawn_mode(context, entry, entry_context, 0);
}

static hl_host_result hl_macos_process_spawn_prepared(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_macos_process_spawn_mode(context, entry, entry_context, 1);
}

static hl_host_result hl_macos_process_wait(void *context, hl_host_handle handle, uint64_t deadline_ns) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pid_t pid;
    pid_t waited;
    int status;
    int options;
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process == NULL || host->destroying) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    process->waiters++;
    while (process != NULL && process->waiting && !process->reaped) {
        if (deadline_ns == 0 ||
            (deadline_ns != HL_HOST_DEADLINE_INFINITE && hl_macos_monotonic_value() >= deadline_ns)) {
            process->waiters--;
            pthread_cond_broadcast(&host->process_changed);
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        hl_macos_process_changed_wait(host, deadline_ns);
        process = hl_macos_process_lookup(host, handle);
    }
    if (process != NULL && process->reaped) {
        hl_host_result result = hl_macos_result(HL_STATUS_OK, process->exit_value, process->exit_kind);
        process->waiters--;
        pthread_cond_broadcast(&host->process_changed);
        pthread_mutex_unlock(&host->lock);
        return result;
    }
    pid = process != NULL ? process->pid : -1;
    if (process != NULL) process->waiting = 1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    options = deadline_ns == HL_HOST_DEADLINE_INFINITE ? 0 : WNOHANG;
    for (;;) {
        do {
            waited = waitpid(pid, &status, options);
        } while (waited < 0 && errno == EINTR);
        if (waited != 0) break;
        if (deadline_ns == 0 || hl_macos_monotonic_value() >= deadline_ns) break;
        hl_macos_sleep_until(deadline_ns);
    }
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process != NULL) {
        process->waiting = 0;
        process->waiters--;
    }
    if (waited > 0 && process != NULL) {
        process->reaped = 1;
        process->exit_kind = WIFEXITED(status) ? HL_HOST_PROCESS_EXIT_CODE : HL_HOST_PROCESS_EXIT_SIGNAL;
        process->exit_value = WIFEXITED(status) ? (uint32_t)WEXITSTATUS(status) : (uint32_t)WTERMSIG(status);
    }
    pthread_cond_broadcast(&host->process_changed);
    pthread_mutex_unlock(&host->lock);
    if (waited == 0) return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    if (waited < 0) return hl_macos_errno();
    if (WIFEXITED(status))
        return hl_macos_result(HL_STATUS_OK, (uint64_t)WEXITSTATUS(status), HL_HOST_PROCESS_EXIT_CODE);
    if (WIFSIGNALED(status))
        return hl_macos_result(HL_STATUS_OK, (uint64_t)WTERMSIG(status), HL_HOST_PROCESS_EXIT_SIGNAL);
    return hl_macos_result(HL_STATUS_CORRUPT, 0, (uint64_t)(uint32_t)status);
}

static hl_host_result hl_macos_process_terminate(void *context, hl_host_handle handle, uint32_t reason) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pid_t pid;
    if (reason != HL_HOST_PROCESS_TERMINATE_INTERRUPT && reason != HL_HOST_PROCESS_TERMINATE_FORCE &&
        (reason <= HL_HOST_PROCESS_TERMINATE_SIGNAL || reason > HL_HOST_PROCESS_TERMINATE_SIGNAL + 64))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    pid = process != NULL && !process->reaped && !host->destroying ? process->pid : -1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int signal_number;
    if (reason == HL_HOST_PROCESS_TERMINATE_INTERRUPT)
        signal_number = SIGINT;
    else if (reason == HL_HOST_PROCESS_TERMINATE_FORCE)
        signal_number = SIGKILL;
    else {
        static const unsigned char linux_to_macos[32] = {0,  1,  2,  3,  4,  5,  6,  10, 8,  9,  30,
                                                         11, 31, 13, 14, 15, 16, 20, 19, 17, 18, 21,
                                                         22, 16, 24, 25, 26, 27, 28, 23, 30, 12};
        uint32_t guest = reason - HL_HOST_PROCESS_TERMINATE_SIGNAL;
        signal_number = guest < 32 ? linux_to_macos[guest] : (int)guest;
    }
    if (kill(pid, signal_number) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_process_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (host->destroying || !process->reaped || process->waiting || process->waiters != 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_BUSY, 0, 0);
    }
    process->active = 0;
    process->pid = -1;
    process->reaped = 0;
    process->exit_kind = 0;
    process->exit_value = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_mutex_create(void *context) {
    hl_host_macos *host = context;
    return hl_host_sync_mutex_create(host->sync);
}

static hl_host_result hl_macos_mutex_lock(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    return hl_host_sync_mutex_lock(host->sync, handle);
}

static hl_host_result hl_macos_mutex_unlock(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    return hl_host_sync_mutex_unlock(host->sync, handle);
}

static hl_host_result hl_macos_mutex_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    return hl_host_sync_mutex_close(host->sync, handle);
}

static hl_host_result hl_macos_park(void *context, uint64_t waiter, uint32_t scope, uint64_t key, const void *address,
                                    uint64_t expected, uint32_t compare_size, uint64_t deadline_ns) {
    hl_host_macos *host = context;
    return hl_host_sync_park(host->sync, waiter, scope, key, address, expected, compare_size, deadline_ns);
}

static hl_host_result hl_macos_unpark(void *context, uint32_t scope, uint64_t key, const void *address,
                                      uint32_t count) {
    hl_host_macos *host = context;
    return hl_host_sync_unpark(host->sync, scope, key, address, count);
}

static hl_host_result hl_macos_interrupt_park(void *context, uint64_t waiter) {
    hl_host_macos *host = context;
    return hl_host_sync_interrupt_park(host->sync, waiter);
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
static hl_host_result hl_macos_terminal_probe(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    int descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Answered from the terminal line discipline itself rather than from the object type, which is
     * the distinction the contract exists for: a character device is not a terminal. */
    return hl_macos_result(HL_STATUS_OK, isatty(descriptor) != 0 ? 1u : 0u, 0);
}

static hl_host_result hl_macos_terminal_get_mode(void *context, hl_host_handle handle, uint32_t *mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    uint32_t value = 0;
    if (mode == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
    if ((attributes.c_lflag & ICANON) == 0) value |= HL_HOST_TERMINAL_RAW_INPUT;
    if ((attributes.c_lflag & ECHO) != 0) value |= HL_HOST_TERMINAL_ECHO;
    if ((attributes.c_lflag & ISIG) != 0) value |= HL_HOST_TERMINAL_SIGNALS;
    if ((attributes.c_iflag & IXON) != 0) value |= HL_HOST_TERMINAL_FLOW_CONTROL;
    if ((attributes.c_oflag & OPOST) != 0) value |= HL_HOST_TERMINAL_OUTPUT_PROCESSING;
    *mode = value;
    return hl_macos_result(HL_STATUS_OK, value, 0);
}

static hl_host_result hl_macos_terminal_set_mode(void *context, hl_host_handle handle, uint32_t mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    if ((mode & ~(uint32_t)(HL_HOST_TERMINAL_RAW_INPUT | HL_HOST_TERMINAL_ECHO | HL_HOST_TERMINAL_SIGNALS |
                            HL_HOST_TERMINAL_FLOW_CONTROL | HL_HOST_TERMINAL_OUTPUT_PROCESSING)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
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
    if (tcsetattr(descriptor, TCSANOW, &attributes) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, mode, 0);
}

static hl_host_result hl_macos_terminal_get_size(void *context, hl_host_handle handle, hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    if (ioctl(descriptor, TIOCGWINSZ, &window) != 0) return hl_macos_errno();
    size->columns = window.ws_col;
    size->rows = window.ws_row;
    size->pixel_width = window.ws_xpixel;
    size->pixel_height = window.ws_ypixel;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_set_size(void *context, hl_host_handle handle,
                                                 const hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL || size->columns > UINT16_MAX || size->rows > UINT16_MAX || size->pixel_width > UINT16_MAX ||
        size->pixel_height > UINT16_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    window.ws_col = (unsigned short)size->columns;
    window.ws_row = (unsigned short)size->rows;
    window.ws_xpixel = (unsigned short)size->pixel_width;
    window.ws_ypixel = (unsigned short)size->pixel_height;
    if (ioctl(descriptor, TIOCSWINSZ, &window) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_read(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output.data, output.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_terminal_write(void *context, hl_host_handle handle, hl_host_const_bytes input) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || input.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

/*
 * Typed absence, not an oversight. On this host a resize is delivered as a process-directed signal
 * that the engine's own signal machinery already owns end to end, so there is no separate object
 * for this to hand back -- and manufacturing one would mean installing a process-wide handler
 * underneath the layer that is already handling that signal, which is a worse bargain than saying
 * so. The operation exists in the contract for a host where the resize arrives in the input stream
 * instead, where waiting for input and then reading it is a deadlock rather than a composition.
 */
static hl_host_result hl_macos_terminal_size_change_event(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    if (hl_macos_file_descriptor(host, handle, 0) < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_macos_fork_prepare(void *context) {
    hl_host_macos *host = context;
    hl_host_result result;
    if (pthread_mutex_lock(&host->fork_gate) != 0) return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_lock(&host->lock) != 0) {
        pthread_mutex_unlock(&host->fork_gate);
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    result = hl_host_sync_fork_prepare(host->sync);
    if (result.status == HL_STATUS_OK) {
        uint32_t index;
        /* clone_for_fork has created one additional handle per OFD.  fork duplicates both the
           parent and child handles into the other process, so reserve one reference for every
           inherited handle before either side closes its unwanted half. */
        for (index = 0; index < host->file_capacity; ++index) {
            if (host->files[index].active && host->files[index].directory_shared != NULL)
                (void)__atomic_add_fetch(&host->files[index].directory_shared->references, 1u, __ATOMIC_ACQ_REL);
            /* Every live stream handle is duplicated by fork. Reserve the
             * child's reference before either process can close an endpoint;
             * otherwise one process may unmap the shared stream bookkeeping
             * while the other still owns its inherited handle. */
            if (host->files[index].active && host->files[index].stream != NULL)
                (void)__atomic_add_fetch(&host->files[index].stream->references, 1u, __ATOMIC_ACQ_REL);
        }
        for (index = 0; index < host->counter_capacity; ++index) {
            hl_macos_counter_object *object;
            uint32_t previous;
            if (!host->counters[index].active) continue;
            object = host->counters[index].object;
            for (previous = 0; previous < index; ++previous)
                if (host->counters[previous].active && host->counters[previous].object == object) break;
            if (previous == index) object->shared->references++;
        }
    }
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_unlock(&host->lock);
        pthread_mutex_unlock(&host->fork_gate);
    }
    return result;
}

static hl_host_result hl_macos_fork_complete(void *context) {
    hl_host_macos *host = context;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

static hl_host_result hl_macos_fork_child(void *context) {
    hl_host_macos *host = context;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    /* Only the forking thread exists here, so every waiter record inherited from the parent is about
     * a thread this process does not have. Left in place they would hand a reused waiter identity
     * someone else's outstanding interruption. */
    hl_host_sync_park_reset(host->sync);
    for (uint32_t index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_macos_counter_subscription *subscription = host->counter_subscriptions[index];
        if (subscription == NULL) continue;
        if (!subscription->active) continue;
        hl_host_process_fd_private_remove(subscription->descriptor);
        hl_host_process_fd_private_remove(subscription->wake[0]);
        hl_host_process_fd_private_remove(subscription->wake[1]);
        close(subscription->descriptor);
        close(subscription->wake[0]);
        close(subscription->wake[1]);
        subscription->active = 0;
        subscription->counter = HL_HOST_HANDLE_INVALID;
        subscription->notify = NULL;
        subscription->observer = NULL;
    }
    for (uint32_t index = 0; index < host->directory_capacity && result.status == HL_STATUS_OK; ++index) {
        hl_macos_directory_object *object;
        uint32_t previous;
        if (!host->directories[index].active) continue;
        object = host->directories[index].object;
        for (previous = 0; previous < index; ++previous)
            if (host->directories[previous].active && host->directories[previous].object == object) break;
        if (previous != index) continue;
        int replacement = kqueue();
        if (replacement < 0) {
            result = hl_macos_errno();
            break;
        }
        (void)fcntl(replacement, F_SETFD, FD_CLOEXEC);
        int adopted = hl_host_process_fd_private_adopt(replacement);
        if (adopted < 0) {
            close(replacement);
            result = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
            break;
        }
        replacement = adopted;
        for (uint32_t watch_index = 0; watch_index < object->watch_capacity; ++watch_index) {
            hl_macos_directory_watch *watch = &object->watches[watch_index];
            if (!watch->active) continue;
            struct kevent change;
            uint16_t flags =
                (uint16_t)(EV_ADD | EV_CLEAR | ((watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
            EV_SET(&change, watch->descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(watch->interests), 0,
                   (void *)(uintptr_t)watch->token);
            if (kevent(replacement, &change, 1, NULL, 0, NULL) != 0) {
                hl_host_process_fd_private_remove(replacement);
                close(replacement);
                result = hl_macos_errno();
                break;
            }
        }
        if (result.status != HL_STATUS_OK) break;
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        object->descriptor = replacement;
    }
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

static void hl_macos_log(void *context, uint32_t event, const char *message, size_t message_size) {
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

static hl_host_result hl_macos_file_validate_private_regular(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, file, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISREG(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_macos_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                         size_t path_size, hl_host_const_bytes input,
                                                         uint32_t permissions) {
    static _Atomic uint64_t sequence;
    hl_host_macos *host = context;
    char name[PATH_MAX], temporary[PATH_MAX];
    int directory_fd = AT_FDCWD, descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(name) || memchr(path, '\0', path_size) != NULL ||
        (permissions & ~0777u) != 0 || (input.size != 0 && input.data == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(name, path, path_size);
    name[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        directory_fd = hl_macos_file_descriptor(host, directory, 0);
        if (directory_fd >= 0) directory_fd = fcntl(directory_fd, F_DUPFD_CLOEXEC, 0);
        if (directory_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
        return hl_macos_errno();
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
    return ok ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_validate_private_directory(void *context, hl_host_handle directory) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, directory, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

hl_status hl_host_macos_create(hl_host_macos **out_host, hl_host_services *out_services) {
    static const hl_host_memory_services memory = {
        HL_HOST_MEMORY_ABI,        sizeof(memory),          hl_macos_reserve,      hl_macos_protect,
        hl_macos_release,          hl_macos_publish,        hl_macos_reserve_code, hl_macos_repair_code,
        hl_macos_begin_code_write, hl_macos_end_code_write, hl_macos_map_file,     hl_macos_mapping_sync,
        hl_macos_unmap_range,      hl_macos_map_anonymous,  hl_macos_discard,      hl_macos_repair_signal_page,
        hl_macos_unmap_address,    hl_macos_wire_range,     hl_macos_unwire_range, hl_macos_protect_address,
        hl_macos_sync_address};
    static const hl_host_clock_services clock = {.abi = HL_HOST_CLOCK_ABI,
                                                 .size = sizeof(clock),
                                                 .monotonic_ns = hl_macos_monotonic,
                                                 .realtime_ns = hl_macos_realtime,
                                                 .raw_monotonic_ns = hl_macos_raw_monotonic,
                                                 .process_cpu_ns = hl_macos_process_cpu,
                                                 .thread_cpu_ns = hl_macos_thread_cpu,
                                                 .sleep_until = hl_macos_clock_sleep_until,
                                                 .architectural_counter_hz = hl_macos_architectural_counter,
                                                 .backoff_ns = hl_macos_backoff};
    static const hl_host_log_services log = {HL_HOST_LOG_ABI, sizeof(log), hl_macos_log};
    static const hl_host_file_services file = {HL_HOST_FILE_ABI,
                                               sizeof(file),
                                               hl_macos_file_open,
                                               hl_macos_file_read,
                                               hl_macos_file_write,
                                               hl_macos_file_append,
                                               hl_macos_file_metadata_get,
                                               hl_macos_file_close,
                                               hl_macos_file_read_sequential,
                                               hl_macos_file_write_sequential,
                                               hl_macos_file_clone_for_fork,
                                               hl_macos_file_seek,
                                               hl_macos_file_readv,
                                               hl_macos_file_writev,
                                               hl_macos_file_readv_at,
                                               hl_macos_file_writev_at,
                                               hl_macos_file_appendv,
                                               hl_macos_file_truncate,
                                               hl_macos_file_sync,
                                               hl_macos_file_sync,
                                               hl_macos_file_rename,
                                               hl_macos_file_unlink,
                                               hl_macos_file_path,
                                               hl_macos_file_standard_stream,
                                               hl_macos_file_readlink,
                                               hl_macos_file_set_owner,
                                               hl_macos_file_resolve_beneath,
                                               hl_macos_file_sync_range,
                                               hl_macos_file_sync_filesystem,
                                               hl_macos_file_open_beneath,
                                               hl_macos_file_allocate_range,
                                               hl_macos_file_filesystem_metadata,
                                               hl_macos_file_set_permissions,
                                               hl_macos_file_set_times,
                                               hl_macos_file_read_directory,
                                               hl_macos_file_mkdir,
                                               hl_macos_file_symlink,
                                               hl_macos_file_link,
                                               hl_macos_file_fifo,
                                               hl_macos_file_validate_private_regular,
                                               hl_macos_file_store_private_atomic,
                                               hl_macos_file_validate_private_directory,
                                               hl_macos_file_rmdir};
    static const hl_host_process_services process = {
        HL_HOST_PROCESS_ABI,        sizeof(process),        hl_macos_process_spawn,         hl_macos_process_wait,
        hl_macos_process_terminate, hl_macos_process_close, hl_macos_process_spawn_prepared};
    static const hl_host_event_services event = {
        HL_HOST_EVENT_ABI,          sizeof(event),       hl_macos_event_create, hl_macos_event_control,
        hl_macos_event_wait,        hl_macos_event_wake, hl_macos_event_close,  hl_macos_event_arm_timer,
        hl_macos_event_disarm_timer};
    static const hl_host_shared_memory_services shared_memory = {HL_HOST_SHARED_MEMORY_ABI, sizeof(shared_memory),
                                                                 hl_macos_shared_create,    hl_macos_shared_open,
                                                                 hl_macos_shared_resize,    hl_macos_file_close};
    static const hl_host_sync_services sync = {HL_HOST_SYNC_ABI,      sizeof(sync),           hl_macos_mutex_create,
                                               hl_macos_mutex_lock,   hl_macos_mutex_unlock,  hl_macos_mutex_close,
                                               hl_macos_fork_prepare, hl_macos_fork_complete, hl_macos_fork_child,
                                               hl_macos_park,         hl_macos_unpark,        hl_macos_interrupt_park};
    static const hl_host_terminal_services terminal = {HL_HOST_TERMINAL_ABI,       sizeof(terminal),
                                                       hl_macos_terminal_probe,    hl_macos_terminal_get_mode,
                                                       hl_macos_terminal_set_mode, hl_macos_terminal_get_size,
                                                       hl_macos_terminal_set_size, hl_macos_terminal_read,
                                                       hl_macos_terminal_write,    hl_macos_terminal_size_change_event};
    static const hl_host_counter_services counter = {
        HL_HOST_COUNTER_ABI,          sizeof(counter),
        hl_macos_counter_create,      hl_macos_counter_read,
        hl_macos_counter_write,       hl_macos_counter_get_flags,
        hl_macos_counter_set_flags,   hl_macos_counter_duplicate,
        hl_macos_counter_readiness,   hl_macos_counter_subscribe,
        hl_macos_counter_unsubscribe, hl_macos_counter_close,
    };
    static const hl_host_transfer_services transfer = {
        HL_HOST_TRANSFER_ABI,    sizeof(transfer),          hl_macos_transfer_channel_pair,
        hl_macos_transfer_send,  hl_macos_transfer_receive, hl_macos_transfer_duplicate,
        hl_macos_transfer_close,
    };
    static const hl_host_directory_services directory = {
        HL_HOST_DIRECTORY_ABI,     sizeof(directory),         hl_macos_directory_create, hl_macos_directory_add,
        hl_macos_directory_modify, hl_macos_directory_remove, hl_macos_directory_read,   hl_macos_directory_duplicate,
        hl_macos_directory_close};
    static const hl_host_watch_services watch = {HL_HOST_WATCH_ABI,    sizeof(watch),        hl_macos_watch_open,
                                                 hl_macos_watch_query, hl_macos_watch_drain, hl_macos_watch_close};
    static const hl_host_stream_services stream = {HL_HOST_STREAM_ABI,        sizeof(stream),
                                                   hl_macos_stream_pipe_pair, hl_macos_stream_read,
                                                   hl_macos_stream_write,     hl_macos_stream_duplicate,
                                                   hl_macos_stream_close,     hl_macos_stream_set_status_flags,
                                                   hl_macos_stream_readiness, hl_macos_stream_move};
    static const hl_host_posix_attachment_services posix_attachment = {
        HL_HOST_POSIX_ATTACHMENT_ABI, sizeof(posix_attachment), hl_macos_attachment_borrow_file,
        hl_macos_attachment_borrow_file_at_least, hl_macos_attachment_release};
    hl_host_macos *host;
    if (out_host == NULL || out_services == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_host = NULL;
    memset(out_services, 0, sizeof(*out_services));
    host = calloc(1, sizeof(*host));
    if (host == NULL) return HL_STATUS_OUT_OF_MEMORY;
    host->mappings = calloc(HL_MACOS_MAPPING_CAPACITY, sizeof(*host->mappings));
    host->files = calloc(HL_MACOS_FILE_CAPACITY, sizeof(*host->files));
    host->counters = calloc(HL_MACOS_COUNTER_CAPACITY, sizeof(*host->counters));
    host->transfers = calloc(HL_MACOS_TRANSFER_CAPACITY, sizeof(*host->transfers));
    host->directories = calloc(HL_MACOS_DIRECTORY_CAPACITY, sizeof(*host->directories));
    host->processes = calloc(HL_MACOS_PROCESS_CAPACITY, sizeof(*host->processes));
    host->events = calloc(HL_MACOS_EVENT_CAPACITY, sizeof(*host->events));
    host->watches = calloc(HL_MACOS_WATCH_CAPACITY, sizeof(*host->watches));
    if (host->mappings == NULL || host->files == NULL || host->counters == NULL || host->transfers == NULL ||
        host->directories == NULL || host->processes == NULL || host->events == NULL || host->watches == NULL) {
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    host->mapping_capacity = HL_MACOS_MAPPING_CAPACITY;
    host->file_capacity = HL_MACOS_FILE_CAPACITY;
    host->counter_capacity = HL_MACOS_COUNTER_CAPACITY;
    host->transfer_capacity = HL_MACOS_TRANSFER_CAPACITY;
    host->directory_capacity = HL_MACOS_DIRECTORY_CAPACITY;
    host->process_capacity = HL_MACOS_PROCESS_CAPACITY;
    host->event_capacity = HL_MACOS_EVENT_CAPACITY;
    host->watch_capacity = HL_MACOS_WATCH_CAPACITY;
    if (pthread_mutex_init(&host->lock, NULL) != 0) {
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_mutex_init(&host->fork_gate, NULL) != 0) {
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_cond_init(&host->process_changed, NULL) != 0) {
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (hl_host_sync_registry_create(&host->sync) != HL_STATUS_OK) {
        pthread_cond_destroy(&host->process_changed);
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    out_services->abi = HL_HOST_SERVICES_ABI;
    out_services->size = sizeof(*out_services);
    out_services->capabilities = HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_LOG | HL_HOST_CAP_FILE |
                                 HL_HOST_CAP_PROCESS | HL_HOST_CAP_EVENT_TIMER | HL_HOST_CAP_SHARED_MEMORY |
                                 HL_HOST_CAP_CODE_MAPPING | HL_HOST_CAP_SYNC | HL_HOST_CAP_EVENT | HL_HOST_CAP_COUNTER |
                                 HL_HOST_CAP_DIRECTORY | HL_HOST_CAP_TRANSFER | HL_HOST_CAP_WATCH | HL_HOST_CAP_STREAM |
                                 HL_HOST_CAP_POSIX_ATTACHMENT | HL_HOST_CAP_TERMINAL;
    out_services->context = host;
    out_services->memory = &memory;
    out_services->clock = &clock;
    out_services->log = &log;
    out_services->file = &file;
    out_services->process = &process;
    out_services->event = &event;
    out_services->shared_memory = &shared_memory;
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

void hl_host_macos_destroy(hl_host_macos *host) {
    uint32_t index;
    if (host == NULL) return;
    pthread_mutex_lock(&host->lock);
    host->destroying = 1;
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        if (process->active && !process->reaped) kill(process->pid, SIGKILL);
    }
    pthread_mutex_unlock(&host->lock);
    /* Subscription threads may call user code and own three descriptors each.
     * Join them before releasing counters or any storage they can observe. */
    for (index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[index] != NULL && host->counter_subscriptions[index]->active)
            handle =
                hl_macos_handle(HL_MACOS_HANDLE_SUBSCRIPTION, index, host->counter_subscriptions[index]->generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_counter_unsubscribe(host, handle);
    }
    for (index = 0; index < host->transfer_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->transfers[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, host->transfers[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_transfer_close(host, handle);
    }
    for (index = 0; index < host->directory_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->directories[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, host->directories[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_directory_close(host, handle);
    }
    for (index = 0; index < host->counter_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counters[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_COUNTER, index, host->counters[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_counter_close(host, handle);
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->event_capacity; ++index)
        if (host->events[index].active) close(host->events[index].descriptor);
    for (index = 0; index < host->watch_capacity; ++index)
        if (host->watches[index].active) close(host->watches[index].descriptor);
    hl_host_sync_registry_destroy(host->sync);
    for (;;) {
        uint32_t waiters = 0;
        for (index = 0; index < host->process_capacity; ++index)
            waiters += host->processes[index].waiters;
        if (waiters == 0) break;
        pthread_cond_wait(&host->process_changed, &host->lock);
    }
    pthread_mutex_unlock(&host->lock);
    for (index = 0; index < host->mapping_capacity; index++) {
        hl_macos_mapping *mapping = &host->mappings[index];
        uint64_t held_offset;
        uint64_t held_size;
        if (!mapping->active) continue;
        /* Teardown gives back only what is still held, for the same reason release does. */
        for (uint32_t part = 0;
             hl_host_hole_set_held_range(&mapping->retired, mapping->size, part, &held_offset, &held_size); ++part)
            munmap((char *)mapping->writable + held_offset, (size_t)held_size);
        if (mapping->executable != NULL && mapping->executable != mapping->writable)
            munmap(mapping->executable, (size_t)mapping->size);
        hl_host_hole_set_release(&mapping->retired);
    }
    for (index = 0; index < host->file_capacity; ++index) {
        hl_macos_file *file = &host->files[index];
        if (!file->active) continue;
        close(file->descriptor);
        if (file->directory != NULL) closedir(file->directory);
        if (file->append_descriptor >= 0) close(file->append_descriptor);
        hl_macos_stream_release(file->stream);
        hl_macos_directory_shared_release(file->directory_shared);
    }
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        int status;
        if (!process->active || process->reaped) continue;
        kill(process->pid, SIGKILL);
        while (waitpid(process->pid, &status, 0) < 0 && errno == EINTR) {}
    }
    pthread_cond_destroy(&host->process_changed);
    pthread_mutex_destroy(&host->fork_gate);
    pthread_mutex_destroy(&host->lock);
    for (index = 0; index < host->counter_subscription_capacity; ++index)
        free(host->counter_subscriptions[index]);
    free(host->counter_subscriptions);
    free(host->watches);
    free(host->events);
    free(host->files);
    free(host->mappings);
    free(host->directories);
    free(host->transfers);
    free(host->counters);
    free(host->processes);
    free(host);
}
