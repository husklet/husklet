static hl_macos_file *hl_macos_file_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_FILE, host->file_capacity, &index) ||
        !host->files[index].active || host->files[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->files[index];
}

static hl_host_result hl_macos_file_register(hl_host_macos *host, int descriptor, int append_descriptor,
                                             uint32_t shared) {
    uint32_t index;
    hl_host_handle handle = 0;
    if (descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        descriptor = adopted;
    }
    if (append_descriptor >= 0) {
        int adopted = hl_host_process_fd_private_adopt(append_descriptor);
        if (adopted < 0) {
            hl_host_process_fd_private_remove(descriptor);
            close(descriptor);
            return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
        }
        append_descriptor = adopted;
    }
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->file_capacity; ++index) {
        hl_macos_file *file = &host->files[index];
        if (!file->active) {
            file->generation++;
            if (file->generation == 0) file->generation = 1;
            file->active = 1;
            file->shared = shared;
            file->descriptor = descriptor;
            file->append_descriptor = append_descriptor;
            file->stream = NULL;
            file->stream_endpoint = 0;
            file->directory = NULL;
            file->directory_position = 0;
            file->directory_shared = NULL;
            handle = hl_macos_handle(HL_MACOS_HANDLE_FILE, index, file->generation);
            break;
        }
    }
    if (handle == 0) {
        uint32_t capacity =
            host->file_capacity > UINT32_C(0x0ffffffe) / 2u ? UINT32_C(0x0ffffffe) : host->file_capacity * 2u;
        hl_macos_file *grown =
            capacity > host->file_capacity ? realloc(host->files, (size_t)capacity * sizeof(*grown)) : NULL;
        if (grown != NULL) {
            memset(grown + host->file_capacity, 0, (size_t)(capacity - host->file_capacity) * sizeof(*grown));
            index = host->file_capacity;
            host->files = grown;
            host->file_capacity = capacity;
            hl_macos_file *file = &host->files[index];
            file->generation = 1;
            file->active = 1;
            file->shared = shared;
            file->descriptor = descriptor;
            file->append_descriptor = append_descriptor;
            file->stream = NULL;
            file->stream_endpoint = 0;
            file->directory = NULL;
            file->directory_position = 0;
            file->directory_shared = NULL;
            handle = hl_macos_handle(HL_MACOS_HANDLE_FILE, index, file->generation);
        }
    }
    pthread_mutex_unlock(&host->lock);
    if (handle == 0) {
        hl_host_process_fd_private_remove(descriptor);
        hl_host_process_fd_private_remove(append_descriptor);
    }
    return handle != 0 ? hl_macos_result(HL_STATUS_OK, handle, 0) : hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

static hl_host_result hl_macos_file_open(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t access, uint32_t creation, uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd = AT_FDCWD;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        pthread_mutex_lock(&host->lock);
        hl_macos_file *file = hl_macos_file_lookup(host, directory);
        directory_fd = file != NULL ? file->descriptor : -1;
        pthread_mutex_unlock(&host->lock);
        if (directory_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if ((access & HL_HOST_FILE_PATH_ONLY) != 0)
#ifdef O_SYMLINK
        flags = (access & HL_HOST_FILE_NOFOLLOW) != 0 ? O_SYMLINK : O_RDONLY | O_NONBLOCK;
#else
        flags = O_RDONLY | O_NONBLOCK;
#endif
    else if ((access & (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE)) == (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE))
        flags = O_RDWR;
    else if ((access & HL_HOST_FILE_WRITE) != 0)
        flags = O_WRONLY;
    else if ((access & HL_HOST_FILE_READ) != 0)
        flags = O_RDONLY;
    else
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
#ifdef O_NOFOLLOW
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0 && (access & HL_HOST_FILE_PATH_ONLY) == 0) flags |= O_NOFOLLOW;
#endif
#ifdef O_DIRECTORY
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) flags |= O_DIRECTORY;
#endif
    /* PATH_ONLY models Linux O_PATH. O_CREAT/O_EXCL/O_TRUNC are ignored by
       that API even though macOS has no native O_PATH flag. */
    if ((access & HL_HOST_FILE_PATH_ONLY) == 0) {
        if ((creation & HL_HOST_FILE_CREATE) != 0) flags |= O_CREAT;
        if ((creation & HL_HOST_FILE_EXCLUSIVE) != 0) flags |= O_EXCL;
        if ((creation & HL_HOST_FILE_TRUNCATE) != 0) flags |= O_TRUNC;
    }
    if ((access & HL_HOST_FILE_APPEND) != 0) flags |= O_APPEND;
    descriptor = openat(directory_fd, local, flags | O_CLOEXEC, (mode_t)(permissions & 07777u));
    if (descriptor < 0) return hl_macos_errno();
    if ((access & HL_HOST_FILE_APPEND) != 0) {
        append_descriptor = dup(descriptor);
        if (append_descriptor < 0) {
            hl_host_result error = hl_macos_errno();
            close(descriptor);
            return error;
        }
    }
    hl_host_result result = hl_macos_file_register(host, descriptor, append_descriptor, 0);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
    }
    return result;
}

