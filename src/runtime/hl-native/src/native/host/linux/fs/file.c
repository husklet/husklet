static int hl_linux_descriptor(hl_host_linux *host, hl_host_handle handle, hl_linux_handle_kind first,
                               hl_linux_handle_kind second) {
    uint32_t low = (uint32_t)handle;
    uint32_t index;
    hl_linux_handle_entry *entry;
    if (handle == HL_HOST_HANDLE_CWD) return AT_FDCWD;
    if (low == 0) return -1;
    index = low - 1u;
    if (index >= host->handle_capacity) return -1;
    entry = &host->handles[index];
    if (entry->generation != (uint32_t)(handle >> 32) || (entry->kind != first && entry->kind != second)) return -1;
    return entry->descriptor;
}

static hl_host_result hl_linux_file_open(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t access, uint32_t creation, uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    int descriptor;
    int append_descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int flags;
    if ((access & HL_HOST_FILE_PATH_ONLY) != 0)
        flags = O_PATH;
    else if ((access & HL_HOST_FILE_READ) != 0 && (access & HL_HOST_FILE_WRITE) != 0)
        flags = O_RDWR;
    else if ((access & HL_HOST_FILE_WRITE) != 0)
        flags = O_WRONLY;
    else if ((access & HL_HOST_FILE_READ) != 0)
        flags = O_RDONLY;
    else
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((access & HL_HOST_FILE_NOFOLLOW) != 0) flags |= O_NOFOLLOW;
    if ((access & HL_HOST_FILE_DIRECTORY) != 0) flags |= O_DIRECTORY;
    /* Linux O_PATH ignores creation and truncation flags. Keep that contract
       in the portable service instead of relying on host-specific open flags. */
    if ((access & HL_HOST_FILE_PATH_ONLY) == 0) {
        if ((creation & HL_HOST_FILE_CREATE) != 0) flags |= O_CREAT;
        if ((creation & HL_HOST_FILE_EXCLUSIVE) != 0) flags |= O_EXCL;
        if ((creation & HL_HOST_FILE_TRUNCATE) != 0) flags |= O_TRUNC;
    }
    descriptor = openat(directory_fd, local, flags | O_CLOEXEC, (mode_t)(permissions & 07777u));
    if (descriptor < 0) return hl_linux_errno_result();
    if ((access & HL_HOST_FILE_APPEND) != 0) {
        char descriptor_path[64];
        /* O_NOFOLLOW governs the caller's original path.  The trusted
         * /proc/self/fd indirection below must follow its magic link to the
         * already-validated file description. */
        int append_flags = flags & ~(O_CREAT | O_EXCL | O_TRUNC | O_NOFOLLOW);
        int length = snprintf(descriptor_path, sizeof(descriptor_path), "/proc/self/fd/%d", descriptor);
        if (length < 0 || (size_t)length >= sizeof(descriptor_path)) {
            close(descriptor);
            return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
        }
        append_descriptor = open(descriptor_path, append_flags | O_APPEND | O_CLOEXEC, 0);
        if (append_descriptor < 0) {
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            return error;
        }
        {
            struct stat primary_status;
            struct stat append_status;
            if (fstat(descriptor, &primary_status) != 0 || fstat(append_descriptor, &append_status) != 0 ||
                primary_status.st_dev != append_status.st_dev || primary_status.st_ino != append_status.st_ino) {
                hl_host_result error = hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
                close(append_descriptor);
                close(descriptor);
                return error;
            }
        }
    }
    hl_host_result result =
        hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
    }
    return result;
}

static hl_host_result hl_linux_file_standard_stream(void *context, uint32_t stream) {
    hl_host_linux *host = context;
    int flags;
    int descriptor;
    int append_descriptor = -1;
    uint32_t detail = 0;
    hl_host_result result;
    if (stream > HL_HOST_STANDARD_ERROR) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    flags = fcntl((int)stream, F_GETFL);
    if (flags < 0) return hl_linux_errno_result();
    descriptor = fcntl((int)stream, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_linux_errno_result();
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
            hl_host_result error = hl_linux_errno_result();
            close(descriptor);
            return error;
        }
    }
    if ((flags & O_NONBLOCK) != 0) detail |= HL_HOST_FILE_NONBLOCK;
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
        return result;
    }
    result.detail = detail;
    return result;
}

