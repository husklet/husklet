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