static int hl_macos_file_descriptor(hl_host_macos *host, hl_host_handle handle, int append);

static hl_host_result hl_macos_file_standard_stream(void *context, uint32_t stream) {
    hl_host_macos *host = context;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    uint32_t detail = 0;
    hl_host_result result;
    if (stream > HL_HOST_STANDARD_ERROR) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    flags = fcntl((int)stream, F_GETFL);
    if (flags < 0) return hl_macos_errno();
    descriptor = fcntl((int)stream, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_errno();
    if ((flags & O_ACCMODE) == O_RDONLY)
        detail |= HL_HOST_FILE_READ;
    else if ((flags & O_ACCMODE) == O_WRONLY)
        detail |= HL_HOST_FILE_WRITE;
    else if ((flags & O_ACCMODE) == O_RDWR)
        detail |= HL_HOST_FILE_READ | HL_HOST_FILE_WRITE;
    if ((flags & O_APPEND) != 0) {
        detail |= HL_HOST_FILE_APPEND;
        append_descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
        if (append_descriptor < 0) {
            hl_host_result error = hl_macos_errno();
            close(descriptor);
            return error;
        }
    }
    if ((flags & O_NONBLOCK) != 0) detail |= HL_HOST_FILE_NONBLOCK;
    result = hl_macos_file_register(host, descriptor, append_descriptor, 0);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
        return result;
    }
    result.detail = detail;
    return result;
}

