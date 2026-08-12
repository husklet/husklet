static hl_macos_process *hl_macos_process_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_PROCESS, host->process_capacity, &index) ||
        !host->processes[index].active || host->processes[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->processes[index];
}

static hl_host_result hl_macos_process_spawn_mode(void *context, hl_host_process_entry entry, void *entry_context,
                                                  int prepared) {
    hl_host_macos *host = context;
    hl_host_handle handle = 0;
    uint32_t index;
    pid_t pid;
    int fork_error;
    int private_prepared = 0;
    if (entry == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!prepared && pthread_mutex_lock(&host->fork_gate) != 0)
        return hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
    if (hl_host_process_fd_private_fork_prepare() != 0) {
        if (!prepared) (void)pthread_mutex_unlock(&host->fork_gate);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    private_prepared = 1;
    pid = fork();
    fork_error = errno;
    int private_status = private_prepared ? hl_host_process_fd_private_fork_complete(pid == 0) : 0;
    if (prepared) {
        /* Only the parent retains its watcher threads.  The child must discard
         * inherited subscription slots before object teardown can join them. */
        hl_host_result completed = pid == 0 ? hl_macos_fork_child(host) : hl_macos_fork_complete(host);
        if (private_status != 0 && completed.status == HL_STATUS_OK)
            completed = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        if (completed.status != HL_STATUS_OK) {
            if (pid > 0) {
                int status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            }
            if (pid == 0) _exit(255);
            return completed;
        }
    } else {
        int unlock_status = pthread_mutex_unlock(&host->fork_gate);
        if (private_status != 0 || unlock_status != 0) {
            if (pid > 0) {
                int status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            }
            if (pid == 0) _exit(255);
            return hl_macos_result(private_status != 0 ? HL_STATUS_RESOURCE_LIMIT : HL_STATUS_PLATFORM_FAILURE, 0, 0);
        }
    }
    if (pid < 0) {
        errno = fork_error;
        return hl_macos_errno();
    }
    if (pid == 0) _exit(entry(entry_context) & 255);
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->process_capacity; ++index) {
        hl_macos_process *process = &host->processes[index];
        if (process->active) continue;
        process->generation++;
        if (process->generation == 0) process->generation = 1;
        process->active = 1;
        process->pid = pid;
        process->reaped = 0;
        process->waiting = 0;
        process->waiters = 0;
        process->exit_kind = 0;
        process->exit_value = 0;
        handle = hl_macos_handle(HL_MACOS_HANDLE_PROCESS, index, process->generation);
        break;
    }
    if (handle == 0) {
        uint32_t capacity =
            hl_macos_grow_capacity(host->process_capacity, HL_MACOS_PROCESS_CAPACITY, sizeof(*host->processes));
        if (capacity == 0) {
            pthread_mutex_unlock(&host->lock);
            int status;
            kill(pid, SIGKILL);
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        hl_macos_process *grown = realloc(host->processes, (size_t)capacity * sizeof(*grown));
        if (grown != NULL) {
            memset(grown + host->process_capacity, 0, (size_t)(capacity - host->process_capacity) * sizeof(*grown));
            index = host->process_capacity;
            host->processes = grown;
            host->process_capacity = capacity;
            grown[index].generation = 1;
            grown[index].active = 1;
            grown[index].pid = pid;
            handle = hl_macos_handle(HL_MACOS_HANDLE_PROCESS, index, 1);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == 0) {
        int status;
        kill(pid, SIGKILL);
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    return hl_macos_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_macos_process_spawn(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_macos_process_spawn_mode(context, entry, entry_context, 0);
}

static hl_host_result hl_macos_process_spawn_prepared(void *context, hl_host_process_entry entry, void *entry_context) {
    return hl_macos_process_spawn_mode(context, entry, entry_context, 1);
}

static hl_host_result hl_macos_process_wait(void *context, hl_host_handle handle, uint64_t deadline_ns) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pid_t pid;
    pid_t waited;
    int status;
    int options;
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process == NULL || host->destroying) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    process->waiters++;
    while (process != NULL && process->waiting && !process->reaped) {
        if (deadline_ns == 0 ||
            (deadline_ns != HL_HOST_DEADLINE_INFINITE && hl_macos_monotonic_value() >= deadline_ns)) {
            process->waiters--;
            pthread_cond_broadcast(&host->process_changed);
            pthread_mutex_unlock(&host->lock);
            return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
        }
        hl_macos_process_changed_wait(host, deadline_ns);
        process = hl_macos_process_lookup(host, handle);
    }
    if (process != NULL && process->reaped) {
        hl_host_result result = hl_macos_result(HL_STATUS_OK, process->exit_value, process->exit_kind);
        process->waiters--;
        pthread_cond_broadcast(&host->process_changed);
        pthread_mutex_unlock(&host->lock);
        return result;
    }
    pid = process != NULL ? process->pid : -1;
    if (process != NULL) process->waiting = 1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    options = deadline_ns == HL_HOST_DEADLINE_INFINITE ? 0 : WNOHANG;
    for (;;) {
        do {
            waited = waitpid(pid, &status, options);
        } while (waited < 0 && errno == EINTR);
        if (waited != 0) break;
        if (deadline_ns == 0 || hl_macos_monotonic_value() >= deadline_ns) break;
        hl_macos_sleep_until(deadline_ns);
    }
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process != NULL) {
        process->waiting = 0;
        process->waiters--;
    }
    if (waited > 0 && process != NULL) {
        process->reaped = 1;
        process->exit_kind = WIFEXITED(status) ? HL_HOST_PROCESS_EXIT_CODE : HL_HOST_PROCESS_EXIT_SIGNAL;
        process->exit_value = WIFEXITED(status) ? (uint32_t)WEXITSTATUS(status) : (uint32_t)WTERMSIG(status);
    }
    pthread_cond_broadcast(&host->process_changed);
    pthread_mutex_unlock(&host->lock);
    if (waited == 0) return hl_macos_result(HL_STATUS_WOULD_BLOCK, 0, 0);
    if (waited < 0) return hl_macos_errno();
    if (WIFEXITED(status))
        return hl_macos_result(HL_STATUS_OK, (uint64_t)WEXITSTATUS(status), HL_HOST_PROCESS_EXIT_CODE);
    if (WIFSIGNALED(status))
        return hl_macos_result(HL_STATUS_OK, (uint64_t)WTERMSIG(status), HL_HOST_PROCESS_EXIT_SIGNAL);
    return hl_macos_result(HL_STATUS_CORRUPT, 0, (uint64_t)(uint32_t)status);
}

static hl_host_result hl_macos_process_terminate(void *context, hl_host_handle handle, uint32_t reason) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pid_t pid;
    if (reason != HL_HOST_PROCESS_TERMINATE_INTERRUPT && reason != HL_HOST_PROCESS_TERMINATE_FORCE &&
        (reason <= HL_HOST_PROCESS_TERMINATE_SIGNAL || reason > HL_HOST_PROCESS_TERMINATE_SIGNAL + 64))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    pid = process != NULL && !process->reaped && !host->destroying ? process->pid : -1;
    pthread_mutex_unlock(&host->lock);
    if (pid < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int signal_number;
    if (reason == HL_HOST_PROCESS_TERMINATE_INTERRUPT)
        signal_number = SIGINT;
    else if (reason == HL_HOST_PROCESS_TERMINATE_FORCE)
        signal_number = SIGKILL;
    else {
        static const unsigned char linux_to_macos[32] = {0,  1,  2,  3,  4,  5,  6,  10, 8,  9,  30,
                                                         11, 31, 13, 14, 15, 16, 20, 19, 17, 18, 21,
                                                         22, 16, 24, 25, 26, 27, 28, 23, 30, 12};
        uint32_t guest = reason - HL_HOST_PROCESS_TERMINATE_SIGNAL;
        signal_number = guest < 32 ? linux_to_macos[guest] : (int)guest;
    }
    if (kill(pid, signal_number) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_process_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_process *process;
    pthread_mutex_lock(&host->lock);
    process = hl_macos_process_lookup(host, handle);
    if (process == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (host->destroying || !process->reaped || process->waiting || process->waiters != 0) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_BUSY, 0, 0);
    }
    process->active = 0;
    process->pid = -1;
    process->reaped = 0;
    process->exit_kind = 0;
    process->exit_value = 0;
    pthread_mutex_unlock(&host->lock);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

