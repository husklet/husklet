static const hl_host_memory_services g_linux_memory_services = {HL_HOST_MEMORY_ABI,
                                                                sizeof(g_linux_memory_services),
                                                                hl_linux_memory_reserve,
                                                                hl_linux_memory_protect,
                                                                hl_linux_memory_release,
                                                                hl_linux_memory_publish,
                                                                hl_linux_memory_reserve_code,
                                                                hl_linux_memory_repair_code,
                                                                hl_linux_memory_code_write,
                                                                hl_linux_memory_code_write,
                                                                hl_linux_memory_map_file,
                                                                hl_linux_memory_sync,
                                                                hl_linux_memory_unmap_range,
                                                                hl_linux_memory_map_anonymous,
                                                                hl_linux_memory_discard,
                                                                hl_linux_memory_repair_signal_page,
                                                                hl_linux_memory_unmap_address,
                                                                hl_linux_memory_wire_range,
                                                                hl_linux_memory_unwire_range,
                                                                hl_linux_memory_protect_address,
                                                                hl_linux_memory_sync_address};
static const hl_host_clock_services g_linux_clock_services = {.abi = HL_HOST_CLOCK_ABI,
                                                              .size = sizeof(g_linux_clock_services),
                                                              .monotonic_ns = hl_linux_monotonic,
                                                              .realtime_ns = hl_linux_realtime,
                                                              .raw_monotonic_ns = hl_linux_raw_monotonic,
                                                              .process_cpu_ns = hl_linux_process_cpu,
                                                              .thread_cpu_ns = hl_linux_thread_cpu,
                                                              .sleep_until = hl_linux_clock_sleep_until,
                                                              .architectural_counter_hz =
                                                                  hl_linux_architectural_counter,
                                                              .backoff_ns = hl_linux_backoff};
static const hl_host_log_services g_linux_log_services = {HL_HOST_LOG_ABI, sizeof(g_linux_log_services), hl_linux_log};
static const hl_host_file_services g_linux_file_services = {HL_HOST_FILE_ABI,
                                                            sizeof(g_linux_file_services),
                                                            hl_linux_file_open,
                                                            hl_linux_file_read,
                                                            hl_linux_file_write,
                                                            hl_linux_file_append,
                                                            hl_linux_file_metadata_get,
                                                            hl_linux_file_close,
                                                            hl_linux_file_read_sequential,
                                                            hl_linux_file_write_sequential,
                                                            hl_linux_file_clone_for_fork,
                                                            hl_linux_file_seek,
                                                            hl_linux_file_readv,
                                                            hl_linux_file_writev,
                                                            hl_linux_file_readv_at,
                                                            hl_linux_file_writev_at,
                                                            hl_linux_file_appendv,
                                                            hl_linux_file_truncate,
                                                            hl_linux_file_sync,
                                                            hl_linux_file_data_sync,
                                                            hl_linux_file_rename,
                                                            hl_linux_file_unlink,
                                                            hl_linux_file_path,
                                                            hl_linux_file_standard_stream,
                                                            hl_linux_file_readlink,
                                                            hl_linux_file_set_owner,
                                                            hl_linux_file_resolve_beneath,
                                                            hl_linux_file_sync_range,
                                                            hl_linux_file_sync_filesystem,
                                                            hl_linux_file_open_beneath,
                                                            hl_linux_file_allocate_range,
                                                            hl_linux_file_filesystem_metadata,
                                                            hl_linux_file_set_permissions,
                                                            hl_linux_file_set_times,
                                                            hl_linux_file_read_directory,
                                                            hl_linux_file_mkdir,
                                                            hl_linux_file_symlink,
                                                            hl_linux_file_link,
                                                            hl_linux_file_fifo,
                                                            hl_linux_file_validate_private_regular,
                                                            hl_linux_file_store_private_atomic,
                                                            hl_linux_file_validate_private_directory,
                                                            hl_linux_file_rmdir};
static const hl_host_event_services g_linux_event_services = {
    HL_HOST_EVENT_ABI,          sizeof(g_linux_event_services),
    hl_linux_event_create,      hl_linux_event_control,
    hl_linux_event_wait,        hl_linux_event_wake,
    hl_linux_event_close,       hl_linux_event_arm_timer,
    hl_linux_event_disarm_timer};