static hl_host_result hl_macos_file_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    char path[PATH_MAX];
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX || descriptor < 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fcntl(descriptor, F_GETPATH, path) != 0) return hl_macos_errno();
    do
        count = readlink(path, output.data, output.size);
    while (count < 0 && errno == EINTR);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_set_owner(void *context, hl_host_handle file, uint32_t uid, uint32_t gid) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    int status;
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchown(descriptor, (uid_t)uid, (gid_t)gid);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_set_permissions(void *context, hl_host_handle file, uint32_t permissions) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    int status;
    if (descriptor < 0 || (permissions & ~07777u) != 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchmod(descriptor, (mode_t)permissions);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_set_times(void *context, hl_host_handle file, const hl_host_file_time times[2]) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    struct timespec native[2];
    int status;
    if (descriptor < 0 || times == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (int index = 0; index < 2; ++index) {
        if (times[index].mode == HL_HOST_FILE_TIME_NOW) {
            native[index].tv_sec = 0;
            native[index].tv_nsec = UTIME_NOW;
        } else if (times[index].mode == HL_HOST_FILE_TIME_OMIT) {
            native[index].tv_sec = 0;
            native[index].tv_nsec = UTIME_OMIT;
        } else if (times[index].mode == HL_HOST_FILE_TIME_EXPLICIT && times[index].nanoseconds < 1000000000u) {
            native[index].tv_sec = (time_t)times[index].seconds;
            native[index].tv_nsec = (long)times[index].nanoseconds;
        } else {
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
    }
    do
        status = futimens(descriptor, native);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static int hl_macos_file_descriptor(hl_host_macos *host, hl_host_handle handle, int append) {
    hl_macos_file *file;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    descriptor = file == NULL ? -1 : (append ? file->append_descriptor : file->descriptor);
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_host_result hl_macos_attachment_borrow_file_at_least(void *context, hl_host_handle handle,
                                                               uint32_t minimum_descriptor) {
    hl_host_macos *host = context;
    int descriptor = -1;
    int found;
    if (minimum_descriptor > INT_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, handle);
    found = file != NULL;
    if (found) descriptor = fcntl(file->descriptor, F_DUPFD_CLOEXEC, (int)minimum_descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return found ? hl_macos_errno() : hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    return hl_macos_result(HL_STATUS_OK, (uint64_t)(unsigned)descriptor, 0);
}

static hl_host_result hl_macos_attachment_borrow_file(void *context, hl_host_handle handle) {
    return hl_macos_attachment_borrow_file_at_least(context, handle, 0);
}

static hl_host_result hl_macos_attachment_release(void *context, uint64_t borrowed_descriptor) {
    (void)context;
    if (borrowed_descriptor > INT_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int descriptor = (int)borrowed_descriptor;
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_read(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || descriptor < 0 || offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pread(descriptor, output.data, output.size, (off_t)offset);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_write(void *context, hl_host_handle file, uint64_t offset,
                                          hl_host_const_bytes input) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || descriptor < 0 || offset > INT64_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pwrite(descriptor, input.data, input.size, (off_t)offset);
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_read_sequential(void *context, hl_host_handle file, void *output,
                                                    uint64_t output_size) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *stream = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    ssize_t count;
    int saved_error;
    if ((output_size != 0 && output == NULL) || output_size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        descriptor = dup(entry->descriptor);
        stream = entry->stream;
        endpoint = entry->stream_endpoint;
        if (descriptor >= 0 && stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (stream != NULL && hl_macos_stream_lock(stream, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_errno();
    }
    count = read(descriptor, output, (size_t)output_size);
    saved_error = errno;
    if (stream != NULL) hl_macos_stream_unlock(stream, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(stream);
    errno = saved_error;
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_write_sequential(void *context, hl_host_handle file, const void *input,
                                                     uint64_t input_size) {
    hl_host_macos *host = context;
    hl_macos_stream_shared *stream = NULL;
    uint32_t endpoint = 0;
    int descriptor = -1;
    ssize_t count;
    int saved_error;
    if ((input_size != 0 && input == NULL) || input_size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        descriptor = dup(entry->descriptor);
        stream = entry->stream;
        endpoint = entry->stream_endpoint;
        if (descriptor >= 0 && stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    descriptor = adopted;
    if (stream != NULL && hl_macos_stream_lock(stream, endpoint) != 0) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        hl_macos_stream_release(stream);
        return hl_macos_errno();
    }
    count = write(descriptor, input, (size_t)input_size);
    saved_error = errno;
    if (stream != NULL) hl_macos_stream_unlock(stream, endpoint);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    hl_macos_stream_release(stream);
    errno = saved_error;
    return count < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_clone_for_fork(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    hl_macos_file *entry;
    int descriptor = -1;
    int append_descriptor = -1;
    int needs_append = 0;
    uint32_t shared = 0;
    hl_macos_stream_shared *stream = NULL;
    uint32_t stream_endpoint = 0;
    hl_macos_directory_shared *directory_shared = NULL;
    int directory_error = 0;
    pthread_mutex_lock(&host->lock);
    entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) {
        needs_append = entry->append_descriptor >= 0;
        shared = entry->shared;
        stream = entry->stream;
        stream_endpoint = entry->stream_endpoint;
        struct stat status;
        if (entry->directory_shared == NULL && fstat(entry->descriptor, &status) == 0 && S_ISDIR(status.st_mode)) {
            entry->directory_shared = hl_macos_directory_shared_create();
            if (entry->directory_shared == NULL) directory_error = 1;
        }
        directory_shared = entry->directory_shared;
        if (directory_shared != NULL) (void)__atomic_add_fetch(&directory_shared->references, 1u, __ATOMIC_ACQ_REL);
        if (stream != NULL) (void)__atomic_add_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
        descriptor = fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
        if (descriptor >= 0 && needs_append) append_descriptor = fcntl(entry->append_descriptor, F_DUPFD_CLOEXEC, 0);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || (needs_append && append_descriptor < 0) || directory_error) {
        hl_host_result error = directory_error ? hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0) : hl_macos_errno();
        if (stream != NULL) (void)__atomic_sub_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
        hl_macos_directory_shared_release(directory_shared);
        if (descriptor >= 0) close(descriptor);
        return error;
    }
    {
        hl_host_result result = hl_macos_file_register(host, descriptor, append_descriptor, shared);
        if (result.status != HL_STATUS_OK) {
            if (stream != NULL) (void)__atomic_sub_fetch(&stream->references, 1u, __ATOMIC_ACQ_REL);
            hl_macos_directory_shared_release(directory_shared);
            close(descriptor);
            if (append_descriptor >= 0) close(append_descriptor);
        } else {
            pthread_mutex_lock(&host->lock);
            hl_macos_file *copy = hl_macos_file_lookup(host, result.value);
            if (copy != NULL) {
                copy->directory_shared = directory_shared;
                copy->stream = stream;
                copy->stream_endpoint = stream_endpoint;
            }
            pthread_mutex_unlock(&host->lock);
        }
        return result;
    }
}

/* Some virtual/shared macOS filesystems reject SEEK_DATA/SEEK_HOLE with ENOTTY even for regular files.
 * Preserve the Linux contract there by discovering logical zero/data runs. Native APFS/HFS extents remain
 * authoritative whenever the filesystem implements the operation. */
static off_t hl_macos_sparse_seek_fallback(int descriptor, off_t offset, uint32_t whence) {
    unsigned char bytes[16384];
    struct stat metadata;
    off_t cursor = offset;
    int want_data = whence == HL_HOST_FILE_SEEK_DATA;
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (fstat(descriptor, &metadata) != 0) return -1;
    if (offset >= metadata.st_size) {
        errno = ENXIO;
        return -1;
    }
    while (cursor < metadata.st_size) {
        size_t amount =
            (uint64_t)(metadata.st_size - cursor) < sizeof(bytes) ? (size_t)(metadata.st_size - cursor) : sizeof(bytes);
        ssize_t count = pread(descriptor, bytes, amount, cursor);
        if (count <= 0) {
            if (count == 0) errno = ENXIO;
            return -1;
        }
        for (ssize_t index = 0; index < count; ++index)
            if ((bytes[index] != 0) == want_data) return cursor + index;
        cursor += count;
    }
    if (!want_data) return metadata.st_size;
    errno = ENXIO;
    return -1;
}

static hl_host_result hl_macos_file_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_macos *host = context;
    int descriptor = -1;
    off_t result;
    if (whence > HL_HOST_FILE_SEEK_HOLE) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry != NULL) descriptor = entry->descriptor;
    if (entry != NULL && entry->directory_shared != NULL) {
        hl_macos_directory_shared *shared = entry->directory_shared;
        pthread_mutex_lock(&shared->lock);
        if (whence == SEEK_SET)
            result = (off_t)offset;
        else if (whence == SEEK_CUR && shared->position <= INT64_MAX)
            result = (off_t)shared->position + (off_t)offset;
        else
            result = -1;
        if (result >= 0) {
            shared->position = (uint64_t)result;
            if (entry->directory != NULL) {
                rewinddir(entry->directory);
                entry->directory_position = 0;
            }
        } else {
            errno = EINVAL;
        }
        pthread_mutex_unlock(&shared->lock);
        pthread_mutex_unlock(&host->lock);
        return result < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)result, 0);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (whence == HL_HOST_FILE_SEEK_DATA)
        result = lseek(descriptor, (off_t)offset, SEEK_DATA);
    else if (whence == HL_HOST_FILE_SEEK_HOLE)
        result = lseek(descriptor, (off_t)offset, SEEK_HOLE);
    else
        result = lseek(descriptor, (off_t)offset, (int)whence);
    if (result < 0 && (whence == HL_HOST_FILE_SEEK_DATA || whence == HL_HOST_FILE_SEEK_HOLE) && errno != EBADF &&
        errno != ESPIPE) {
        result = hl_macos_sparse_seek_fallback(descriptor, (off_t)offset, whence);
        if (result >= 0 && lseek(descriptor, result, SEEK_SET) < 0) result = -1;
    }
    return result < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_OK, (uint64_t)result, 0);
}

