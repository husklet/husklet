static hl_host_result hl_linux_event_create(void *context) {
    hl_host_linux *host = context;
    struct epoll_event event = {0};
    int pollset = epoll_create1(EPOLL_CLOEXEC);
    int wake;
    if (pollset < 0) return hl_linux_errno_result();
    wake = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (wake < 0) {
        close(pollset);
        return hl_linux_errno_result();
    }
    event.events = EPOLLIN;
    event.data.u64 = 0;
    if (epoll_ctl(pollset, EPOLL_CTL_ADD, wake, &event) != 0) {
        close(wake);
        close(pollset);
        return hl_linux_errno_result();
    }
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_POLLSET, pollset, NULL, NULL, 0, wake);
    if (result.status != HL_STATUS_OK) {
        close(wake);
        close(pollset);
    }
    return result;
}

static uint32_t hl_linux_epoll_events(uint32_t interests) {
    uint32_t events = 0;
    if ((interests & HL_HOST_READY_READ) != 0) events |= EPOLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) events |= EPOLLOUT;
    if ((interests & HL_HOST_READY_EDGE) != 0) events |= EPOLLET;
    if ((interests & HL_HOST_READY_ONESHOT) != 0) events |= EPOLLONESHOT;
    return events;
}

static hl_linux_timer_entry *hl_linux_event_timer(hl_host_linux *host, hl_host_handle pollset, uint64_t token);