static const hl_host_network_services g_linux_network_services = {HL_HOST_NETWORK_ABI,
                                                                  sizeof(g_linux_network_services),
                                                                  hl_linux_network_socket,
                                                                  hl_linux_network_bind,
                                                                  hl_linux_network_connect,
                                                                  hl_linux_network_send,
                                                                  hl_linux_network_receive,
                                                                  hl_linux_network_close,
                                                                  hl_linux_network_listen,
                                                                  hl_linux_network_accept,
                                                                  hl_linux_network_pair,
                                                                  hl_linux_network_shutdown,
                                                                  hl_linux_network_local_address,
                                                                  hl_linux_network_peer_address,
                                                                  hl_linux_network_get_option,
                                                                  hl_linux_network_set_option,
                                                                  hl_linux_network_send_message,
                                                                  hl_linux_network_receive_message,
                                                                  hl_linux_network_readiness,
                                                                  hl_linux_network_wait_handle,
                                                                  hl_linux_network_set_status_flags,
                                                                  hl_linux_network_duplicate};
static const hl_host_shared_memory_services g_linux_shared_memory_services = {
    HL_HOST_SHARED_MEMORY_ABI, sizeof(g_linux_shared_memory_services),
    hl_linux_shared_create,    hl_linux_shared_open,
    hl_linux_shared_resize,    hl_linux_shared_close};
static const hl_host_counter_services g_linux_counter_services = {
    HL_HOST_COUNTER_ABI,        sizeof(g_linux_counter_services), hl_linux_counter_create,
    hl_linux_counter_read,      hl_linux_counter_write,           hl_linux_counter_get_flags,
    hl_linux_counter_set_flags, hl_linux_counter_duplicate,       hl_linux_counter_readiness,
    hl_linux_counter_subscribe, hl_linux_counter_unsubscribe,     hl_linux_counter_close,
};
static const hl_host_transfer_services g_linux_transfer_services = {
    HL_HOST_TRANSFER_ABI,    sizeof(g_linux_transfer_services), hl_linux_transfer_channel_pair,
    hl_linux_transfer_send,  hl_linux_transfer_receive,         hl_linux_transfer_duplicate,
    hl_linux_transfer_close,
};
static const hl_host_directory_services g_linux_directory_services = {
    HL_HOST_DIRECTORY_ABI,   sizeof(g_linux_directory_services), hl_linux_directory_create,
    hl_linux_directory_add,  hl_linux_directory_modify,          hl_linux_directory_remove,
    hl_linux_directory_read, hl_linux_directory_duplicate,       hl_linux_directory_close};
static const hl_host_watch_services g_linux_watch_services = {HL_HOST_WATCH_ABI,    sizeof(g_linux_watch_services),
                                                              hl_linux_watch_open,  hl_linux_watch_query,
                                                              hl_linux_watch_drain, hl_linux_watch_close};
static const hl_host_stream_services g_linux_stream_services = {
    HL_HOST_STREAM_ABI,        sizeof(g_linux_stream_services),
    hl_linux_stream_pipe_pair, hl_linux_stream_read,
    hl_linux_stream_write,     hl_linux_stream_duplicate,
    hl_linux_stream_close,     hl_linux_stream_set_status_flags,
    hl_linux_stream_readiness, hl_linux_stream_move};
static const hl_host_posix_attachment_services g_linux_posix_attachment_services = {
    HL_HOST_POSIX_ATTACHMENT_ABI, sizeof(g_linux_posix_attachment_services), hl_linux_attachment_borrow_file,
    hl_linux_attachment_borrow_file_at_least, hl_linux_attachment_release};
static const hl_host_process_services g_linux_process_services = {
    HL_HOST_PROCESS_ABI,        sizeof(g_linux_process_services), hl_linux_process_spawn,         hl_linux_process_wait,
    hl_linux_process_terminate, hl_linux_process_close,           hl_linux_process_spawn_prepared};
