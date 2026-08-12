static hl_host_result hl_macos_event_create(void *context) {
    hl_host_macos *host = context;
    struct kevent wake;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status exhausted = HL_STATUS_RESOURCE_LIMIT;
    uint32_t index;
    int descriptor = kqueue();
    if (descriptor < 0) return hl_macos_errno();
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    EV_SET(&wake, 0, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
    if (kevent(descriptor, &wake, 1, NULL, 0, NULL) != 0) {
        hl_host_result error = hl_macos_errno();
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return error;
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->event_capacity; ++index) {
        hl_macos_event *event = &host->events[index];
        if (event->active) continue;
        event->generation++;
        if (event->generation == 0) event->generation = 1;
        event->active = 1;
        event->descriptor = descriptor;
        event->timers = calloc(HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
        if (event->timers == NULL) continue;
        event->timer_capacity = HL_MACOS_TIMER_CAPACITY;
        handle = hl_macos_handle(HL_MACOS_HANDLE_EVENT, index, event->generation);
        break;
    }
    if (handle == HL_HOST_HANDLE_INVALID) {
        uint32_t capacity = host->event_capacity > UINT32_MAX / 2u ? UINT32_MAX : host->event_capacity * 2u;
        hl_macos_event *grown =
            capacity > host->event_capacity ? realloc(host->events, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            memset(grown + host->event_capacity, 0, (size_t)(capacity - host->event_capacity) * sizeof(*grown));
            index = host->event_capacity;
            host->events = grown;
            host->event_capacity = capacity;
            hl_macos_event *event = &host->events[index];
            event->generation = 1;
            event->active = 1;
            event->descriptor = descriptor;
            event->timers = calloc(HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
            if (event->timers == NULL) {
                event->active = 0;
                exhausted = HL_STATUS_OUT_OF_MEMORY;
            } else {
                event->timer_capacity = HL_MACOS_TIMER_CAPACITY;
                handle = hl_macos_handle(HL_MACOS_HANDLE_EVENT, index, event->generation);
            }
        } else if (capacity > host->event_capacity)
            exhausted = HL_STATUS_OUT_OF_MEMORY;
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == HL_HOST_HANDLE_INVALID) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return hl_macos_result(exhausted, 0, 0);
    }
    return hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_macos_timer *hl_macos_event_timer(hl_macos_event *event, uint64_t token) {
    uint32_t index;
    for (index = 0; index < event->timer_capacity; ++index)
        if (event->timers[index].active && event->timers[index].token == token) return &event->timers[index];
    return NULL;
}

static hl_host_result hl_macos_event_control(void *context, hl_host_handle pollset, uint32_t operation,
                                             hl_host_handle object_handle, uint64_t token, uint32_t interests) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_counter *counter;
    hl_macos_directory *directory;
    hl_macos_transfer *transfer;
    hl_macos_watch *watch;
    hl_macos_file *stream;
    struct kevent changes[2];
    int count = 0;
    int descriptor;
    uint16_t flags;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    counter = hl_macos_counter_lookup(host, object_handle);
    directory = hl_macos_directory_lookup(host, object_handle);
    transfer = hl_macos_transfer_lookup(host, object_handle);
    watch = hl_macos_watch_lookup(host, object_handle);
    stream = hl_macos_file_lookup(host, object_handle);
    if (stream != NULL && stream->stream == NULL) stream = NULL;
    descriptor =
        event == NULL || (counter == NULL && directory == NULL && transfer == NULL && watch == NULL && stream == NULL)
            ? -1
            : event->descriptor;
    if (descriptor >= 0 && counter != NULL) object_handle = (hl_host_handle)counter->object->readable;
    if (descriptor >= 0 && directory != NULL) object_handle = (hl_host_handle)directory->object->descriptor;
    if (descriptor >= 0 && transfer != NULL) object_handle = (hl_host_handle)transfer->descriptor;
    if (descriptor >= 0 && watch != NULL) object_handle = (hl_host_handle)watch->descriptor;
    if (descriptor >= 0 && stream != NULL) object_handle = (hl_host_handle)stream->descriptor;
    if (descriptor < 0 || token == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (operation == HL_HOST_EVENT_DELETE)
        flags = EV_DELETE;
    else if (operation == HL_HOST_EVENT_ADD || operation == HL_HOST_EVENT_MODIFY)
        flags = (uint16_t)(EV_ADD | EV_ENABLE | ((interests & HL_HOST_READY_EDGE) ? EV_CLEAR : 0) |
                           ((interests & HL_HOST_READY_ONESHOT) ? EV_ONESHOT : 0));
    else {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((interests & HL_HOST_READY_READ) != 0) {
        if (watch != NULL)
            EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_VNODE, flags,
                   NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_DELETE | NOTE_RENAME, 0, (void *)(uintptr_t)token);
        else
            EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_READ, flags, 0, 0, (void *)(uintptr_t)token);
    }
    if (stream != NULL && (interests & HL_HOST_READY_WRITE) != 0)
        EV_SET(&changes[count++], (uintptr_t)object_handle, EVFILT_WRITE, flags, 0, 0, (void *)(uintptr_t)token);
    if (count == 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int result = kevent(descriptor, changes, count, NULL, 0, NULL);
    hl_host_result output = result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
    pthread_mutex_unlock(&host->lock);
    return output;
}

static int hl_macos_event_submit_timer(int descriptor, uint64_t token, uint64_t delay_ns) {
    struct kevent change;
    if (delay_ns == 0) delay_ns = 1;
    if (delay_ns > INT64_MAX) delay_ns = INT64_MAX;
    // NOTE_CRITICAL: opt out of macOS power-aware timer coalescing. These kqueue timers back guest
    // timerfd/POSIX-timer/itimer expiries, which on Linux are hrtimers with no coalescing slop; the default
    // leeway lets the kernel slide an expiry by tens of ms (much more under a background QoS band), which
    // reorders or merges guest expiries that Linux keeps distinct.
    EV_SET(&change, (uintptr_t)token, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL,
           (intptr_t)delay_ns, (void *)(uintptr_t)token);
    return kevent(descriptor, &change, 1, NULL, 0, NULL);
}

static hl_host_result hl_macos_event_arm_timer(void *context, hl_host_handle pollset, uint64_t token,
                                               uint64_t deadline_ns, uint64_t interval_ns) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_timer *timer;
    uint32_t index;
    uint64_t now;
    int result;
    if (token == 0 || deadline_ns == HL_HOST_DEADLINE_INFINITE)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    if (event == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    timer = hl_macos_event_timer(event, token);
    if (timer == NULL) {
        for (index = 0; index < event->timer_capacity; ++index)
            if (!event->timers[index].active) {
                timer = &event->timers[index];
                break;
            }
    }
    if (timer == NULL) {
        uint32_t capacity =
            hl_macos_grow_capacity(event->timer_capacity, HL_MACOS_TIMER_CAPACITY, sizeof(*event->timers));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_timer *grown = realloc(event->timers, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        }
        memset(grown + event->timer_capacity, 0, (size_t)(capacity - event->timer_capacity) * sizeof(*grown));
        timer = &grown[event->timer_capacity];
        event->timers = grown;
        event->timer_capacity = capacity;
    }
    now = hl_macos_monotonic_value();
    result = hl_macos_event_submit_timer(event->descriptor, token, deadline_ns > now ? deadline_ns - now : 1);
    if (result == 0) {
        timer->active = 1;
        timer->token = token;
        timer->interval_ns = interval_ns;
    }
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_event_disarm_timer(void *context, hl_host_handle pollset, uint64_t token) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    hl_macos_timer *timer;
    struct kevent change;
    if (token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    timer = event == NULL ? NULL : hl_macos_event_timer(event, token);
    if (timer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    EV_SET(&change, (uintptr_t)token, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
    (void)kevent(event->descriptor, &change, 1, NULL, 0, NULL);
    memset(timer, 0, sizeof(*timer));
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_event_wait(void *context, hl_host_handle pollset, hl_host_event_record *events,
                                          size_t event_capacity, uint64_t deadline_ns) {
    hl_host_macos *host = context;
    struct kevent native[64];
    struct timespec timeout;
    struct timespec *timeout_pointer = NULL;
    hl_macos_event *event;
    int descriptor;
    int count;
    int index;
    size_t output_count = 0;
    if (events == NULL || event_capacity == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (event_capacity > HL_ARRAY_COUNT(native)) event_capacity = HL_ARRAY_COUNT(native);
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    descriptor = event == NULL ? -1 : event->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (deadline_ns != HL_HOST_DEADLINE_INFINITE) {
        uint64_t now = hl_macos_monotonic_value();
        uint64_t remaining = deadline_ns > now ? deadline_ns - now : 0;
        timeout.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
        timeout.tv_nsec = (long)(remaining % UINT64_C(1000000000));
        timeout_pointer = &timeout;
    }
    count = kevent(descriptor, NULL, 0, native, (int)event_capacity, timeout_pointer);
    if (count < 0) return hl_macos_errno();
    for (index = 0; index < count; ++index) {
        uint32_t readiness = 0;
        uint64_t token = (uint64_t)(uintptr_t)native[index].udata;
        if (native[index].filter == EVFILT_USER) continue;
        if (native[index].filter == EVFILT_READ) readiness |= HL_HOST_READY_READ;
        if (native[index].filter == EVFILT_VNODE) readiness |= HL_HOST_READY_READ;
        if (native[index].filter == EVFILT_WRITE) readiness |= HL_HOST_READY_WRITE;
        if ((native[index].flags & EV_ERROR) != 0) readiness |= HL_HOST_READY_ERROR;
        if ((native[index].flags & EV_EOF) != 0) readiness |= HL_HOST_READY_HANGUP;
        if (native[index].filter == EVFILT_VNODE) {
            pthread_mutex_lock(&host->lock);
            hl_macos_watch_note(host, native[index].ident, native[index].fflags);
            pthread_mutex_unlock(&host->lock);
        }
        if (native[index].filter == EVFILT_TIMER) {
            readiness |= HL_HOST_READY_TIMER;
            token = (uint64_t)native[index].ident;
            pthread_mutex_lock(&host->lock);
            event = hl_macos_event_lookup(host, pollset);
            hl_macos_timer *timer = event == NULL ? NULL : hl_macos_event_timer(event, token);
            if (timer != NULL && timer->interval_ns != 0)
                (void)hl_macos_event_submit_timer(event->descriptor, token, timer->interval_ns);
            else if (timer != NULL)
                memset(timer, 0, sizeof(*timer));
            pthread_mutex_unlock(&host->lock);
        }
        events[output_count++] = (hl_host_event_record){token, readiness, 0};
    }
    return hl_macos_result(HL_STATUS_OK, output_count, 0);
}

static hl_host_result hl_macos_event_wake(void *context, hl_host_handle pollset) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    struct kevent trigger;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    descriptor = event == NULL ? -1 : event->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    EV_SET(&trigger, 0, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    return kevent(descriptor, &trigger, 1, NULL, 0, NULL) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_event_close(void *context, hl_host_handle pollset) {
    hl_host_macos *host = context;
    hl_macos_event *event;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    event = hl_macos_event_lookup(host, pollset);
    if (event == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = event->descriptor;
    event->active = 0;
    event->descriptor = -1;
    free(event->timers);
    event->timers = NULL;
    event->timer_capacity = 0;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

