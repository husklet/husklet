static uint32_t hl_linux_directory_mask(uint32_t interests) {
    uint32_t mask = 0;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) mask |= IN_ACCESS | IN_OPEN | IN_CLOSE;
    if ((interests & HL_HOST_DIRECTORY_MODIFY) != 0) mask |= IN_MODIFY | IN_CLOSE_WRITE;
    if ((interests & HL_HOST_DIRECTORY_CREATE) != 0) mask |= IN_CREATE;
    if ((interests & HL_HOST_DIRECTORY_DELETE) != 0) mask |= IN_DELETE | IN_DELETE_SELF;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) mask |= IN_MOVED_FROM | IN_MOVED_TO | IN_MOVE_SELF;
    if ((interests & HL_HOST_DIRECTORY_ATTRIB) != 0) mask |= IN_ATTRIB;
    if ((interests & HL_HOST_DIRECTORY_ONESHOT) != 0) mask |= IN_ONESHOT;
    return mask;
}

static hl_linux_directory_watch *hl_linux_directory_watch_for_token(hl_linux_directory_object *object, uint64_t token) {
    uint32_t index;
    for (index = 0; index < object->watch_capacity; ++index)
        if (object->watches[index].active != 0 && object->watches[index].token == token) return &object->watches[index];
    return NULL;
}

static hl_linux_directory_watch *hl_linux_directory_watch_for_id(hl_linux_directory_object *object, int watch) {
    uint32_t index;
    for (index = 0; index < object->watch_capacity; ++index)
        if (object->watches[index].watch == watch) return &object->watches[index];
    return NULL;
}

static hl_host_result hl_linux_directory_create(void *context) {
    hl_host_linux *host = context;
    hl_linux_directory_object *object = calloc(1, sizeof(*object));
    hl_host_result result;
    uint32_t index;
    int descriptor;
    if (object == NULL) return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    descriptor = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (descriptor < 0) {
        free(object);
        return hl_linux_errno_result();
    }
    object->references = 1;
    object->watch_capacity = HL_LINUX_DIRECTORY_WATCHES;
    object->watches = calloc(object->watch_capacity, sizeof(*object->watches));
    if (object->watches == NULL) {
        close(descriptor);
        free(object);
        return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    for (index = 0; index < object->watch_capacity; ++index) {
        object->watches[index].watch = -1;
    }
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_DIRECTORY, descriptor, object, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        free(object->watches);
        free(object);
    }
    return result;
}