hl_host_result hl_host_linux_import_file(hl_host_linux *host, int source) {
    int flags;
    int descriptor;
    int append_descriptor = -1;
    uint32_t detail = 0;
    hl_host_result result;
    if (host == NULL || source < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    flags = fcntl(source, F_GETFL);
    if (flags < 0) return hl_linux_errno_result();
    descriptor = fcntl(source, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_linux_errno_result();
    if ((flags & O_ACCMODE) == O_RDONLY)
        detail |= HL_HOST_FILE_READ;
    else if ((flags & O_ACCMODE) == O_WRONLY)
        detail |= HL_HOST_FILE_WRITE;
    else
        detail |= HL_HOST_FILE_READ | HL_HOST_FILE_WRITE;
    if ((flags & O_APPEND) != 0) {
        detail |= HL_HOST_FILE_APPEND;
        append_descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
        if (append_descriptor < 0) {
            result = hl_linux_errno_result();
            close(descriptor);
            return result;
        }
    }
    if ((flags & O_NONBLOCK) != 0) detail |= HL_HOST_FILE_NONBLOCK;
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
    if (result.status != HL_STATUS_OK) {
        close(descriptor);
        if (append_descriptor >= 0) close(append_descriptor);
        return result;
    }
    result.detail = detail;
    return result;
}

static hl_host_result hl_linux_file_validate_private_regular(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    struct stat st;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_linux_errno_result();
    return S_ISREG(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_linux_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                         size_t path_size, hl_host_const_bytes input,
                                                         uint32_t permissions) {
    static _Atomic uint64_t sequence;
    hl_host_linux *host = context;
    char name[PATH_MAX], temporary[PATH_MAX];
    int directory_fd = AT_FDCWD, descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(name) || memchr(path, '\0', path_size) != NULL ||
        (permissions & ~0777u) != 0 || (input.size != 0 && input.data == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(name, path, path_size);
    name[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        pthread_mutex_lock(&host->lock);
        directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
        if (directory_fd >= 0) directory_fd = fcntl(directory_fd, F_DUPFD_CLOEXEC, 0);
        pthread_mutex_unlock(&host->lock);
        if (directory_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
        return hl_linux_errno_result();
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
    return ok ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_validate_private_directory(void *context, hl_host_handle directory) {
    hl_host_linux *host = context;
    struct stat st;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_linux_errno_result();
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_linux_file_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        count = readlinkat(descriptor, "", output.data, output.size);
    while (count < 0 && errno == EINTR);
    if (count < 0) {
        /* An empty-path readlinkat only names the link node when the fd is an O_PATH|O_NOFOLLOW handle on a
           SYMLINK; on a regular file / directory the kernel returns ENOENT for the empty path, but Linux's
           readlink(2) contract is EINVAL ("not a symbolic link"). Distinguish the two so a readlinkat on a
           non-symlink reports EINVAL, matching native. */
        int saved = errno;
        struct stat metadata;
        if ((saved == ENOENT || saved == EINVAL) && fstatat(descriptor, "", &metadata, AT_EMPTY_PATH) == 0 &&
            !S_ISLNK(metadata.st_mode))
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        errno = saved;
        return hl_linux_errno_result();
    }
    return hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_linux_file_set_owner(void *context, hl_host_handle file, uint32_t uid, uint32_t gid) {
    hl_host_linux *host = context;
    int descriptor;
    int status;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    do
        status = fchownat(descriptor, "", (uid_t)uid, (gid_t)gid, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_set_permissions(void *context, hl_host_handle file, uint32_t permissions) {
    hl_host_linux *host = context;
    int descriptor;
    int status;
    if ((permissions & ~07777u) != 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* File-service path handles are deliberately opened with O_PATH. fchmod(2) rejects those descriptors;
       operate on the pinned object through fchmodat2's empty relative path instead, preserving the handle
       identity without resolving the caller's original pathname again.  The legacy fchmodat syscall has no
       flags argument on Linux; libc emulation cannot implement AT_EMPTY_PATH for an O_PATH descriptor and
       returns EPERM on overlay-backed files. */
    do
        status = (int)syscall(452 /* fchmodat2 */, descriptor, "", (mode_t)permissions, AT_EMPTY_PATH);
    while (status != 0 && errno == EINTR);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_attachment_borrow_file_at_least(void *context, hl_host_handle file,
                                                               uint32_t minimum_descriptor) {
    hl_host_linux *host = context;
    int descriptor;
    int borrowed;
    if (minimum_descriptor > INT_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    borrowed = descriptor < 0 ? -1 : fcntl(descriptor, F_DUPFD_CLOEXEC, (int)minimum_descriptor);
    pthread_mutex_unlock(&host->lock);
    if (borrowed < 0)
        return descriptor < 0 ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_linux_errno_result();
    int adopted = hl_host_process_fd_private_adopt(borrowed);
    if (adopted < 0) {
        close(borrowed);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    borrowed = adopted;
    return hl_linux_result(HL_STATUS_OK, (uint64_t)(unsigned)borrowed, 0);
}

static hl_host_result hl_linux_attachment_borrow_file(void *context, hl_host_handle file) {
    return hl_linux_attachment_borrow_file_at_least(context, file, 0);
}

static hl_host_result hl_linux_attachment_release(void *context, uint64_t borrowed_descriptor) {
    (void)context;
    if (borrowed_descriptor > INT_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int descriptor = (int)borrowed_descriptor;
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_set_times(void *context, hl_host_handle file, const hl_host_file_time times[2]) {
    hl_host_linux *host = context;
    struct timespec native[2];
    int descriptor;
    int status;
    if (times == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        }
    }
    do
        status = futimens(descriptor, native);
    while (status != 0 && errno == EINTR);
    /* futimens(2) rejects an O_PATH descriptor with EBADF, and PATH_ONLY opens
       are exactly O_PATH here -- utimensat(dirfd, RELATIVE) resolves its target
       with PATH_ONLY, so every *at time-setter through a directory handle failed
       (guest EIO). macOS models PATH_ONLY as a real O_RDONLY open, which is why
       only the Linux engine broke, and why no lane caught it: compat-core-syscall
       ran on macOS only. /proc/self/fd/<n> is the documented way to reach the
       inode behind an O_PATH fd. No AT_SYMLINK_NOFOLLOW: the magic link must be
       traversed to reach the target at all. */
    if (status != 0 && errno == EBADF) {
        char descriptor_path[64];
        snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", descriptor);
        do
            status = utimensat(AT_FDCWD, descriptor_path, native, 0);
        while (status != 0 && errno == EINTR);
    }
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_read(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if (output.size != 0 && output.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || offset > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pread(descriptor, output.data, output.size, (off_t)offset);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_write(void *context, hl_host_handle file, uint64_t offset,
                                          hl_host_const_bytes input) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || offset > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = pwrite(descriptor, input.data, input.size, (off_t)offset);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_read_sequential(void *context, hl_host_handle file, void *output,
                                                    uint64_t output_size) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((output_size != 0 && output == NULL) || output_size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output, (size_t)output_size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_write_sequential(void *context, hl_host_handle file, const void *input,
                                                     uint64_t input_size) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if ((input_size != 0 && input == NULL) || input_size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input, (size_t)input_size);
    return count >= 0 ? hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_clone_for_fork(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)file;
    hl_linux_handle_entry *entry = NULL;
    int descriptor = -1;
    int append_descriptor = -1;
    int needs_append = 0;
    pthread_mutex_lock(&host->lock);
    if (low != 0 && low - 1u < host->handle_capacity) entry = &host->handles[low - 1u];
    if (entry != NULL && entry->generation == (uint32_t)(file >> 32) && entry->kind == HL_LINUX_HANDLE_FILE) {
        needs_append = entry->wake_descriptor >= 0;
        descriptor = dup(entry->descriptor);
        if (descriptor >= 0 && needs_append) append_descriptor = dup(entry->wake_descriptor);
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || (needs_append && append_descriptor < 0)) {
        hl_host_result error = hl_linux_errno_result();
        if (descriptor >= 0) close(descriptor);
        return error;
    }
    {
        hl_host_result result =
            hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, descriptor, NULL, NULL, 0, append_descriptor);
        if (result.status != HL_STATUS_OK) {
            close(descriptor);
            if (append_descriptor >= 0) close(append_descriptor);
        }
        return result;
    }
}

static hl_host_result hl_linux_file_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_linux *host = context;
    int descriptor;
    off_t result;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || whence > HL_HOST_FILE_SEEK_HOLE) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    result = lseek(descriptor, (off_t)offset, (int)whence);
    return result < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_OK, (uint64_t)result, 0);
}

/*
 * The descriptor an appending write must issue on, established on demand.
 *
 * O_APPEND belongs to the open file description and a guest may turn it on long after the open:
 * fcntl(F_SETFL) is exactly that, and descriptors 0, 1 and 2 are adopted with whatever flags the
 * launching process left on them. The open paths above can only establish an appending view for a
 * description that ALREADY carried O_APPEND, so a later F_SETFL(O_APPEND) reached an entry whose
 * appending descriptor was still -1 and the write failed EINVAL -- which is why `make --version`
 * through a pipe reported "write error: stdout". Linux never fails that write.
 *
 * What Linux does depends on the object, and so does this:
 *
 *   - a regular or block object has a position, so appending is real behaviour and needs a second
 *     description opened O_APPEND through the /proc/self/fd magic link -- the same indirection
 *     hl_linux_file_open uses, and the same identity check afterwards;
 *   - every other object -- pipe, socket, character device, terminal -- has no position, so the
 *     kernel accepts O_APPEND and ignores it. An ordinary write on this description already IS the
 *     appending write, and a duplicate of the descriptor is that write while keeping one ownership
 *     rule for the field: the handle owns it and close() closes it.
 *
 * Returns the descriptor to write on. On failure returns -1 with errno set, or -1 and errno 0 when
 * the handle itself does not name a live file.
 */
static int hl_linux_file_append_descriptor(hl_host_linux *host, hl_host_handle file) {
    uint32_t low = (uint32_t)file;
    hl_linux_handle_entry *entry;
    struct stat primary_status;
    int descriptor;
    int established;
    int adopted;
    pthread_mutex_lock(&host->lock);
    if (low == 0 || low - 1u >= host->handle_capacity) {
        pthread_mutex_unlock(&host->lock);
        errno = 0;
        return -1;
    }
    entry = &host->handles[low - 1u];
    if (entry->generation != (uint32_t)(file >> 32) || entry->kind != HL_LINUX_HANDLE_FILE) {
        pthread_mutex_unlock(&host->lock);
        errno = 0;
        return -1;
    }
    if (entry->wake_descriptor >= 0) {
        int ready = entry->wake_descriptor;
        pthread_mutex_unlock(&host->lock);
        return ready;
    }
    descriptor = entry->descriptor;
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) {
        errno = 0;
        return -1;
    }
    if (fstat(descriptor, &primary_status) != 0) return -1;
    if (S_ISREG(primary_status.st_mode) || S_ISBLK(primary_status.st_mode)) {
        char descriptor_path[64];
        struct stat append_status;
        int flags = fcntl(descriptor, F_GETFL);
        int length = snprintf(descriptor_path, sizeof(descriptor_path), "/proc/self/fd/%d", descriptor);
        if (flags < 0) return -1;
        if (length < 0 || (size_t)length >= sizeof(descriptor_path)) {
            errno = ENAMETOOLONG;
            return -1;
        }
        established = open(descriptor_path, (flags & O_ACCMODE) | O_APPEND | O_CLOEXEC, 0);
        if (established < 0) return -1;
        if (fstat(established, &append_status) != 0 || append_status.st_dev != primary_status.st_dev ||
            append_status.st_ino != primary_status.st_ino) {
            close(established);
            errno = EINVAL;
            return -1;
        }
    } else {
        established = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
        if (established < 0) return -1;
    }
    adopted = hl_host_process_fd_private_adopt(established);
    if (adopted < 0) {
        close(established);
        errno = EMFILE;
        return -1;
    }
    established = adopted;
    pthread_mutex_lock(&host->lock);
    /* The table may have been grown while this thread was opening, so re-derive the entry. */
    entry = low - 1u < host->handle_capacity ? &host->handles[low - 1u] : NULL;
    if (entry == NULL || entry->generation != (uint32_t)(file >> 32) || entry->kind != HL_LINUX_HANDLE_FILE) {
        pthread_mutex_unlock(&host->lock);
        hl_host_process_fd_private_remove(established);
        close(established);
        errno = 0;
        return -1;
    }
    if (entry->wake_descriptor >= 0) {
        /* Another thread established one first; its descriptor is the one the handle owns. */
        int ready = entry->wake_descriptor;
        pthread_mutex_unlock(&host->lock);
        hl_host_process_fd_private_remove(established);
        close(established);
        return ready;
    }
    entry->wake_descriptor = established;
    pthread_mutex_unlock(&host->lock);
    return established;
}

static hl_host_result hl_linux_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    hl_host_linux *host = context;
    int descriptor;
    ssize_t count;
    if (input.size != 0 && input.data == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    errno = 0;
    descriptor = hl_linux_file_append_descriptor(host, file);
    if (descriptor < 0) return errno == 0 ? hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_linux_errno_result();
    /* The descriptor carries O_APPEND wherever the object has a position: this write is atomic with
     * every other append on the object. */
    count = write(descriptor, input.data, input.size);
    if (count < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_linux_file_vector(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                           uint32_t count, uint64_t offset, int operation) {
    hl_host_linux *host = context;
    struct iovec native[HL_HOST_FILE_IOV_MAX];
    int descriptor;
    ssize_t transferred;
    uint32_t index;
    if (operation == 4) {
        /* Appending vector write: the same on-demand appending descriptor hl_linux_file_append issues on. */
        errno = 0;
        descriptor = hl_linux_file_append_descriptor(host, file);
        if (descriptor < 0 && errno != 0) return hl_linux_errno_result();
    } else {
        pthread_mutex_lock(&host->lock);
        descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
        pthread_mutex_unlock(&host->lock);
    }
    if ((count != 0 && vectors == NULL) || count > HL_HOST_FILE_IOV_MAX || descriptor < 0 ||
        ((operation == 2 || operation == 3) && offset > INT64_MAX))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > SIZE_MAX || vectors[index].address > UINTPTR_MAX)
            return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        native[index].iov_base = (void *)(uintptr_t)vectors[index].address;
        native[index].iov_len = (size_t)vectors[index].size;
    }
    switch (operation) {
    case 0: transferred = readv(descriptor, native, (int)count); break;
    case 1: transferred = writev(descriptor, native, (int)count); break;
    case 2: transferred = preadv(descriptor, native, (int)count, (off_t)offset); break;
    case 3: transferred = pwritev(descriptor, native, (int)count, (off_t)offset); break;
    default: transferred = writev(descriptor, native, (int)count); break;
    }
    if (transferred < 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, (uint64_t)transferred, 0);
}

#define HL_LINUX_VECTOR_WRAPPER(name, operation)                                                                       \
    static hl_host_result name(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {     \
        return hl_linux_file_vector(context, file, vectors, count, 0, operation);                                      \
    }
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_readv, 0)
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_writev, 1)
HL_LINUX_VECTOR_WRAPPER(hl_linux_file_appendv, 4)

static hl_host_result hl_linux_file_truncate(void *context, hl_host_handle file, uint64_t size) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0 || size > INT64_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return ftruncate(descriptor, (off_t)size) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fsync(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync_range(void *context, hl_host_handle file, uint64_t offset, uint64_t size,
                                               uint32_t flags) {
    hl_host_linux *host = context;
    int descriptor;
    unsigned int native = 0;
    if ((flags & ~7u) != 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (flags & HL_HOST_FILE_SYNC_WAIT_BEFORE) native |= SYNC_FILE_RANGE_WAIT_BEFORE;
    if (flags & HL_HOST_FILE_SYNC_WRITE) native |= SYNC_FILE_RANGE_WRITE;
    if (flags & HL_HOST_FILE_SYNC_WAIT_AFTER) native |= SYNC_FILE_RANGE_WAIT_AFTER;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return sync_file_range(descriptor, (off64_t)offset, (off64_t)size, native) == 0
               ? hl_linux_result(HL_STATUS_OK, 0, 0)
               : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_sync_filesystem(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return syncfs(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_data_sync(void *context, hl_host_handle file) {
    hl_host_linux *host = context;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fdatasync(descriptor) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_rename(void *context, hl_host_handle old_directory, const char *old_path,
                                           size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                           size_t new_path_size) {
    hl_host_linux *host = context;
    char old_local[PATH_MAX];
    char new_local[PATH_MAX];
    int old_fd;
    int new_fd;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = '\0';
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    old_fd = hl_linux_descriptor(host, old_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    new_fd = hl_linux_descriptor(host, new_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (renameat(old_fd, old_local, new_fd, new_local) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_unlink(void *context, hl_host_handle directory, const char *path,
                                           size_t path_size) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, 0) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_rmdir(void *context, hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, AT_REMOVEDIR) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_mkdir(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                          uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkdirat(directory_fd, local, (mode_t)permissions) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_fifo(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t permissions) {
    hl_host_linux *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkfifoat(directory_fd, local, (mode_t)permissions) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                   : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_symlink(void *context, const char *target, size_t target_size,
                                            hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_linux *host = context;
    char target_local[PATH_MAX], path_local[PATH_MAX];
    if (target == NULL || path == NULL || target_size == 0 || path_size == 0 || target_size >= sizeof(target_local) ||
        path_size >= sizeof(path_local))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(target_local, target, target_size);
    target_local[target_size] = 0;
    memcpy(path_local, path, path_size);
    path_local[path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int directory_fd = hl_linux_descriptor(host, directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return symlinkat(target_local, directory_fd, path_local) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_link(void *context, hl_host_handle old_directory, const char *old_path,
                                         size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                         size_t new_path_size, uint32_t flags) {
    hl_host_linux *host = context;
    char old_local[PATH_MAX], new_local[PATH_MAX];
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local) || (flags & ~1u) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = 0;
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = 0;
    pthread_mutex_lock(&host->lock);
    int old_fd = hl_linux_descriptor(host, old_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    int new_fd = hl_linux_descriptor(host, new_directory, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int native_flags = (flags & 1u) != 0 ? AT_SYMLINK_FOLLOW : 0;
    return linkat(old_fd, old_local, new_fd, new_local, native_flags) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                           : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset) {
    return hl_linux_file_vector(context, file, vectors, count, offset, 2);
}

static hl_host_result hl_linux_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count, uint64_t offset) {
    return hl_linux_file_vector(context, file, vectors, count, offset, 3);
}

static hl_host_result hl_linux_file_metadata_get(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    hl_host_linux *host = context;
    struct stat status;
    int descriptor;
    if (output == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Define *output before any early return: hl_linux_errno_result() maps errno 0 to HL_STATUS_OK,
     * so a caller keying off .status could otherwise read an unwritten field. */
    memset(output, 0, sizeof(*output));
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstat(descriptor, &status) != 0) return hl_linux_errno_result();
    output->stable_device = (uint64_t)status.st_dev;
    output->stable_object = (uint64_t)status.st_ino;
    output->size = (uint64_t)status.st_size;
    output->allocated_size = (uint64_t)status.st_blocks * 512u;
    output->modified_ns = (uint64_t)status.st_mtim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtim.tv_nsec;
    output->accessed_ns = (uint64_t)status.st_atim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_atim.tv_nsec;
    output->changed_ns = (uint64_t)status.st_ctim.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctim.tv_nsec;
    /* A plain fstat() carries no birth time, so consult statx() for it: a filesystem that tracks
       creation time (tmpfs/ext4) reports STATX_BTIME, and leaving created_ns non-zero here lets an
       AT_EMPTY_PATH statx advertise the mask bit honestly -- byte-identical to native and to the
       path-based statx (fs.c hl_statx_host_btime). Filesystems that do not track it (procfs) leave
       the bit clear, so created_ns stays 0. Only statx consumes created_ns; plain stat ignores it. */
#if defined(SYS_statx) && defined(STATX_BTIME)
    {
        struct statx birth;
        memset(&birth, 0, sizeof birth);
        if (syscall(SYS_statx, descriptor, "", AT_EMPTY_PATH, STATX_BTIME, &birth) == 0 &&
            (birth.stx_mask & STATX_BTIME) != 0)
            output->created_ns =
                (uint64_t)birth.stx_btime.tv_sec * UINT64_C(1000000000) + (uint64_t)birth.stx_btime.tv_nsec;
    }
#endif
    output->device = (uint64_t)status.st_rdev;
    output->link_count = (uint64_t)status.st_nlink;
    output->user = (uint32_t)status.st_uid;
    output->group = (uint32_t)status.st_gid;
    output->permissions = (uint32_t)status.st_mode & 07777u;
    if (S_ISREG(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_REGULAR;
    else if (S_ISDIR(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_DIRECTORY;
    else if (S_ISLNK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_SYMLINK;
    else if (S_ISCHR(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_CHARACTER;
    else if (S_ISBLK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_BLOCK;
    else if (S_ISFIFO(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_FIFO;
    else if (S_ISSOCK(status.st_mode))
        output->type = HL_HOST_FILE_TYPE_SOCKET;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_resolve_beneath(void *context, hl_host_handle root, const char *path,
                                                    size_t path_size, uint32_t policy,
                                                    hl_host_file_resolution *output) {
    hl_host_linux *host = context;
    hl_host_resolved_path resolved;
    hl_host_result parent;
    hl_host_result target = {HL_STATUS_OK, 0, HL_HOST_HANDLE_INVALID, 0};
    char local[PATH_MAX];
    int root_fd;
    /* Resolution must not participate in special-file I/O.  In particular,
     * opening a FIFO O_RDONLY makes blocked writers runnable and can create a
     * false reader window before the guest opens it.  O_PATH pins identity
     * for metadata without changing FIFO/socket/device lifecycle state. */
    int target_flags = O_PATH;
    if (output == NULL || path == NULL || path_size == 0 || path_size >= sizeof(local) ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS |
                              HL_HOST_RESOLVE_ALLOW_MISSING)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    pthread_mutex_lock(&host->lock);
    root_fd = hl_linux_descriptor(host, root, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (root_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((policy & HL_HOST_RESOLVE_ALLOW_MISSING) != 0) target_flags = -1;
    if (hl_host_resolve_beneath(root_fd, local, policy & (HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS),
                                target_flags, &resolved) != 0)
        return hl_linux_errno_result();
    if (strlen(resolved.leaf) >= sizeof(output->final)) {
        hl_host_resolved_path_destroy(&resolved);
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    parent = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, resolved.parent_fd, NULL, NULL, 0, -1);
    if (parent.status != HL_STATUS_OK) {
        hl_host_resolved_path_destroy(&resolved);
        return parent;
    }
    resolved.parent_fd = -1;
    if (resolved.target_fd >= 0) {
        target = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_FILE, resolved.target_fd, NULL, NULL, 0, -1);
        if (target.status != HL_STATUS_OK) {
            (void)hl_linux_close_descriptor(host, parent.value);
            hl_host_resolved_path_destroy(&resolved);
            return target;
        }
        resolved.target_fd = -1;
    }
    memset(output, 0, sizeof(*output));
    output->parent = parent.value;
    output->target = target.value;
    output->final_size = strlen(resolved.leaf);
    memcpy(output->final, resolved.leaf, output->final_size + 1);
    if (output->target != HL_HOST_HANDLE_INVALID) {
        hl_host_file_metadata metadata;
        if (hl_linux_file_metadata_get(host, output->target, &metadata).status == HL_STATUS_OK)
            output->target_type = metadata.type;
    }
    hl_host_resolved_path_destroy(&resolved);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_open_beneath(void *context, hl_host_handle root, const char *path, size_t path_size,
                                                 uint32_t access, uint32_t creation, uint32_t permissions,
                                                 uint32_t policy) {
    hl_host_file_resolution resolved = {
        .parent = HL_HOST_HANDLE_INVALID,
        .target = HL_HOST_HANDLE_INVALID,
    };
    hl_host_result result;
    if (path == NULL || path_size == 0 || path[0] == '/' || memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    uint32_t resolve_policy = policy | HL_HOST_RESOLVE_ALLOW_MISSING;
    if ((creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        resolve_policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_linux_file_resolve_beneath(context, root, path, path_size, resolve_policy, &resolved);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_linux_file_open(context, resolved.parent, resolved.final, resolved.final_size,
                                access | HL_HOST_FILE_NOFOLLOW, creation, permissions);
    if (resolved.target != HL_HOST_HANDLE_INVALID) (void)hl_linux_close_descriptor(context, resolved.target);
    (void)hl_linux_close_descriptor(context, resolved.parent);
    return result;
}

static int hl_linux_write_zeros(int descriptor, off_t begin, off_t end) {
    static const unsigned char zeros[65536];
    while (begin < end) {
        size_t request = (uint64_t)(end - begin) < sizeof(zeros) ? (size_t)(end - begin) : sizeof(zeros);
        ssize_t count = pwrite(descriptor, zeros, request, begin);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) {
            if (count == 0) errno = EIO;
            return -1;
        }
        begin += count;
    }
    return 0;
}

static hl_host_result hl_linux_file_allocate_range(void *context, hl_host_handle file, uint32_t mode, uint64_t offset,
                                                   uint64_t size) {
    const uint32_t keep = HL_HOST_FILE_ALLOC_KEEP_SIZE;
    const uint32_t punch = HL_HOST_FILE_ALLOC_PUNCH_HOLE;
    const uint32_t collapse = HL_HOST_FILE_ALLOC_COLLAPSE_RANGE;
    const uint32_t zero = HL_HOST_FILE_ALLOC_ZERO_RANGE;
    const uint32_t insert = HL_HOST_FILE_ALLOC_INSERT_RANGE;
    const uint32_t unshare = HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    const uint32_t allowed = HL_HOST_FILE_ALLOC_KEEP_SIZE | HL_HOST_FILE_ALLOC_PUNCH_HOLE |
                             HL_HOST_FILE_ALLOC_COLLAPSE_RANGE | HL_HOST_FILE_ALLOC_ZERO_RANGE |
                             HL_HOST_FILE_ALLOC_INSERT_RANGE | HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    hl_host_linux *host = context;
    struct stat metadata;
    int descriptor;
    if (size == 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size || (mode & ~allowed) != 0 ||
        ((mode & punch) != 0 && (mode & keep) == 0) || ((mode & collapse) != 0 && mode != collapse) ||
        ((mode & insert) != 0 && mode != insert) || ((mode & unshare) != 0 && (mode & ~(unshare | keep)) != 0))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fallocate(descriptor, (int)mode, (off_t)offset, (off_t)size) == 0) return hl_linux_result(HL_STATUS_OK, 0, 0);
    if ((mode & zero) == 0 || (errno != EOPNOTSUPP && errno != ENOSYS)) return hl_linux_errno_result();
    if (fstat(descriptor, &metadata) != 0) return hl_linux_errno_result();
    off_t begin = (off_t)offset;
    off_t end = begin + (off_t)size;
    off_t zero_end = (mode & keep) != 0 && end > metadata.st_size ? metadata.st_size : end;
    if ((mode & keep) == 0 && end > metadata.st_size && ftruncate(descriptor, end) != 0) return hl_linux_errno_result();
    if (begin < zero_end && hl_linux_write_zeros(descriptor, begin, zero_end) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_file_filesystem_metadata(void *context, hl_host_handle file,
                                                        hl_host_filesystem_metadata *output) {
    hl_host_linux *host = context;
    struct statfs status;
    int descriptor;
    if (output == NULL) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstatfs(descriptor, &status) != 0) return hl_linux_errno_result();
    memset(output, 0, sizeof(*output));
    output->blocks = status.f_blocks;
    output->blocks_free = status.f_bfree;
    output->blocks_available = status.f_bavail;
    output->files = status.f_files;
    output->files_free = status.f_ffree;
    output->filesystem_id[0] = (uint32_t)status.f_fsid.__val[0];
    output->filesystem_id[1] = (uint32_t)status.f_fsid.__val[1];
    output->block_size = (uint64_t)status.f_bsize;
    output->fragment_size = (uint64_t)status.f_bsize;
    output->name_max = NAME_MAX;
    output->flags = (uint64_t)status.f_flags;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

typedef struct hl_linux_dirent64 {
    uint64_t object;
    int64_t offset;
    uint16_t record_size;
    uint8_t type;
    char name[];
} hl_linux_dirent64;

static hl_host_result hl_linux_file_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                                   uint32_t entry_capacity, uint32_t byte_capacity) {
    hl_host_linux *host = context;
    int descriptor;
    uint8_t *buffer;
    long count;
    uint32_t at = 0, produced = 0;
    if (entries == NULL || entry_capacity == 0 || byte_capacity < 24 || byte_capacity > UINT32_C(1 << 20))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    buffer = malloc(byte_capacity);
    if (buffer == NULL) return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    do
        count = syscall(SYS_getdents64, descriptor, buffer, byte_capacity);
    while (count < 0 && errno == EINTR);
    if (count < 0) {
        free(buffer);
        return hl_linux_errno_result();
    }
    while (at < (uint32_t)count) {
        const hl_linux_dirent64 *native = (const hl_linux_dirent64 *)(buffer + at);
        size_t maximum, name_size;
        if (native->record_size < 24 || native->record_size > (uint32_t)count - at || produced == entry_capacity) {
            free(buffer);
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        }
        maximum = native->record_size - offsetof(hl_linux_dirent64, name);
        name_size = strnlen(native->name, maximum);
        if (name_size == maximum || name_size > 255) {
            free(buffer);
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        }
        entries[produced].object = native->object;
        entries[produced].next_offset = (uint64_t)native->offset;
        entries[produced].type = native->type;
        entries[produced].name_size = (uint32_t)name_size;
        memcpy(entries[produced].name, native->name, name_size + 1);
        produced++;
        at += native->record_size;
    }
    free(buffer);
    return hl_linux_result(HL_STATUS_OK, produced, (uint64_t)count);
}

static hl_host_result hl_linux_file_path(void *context, hl_host_handle file, hl_host_bytes output) {
    hl_host_linux *host = context;
    char link[64];
    char path[PATH_MAX];
    int descriptor;
    ssize_t length;
    if ((output.size != 0 && output.data == 0) || output.size > SIZE_MAX)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_FILE);
    if (descriptor >= 0) {
        snprintf(link, sizeof link, "/proc/self/fd/%d", descriptor);
        length = readlink(link, path, sizeof path);
    } else {
        length = -1;
    }
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (length < 0) return hl_linux_errno_result();
    if ((uint64_t)length > output.size) return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, (uint64_t)length, 0);
    if (length != 0) memcpy(output.data, path, (size_t)length);
    return hl_linux_result(HL_STATUS_OK, (uint64_t)length, 0);
}

static hl_host_result hl_linux_close_descriptor(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_NONE);
}

static hl_host_result hl_linux_close_descriptor_kind(void *context, hl_host_handle handle,
                                                     hl_linux_handle_kind expected) {
    hl_host_linux *host = context;
    uint32_t low = (uint32_t)handle;
    hl_linux_handle_entry *entry;
    int descriptor;
    int wake_descriptor;
    int result;
    if (low == 0 || low - 1u >= host->handle_capacity) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (expected == HL_LINUX_HANDLE_NONE || expected == HL_LINUX_HANDLE_COUNTER)
        hl_linux_counter_unsubscribe_all(host, handle);
    pthread_mutex_lock(&host->lock);
    entry = &host->handles[low - 1u];
    if (entry->generation != (uint32_t)(handle >> 32) || entry->kind == HL_LINUX_HANDLE_NONE ||
        entry->kind == HL_LINUX_HANDLE_MAPPING || (expected != HL_LINUX_HANDLE_NONE && entry->kind != expected)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = entry->descriptor;
    wake_descriptor = entry->wake_descriptor;
    entry->kind = HL_LINUX_HANDLE_NONE;
    entry->descriptor = -1;
    entry->wake_descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    hl_host_process_fd_private_remove(wake_descriptor);
    result = close(descriptor);
    if (wake_descriptor >= 0) close(wake_descriptor);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_file_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_FILE);
}

static hl_host_result hl_linux_network_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_SOCKET);
}

static hl_host_result hl_linux_shared_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_SHARED_MEMORY);
}

static hl_host_result hl_linux_counter_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_COUNTER);
}

static hl_host_result hl_linux_transfer_close(void *context, hl_host_handle handle) {
    return hl_linux_close_descriptor_kind(context, handle, HL_LINUX_HANDLE_TRANSFER);
}