static const hl_host_sync_services g_linux_sync_services = {HL_HOST_SYNC_ABI,      sizeof(g_linux_sync_services),
                                                            hl_linux_mutex_create, hl_linux_mutex_lock,
                                                            hl_linux_mutex_unlock, hl_linux_mutex_close,
                                                            hl_linux_fork_prepare, hl_linux_fork_complete,
                                                            hl_linux_fork_child,   hl_linux_park,
                                                            hl_linux_unpark,       hl_linux_interrupt_park};
static const hl_host_terminal_services g_linux_terminal_services = {
    HL_HOST_TERMINAL_ABI,       sizeof(g_linux_terminal_services),
    hl_linux_terminal_probe,    hl_linux_terminal_get_mode,
    hl_linux_terminal_set_mode, hl_linux_terminal_get_size,
    hl_linux_terminal_set_size, hl_linux_terminal_read,
    hl_linux_terminal_write,    hl_linux_terminal_size_change_event};

hl_status hl_host_linux_create(hl_host_linux **out_host, hl_host_services *out_services) {
    hl_host_linux *host;
    uint32_t i;
    if (out_host == NULL || out_services == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_host = NULL;
    memset(out_services, 0, sizeof(*out_services));
    host = calloc(1, sizeof(*host));
    if (host == NULL) return HL_STATUS_OUT_OF_MEMORY;
    host->handles = calloc(HL_LINUX_HANDLE_CAPACITY, sizeof(*host->handles));
    host->timers = calloc(HL_LINUX_TIMER_CAPACITY, sizeof(*host->timers));
    if (host->handles == NULL || host->timers == NULL) {
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    host->handle_capacity = HL_LINUX_HANDLE_CAPACITY;
    host->timer_capacity = HL_LINUX_TIMER_CAPACITY;
    if (pthread_mutex_init(&host->lock, NULL) != 0) {
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_mutex_init(&host->fork_gate, NULL) != 0) {
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (pthread_cond_init(&host->process_changed, NULL) != 0) {
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    if (hl_host_sync_registry_create(&host->sync) != HL_STATUS_OK) {
        pthread_cond_destroy(&host->process_changed);
        pthread_mutex_destroy(&host->fork_gate);
        pthread_mutex_destroy(&host->lock);
        free(host->timers);
        free(host->handles);
        free(host);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    for (i = 0; i < host->handle_capacity; ++i) {
        host->handles[i].descriptor = -1;
        host->handles[i].wake_descriptor = -1;
    }
    for (i = 0; i < host->timer_capacity; ++i)
        host->timers[i].descriptor = -1;
    out_services->abi = HL_HOST_SERVICES_ABI;
    out_services->size = sizeof(*out_services);
    out_services->capabilities = HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_LOG | HL_HOST_CAP_FILE |
                                 HL_HOST_CAP_EVENT | HL_HOST_CAP_EVENT_TIMER | HL_HOST_CAP_NETWORK |
                                 HL_HOST_CAP_SHARED_MEMORY | HL_HOST_CAP_PROCESS | HL_HOST_CAP_CODE_MAPPING |
                                 HL_HOST_CAP_SYNC | HL_HOST_CAP_COUNTER | HL_HOST_CAP_TRANSFER | HL_HOST_CAP_DIRECTORY |
                                 HL_HOST_CAP_WATCH | HL_HOST_CAP_STREAM | HL_HOST_CAP_POSIX_ATTACHMENT |
                                 HL_HOST_CAP_TERMINAL;
    out_services->context = host;
    out_services->memory = &g_linux_memory_services;
    out_services->clock = &g_linux_clock_services;
    out_services->log = &g_linux_log_services;
    out_services->file = &g_linux_file_services;
    out_services->event = &g_linux_event_services;
    out_services->network = &g_linux_network_services;
    out_services->shared_memory = &g_linux_shared_memory_services;
    out_services->process = &g_linux_process_services;
    out_services->sync = &g_linux_sync_services;
    out_services->counter = &g_linux_counter_services;
    out_services->transfer = &g_linux_transfer_services;
    out_services->directory = &g_linux_directory_services;
    out_services->watch = &g_linux_watch_services;
    out_services->stream = &g_linux_stream_services;
    out_services->posix_attachment = &g_linux_posix_attachment_services;
    out_services->terminal = &g_linux_terminal_services;
    *out_host = host;
    return HL_STATUS_OK;
}

void hl_host_linux_destroy(hl_host_linux *host) {
    uint32_t i;
    if (host == NULL) return;
    pthread_mutex_lock(&host->lock);
    host->destroying = 1;
    for (i = 0; i < host->handle_capacity; ++i) {
        hl_linux_handle_entry *entry = &host->handles[i];
        if (entry->kind == HL_LINUX_HANDLE_PROCESS && !entry->process_reaped) kill(entry->descriptor, SIGKILL);
    }
    for (;;) {
        uint32_t waiters = 0;
        for (i = 0; i < host->handle_capacity; ++i)
            waiters += host->handles[i].process_waiters;
        if (waiters == 0) break;
        pthread_cond_wait(&host->process_changed, &host->lock);
    }
    pthread_mutex_unlock(&host->lock);
    for (i = 0; i < host->counter_subscription_capacity; ++i) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        pthread_mutex_lock(&host->lock);
        if (host->counter_subscriptions[i] != NULL && host->counter_subscriptions[i]->active)
            handle = ((uint64_t)host->counter_subscriptions[i]->generation << 32) | (uint64_t)(i + 1u);
        pthread_mutex_unlock(&host->lock);
        if (handle != HL_HOST_HANDLE_INVALID) (void)hl_linux_counter_unsubscribe(host, handle);
    }
    for (i = 0; i < host->handle_capacity; ++i) {
        hl_linux_handle_entry *entry = &host->handles[i];
        if (entry->kind == HL_LINUX_HANDLE_MAPPING) {
            /* Teardown gives back only what is still held, for the same reason release does. */
            uint64_t held_offset;
            uint64_t held_size;
            for (uint32_t part = 0;
                 hl_host_hole_set_held_range(&entry->retired, entry->size, part, &held_offset, &held_size); ++part)
                munmap((char *)entry->address + held_offset, (size_t)held_size);
            if (entry->executable_address != NULL && entry->executable_address != entry->address)
                munmap(entry->executable_address, (size_t)entry->size);
            if (entry->descriptor >= 0) close(entry->descriptor);
            hl_host_hole_set_release(&entry->retired);
        } else if (entry->kind == HL_LINUX_HANDLE_PROCESS) {
            int status;
            if (entry->process_reaped) continue;
            kill(entry->descriptor, SIGKILL);
            while (waitpid(entry->descriptor, &status, 0) < 0 && errno == EINTR) {}
        } else if (entry->kind == HL_LINUX_HANDLE_DIRECTORY) {
            hl_linux_directory_object *object = entry->address;
            close(entry->descriptor);
            if (--object->references == 0) {
                free(object->pending);
                free(object->watches);
                free(object);
            }
        } else if (entry->kind == HL_LINUX_HANDLE_WATCH) {
            close(entry->descriptor);
            close(entry->wake_descriptor);
            free(entry->address);
        } else if (entry->kind != HL_LINUX_HANDLE_NONE) {
            if (entry->descriptor >= 0) close(entry->descriptor);
            if (entry->wake_descriptor >= 0) close(entry->wake_descriptor);
        }
    }
    hl_host_sync_registry_destroy(host->sync);
    pthread_cond_destroy(&host->process_changed);
    pthread_mutex_destroy(&host->fork_gate);
    pthread_mutex_destroy(&host->lock);
    for (i = 0; i < host->counter_subscription_capacity; ++i)
        free(host->counter_subscriptions[i]);
    free(host->counter_subscriptions);
    free(host->handles);
    free(host->timers);
    free(host);
}

#if defined(HL_NATIVE_TEST_HOOKS)
/* The macOS host is the only implementation with a cached DIR* whose descriptor must live in the
   engine-private band.  Linux has neither that cache nor a private descriptor band, but every symbol
   in the test-hook export manifest is resolved against the staged artifact.  Keep that contract
   complete and refuse the inapplicable probe, as the Windows host does. */
HL_API int32_t hl_c_backend_directory_stream_private_test(uint32_t scenario);

HL_API int32_t hl_c_backend_directory_stream_private_test(uint32_t scenario) {
    (void)scenario;
    return -ENOTSUP;
}
#endif