static hl_host_result hl_linux_directory_add(void *context, hl_host_handle instance, hl_host_handle file,
                                             uint64_t token, uint32_t interests) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *instance_entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot = NULL;
    char path[64];
    int file_descriptor;
    int watch;
    uint32_t index;
    uint32_t valid = UINT32_C(0x8000007f);
    if (token == 0 || (interests & ~valid) != 0 || (interests & ~HL_HOST_DIRECTORY_ONESHOT) == 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    instance_entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = instance_entry == NULL ? NULL : instance_entry->address;
    file_descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (object != NULL && hl_linux_directory_watch_for_token(object, token) == NULL) {
        for (index = 0; index < object->watch_capacity; ++index)
            if (object->watches[index].watch < 0) {
                slot = &object->watches[index];
                break;
            }
        if (slot == NULL) {
            uint32_t capacity;
            hl_linux_directory_watch *grown = hl_linux_grow_capacity(object->watch_capacity, HL_LINUX_DIRECTORY_WATCHES,
                                                                     sizeof(*object->watches), &capacity) == 0
                                                  ? realloc(object->watches, (size_t)capacity * sizeof(*grown))
                                                  : NULL;
            if (grown != NULL) {
                for (index = object->watch_capacity; index < capacity; ++index) {
                    grown[index] = (hl_linux_directory_watch){0};
                    grown[index].watch = -1;
                }
                slot = &grown[object->watch_capacity];
                object->watches = grown;
                object->watch_capacity = capacity;
            }
        }
    }
    if (instance_entry == NULL || slot == NULL || file_descriptor < 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(slot == NULL && object != NULL ? HL_STATUS_RESOURCE_LIMIT : HL_STATUS_INVALID_ARGUMENT,
                               0, 0);
    }
    snprintf(path, sizeof(path), "/proc/self/fd/%d", file_descriptor);
    watch = inotify_add_watch(instance_entry->descriptor, path, hl_linux_directory_mask(UINT32_C(0x7f)));
    if (watch >= 0) *slot = (hl_linux_directory_watch){watch, token, interests, 1};
    pthread_mutex_unlock(&host->lock);
    if (watch < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_directory_modify(void *context, hl_host_handle instance, uint64_t token,
                                                uint32_t interests) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot;
    uint32_t valid = UINT32_C(0x8000007f);
    if (token == 0 || (interests & ~valid) != 0 || (interests & ~HL_HOST_DIRECTORY_ONESHOT) == 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    slot = object == NULL ? NULL : hl_linux_directory_watch_for_token(object, token);
    if (slot == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    slot->interests = interests;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_directory_remove(void *context, hl_host_handle instance, uint64_t token) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    hl_linux_directory_watch *slot;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    slot = object == NULL ? NULL : hl_linux_directory_watch_for_token(object, token);
    if (slot == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_NOT_FOUND, 0, 0);
    }
    result = inotify_rm_watch(entry->descriptor, slot->watch);
    slot->active = 0;
    slot->watch = -1; /* free the slot so the `watch < 0` scan in add_watch reuses it */
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static uint32_t hl_linux_directory_changes(uint32_t mask) {
    uint32_t changes = 0;
    if ((mask & (IN_ACCESS | IN_OPEN | IN_CLOSE)) != 0) changes |= HL_HOST_DIRECTORY_ACCESS;
    if ((mask & (IN_MODIFY | IN_CLOSE_WRITE)) != 0) changes |= HL_HOST_DIRECTORY_MODIFY;
    if ((mask & IN_CREATE) != 0) changes |= HL_HOST_DIRECTORY_CREATE;
    if ((mask & (IN_DELETE | IN_DELETE_SELF)) != 0) changes |= HL_HOST_DIRECTORY_DELETE;
    if ((mask & (IN_MOVED_FROM | IN_MOVED_TO | IN_MOVE_SELF)) != 0) changes |= HL_HOST_DIRECTORY_RENAME;
    if ((mask & IN_ATTRIB) != 0) changes |= HL_HOST_DIRECTORY_ATTRIB;
    if ((mask & (IN_IGNORED | IN_Q_OVERFLOW)) != 0) changes |= HL_HOST_DIRECTORY_IGNORED;
    return changes;
}

static int hl_linux_directory_append(hl_linux_directory_object *object, hl_host_directory_record record) {
    if (object->pending_count == object->pending_capacity) {
        uint32_t capacity;
        hl_host_directory_record *pending =
            hl_linux_grow_capacity(object->pending_capacity, 32u, sizeof(*object->pending), &capacity) == 0
                ? realloc(object->pending, (size_t)capacity * sizeof(*pending))
                : NULL;
        if (pending == NULL) return -1;
        object->pending = pending;
        object->pending_capacity = capacity;
    }
    object->pending[object->pending_count++] = record;
    return 0;
}

static hl_host_result hl_linux_directory_read(void *context, hl_host_handle instance, hl_host_directory_record *records,
                                              uint32_t capacity) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    _Alignas(struct inotify_event) char buffer[16384];
    ssize_t size;
    size_t offset;
    uint32_t count;
    if (records == NULL || capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    if (object == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (object->pending_count == 0) {
        size = read(entry->descriptor, buffer, sizeof(buffer));
        if (size < 0) {
            pthread_mutex_unlock(&host->lock);
            return hl_linux_errno_result();
        }
        for (offset = 0; offset < (size_t)size;) {
            const struct inotify_event *event = (const struct inotify_event *)(buffer + offset);
            hl_linux_directory_watch *watch = hl_linux_directory_watch_for_id(object, event->wd);
            uint64_t token = watch == NULL ? 0 : watch->token;
            uint32_t changes = hl_linux_directory_changes(event->mask);
            int oneshot = 0;
            if (watch != NULL) {
                changes = (changes & watch->interests) | (changes & HL_HOST_DIRECTORY_IGNORED);
                oneshot = (watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0 &&
                          (changes & ~(uint32_t)HL_HOST_DIRECTORY_IGNORED) != 0;
                if (oneshot) changes |= HL_HOST_DIRECTORY_IGNORED;
            } else {
                changes = 0;
            }
            if (changes != 0 && hl_linux_directory_append(object, (hl_host_directory_record){token, changes, 0}) != 0) {
                pthread_mutex_unlock(&host->lock);
                return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
            }
            if (oneshot) {
                (void)inotify_rm_watch(entry->descriptor, watch->watch);
                *watch = (hl_linux_directory_watch){-1, 0, 0, 0};
            } else if ((event->mask & IN_IGNORED) != 0 && watch != NULL) {
                *watch = (hl_linux_directory_watch){-1, 0, 0, 0};
            }
            offset += sizeof(*event) + event->len;
        }
    }
    count = capacity < object->pending_count ? capacity : object->pending_count;
    if (count != 0) memcpy(records, object->pending, count * sizeof(*records));
    object->pending_count -= count;
    if (object->pending_count != 0)
        memmove(object->pending, object->pending + count, object->pending_count * sizeof(*object->pending));
    pthread_mutex_unlock(&host->lock);
    return count == 0 ? hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0) : hl_linux_result(HL_STATUS_OK, count, 0);
}

static hl_host_result hl_linux_directory_duplicate(void *context, hl_host_handle instance) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    int descriptor;
    hl_host_result result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    object = entry == NULL ? NULL : entry->address;
    descriptor = entry == NULL ? -1 : fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor >= 0) object->references++;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_DIRECTORY, descriptor, object, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_lock(&host->lock);
        object->references--;
        pthread_mutex_unlock(&host->lock);
        close(descriptor);
    }
    return result;
}

static hl_host_result hl_linux_directory_close(void *context, hl_host_handle instance) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_directory_object *object;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, instance, HL_LINUX_HANDLE_DIRECTORY);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = entry->descriptor;
    object = entry->address;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->address = NULL;
    if (--object->references == 0) {
        free(object->pending);
        free(object->watches);
        free(object);
    }
    pthread_mutex_unlock(&host->lock);
    // The dir fd was privatized (relocated + registered) by hl_linux_allocate_handle; drop its private-registry
    // cell before closing, exactly as hl_linux_close_descriptor_kind does -- otherwise the cell leaks and, once
    // the OS reuses this fd number, hl_host_process_fd_private_is() misclassifies a guest fd as engine-private.
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

