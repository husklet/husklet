static hl_host_result hl_linux_stream_pipe_pair(void *context, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptors[2];
    int native_flags = O_CLOEXEC;
    hl_host_result input, output;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((flags & HL_HOST_STREAM_NONBLOCK) != 0) native_flags |= O_NONBLOCK;
    if (pipe2(descriptors, native_flags) != 0) return hl_linux_errno_result();
    input = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptors[0], NULL, NULL, 0, -1);
    if (input.status != HL_STATUS_OK) {
        close(descriptors[0]);
        close(descriptors[1]);
        return input;
    }
    output = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptors[1], NULL, NULL, 0, -1);
    if (output.status != HL_STATUS_OK) {
        (void)hl_linux_close_descriptor(host, input.value);
        close(descriptors[1]);
        return output;
    }
    input.detail = output.value;
    return input;
}

static hl_host_result hl_linux_stream_set_status_flags(void *context, hl_host_handle stream, uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor, current;
    if ((flags & ~(uint32_t)HL_HOST_STREAM_NONBLOCK) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, stream, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    current = fcntl(descriptor, F_GETFL);
    if (current < 0) {
        hl_host_result error = hl_linux_errno_result();
        close(descriptor);
        return error;
    }
    current = (current & ~O_NONBLOCK) | ((flags & HL_HOST_STREAM_NONBLOCK) != 0 ? O_NONBLOCK : 0);
    hl_host_result result =
        fcntl(descriptor, F_SETFL, current) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
    close(descriptor);
    return result;
}

static int hl_linux_stream_descriptor(hl_host_linux *host, hl_host_handle stream) {
    int descriptor, pinned = -1;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, stream, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (descriptor >= 0) pinned = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    return pinned;
}

static hl_host_result hl_linux_stream_read(void *context, hl_host_handle stream, hl_host_bytes output) {
    int descriptor;
    ssize_t result;
    if (output.size != 0 && output.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_stream_descriptor(context, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = read(descriptor, output.data, output.size);
    hl_host_result output_result =
        result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(descriptor);
    return output_result;
}

static hl_host_result hl_linux_stream_write(void *context, hl_host_handle stream, hl_host_const_bytes input) {
    int descriptor;
    ssize_t result;
    sigset_t blocked, previous;
    int saved_error;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_linux_stream_descriptor(context, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    result = write(descriptor, input.data, input.size);
    saved_error = errno;
    if (result < 0 && saved_error == EPIPE && !sigismember(&previous, SIGPIPE)) {
        struct timespec immediate = {0, 0};
        (void)sigtimedwait(&blocked, NULL, &immediate);
    }
    pthread_sigmask(SIG_SETMASK, &previous, NULL);
    errno = saved_error;
    hl_host_result output_result =
        result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(descriptor);
    return output_result;
}

static hl_host_result hl_linux_stream_duplicate(void *context, hl_host_handle stream) {
    hl_host_linux *host = context;
    int descriptor = hl_linux_stream_descriptor(host, stream);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_STREAM, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}

static hl_host_result hl_linux_stream_close(void *context, hl_host_handle stream) {
    int pinned = hl_linux_stream_descriptor(context, stream);
    if (pinned < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    close(pinned);
    return hl_linux_close_descriptor(context, stream);
}

/* Readiness is asked about any pollable host object, not just pipes. Imported
 * stdio -- a pty slave included -- is registered as HL_LINUX_HANDLE_FILE, so the
 * stream-only lookup would reject it and the caller would fall back to
 * always-ready. Accept files here (macOS does the same); the other stream entry
 * points stay stream-typed. */
static int hl_linux_pollable_descriptor(hl_host_linux *host, hl_host_handle handle) {
    int descriptor, pinned = -1;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, handle, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) pinned = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    return pinned;
}

static hl_host_result hl_linux_stream_readiness(void *context, hl_host_handle stream, uint32_t interests) {
    int descriptor = hl_linux_pollable_descriptor(context, stream);
    struct pollfd probe = {descriptor, 0, 0};
    uint32_t ready = 0;
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((interests & HL_HOST_READY_READ) != 0) probe.events |= POLLIN;
    if ((interests & HL_HOST_READY_WRITE) != 0) probe.events |= POLLOUT;
    if (poll(&probe, 1, 0) < 0) {
        hl_host_result error = hl_linux_errno_result();
        close(descriptor);
        return error;
    }
    if ((probe.revents & (POLLIN | POLLHUP)) != 0) ready |= HL_HOST_READY_READ;
    if ((probe.revents & POLLOUT) != 0) ready |= HL_HOST_READY_WRITE;
    if ((probe.revents & POLLERR) != 0) ready |= HL_HOST_READY_ERROR;
    if ((probe.revents & POLLHUP) != 0) ready |= HL_HOST_READY_HANGUP;
    close(descriptor);
    return hl_linux_result(HL_STATUS_OK, ready & interests, 0);
}

static hl_host_result hl_linux_stream_move(void *context, hl_host_handle source, uint64_t source_offset,
                                           hl_host_handle destination, uint64_t destination_offset, uint64_t size,
                                           uint32_t flags) {
    hl_host_linux *host = context;
    int input, output;
    off_t input_offset, output_offset;
    off_t *input_pointer = NULL, *output_pointer = NULL;
    ssize_t result;
    sigset_t blocked, previous;
    int saved_error;
    uint32_t allowed = HL_HOST_STREAM_SOURCE_POSITIONED | HL_HOST_STREAM_DESTINATION_POSITIONED;
    int source_file = 0, destination_file = 0;
    if ((flags & ~allowed) != 0 || source_offset > INT64_MAX || destination_offset > INT64_MAX || size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    input = hl_linux_descriptor(host, source, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (input < 0) {
        input = hl_linux_descriptor(host, source, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        source_file = input >= 0;
    }
    output = hl_linux_descriptor(host, destination, HL_LINUX_HANDLE_STREAM, HL_LINUX_HANDLE_STREAM);
    if (output < 0) {
        output = hl_linux_descriptor(host, destination, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        destination_file = output >= 0;
    }
    if (input >= 0) input = fcntl(input, F_DUPFD_CLOEXEC, 0);
    if (output >= 0) output = fcntl(output, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (input < 0 || output < 0) {
        if (input >= 0) close(input);
        if (output >= 0) close(output);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((source_file && destination_file) || ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0 && !source_file) ||
        ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0 && !destination_file) ||
        (source_file && (flags & HL_HOST_STREAM_SOURCE_POSITIONED) == 0) ||
        (destination_file && (flags & HL_HOST_STREAM_DESTINATION_POSITIONED) == 0)) {
        close(input);
        close(output);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    input_offset = (off_t)source_offset;
    output_offset = (off_t)destination_offset;
    if ((flags & HL_HOST_STREAM_SOURCE_POSITIONED) != 0) input_pointer = &input_offset;
    if ((flags & HL_HOST_STREAM_DESTINATION_POSITIONED) != 0) output_pointer = &output_offset;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    result = splice(input, input_pointer, output, output_pointer, (size_t)size, 0);
    saved_error = errno;
    if (result < 0 && saved_error == EPIPE && !sigismember(&previous, SIGPIPE)) {
        struct timespec immediate = {0, 0};
        (void)sigtimedwait(&blocked, NULL, &immediate);
    }
    pthread_sigmask(SIG_SETMASK, &previous, NULL);
    errno = saved_error;
    hl_host_result moved = result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
    close(input);
    close(output);
    return moved;
}

