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

