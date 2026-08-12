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

static hl_macos_transfer *hl_macos_transfer_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_TRANSFER, host->transfer_capacity, &index) ||
        !host->transfers[index].active || host->transfers[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->transfers[index];
}

static hl_host_result hl_macos_transfer_register(hl_host_macos *host, int descriptor) {
    uint32_t index;
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    descriptor = adopted;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->transfer_capacity; ++index) {
        hl_macos_transfer *transfer = &host->transfers[index];
        if (transfer->active) continue;
        transfer->generation++;
        if (transfer->generation == 0) transfer->generation = 1;
        transfer->active = 1;
        transfer->descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, transfer->generation), 0);
    }
    uint32_t capacity =
        hl_macos_grow_capacity(host->transfer_capacity, HL_MACOS_TRANSFER_CAPACITY, sizeof(*host->transfers));
    if (capacity == 0) {
        pthread_mutex_unlock(&host->lock);
        hl_host_process_fd_private_remove(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    hl_macos_transfer *grown = realloc(host->transfers, (size_t)capacity * sizeof(*grown));
    if (grown != NULL) {
        memset(grown + host->transfer_capacity, 0, (size_t)(capacity - host->transfer_capacity) * sizeof(*grown));
        index = host->transfer_capacity;
        host->transfers = grown;
        host->transfer_capacity = capacity;
        grown[index].generation = 1;
        grown[index].active = 1;
        grown[index].descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, 1), 0);
    }
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

typedef struct hl_macos_transfer_wire {
    uint32_t data_size;
    uint32_t attachment_count;
    uint32_t flags[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t rights[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint8_t data[HL_HOST_TRANSFER_MAX_DATA];
} hl_macos_transfer_wire;

static hl_host_result hl_macos_counter_register(hl_host_macos *host, hl_macos_counter_object *object, uint32_t rights);
static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle);

static hl_host_result hl_macos_transfer_channel_pair(void *context) {
    hl_host_macos *host = context;
    int pair[2];
    hl_host_result first;
    hl_host_result second;
    /* Darwin does not provide AF_UNIX SOCK_SEQPACKET; datagrams retain message boundaries. */
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) return hl_macos_errno();
    (void)fcntl(pair[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(pair[1], F_SETFD, FD_CLOEXEC);
    first = hl_macos_transfer_register(host, pair[0]);
    if (first.status != HL_STATUS_OK) {
        close(pair[0]);
        close(pair[1]);
        return first;
    }
    second = hl_macos_transfer_register(host, pair[1]);
    if (second.status != HL_STATUS_OK) {
        close(pair[1]);
        (void)hl_macos_transfer_close(host, first.value);
        return second;
    }
    return hl_macos_result(HL_STATUS_OK, first.value, second.value);
}

static hl_host_result hl_macos_transfer_send(void *context, hl_host_handle channel, hl_host_const_bytes data,
                                             const hl_host_transfer_attachment *attachments, uint32_t count) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire = {0};
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int descriptors[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    if (data.size > HL_HOST_TRANSFER_MAX_DATA || (data.size != 0 && data.data == NULL) ||
        count > HL_HOST_TRANSFER_MAX_ATTACHMENTS || (count != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    for (index = 0; index < count && channel_fd >= 0; ++index) {
        hl_macos_counter *counter = hl_macos_counter_lookup(host, attachments[index].object);
        uint32_t valid =
            HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
        if (counter == NULL || attachments[index].kind != HL_HOST_TRANSFER_KIND_COUNTER ||
            (attachments[index].rights & ~valid) != 0 ||
            (attachments[index].rights & counter->rights) != attachments[index].rights) {
            channel_fd = -1;
            break;
        }
        descriptors[index * 3u] = counter->object->backing;
        descriptors[index * 3u + 1u] = counter->object->readable;
        descriptors[index * 3u + 2u] = counter->object->signal;
        wire.flags[index] = counter->object->shared->flags;
        wire.rights[index] = attachments[index].rights;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    wire.data_size = (uint32_t)data.size;
    wire.attachment_count = count;
    if (data.size != 0) memcpy(wire.data, data.data, data.size);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (count != 0) {
        struct cmsghdr *header;
        size_t descriptor_bytes = sizeof(int) * count * 3u;
        message.msg_control = control;
        message.msg_controllen = (socklen_t)CMSG_SPACE(descriptor_bytes);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = (socklen_t)CMSG_LEN(descriptor_bytes);
        memcpy(CMSG_DATA(header), descriptors, descriptor_bytes);
    }
    return sendmsg(channel_fd, &message, 0) == (ssize_t)sizeof(wire) ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                     : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_receive(void *context, hl_host_handle channel, hl_host_bytes data,
                                                hl_host_transfer_attachment *attachments, uint32_t capacity) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire;
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int received[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    ssize_t bytes;
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0 || (data.size != 0 && data.data == NULL) || (capacity != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    bytes = recv(channel_fd, &wire, sizeof(wire), MSG_PEEK);
    if (bytes < 0) return hl_macos_errno();
    if (bytes != (ssize_t)sizeof(wire) || wire.data_size > HL_HOST_TRANSFER_MAX_DATA ||
        wire.attachment_count > HL_HOST_TRANSFER_MAX_ATTACHMENTS)
        return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.data_size > data.size || wire.attachment_count > capacity)
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, wire.data_size, wire.attachment_count);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    bytes = recvmsg(channel_fd, &message, 0);
    if (bytes != (ssize_t)sizeof(wire)) return bytes < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.attachment_count != 0) {
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        size_t descriptor_bytes = sizeof(int) * wire.attachment_count * 3u;
        if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
            header->cmsg_len != CMSG_LEN(descriptor_bytes))
            return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
        memcpy(received, CMSG_DATA(header), descriptor_bytes);
    }
    for (index = 0; index < wire.attachment_count; ++index) {
        hl_macos_counter_object *object = calloc(1, sizeof(*object));
        hl_host_result installed;
        if (object == NULL) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        object->backing = received[index * 3u];
        object->readable = received[index * 3u + 1u];
        object->signal = received[index * 3u + 2u];
        object->shared = mmap(NULL, sizeof(*object->shared), PROT_READ | PROT_WRITE, MAP_SHARED, object->backing, 0);
        if (object->shared == MAP_FAILED) {
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return hl_macos_errno();
        }
        installed = hl_macos_counter_register(host, object, wire.rights[index]);
        if (installed.status != HL_STATUS_OK) {
            munmap(object->shared, sizeof(*object->shared));
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return installed;
        }
        attachments[index] =
            (hl_host_transfer_attachment){installed.value, HL_HOST_TRANSFER_KIND_COUNTER, wire.rights[index]};
    }
    if (wire.data_size != 0) memcpy(data.data, wire.data, wire.data_size);
    return hl_macos_result(HL_STATUS_OK, wire.data_size, wire.attachment_count);
}

static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    if (transfer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = transfer->descriptor;
    transfer->active = 0;
    transfer->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_duplicate(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    descriptor = transfer == NULL ? -1 : dup(transfer->descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_errno();
    {
        hl_host_result result = hl_macos_transfer_register(host, descriptor);
        if (result.status != HL_STATUS_OK) close(descriptor);
        return result;
    }
}

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