static hl_host_result hl_linux_event_control(void *context, hl_host_handle pollset, uint32_t operation,
                                             hl_host_handle object, uint64_t token, uint32_t interests) {
    hl_host_linux *host = context;
    struct epoll_event event = {hl_linux_epoll_events(interests), {.u64 = token}};
    int pollset_fd;
    int object_fd;
    int native_operation;
    pthread_mutex_lock(&host->lock);
    pollset_fd = hl_linux_descriptor(host, pollset, HL_LINUX_HANDLE_POLLSET, HL_LINUX_HANDLE_POLLSET);
    object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SOCKET);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_COUNTER, HL_LINUX_HANDLE_COUNTER);
    if (object_fd < 0)
        object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_DIRECTORY, HL_LINUX_HANDLE_DIRECTORY);
    if (object_fd < 0)
        object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    if (object_fd < 0) object_fd = hl_linux_descriptor(host, object, HL_LINUX_HANDLE_WATCH, HL_LINUX_HANDLE_WATCH);
    if (pollset_fd < 0 || object_fd < 0 || token == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (operation == HL_HOST_EVENT_ADD)
        native_operation = EPOLL_CTL_ADD;
    else if (operation == HL_HOST_EVENT_MODIFY)
        native_operation = EPOLL_CTL_MOD;
    else if (operation == HL_HOST_EVENT_DELETE)
        native_operation = EPOLL_CTL_DEL;
    else {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int result = epoll_ctl(pollset_fd, native_operation, object_fd, &event);
    hl_host_result output = result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
    pthread_mutex_unlock(&host->lock);
    return output;
}

static hl_host_result hl_linux_event_wait(void *context, hl_host_handle pollset, hl_host_event_record *events,
                                          size_t event_capacity, uint64_t deadline_ns) {
    hl_host_linux *host = context;
    struct epoll_event native_events[64];
    int pollset_fd;
    int wake_descriptor;
    int timeout;
    int count;
    size_t i;
    if (events == NULL || event_capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (event_capacity > HL_ARRAY_COUNT(native_events)) event_capacity = HL_ARRAY_COUNT(native_events);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    pollset_fd = pollset_entry == NULL ? -1 : pollset_entry->descriptor;
    wake_descriptor = pollset_entry == NULL ? -1 : pollset_entry->wake_descriptor;
    pthread_mutex_unlock(&host->lock);
    if (pollset_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (deadline_ns == HL_HOST_DEADLINE_INFINITE)
        timeout = -1;
    else {
        uint64_t now = hl_linux_monotonic_value();
        uint64_t remaining = deadline_ns > now ? deadline_ns - now : 0;
        uint64_t milliseconds = (remaining + UINT64_C(999999)) / UINT64_C(1000000);
        timeout = milliseconds > INT_MAX ? INT_MAX : (int)milliseconds;
    }
    count = epoll_wait(pollset_fd, native_events, (int)event_capacity, timeout);
    if (count < 0) return hl_linux_errno_result();
    size_t output_count = 0;
    for (i = 0; i < (size_t)count; ++i) {
        uint32_t ready = 0;
        if (native_events[i].data.u64 == 0) {
            uint64_t ignored;
            while (read(wake_descriptor, &ignored, sizeof(ignored)) == (ssize_t)sizeof(ignored)) {}
            continue;
        }
        pthread_mutex_lock(&host->lock);
        hl_linux_timer_entry *timer = hl_linux_event_timer(host, pollset, native_events[i].data.u64);
        int timer_descriptor = timer == NULL ? -1 : timer->descriptor;
        pthread_mutex_unlock(&host->lock);
        if (timer_descriptor >= 0) {
            uint64_t expirations;
            ssize_t consumed = read(timer_descriptor, &expirations, sizeof(expirations));
            (void)consumed;
            ready |= HL_HOST_READY_TIMER;
        }
        if ((native_events[i].events & EPOLLIN) != 0) ready |= HL_HOST_READY_READ;
        if ((native_events[i].events & EPOLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
        if ((native_events[i].events & EPOLLERR) != 0) ready |= HL_HOST_READY_ERROR;
        if ((native_events[i].events & EPOLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
        events[output_count++] = (hl_host_event_record){native_events[i].data.u64, ready, 0};
    }
    return hl_linux_result(HL_STATUS_OK, output_count, 0);
}

static hl_host_result hl_linux_event_wake(void *context, hl_host_handle pollset) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    uint64_t one = 1;
    ssize_t result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = write(entry->wake_descriptor, &one, sizeof(one));
    pthread_mutex_unlock(&host->lock);
    return result == (ssize_t)sizeof(one) ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_linux_timer_entry *hl_linux_event_timer(hl_host_linux *host, hl_host_handle pollset, uint64_t token) {
    uint32_t index;
    for (index = 0; index < host->timer_capacity; ++index)
        if (host->timers[index].descriptor >= 0 && host->timers[index].pollset == pollset &&
            host->timers[index].token == token)
            return &host->timers[index];
    return NULL;
}

static hl_host_result hl_linux_event_arm_timer(void *context, hl_host_handle pollset, uint64_t token,
                                               uint64_t deadline_ns, uint64_t interval_ns) {
    hl_host_linux *host = context;
    hl_linux_timer_entry *timer;
    hl_linux_handle_entry *pollset_entry;
    struct itimerspec setting = {0};
    struct epoll_event event = {EPOLLIN, {.u64 = token}};
    int descriptor;
    uint32_t index;
    if (token == 0 || deadline_ns == HL_HOST_DEADLINE_INFINITE)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    if (pollset_entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    timer = hl_linux_event_timer(host, pollset, token);
    if (timer == NULL) {
        for (index = 0; index < host->timer_capacity; ++index)
            if (host->timers[index].descriptor < 0) {
                timer = &host->timers[index];
                break;
            }
        if (timer == NULL) {
            uint32_t capacity;
            hl_linux_timer_entry *grown = hl_linux_grow_capacity(host->timer_capacity, HL_LINUX_TIMER_CAPACITY,
                                                                 sizeof(*host->timers), &capacity) == 0
                                              ? realloc(host->timers, (size_t)capacity * sizeof(*grown))
                                              : NULL;
            if (grown != NULL) {
                for (index = host->timer_capacity; index < capacity; ++index) {
                    grown[index] = (hl_linux_timer_entry){0};
                    grown[index].descriptor = -1;
                }
                timer = &grown[host->timer_capacity];
                host->timers = grown;
                host->timer_capacity = capacity;
            }
        }
        if (timer == NULL) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        descriptor = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
        if (descriptor < 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_errno_result();
        }
        if (epoll_ctl(pollset_entry->descriptor, EPOLL_CTL_ADD, descriptor, &event) != 0) {
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            pthread_mutex_unlock(&host->lock);
            return error;
        }
        timer->pollset = pollset;
        timer->token = token;
        timer->descriptor = descriptor;
    }
    if (deadline_ns <= hl_linux_monotonic_value()) deadline_ns = hl_linux_monotonic_value() + 1;
    setting.it_value.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    setting.it_value.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    setting.it_interval.tv_sec = (time_t)(interval_ns / UINT64_C(1000000000));
    setting.it_interval.tv_nsec = (long)(interval_ns % UINT64_C(1000000000));
    descriptor = timer->descriptor;
    int configured = timerfd_settime(descriptor, TFD_TIMER_ABSTIME, &setting, NULL);
    pthread_mutex_unlock(&host->lock);
    return configured == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_event_disarm_timer(void *context, hl_host_handle pollset, uint64_t token) {
    hl_host_linux *host = context;
    hl_linux_timer_entry *timer;
    hl_linux_handle_entry *pollset_entry;
    int descriptor;
    if (token == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    pollset_entry = hl_linux_lookup_locked(host, pollset, HL_LINUX_HANDLE_POLLSET);
    timer = pollset_entry == NULL ? NULL : hl_linux_event_timer(host, pollset, token);
    if (timer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    descriptor = timer->descriptor;
    timer->descriptor = -1;
    timer->pollset = HL_HOST_HANDLE_INVALID;
    timer->token = 0;
    (void)epoll_ctl(pollset_entry->descriptor, EPOLL_CTL_DEL, descriptor, NULL);
    pthread_mutex_unlock(&host->lock);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_event_close(void *context, hl_host_handle pollset) {
    hl_host_linux *host = context;
    uint32_t index;
    pthread_mutex_lock(&host->lock);
    if (hl_linux_descriptor(host, pollset, HL_LINUX_HANDLE_POLLSET, HL_LINUX_HANDLE_POLLSET) < 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (index = 0; index < host->timer_capacity; ++index) {
        hl_linux_timer_entry *timer = &host->timers[index];
        if (timer->descriptor < 0 || timer->pollset != pollset) continue;
        close(timer->descriptor);
        timer->descriptor = -1;
        timer->pollset = HL_HOST_HANDLE_INVALID;
        timer->token = 0;
    }
    pthread_mutex_unlock(&host->lock);
    return hl_linux_close_descriptor(context, pollset);
}

