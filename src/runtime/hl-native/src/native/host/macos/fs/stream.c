static hl_host_result hl_macos_stream_pipe_pair(void *context, uint32_t flags) {
    hl_host_macos *host = context;
    int descriptors[2] = {-1, -1};
    int native_flags = (flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0;
    int no_sigpipe = 1;
    hl_host_result input, output;
    hl_macos_stream_shared *shared;
    unsigned short semaphore_values[2] = {1, 1};
    union semun semaphore_argument;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
    if (shared == MAP_FAILED) return hl_macos_errno();
    shared->semaphore = semget(IPC_PRIVATE, 2, IPC_CREAT | 0600);
    shared->references = 2;
    semaphore_argument.array = semaphore_values;
    if (shared->semaphore < 0 || semctl(shared->semaphore, 0, SETALL, semaphore_argument) != 0) {
        hl_host_result error = hl_macos_errno();
        if (shared->semaphore >= 0) (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return error;
    }
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, descriptors) != 0) {
        hl_host_result error = hl_macos_errno();
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return error;
    }
    (void)shutdown(descriptors[0], SHUT_WR);
    (void)shutdown(descriptors[1], SHUT_RD);
    (void)setsockopt(descriptors[1], SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
    if (native_flags != 0) {
        (void)fcntl(descriptors[0], F_SETFL, fcntl(descriptors[0], F_GETFL) | native_flags);
        (void)fcntl(descriptors[1], F_SETFL, fcntl(descriptors[1], F_GETFL) | native_flags);
    }
    /* Host descriptors never survive a native exec; guest CLOEXEC remains ABI-table state. */
    (void)fcntl(descriptors[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(descriptors[1], F_SETFD, FD_CLOEXEC);
    input = hl_macos_file_register(host, descriptors[0], -1, 0);
    if (input.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return input;
    }
    output = hl_macos_file_register(host, descriptors[1], -1, 0);
    if (output.status != HL_STATUS_OK) {
        (void)hl_macos_file_close(host, input.value);
        close(descriptors[1]);
        (void)semctl(shared->semaphore, 0, IPC_RMID);
        munmap(shared, sizeof(*shared));
        return output;
    }
    pthread_mutex_lock(&host->lock);
    hl_macos_file *input_file = hl_macos_file_lookup(host, input.value);
    hl_macos_file *output_file = hl_macos_file_lookup(host, output.value);
    input_file->stream = shared;
    input_file->stream_endpoint = 0;
    output_file->stream = shared;
    output_file->stream_endpoint = 1;
    pthread_mutex_unlock(&host->lock);
    input.detail = output.value;
    return input;
}

static hl_host_result hl_macos_stream_set_status_flags(void *context, hl_host_handle stream, uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *shared = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    int current;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, stream);
    if (file != NULL && file->stream != NULL) {
        descriptor = dup(file->descriptor);
        shared = file->stream;
        endpoint = file->stream_endpoint;
        if (descriptor >= 0) (void)__atomic_add_fetch(&shared->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(shared);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (hl_macos_stream_lock(shared, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(shared);
        return hl_macos_errno();
    }
    current = fcntl(descriptor, F_GETFL);
    if (current >= 0) {
        current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0);
        if (fcntl(descriptor, F_SETFL, current) != 0) current = -1;
    }
    hl_macos_stream_unlock(shared, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(shared);
    return current >= 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static int hl_macos_stream_handle(hl_host_macos *host, hl_host_handle handle) {
    int valid;
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, handle);
    valid = file != NULL && file->stream != NULL;
    pthread_mutex_unlock(&host->lock);
    return valid;
}

static hl_host_result hl_macos_stream_read(void *context, hl_host_handle stream, hl_host_bytes output) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_read_sequential(context, stream, output.data, output.size);
}

static hl_host_result hl_macos_stream_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_write_sequential(context, stream, input.data, input.size);
}

static hl_host_result hl_macos_stream_duplicate(void *context, hl_host_handle stream) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_clone_for_fork(context, stream);
}

static hl_host_result hl_macos_stream_close(void *context, hl_host_handle stream) {
    if (!hl_macos_stream_handle(context, stream)) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_file_close(context, stream);
}

static hl_host_result hl_macos_stream_readiness(void *context, hl_host_handle stream, uint32_t interests) {
    int descriptor;
    struct pollfd probe;
    descriptor = hl_macos_file_descriptor(context, stream, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    probe = (struct pollfd){descriptor, 0, 0};
    if ((interests & HL_HOST_READY_READ) != 0) probe.events |= POLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) probe.events |= POLLOUT;
    if (poll(&probe, 1, 0) < 0) return hl_macos_errno();
    uint32_t ready = 0;
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    return hl_macos_result(HL_STATUS_OK, ready & interests, 0);
}

static hl_host_result hl_macos_stream_move(void *context, hl_host_handle source, uint64_t source_offset,
                                           hl_host_handle destination, uint64_t destination_offset, uint64_t size,
                                           uint32_t flags) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *locks[2] = {NULL, NULL};
    hl_macos_stream_shared *input_pin = NULL, *output_pin = NULL;
    uint32_t endpoints[2] = {0, 0};
    uint32_t locked = 0;
    int input = -1, output = -1;
    unsigned char buffer[65536];
    struct stat input_status, output_status;
    off_t input_position = 0, output_position = 0;
    size_t request;
    ssize_t read_count, written;
    int input_stream, output_stream;
    hl_host_result result;
    uint32_t allowed = HL_HOST_STREAM_SOURCE_POSITIONED | HL_HOST_STREAM_DESTINATION_POSITIONED;
    if ((flags & ~allowed) != 0 || source_offset > INT64_MAX || destination_offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (size == 0) return hl_macos_result(HL_STATUS_OK, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *input_file = hl_macos_file_lookup(host, source);
    hl_macos_file *output_file = hl_macos_file_lookup(host, destination);
    if (input_file != NULL && output_file != NULL) {
        input = dup(input_file->descriptor);
        output = dup(output_file->descriptor);
        locks[0] = input_file->stream;
        endpoints[0] = input_file->stream_endpoint;
        locks[1] = output_file->stream;
        endpoints[1] = output_file->stream_endpoint;
        if (input >= 0 && locks[0] != NULL) {
            input_pin = locks[0];
            (void)__atomic_add_fetch(&input_pin->references, 1u, __ATOMIC_ACQ_REL);
        }
        if (output >= 0 && locks[1] != NULL) {
            output_pin = locks[1];
            (void)__atomic_add_fetch(&output_pin->references, 1u, __ATOMIC_ACQ_REL);
        }
        if (input < 0) locks[0] = NULL;
        if (output < 0) locks[1] = NULL;
        if (locks[1] != NULL && (locks[0] == NULL || locks[1]->semaphore < locks[0]->semaphore ||
                                 (locks[1] == locks[0] && endpoints[1] < endpoints[0]))) {
            hl_macos_stream_shared *swap_lock = locks[0];
            uint32_t swap_endpoint = endpoints[0];
            locks[0] = locks[1];
            endpoints[0] = endpoints[1];
            locks[1] = swap_lock;
            endpoints[1] = swap_endpoint;
        }
        for (locked = 0; locked < 2 && locks[locked] != NULL; ++locked) {
            if (locked != 0 && locks[locked] == locks[locked - 1] && endpoints[locked] == endpoints[locked - 1])
                continue;
            if (hl_macos_stream_lock(locks[locked], endpoints[locked]) != 0) break;
        }
    }
    pthread_mutex_unlock(&host->lock);
    {
        int descriptors[2] = {input, output};
        if (hl_macos_private_add_many(descriptors, 2) != 0) {
            result = hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
            goto done;
        }
        input = descriptors[0];
        output = descriptors[1];
    }
    if (input < 0 || output < 0 || (locked < 2 && locks[locked] != NULL)) {
        result = hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        goto done;
    }
    if (fstat(input, &input_status) != 0 || fstat(output, &output_status) != 0) {
        result = hl_macos_errno();
        goto done;
    }
    input_stream = S_ISSOCK(input_status.st_mode) || S_ISFIFO(input_status.st_mode);
    output_stream = S_ISSOCK(output_status.st_mode) || S_ISFIFO(output_status.st_mode);
    if ((!input_stream && !output_stream) || ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0 && input_stream) ||
        ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0 && output_stream) ||
        (!input_stream && (flags & HL_HOST_STREAM_SOURCE_POSITIONED) == 0) ||
        (!output_stream && (flags & HL_HOST_STREAM_DESTINATION_POSITIONED) == 0)) {
        result = hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        goto done;
    }
    request = size < sizeof(buffer) ? (size_t)size : sizeof(buffer);
    if (input_stream) {
        read_count = recv(input, buffer, request, MSG_PEEK);
    } else {
        input_position = (off_t)source_offset;
        read_count = pread(input, buffer, request, input_position);
    }
    if (read_count <= 0) {
        result = read_count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, 0, 0);
        goto done;
    }
    if (output_stream) {
        written = send(output, buffer, (size_t)read_count, 0);
    } else {
        output_position = (off_t)destination_offset;
        written = pwrite(output, buffer, (size_t)read_count, output_position);
    }
    if (written <= 0) {
        result = written < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, 0, 0);
        goto done;
    }
    if (input_stream) {
        ssize_t consumed = recv(input, buffer, (size_t)written, 0);
        if (consumed != written) {
            result = hl_macos_result(HL_STATUS_PLATFORM_FAILURE, 0, 0);
            goto done;
        }
    }
    result = hl_macos_result(HL_STATUS_OK, (uint64_t)written, 0);
done:
    while (locked != 0) {
        --locked;
        if (locked != 0 && locks[locked] == locks[locked - 1] && endpoints[locked] == endpoints[locked - 1]) continue;
        hl_macos_stream_unlock(locks[locked], endpoints[locked]);
    }
    if (input >= 0) {
        hl_host_process_fd_private_remove(input);
        close(input);
    }
    if (output >= 0) {
        hl_host_process_fd_private_remove(output);
        close(output);
    }
    hl_macos_stream_release(input_pin);
    hl_macos_stream_release(output_pin);
    return result;
}
