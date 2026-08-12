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

