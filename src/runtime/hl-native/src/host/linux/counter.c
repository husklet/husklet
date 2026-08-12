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