// `descriptor` is the live watched fd held by the owning handle entry (entry->wake_descriptor). The watch
// struct must NOT cache the fd it was opened with: hl_linux_allocate_handle privatizes (relocates + closes)
// the passed descriptor via hl_host_process_fd_private_adopt, so any pre-adoption copy is a stale, closed fd.
static int hl_linux_watch_refresh(hl_linux_watch *watch, int descriptor) {
    struct stat status;
    if (fstat(descriptor, &status) != 0) return -1;
    uint64_t modified = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    uint64_t changed = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    uint64_t size = status.st_size < 0 ? 0 : (uint64_t)status.st_size;
    uint32_t changes = 0;
    if ((uint64_t)status.st_dev != watch->record.stable_device ||
        (uint64_t)status.st_ino != watch->record.stable_object)
        changes |= HL_HOST_WATCH_IDENTITY;
    if (size != watch->record.size) changes |= HL_HOST_WATCH_SIZE;
    if (modified != watch->modified_ns || changed != watch->changed_ns) changes |= HL_HOST_WATCH_DATA;
    if (status.st_nlink == 0 && watch->links != 0) changes |= HL_HOST_WATCH_DELETED;
    if (changes != 0) {
        if (watch->record.generation != watch->delivered_generation) changes |= watch->record.changes;
        watch->record.generation++;
        if (watch->record.generation == 0) watch->record.generation = 1;
        watch->record.stable_device = (uint64_t)status.st_dev;
        watch->record.stable_object = (uint64_t)status.st_ino;
        watch->record.size = size;
        watch->record.changes = changes;
        watch->modified_ns = modified;
        watch->changed_ns = changed;
        watch->links = status.st_nlink;
    }
    return 0;
}

