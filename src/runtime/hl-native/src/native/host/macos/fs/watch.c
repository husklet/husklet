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

