static hl_macos_event *hl_macos_event_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_EVENT, host->event_capacity, &index) ||
        !host->events[index].active || host->events[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->events[index];
}

static hl_macos_counter *hl_macos_counter_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_COUNTER, host->counter_capacity, &index) ||
        !host->counters[index].active || host->counters[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->counters[index];
}

static hl_macos_directory *hl_macos_directory_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_DIRECTORY, host->directory_capacity, &index) ||
        !host->directories[index].active || host->directories[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->directories[index];
}

static hl_host_result hl_macos_directory_register(hl_host_macos *host, hl_macos_directory_object *object) {
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    pthread_mutex_lock(&host->lock);
    for (uint32_t index = 0; index < host->directory_capacity; ++index) {
        hl_macos_directory *directory = &host->directories[index];
        if (directory->active) continue;
        directory->generation++;
        if (directory->generation == 0) directory->generation = 1;
        directory->active = 1;
        directory->object = object;
        object->references++;
        handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, directory->generation);
        break;
    }
    if (handle == HL_HOST_HANDLE_INVALID) {
        uint32_t capacity =
            hl_macos_grow_capacity(host->directory_capacity, HL_MACOS_DIRECTORY_CAPACITY, sizeof(*host->directories));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_directory *grown = realloc(host->directories, (size_t)capacity * sizeof(*grown));
        if (grown != NULL) {
            memset(grown + host->directory_capacity, 0, (size_t)(capacity - host->directory_capacity) * sizeof(*grown));
            uint32_t index = host->directory_capacity;
            host->directories = grown;
            host->directory_capacity = capacity;
            grown[index].generation = 1;
            grown[index].active = 1;
            grown[index].object = object;
            object->references++;
            handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, 1);
        }
    }
    pthread_mutex_unlock(&host->lock);
    return handle == HL_HOST_HANDLE_INVALID ? hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0)
                                            : hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_directory_create(void *context) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = calloc(1, sizeof(*object));
    if (object == NULL) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    object->descriptor = kqueue();
    if (object->descriptor < 0) {
        free(object);
        return hl_macos_errno();
    }
    (void)fcntl(object->descriptor, F_SETFD, FD_CLOEXEC);
    int adopted = hl_host_process_fd_private_adopt(object->descriptor);
    if (adopted < 0) {
        close(object->descriptor);
        free(object);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    object->descriptor = adopted;
    hl_host_result result = hl_macos_directory_register(host, object);
    if (result.status != HL_STATUS_OK) {
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        free(object);
    }
    return result;
}

static hl_macos_directory_object *hl_macos_directory_object_get(hl_host_macos *host, hl_host_handle handle) {
    pthread_mutex_lock(&host->lock);
    hl_macos_directory *directory = hl_macos_directory_lookup(host, handle);
    hl_macos_directory_object *object = directory == NULL ? NULL : directory->object;
    pthread_mutex_unlock(&host->lock);
    return object;
}

static uint32_t hl_macos_directory_native(uint32_t interests) {
    uint32_t flags = 0;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) flags |= NOTE_ATTRIB;
    if ((interests & HL_HOST_DIRECTORY_MODIFY) != 0) flags |= NOTE_WRITE | NOTE_EXTEND;
    if ((interests & (HL_HOST_DIRECTORY_CREATE | HL_HOST_DIRECTORY_DELETE)) != 0) flags |= NOTE_WRITE | NOTE_LINK;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) flags |= NOTE_RENAME;
    if ((interests & HL_HOST_DIRECTORY_ATTRIB) != 0) flags |= NOTE_ATTRIB;
    return flags == 0 ? NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND | NOTE_LINK : flags;
}

static void hl_macos_directory_descriptor_close(int descriptor) {
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
}