static hl_host_result hl_linux_watch_open(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int source, watched = -1, notify = -1, watch_id = -1;
    pthread_mutex_lock(&host->lock);
    source = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    if (source >= 0) watched = fcntl(source, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (watched < 0) return source < 0 ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_linux_errno_result();
    notify = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    char path[64];
    if (notify >= 0 && snprintf(path, sizeof path, "/proc/self/fd/%d", watched) < (int)sizeof path)
        watch_id = inotify_add_watch(notify, path, IN_MODIFY | IN_ATTRIB | IN_MOVE_SELF | IN_DELETE_SELF);
    struct stat status;
    if (notify < 0 || watch_id < 0 || fstat(watched, &status) != 0) {
        hl_host_result error = hl_linux_errno_result();
        if (notify >= 0) close(notify);
        close(watched);
        return error;
    }
    hl_linux_watch *watch = calloc(1, sizeof(*watch));
    if (watch == NULL) {
        close(notify);
        close(watched);
        return hl_linux_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    watch->watch_id = watch_id;
    watch->record = (hl_host_watch_record){
        1, (uint64_t)status.st_dev, (uint64_t)status.st_ino, status.st_size < 0 ? 0 : (uint64_t)status.st_size, 0, 0};
    watch->delivered_generation = 1;
    watch->modified_ns = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    watch->changed_ns = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    watch->links = status.st_nlink;
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_WATCH, notify, watch, NULL, 0, watched);
    if (result.status != HL_STATUS_OK) {
        close(notify);
        close(watched);
        free(watch);
    }
    return result;
}

static hl_host_result hl_linux_watch_query(void *context, hl_host_handle handle, hl_host_watch_record *record) {
    hl_host_linux *host = context;
    if (record == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    hl_linux_watch *watch = entry == NULL ? NULL : entry->address;
    int result = watch == NULL ? -2 : hl_linux_watch_refresh(watch, entry->wake_descriptor);
    if (result == 0) *record = watch->record;
    pthread_mutex_unlock(&host->lock);
    if (result == -2) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_watch_drain(void *context, hl_host_handle handle, hl_host_watch_record *records,
                                           size_t capacity) {
    hl_host_linux *host = context;
    char buffer[4096];
    if (records == NULL || capacity == 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    hl_linux_watch *watch = entry == NULL ? NULL : entry->address;
    if (watch == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    uint32_t native_changes = 0;
    for (;;) {
        ssize_t count = read(entry->descriptor, buffer, sizeof buffer);
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) break;
        if (count <= 0) break;
        for (size_t offset = 0; offset < (size_t)count;) {
            struct inotify_event *event = (struct inotify_event *)(void *)(buffer + offset);
            native_changes |= event->mask;
            offset += sizeof(*event) + event->len;
        }
    }
    if (native_changes != 0) {
        watch->record.generation++;
        if (watch->record.generation == 0) watch->record.generation = 1;
        watch->record.changes = 0;
        if (native_changes & (IN_MODIFY | IN_ATTRIB)) watch->record.changes |= HL_HOST_WATCH_DATA;
        if (native_changes & IN_MOVE_SELF) watch->record.changes |= HL_HOST_WATCH_IDENTITY;
        if (native_changes & (IN_DELETE_SELF | IN_IGNORED)) watch->record.changes |= HL_HOST_WATCH_DELETED;
    }
    int refreshed = hl_linux_watch_refresh(watch, entry->wake_descriptor);
    int available = refreshed == 0 && watch->record.generation != watch->delivered_generation;
    if (available) {
        records[0] = watch->record;
        watch->delivered_generation = watch->record.generation;
    }
    pthread_mutex_unlock(&host->lock);
    if (refreshed < 0) return hl_linux_errno_result();
    return available ? hl_linux_result(HL_STATUS_OK, 1, 0) : hl_linux_result(HL_STATUS_WOULD_BLOCK, 0, 0);
}

static hl_host_result hl_linux_watch_close(void *context, hl_host_handle handle) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)handle;
    if (low == 0 || low - 1u >= host->handle_capacity) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, handle, HL_LINUX_HANDLE_WATCH);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    int notify = entry->descriptor, watched = entry->wake_descriptor;
    hl_linux_watch *watch = entry->address;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->wake_descriptor = -1;
    entry->address = NULL;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(notify);
    hl_host_process_fd_private_remove(watched);
    close(notify);
    close(watched);
    free(watch);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

