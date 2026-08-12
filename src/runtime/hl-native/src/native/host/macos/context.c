static const hl_host_memory_services memory = {
    HL_HOST_MEMORY_ABI,        sizeof(memory),          hl_macos_reserve,      hl_macos_protect,
    hl_macos_release,          hl_macos_publish,        hl_macos_reserve_code, hl_macos_repair_code,
    hl_macos_begin_code_write, hl_macos_end_code_write, hl_macos_map_file,     hl_macos_mapping_sync,
    hl_macos_unmap_range,      hl_macos_map_anonymous,  hl_macos_discard,      hl_macos_repair_signal_page,
    hl_macos_unmap_address,    hl_macos_wire_range,     hl_macos_unwire_range, hl_macos_protect_address,
    hl_macos_sync_address};
static const hl_host_clock_services clock_services = {.abi = HL_HOST_CLOCK_ABI,
                                                      .size = sizeof(clock_services),
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
static const hl_host_sync_services sync_services = {
    HL_HOST_SYNC_ABI,      sizeof(sync_services), hl_macos_mutex_create, hl_macos_mutex_lock,
    hl_macos_mutex_unlock, hl_macos_mutex_close,  hl_macos_fork_prepare, hl_macos_fork_complete,
    hl_macos_fork_child,   hl_macos_park,         hl_macos_unpark,       hl_macos_interrupt_park};
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

hl_status hl_host_macos_create(hl_host_macos **out_host, hl_host_services *out_services) {
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
    out_services->clock = &clock_services;
    out_services->log = &log;
    out_services->file = &file;
    out_services->process = &process;
    out_services->event = &event;
    out_services->shared_memory = &shared_memory;
    out_services->sync = &sync_services;
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
