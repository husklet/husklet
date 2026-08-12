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
