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

