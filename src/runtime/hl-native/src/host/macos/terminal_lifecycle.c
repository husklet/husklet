/* --- terminal ---------------------------------------------------------------------------
 *
 * The device, and only the device. Everything the guest's terminal vocabulary is made of -- the
 * attribute structure, its control characters, canonical buffering, line editing, signal
 * generation, minimum-and-timeout reads, output post-processing -- is above this and is not
 * reachable from here on purpose.
 *
 * The five mode bits are the abstract capabilities the contract names, mapped onto the five native
 * flags that carry the same meaning. Nothing else in the native attributes is read or written, so
 * a caller that sets a mode gets exactly the change it named and no adjacent policy it did not.
 */
static hl_host_result hl_macos_terminal_probe(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    int descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Answered from the terminal line discipline itself rather than from the object type, which is
     * the distinction the contract exists for: a character device is not a terminal. */
    return hl_macos_result(HL_STATUS_OK, isatty(descriptor) != 0 ? 1u : 0u, 0);
}

static hl_host_result hl_macos_terminal_get_mode(void *context, hl_host_handle handle, uint32_t *mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    uint32_t value = 0;
    if (mode == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
    if ((attributes.c_lflag & ICANON) == 0) value |= HL_HOST_TERMINAL_RAW_INPUT;
    if ((attributes.c_lflag & ECHO) != 0) value |= HL_HOST_TERMINAL_ECHO;
    if ((attributes.c_lflag & ISIG) != 0) value |= HL_HOST_TERMINAL_SIGNALS;
    if ((attributes.c_iflag & IXON) != 0) value |= HL_HOST_TERMINAL_FLOW_CONTROL;
    if ((attributes.c_oflag & OPOST) != 0) value |= HL_HOST_TERMINAL_OUTPUT_PROCESSING;
    *mode = value;
    return hl_macos_result(HL_STATUS_OK, value, 0);
}

static hl_host_result hl_macos_terminal_set_mode(void *context, hl_host_handle handle, uint32_t mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    if ((mode & ~(uint32_t)(HL_HOST_TERMINAL_RAW_INPUT | HL_HOST_TERMINAL_ECHO | HL_HOST_TERMINAL_SIGNALS |
                            HL_HOST_TERMINAL_FLOW_CONTROL | HL_HOST_TERMINAL_OUTPUT_PROCESSING)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
    if ((mode & HL_HOST_TERMINAL_RAW_INPUT) != 0)
        attributes.c_lflag &= (tcflag_t)~ICANON;
    else
        attributes.c_lflag |= (tcflag_t)ICANON;
    if ((mode & HL_HOST_TERMINAL_ECHO) != 0)
        attributes.c_lflag |= (tcflag_t)ECHO;
    else
        attributes.c_lflag &= (tcflag_t)~ECHO;
    if ((mode & HL_HOST_TERMINAL_SIGNALS) != 0)
        attributes.c_lflag |= (tcflag_t)ISIG;
    else
        attributes.c_lflag &= (tcflag_t)~ISIG;
    if ((mode & HL_HOST_TERMINAL_FLOW_CONTROL) != 0)
        attributes.c_iflag |= (tcflag_t)IXON;
    else
        attributes.c_iflag &= (tcflag_t)~IXON;
    if ((mode & HL_HOST_TERMINAL_OUTPUT_PROCESSING) != 0)
        attributes.c_oflag |= (tcflag_t)OPOST;
    else
        attributes.c_oflag &= (tcflag_t)~OPOST;
    /* Applied now rather than after the queued output drains: a caller turning echo off before
     * asking for a secret cannot be made to wait on a writer it does not control. */
    if (tcsetattr(descriptor, TCSANOW, &attributes) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, mode, 0);
}

static hl_host_result hl_macos_terminal_get_size(void *context, hl_host_handle handle, hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    if (ioctl(descriptor, TIOCGWINSZ, &window) != 0) return hl_macos_errno();
    size->columns = window.ws_col;
    size->rows = window.ws_row;
    size->pixel_width = window.ws_xpixel;
    size->pixel_height = window.ws_ypixel;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_set_size(void *context, hl_host_handle handle,
                                                 const hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL || size->columns > UINT16_MAX || size->rows > UINT16_MAX || size->pixel_width > UINT16_MAX ||
        size->pixel_height > UINT16_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    window.ws_col = (unsigned short)size->columns;
    window.ws_row = (unsigned short)size->rows;
    window.ws_xpixel = (unsigned short)size->pixel_width;
    window.ws_ypixel = (unsigned short)size->pixel_height;
    if (ioctl(descriptor, TIOCSWINSZ, &window) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_read(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output.data, output.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_terminal_write(void *context, hl_host_handle handle, hl_host_const_bytes input) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || input.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

/*
 * Typed absence, not an oversight. On this host a resize is delivered as a process-directed signal
 * that the engine's own signal machinery already owns end to end, so there is no separate object
 * for this to hand back -- and manufacturing one would mean installing a process-wide handler
 * underneath the layer that is already handling that signal, which is a worse bargain than saying
 * so. The operation exists in the contract for a host where the resize arrives in the input stream
 * instead, where waiting for input and then reading it is a deadlock rather than a composition.
 */
static hl_host_result hl_macos_terminal_size_change_event(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    if (hl_macos_file_descriptor(host, handle, 0) < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

static hl_host_result hl_macos_fork_prepare(void *context) {
    hl_host_macos *host = context;
    hl_host_result result;
    if (pthread_mutex_lock(&host->fork_gate) != 0) return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_lock(&host->lock) != 0) {
        pthread_mutex_unlock(&host->fork_gate);
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    }
    result = hl_host_sync_fork_prepare(host->sync);
    if (result.status == HL_STATUS_OK) {
        uint32_t index;
        /* clone_for_fork has created one additional handle per OFD.  fork duplicates both the
           parent and child handles into the other process, so reserve one reference for every
           inherited handle before either side closes its unwanted half. */
        for (index = 0; index < host->file_capacity; ++index) {
            if (host->files[index].active && host->files[index].directory_shared != NULL)
                (void)__atomic_add_fetch(&host->files[index].directory_shared->references, 1u, __ATOMIC_ACQ_REL);
            /* Every live stream handle is duplicated by fork. Reserve the
             * child's reference before either process can close an endpoint;
             * otherwise one process may unmap the shared stream bookkeeping
             * while the other still owns its inherited handle. */
            if (host->files[index].active && host->files[index].stream != NULL)
                (void)__atomic_add_fetch(&host->files[index].stream->references, 1u, __ATOMIC_ACQ_REL);
        }
        for (index = 0; index < host->counter_capacity; ++index) {
            hl_macos_counter_object *object;
            uint32_t previous;
            if (!host->counters[index].active) continue;
            object = host->counters[index].object;
            for (previous = 0; previous < index; ++previous)
                if (host->counters[previous].active && host->counters[previous].object == object) break;
            if (previous == index) object->shared->references++;
        }
    }
    if (result.status != HL_STATUS_OK) {
        pthread_mutex_unlock(&host->lock);
        pthread_mutex_unlock(&host->fork_gate);
    }
    return result;
}

static hl_host_result hl_macos_fork_complete(void *context) {
    hl_host_macos *host = context;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

static hl_host_result hl_macos_fork_child(void *context) {
    hl_host_macos *host = context;
    hl_host_result result = hl_host_sync_fork_complete(host->sync);
    /* Only the forking thread exists here, so every waiter record inherited from the parent is about
     * a thread this process does not have. Left in place they would hand a reused waiter identity
     * someone else's outstanding interruption. */
    hl_host_sync_park_reset(host->sync);
    for (uint32_t index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_macos_counter_subscription *subscription = host->counter_subscriptions[index];
        if (subscription == NULL) continue;
        if (!subscription->active) continue;
        hl_host_process_fd_private_remove(subscription->descriptor);
        hl_host_process_fd_private_remove(subscription->wake[0]);
        hl_host_process_fd_private_remove(subscription->wake[1]);
        close(subscription->descriptor);
        close(subscription->wake[0]);
        close(subscription->wake[1]);
        subscription->active = 0;
        subscription->counter = HL_HOST_HANDLE_INVALID;
        subscription->notify = NULL;
        subscription->observer = NULL;
    }
    for (uint32_t index = 0; index < host->directory_capacity && result.status == HL_STATUS_OK; ++index) {
        hl_macos_directory_object *object;
        uint32_t previous;
        if (!host->directories[index].active) continue;
        object = host->directories[index].object;
        for (previous = 0; previous < index; ++previous)
            if (host->directories[previous].active && host->directories[previous].object == object) break;
        if (previous != index) continue;
        int replacement = kqueue();
        if (replacement < 0) {
            result = hl_macos_errno();
            break;
        }
        (void)fcntl(replacement, F_SETFD, FD_CLOEXEC);
        int adopted = hl_host_process_fd_private_adopt(replacement);
        if (adopted < 0) {
            close(replacement);
            result = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
            break;
        }
        replacement = adopted;
        for (uint32_t watch_index = 0; watch_index < object->watch_capacity; ++watch_index) {
            hl_macos_directory_watch *watch = &object->watches[watch_index];
            if (!watch->active) continue;
            struct kevent change;
            uint16_t flags =
                (uint16_t)(EV_ADD | EV_CLEAR | ((watch->interests & HL_HOST_DIRECTORY_ONESHOT) != 0 ? EV_ONESHOT : 0));
            EV_SET(&change, watch->descriptor, EVFILT_VNODE, flags, hl_macos_directory_native(watch->interests), 0,
                   (void *)(uintptr_t)watch->token);
            if (kevent(replacement, &change, 1, NULL, 0, NULL) != 0) {
                hl_host_process_fd_private_remove(replacement);
                close(replacement);
                result = hl_macos_errno();
                break;
            }
        }
        if (result.status != HL_STATUS_OK) break;
        hl_host_process_fd_private_remove(object->descriptor);
        close(object->descriptor);
        object->descriptor = replacement;
    }
    if (pthread_mutex_unlock(&host->lock) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (pthread_mutex_unlock(&host->fork_gate) != 0 && result.status == HL_STATUS_OK)
        result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    return result;
}

static void hl_macos_log(void *context, uint32_t event, const char *message, size_t message_size) {
    size_t written = 0;
    (void)context;
    (void)event;
    while (written < message_size) {
        ssize_t result = write(STDERR_FILENO, message + written, message_size - written);
        if (result > 0) {
            written += (size_t)result;
            continue;
        }
        if (result < 0 && errno == EINTR) continue;
        break;
    }
}

static hl_host_result hl_macos_file_validate_private_regular(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, file, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISREG(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_macos_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                         size_t path_size, hl_host_const_bytes input,
                                                         uint32_t permissions) {
    static _Atomic uint64_t sequence;
    hl_host_macos *host = context;
    char name[PATH_MAX], temporary[PATH_MAX];
    int directory_fd = AT_FDCWD, descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(name) || memchr(path, '\0', path_size) != NULL ||
        (permissions & ~0777u) != 0 || (input.size != 0 && input.data == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(name, path, path_size);
    name[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        directory_fd = hl_macos_file_descriptor(host, directory, 0);
        if (directory_fd >= 0) directory_fd = fcntl(directory_fd, F_DUPFD_CLOEXEC, 0);
        if (directory_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (unsigned attempt = 0; attempt < 16; ++attempt) {
        uint64_t token = atomic_fetch_add_explicit(&sequence, 1, memory_order_relaxed);
        int count = snprintf(temporary, sizeof temporary, "%s.hl-%llx-%llx.tmp", name,
                             (unsigned long long)(uint64_t)getpid(), (unsigned long long)token);
        if (count <= 0 || (size_t)count >= sizeof temporary) break;
        descriptor =
            openat(directory_fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, (mode_t)permissions);
        if (descriptor >= 0 || errno != EEXIST) break;
    }
    if (descriptor < 0) {
        if (directory_fd != AT_FDCWD) close(directory_fd);
        return hl_macos_errno();
    }
    size_t done = 0;
    int saved = 0;
    while (done < input.size) {
        ssize_t count = write(descriptor, (const uint8_t *)input.data + done, input.size - done);
        if (count > 0)
            done += (size_t)count;
        else if (count < 0 && errno == EINTR)
            continue;
        else {
            saved = count == 0 ? EIO : errno;
            break;
        }
    }
    int ok = done == input.size;
    if (ok && fsync(descriptor) != 0) {
        ok = 0;
        saved = errno;
    }
    if (close(descriptor) != 0 && ok) {
        ok = 0;
        saved = errno;
    }
    if (ok && renameat(directory_fd, temporary, directory_fd, name) != 0) {
        ok = 0;
        saved = errno;
    }
    if (!ok) (void)unlinkat(directory_fd, temporary, 0);
    if (directory_fd != AT_FDCWD) close(directory_fd);
    errno = saved != 0 ? saved : EIO;
    return ok ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_validate_private_directory(void *context, hl_host_handle directory) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, directory, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

hl_status hl_host_macos_create(hl_host_macos **out_host, hl_host_services *out_services) {
    static const hl_host_memory_services memory = {
        HL_HOST_MEMORY_ABI,        sizeof(memory),          hl_macos_reserve,      hl_macos_protect,
        hl_macos_release,          hl_macos_publish,        hl_macos_reserve_code, hl_macos_repair_code,
        hl_macos_begin_code_write, hl_macos_end_code_write, hl_macos_map_file,     hl_macos_mapping_sync,
        hl_macos_unmap_range,      hl_macos_map_anonymous,  hl_macos_discard,      hl_macos_repair_signal_page,
        hl_macos_unmap_address,    hl_macos_wire_range,     hl_macos_unwire_range, hl_macos_protect_address,
        hl_macos_sync_address};
    static const hl_host_clock_services clock = {.abi = HL_HOST_CLOCK_ABI,
                                                 .size = sizeof(clock),
                                                 .monotonic_ns = hl_macos_monotonic,
                                                 .realtime_ns = hl_macos_realtime,
                                                 .raw_monotonic_ns = hl_macos_raw_monotonic,
                                                 .process_cpu_ns = hl_macos_process_cpu,
                                                 .thread_cpu_ns = hl_macos_thread_cpu,
                                                 .sleep_until = hl_macos_clock_sleep_until,
                                                 .architectural_counter_hz = hl_macos_architectural_counter,
                                                 .backoff_ns = hl_macos_backoff};
    static const hl_host_log_services log = {HL_HOST_LOG_ABI, sizeof(log), hl_macos_log};
    static const hl_host_file_services file = {HL_HOST_FILE_ABI,
                                               sizeof(file),
                                               hl_macos_file_open,
                                               hl_macos_file_read,
                                               hl_macos_file_write,
                                               hl_macos_file_append,
                                               hl_macos_file_metadata_get,
                                               hl_macos_file_close,
                                               hl_macos_file_read_sequential,
                                               hl_macos_file_write_sequential,
                                               hl_macos_file_clone_for_fork,
                                               hl_macos_file_seek,
                                               hl_macos_file_readv,
                                               hl_macos_file_writev,
                                               hl_macos_file_readv_at,
                                               hl_macos_file_writev_at,
                                               hl_macos_file_appendv,
                                               hl_macos_file_truncate,
                                               hl_macos_file_sync,
                                               hl_macos_file_sync,
                                               hl_macos_file_rename,
                                               hl_macos_file_unlink,
                                               hl_macos_file_path,
                                               hl_macos_file_standard_stream,
                                               hl_macos_file_readlink,
                                               hl_macos_file_set_owner,
                                               hl_macos_file_resolve_beneath,
                                               hl_macos_file_sync_range,
                                               hl_macos_file_sync_filesystem,
                                               hl_macos_file_open_beneath,
                                               hl_macos_file_allocate_range,
                                               hl_macos_file_filesystem_metadata,
                                               hl_macos_file_set_permissions,
                                               hl_macos_file_set_times,
                                               hl_macos_file_read_directory,
                                               hl_macos_file_mkdir,
                                               hl_macos_file_symlink,
                                               hl_macos_file_link,
                                               hl_macos_file_fifo,
                                               hl_macos_file_validate_private_regular,
                                               hl_macos_file_store_private_atomic,
                                               hl_macos_file_validate_private_directory,
                                               hl_macos_file_rmdir};
    static const hl_host_process_services process = {
        HL_HOST_PROCESS_ABI,        sizeof(process),        hl_macos_process_spawn,         hl_macos_process_wait,
        hl_macos_process_terminate, hl_macos_process_close, hl_macos_process_spawn_prepared};
    static const hl_host_event_services event = {
        HL_HOST_EVENT_ABI,          sizeof(event),       hl_macos_event_create, hl_macos_event_control,
        hl_macos_event_wait,        hl_macos_event_wake, hl_macos_event_close,  hl_macos_event_arm_timer,
        hl_macos_event_disarm_timer};
    static const hl_host_shared_memory_services shared_memory = {HL_HOST_SHARED_MEMORY_ABI, sizeof(shared_memory),
                                                                 hl_macos_shared_create,    hl_macos_shared_open,
                                                                 hl_macos_shared_resize,    hl_macos_file_close};
    static const hl_host_sync_services sync = {HL_HOST_SYNC_ABI,      sizeof(sync),           hl_macos_mutex_create,
                                               hl_macos_mutex_lock,   hl_macos_mutex_unlock,  hl_macos_mutex_close,
                                               hl_macos_fork_prepare, hl_macos_fork_complete, hl_macos_fork_child,
                                               hl_macos_park,         hl_macos_unpark,        hl_macos_interrupt_park};
    static const hl_host_terminal_services terminal = {HL_HOST_TERMINAL_ABI,       sizeof(terminal),
                                                       hl_macos_terminal_probe,    hl_macos_terminal_get_mode,
                                                       hl_macos_terminal_set_mode, hl_macos_terminal_get_size,
                                                       hl_macos_terminal_set_size, hl_macos_terminal_read,
                                                       hl_macos_terminal_write,    hl_macos_terminal_size_change_event};
    static const hl_host_counter_services counter = {
        HL_HOST_COUNTER_ABI,          sizeof(counter),
        hl_macos_counter_create,      hl_macos_counter_read,
        hl_macos_counter_write,       hl_macos_counter_get_flags,
        hl_macos_counter_set_flags,   hl_macos_counter_duplicate,
        hl_macos_counter_readiness,   hl_macos_counter_subscribe,
        hl_macos_counter_unsubscribe, hl_macos_counter_close,
    };
    static const hl_host_transfer_services transfer = {
        HL_HOST_TRANSFER_ABI,    sizeof(transfer),          hl_macos_transfer_channel_pair,
        hl_macos_transfer_send,  hl_macos_transfer_receive, hl_macos_transfer_duplicate,
        hl_macos_transfer_close,
    };
    static const hl_host_directory_services directory = {
        HL_HOST_DIRECTORY_ABI,     sizeof(directory),         hl_macos_directory_create, hl_macos_directory_add,
        hl_macos_directory_modify, hl_macos_directory_remove, hl_macos_directory_read,   hl_macos_directory_duplicate,
        hl_macos_directory_close};
    static const hl_host_watch_services watch = {HL_HOST_WATCH_ABI,    sizeof(watch),        hl_macos_watch_open,
                                                 hl_macos_watch_query, hl_macos_watch_drain, hl_macos_watch_close};
    static const hl_host_stream_services stream = {HL_HOST_STREAM_ABI,        sizeof(stream),
                                                   hl_macos_stream_pipe_pair, hl_macos_stream_read,
                                                   hl_macos_stream_write,     hl_macos_stream_duplicate,
                                                   hl_macos_stream_close,     hl_macos_stream_set_status_flags,
                                                   hl_macos_stream_readiness, hl_macos_stream_move};
    static const hl_host_posix_attachment_services posix_attachment = {
        HL_HOST_POSIX_ATTACHMENT_ABI, sizeof(posix_attachment), hl_macos_attachment_borrow_file,
        hl_macos_attachment_borrow_file_at_least, hl_macos_attachment_release};
    hl_host_macos *host;
    if (out_host == NULL || out_services == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_host = NULL;
    memset(out_services, 0, sizeof(*out_services));
    host = calloc(1, sizeof(*host));
    if (host == NULL) return HL_STATUS_OUT_OF_MEMORY;
    host->mappings = calloc(HL_MACOS_MAPPING_CAPACITY, sizeof(*host->mappings));
    host->files = calloc(HL_MACOS_FILE_CAPACITY, sizeof(*host->files));
    host->counters = calloc(HL_MACOS_COUNTER_CAPACITY, sizeof(*host->counters));
    host->transfers = calloc(HL_MACOS_TRANSFER_CAPACITY, sizeof(*host->transfers));
    host->directories = calloc(HL_MACOS_DIRECTORY_CAPACITY, sizeof(*host->directories));
    host->processes = calloc(HL_MACOS_PROCESS_CAPACITY, sizeof(*host->processes));
    host->events = calloc(HL_MACOS_EVENT_CAPACITY, sizeof(*host->events));
    host->watches = calloc(HL_MACOS_WATCH_CAPACITY, sizeof(*host->watches));
    if (host->mappings == NULL || host->files == NULL || host->counters == NULL || host->transfers == NULL ||
        host->directories == NULL || host->processes == NULL || host->events == NULL || host->watches == NULL) {
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    host->mapping_capacity = HL_MACOS_MAPPING_CAPACITY;
    host->file_capacity = HL_MACOS_FILE_CAPACITY;
    host->counter_capacity = HL_MACOS_COUNTER_CAPACITY;
    host->transfer_capacity = HL_MACOS_TRANSFER_CAPACITY;
    host->directory_capacity = HL_MACOS_DIRECTORY_CAPACITY;
    host->process_capacity = HL_MACOS_PROCESS_CAPACITY;
    host->event_capacity = HL_MACOS_EVENT_CAPACITY;
    host->watch_capacity = HL_MACOS_WATCH_CAPACITY;
    if (pthread_mutex_init(&host->lock, NULL) != 0) {
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_mutex_init(&host->fork_gate, NULL) != 0) {
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_cond_init(&host->process_changed, NULL) != 0) {
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (hl_host_sync_registry_create(&host->sync) != HL_STATUS_OK) {
        pthread_cond_destroy(&host->process_changed);
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->watches);
        free(host->events);
        free(host->files);
        free(host->mappings);
        free(host->directories);
        free(host->transfers);
        free(host->counters);
        free(host->processes);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    out_services->abi = HL_HOST_SERVICES_ABI;
    out_services->size = sizeof(*out_services);
    out_services->capabilities = HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_LOG | HL_HOST_CAP_FILE |
                                 HL_HOST_CAP_PROCESS | HL_HOST_CAP_EVENT_TIMER | HL_HOST_CAP_SHARED_MEMORY |
                                 HL_HOST_CAP_CODE_MAPPING | HL_HOST_CAP_SYNC | HL_HOST_CAP_EVENT | HL_HOST_CAP_COUNTER |
                                 HL_HOST_CAP_DIRECTORY | HL_HOST_CAP_TRANSFER | HL_HOST_CAP_WATCH | HL_HOST_CAP_STREAM |
                                 HL_HOST_CAP_POSIX_ATTACHMENT | HL_HOST_CAP_TERMINAL;
    out_services->context = host;
    out_services->memory = &memory;
    out_services->clock = &clock;
    out_services->log = &log;
    out_services->file = &file;
    out_services->process = &process;
    out_services->event = &event;
    out_services->shared_memory = &shared_memory;
    out_services->sync = &sync;
    out_services->counter = &counter;
    out_services->transfer = &transfer;
    out_services->directory = &directory;
    out_services->watch = &watch;
    out_services->stream = &stream;
    out_services->posix_attachment = &posix_attachment;
    out_services->terminal = &terminal;
    *out_host = host;
    return HL_STATUS_OK;
}

void hl_host_macos_destroy(hl_host_macos *host) {
    uint32_t index;
    if (host == NULL) return;
    pthread_mutex_lock(&host->lock);
    host->destroying = 1;
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        if (process->active && !process->reaped) kill(process->pid, SIGKILL);
    }
    pthread_mutex_unlock(&host->lock);
    /* Subscription threads may call user code and own three descriptors each.
     * Join them before releasing counters or any storage they can observe. */
    for (index = 0; index < host->counter_subscription_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[index] != NULL && host->counter_subscriptions[index]->active)
            handle =
                hl_macos_handle(HL_MACOS_HANDLE_SUBSCRIPTION, index, host->counter_subscriptions[index]->generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_counter_unsubscribe(host, handle);
    }
    for (index = 0; index < host->transfer_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->transfers[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, host->transfers[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_transfer_close(host, handle);
    }
    for (index = 0; index < host->directory_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->directories[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_DIRECTORY, index, host->directories[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_directory_close(host, handle);
    }
    for (index = 0; index < host->counter_capacity; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counters[index].active)
            handle = hl_macos_handle(HL_MACOS_HANDLE_COUNTER, index, host->counters[index].generation);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_macos_counter_close(host, handle);
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->event_capacity; ++index)
        if (host->events[index].active) close(host->events[index].descriptor);
    for (index = 0; index < host->watch_capacity; ++index)
        if (host->watches[index].active) close(host->watches[index].descriptor);
    hl_host_sync_registry_destroy(host->sync);
    for (;;) {
        uint32_t waiters = 0;
        for (index = 0; index < host->process_capacity; ++index)
            waiters += host->processes[index].waiters;
        if (waiters == 0) break;
        pthread_cond_wait(&host->process_changed, &host->lock);
    }
    pthread_mutex_unlock(&host->lock);
    for (index = 0; index < host->mapping_capacity; index++) {
        hl_macos_mapping *mapping = &host->mappings[index];
        uint64_t held_offset;
        uint64_t held_size;
        if (!mapping->active) continue;
        /* Teardown gives back only what is still held, for the same reason release does. */
        for (uint32_t part = 0;
             hl_host_hole_set_held_range(&mapping->retired, mapping->size, part, &held_offset, &held_size); ++part)
            munmap((char *)mapping->writable + held_offset, (size_t)held_size);
        if (mapping->executable != NULL && mapping->executable != mapping->writable)
            munmap(mapping->executable, (size_t)mapping->size);
        hl_host_hole_set_release(&mapping->retired);
    }
    for (index = 0; index < host->file_capacity; ++index) {
        hl_macos_file *file = &host->files[index];
        if (!file->active) continue;
        close(file->descriptor);
        if (file->directory != NULL) closedir(file->directory);
        if (file->append_descriptor >= 0) close(file->append_descriptor);
        hl_macos_stream_release(file->stream);
        hl_macos_directory_shared_release(file->directory_shared);
    }
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        int status;
        if (!process->active || process->reaped) continue;
        kill(process->pid, SIGKILL);
        while (waitpid(process->pid, &status, 0) < 0 && errno == EINTR) {}
    }
    pthread_cond_destroy(&host->process_changed);
    pthread_mutex_destroy(&host->fork_gate);
    pthread_mutex_destroy(&host->lock);
    for (index = 0; index < host->counter_subscription_capacity; ++index)
        free(host->counter_subscriptions[index]);
    free(host->counter_subscriptions);
    free(host->watches);
    free(host->events);
    free(host->files);
    free(host->mappings);
    free(host->directories);
    free(host->transfers);
    free(host->counters);
    free(host->processes);
    free(host);
}
