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

