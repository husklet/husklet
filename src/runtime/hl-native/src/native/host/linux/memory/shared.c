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

