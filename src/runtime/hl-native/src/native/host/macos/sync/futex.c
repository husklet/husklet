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
