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