static hl_host_result hl_macos_directory_add(void *context, hl_host_handle instance, hl_host_handle file,
                                             uint64_t token, uint32_t interests) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = hl_macos_directory_object_get(host, instance);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file_entry = hl_macos_file_lookup(host, file);
    int descriptor = file_entry == NULL ? -1 : fcntl(file_entry->descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) {
            close(descriptor);
            descriptor = -1;
        } else {
            descriptor = adopted;
        }
    }
    if (object == NULL || descriptor < 0 || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (watch->active && watch->token != token) continue;
        if (!watch->active || watch->token == token) {
            struct kevent change;
            uint16_t flags =
                (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
            EV_SET(&change, descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0,
                   (void *)(uintptr_t)token);
            if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) {
                hl_macos_directory_descriptor_close(descriptor);
                return hl_macos_errno();
            }
            if (watch->active) hl_macos_directory_descriptor_close(watch->descriptor);
            *watch = (hl_macos_directory_watch){token, interests, descriptor, 1};
            return hl_macos_result(HL_STATUS_OK, 0, 0);
        }
    }
    uint32_t capacity = object->watch_capacity == 0 ? HL_MACOS_DIRECTORY_WATCH_CAPACITY : object->watch_capacity * 2u;
    hl_macos_directory_watch *grown = realloc(object->watches, (size_t)capacity * sizeof(*grown));
    if (grown == NULL) {
        hl_macos_directory_descriptor_close(descriptor);
        return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    memset(grown + object->watch_capacity, 0, (size_t)(capacity - object->watch_capacity) * sizeof(*grown));
    uint32_t index = object->watch_capacity;
    object->watches = grown;
    object->watch_capacity = capacity;
    struct kevent change;
    uint16_t flags = (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
    EV_SET(&change, descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0, (void *)(uintptr_t)token);
    if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) {
        hl_macos_directory_descriptor_close(descriptor);
        return hl_macos_errno();
    }
    object->watches[index] = (hl_macos_directory_watch){token, interests, descriptor, 1};
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_directory_modify(void *context, hl_host_handle instance, uint64_t token,
                                                uint32_t interests) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    if (object == NULL || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (!watch->active || watch->token != token) continue;
        struct kevent change;
        uint16_t flags =
            (uint16_t)(EV_ADD | EV_CLEAR | ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
        EV_SET(&change, watch->descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(interests), 0,
               (void *)(uintptr_t)token);
        if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0) return hl_macos_errno();
        watch->interests = interests;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
}

static hl_host_result hl_macos_directory_remove(void *context, hl_host_handle instance, uint64_t token) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    if (object == NULL || token == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (uint32_t index = 0; index < object->watch_capacity; ++index) {
        hl_macos_directory_watch *watch = &object->watches[index];
        if (!watch->active || watch->token != token) continue;
        struct kevent change;
        EV_SET(&change, watch->descriptor, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        if (kevent(object->descriptor, &change, 1, NULL, 0, NULL) != 0 && errno != ENOENT) return hl_macos_errno();
        watch->active = 0;
        hl_macos_directory_descriptor_close(watch->descriptor);
        watch->descriptor = -1;
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    return hl_macos_result(HL_STATUS_NOT_FOUND, 0, 0);
}

static uint32_t hl_macos_directory_changes(uint32_t flags, uint32_t interests) {
    uint32_t changes = 0;
    if ((flags & (NOTE_WRITE | NOTE_EXTEND | NOTE_LINK)) != 0)
        changes |= interests & (HL_HOST_DIRECTORY_MODIFY | HL_HOST_DIRECTORY_CREATE | HL_HOST_DIRECTORY_DELETE);
    if ((flags & NOTE_ATTRIB) != 0) changes |= HL_HOST_DIRECTORY_ATTRIB;
    if ((flags & NOTE_DELETE) != 0) changes |= HL_HOST_DIRECTORY_DELETE;
    if ((flags & NOTE_RENAME) != 0) changes |= HL_HOST_DIRECTORY_RENAME;
    return changes;
}

static hl_host_result hl_macos_directory_read(void *context, hl_host_handle instance, hl_host_directory_record *records,
                                              uint32_t capacity) {
    hl_macos_directory_object *object = hl_macos_directory_object_get(context, instance);
    struct kevent native[64];
    struct timespec zero = {0, 0};
    if (object == NULL || records == NULL || capacity == 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (capacity > 64) capacity = 64;
    int count = kevent(object->descriptor, NULL, 0, native, (int)capacity, &zero);
    if (count < 0) return hl_macos_errno();
    if (count == 0) return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    for (int index = 0; index < count; ++index) {
        uint64_t token = (uint64_t)(uintptr_t)native[index].udata;
        uint32_t interests = 0;
        for (uint32_t watch_index = 0; watch_index < object->watch_capacity; ++watch_index) {
            hl_macos_directory_watch *watch = &object->watches[watch_index];
            if (watch->active && watch->token == token) interests = watch->interests;
            if (watch->active && watch->token == token && (watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0) {
                watch->active = 0;
                hl_macos_directory_descriptor_close(watch->descriptor);
                watch->descriptor = -1;
                interests |= HL_HOST_DIRECTORY_IGNORED;
            }
        }
        records[index] =
            (hl_host_directory_record){token, hl_macos_directory_changes(native[index].fflags, interests), 0};
        if ((interests & HL_HOST_DIRECTORY_IGNORED) != 0) records[index].changes |= HL_HOST_DIRECTORY_IGNORED;
    }
    return hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_directory_duplicate(void *context, hl_host_handle instance) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object = hl_macos_directory_object_get(host, instance);
    if (object == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_directory_register(host, object);
}

static hl_host_result hl_macos_directory_close(void *context, hl_host_handle instance) {
    hl_host_macos *host = context;
    hl_macos_directory_object *object;
    int final = 0;
    pthread_mutex_lock(&host->lock);
    hl_macos_directory *directory = hl_macos_directory_lookup(host, instance);
    if (directory == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = directory->object;
    directory->active = 0;
    directory->object = NULL;
    final = --object->references == 0;
    pthread_mutex_unlock(&host->lock);
    if (final) {
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        for (uint32_t index = 0; index < object->watch_capacity; ++index)
            if (object->watches[index].active) hl_macos_directory_descriptor_close(object->watches[index].descriptor);
        free(object->watches);
        free(object);
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

