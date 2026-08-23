static int64_t hl_linux_file_control(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t size, uint32_t operation) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    hl_status status;
    int64_t result;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (operation == 0 && size > INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if (operation == 0 && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        if (operation == 0)
            host_result = files->truncate == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                                  : files->truncate(linux_abi->host->context, ofd->host_handle, size);
        else if (operation == 1)
            host_result = files->sync == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                              : files->sync(linux_abi->host->context, ofd->host_handle);
        else
            host_result = files->data_sync == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                                   : files->data_sync(linux_abi->host->context, ofd->host_handle);
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_ftruncate(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t size) {
    return hl_linux_file_control(linux_abi, fd, size, 0);
}

int64_t hl_linux_fsync(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_file_control(linux_abi, fd, 0, 1);
}

int64_t hl_linux_fdatasync(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_file_control(linux_abi, fd, 0, 2);
}

static int64_t hl_linux_extended_sync(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t offset, uint64_t size,
                                      uint32_t flags, int filesystem) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    hl_status status;
    int64_t result;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else if (filesystem)
        host_result = files->sync_filesystem == NULL
                          ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                          : files->sync_filesystem(linux_abi->host->context, ofd->host_handle),
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    else
        host_result = files->sync_range == NULL
                          ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                          : files->sync_range(linux_abi->host->context, ofd->host_handle, offset, size, flags),
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_sync_range(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t offset, uint64_t size, uint32_t flags) {
    if ((flags & ~7u) != 0) return -HL_LINUX_EINVAL;
    return hl_linux_extended_sync(linux_abi, fd, offset, size, flags, 0);
}

int64_t hl_linux_sync_filesystem(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_extended_sync(linux_abi, fd, 0, 0, 0, 1);
}

static int64_t hl_linux_openat_install(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                       int32_t directory_fd, hl_host_handle direct_directory, const char *path,
                                       size_t path_size, uint32_t flags, uint32_t mode) {
    const uint32_t supported = HL_LINUX_O_ACCMODE | HL_LINUX_O_CREAT | HL_LINUX_O_EXCL | HL_LINUX_O_TRUNC |
                               HL_LINUX_O_APPEND | HL_LINUX_O_NONBLOCK | HL_LINUX_O_NOFOLLOW | HL_LINUX_O_DIRECTORY |
                               HL_LINUX_O_PATH | HL_LINUX_O_CLOEXEC;
    const hl_host_file_services *files;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *directory_ofd = NULL;
    hl_host_handle directory = direct_directory == HL_HOST_HANDLE_INVALID ? HL_HOST_HANDLE_CWD : direct_directory;
    hl_host_result opened;
    hl_linux_fd installed;
    uint32_t access;
    uint32_t creation = 0;
    hl_status status;
    if (linux_abi == NULL || path == NULL || path_size == 0 || (flags & ~supported) != 0) return -HL_LINUX_EINVAL;
    switch (flags & HL_LINUX_O_ACCMODE) {
    case HL_LINUX_O_RDONLY: access = HL_HOST_FILE_READ; break;
    case HL_LINUX_O_WRONLY: access = HL_HOST_FILE_WRITE; break;
    case HL_LINUX_O_RDWR: access = HL_HOST_FILE_READ | HL_HOST_FILE_WRITE; break;
    default: return -HL_LINUX_EINVAL;
    }
    if ((flags & HL_LINUX_O_APPEND) != 0) access |= HL_HOST_FILE_APPEND;
    if ((flags & HL_LINUX_O_NONBLOCK) != 0) access |= HL_HOST_FILE_NONBLOCK;
    if ((flags & HL_LINUX_O_NOFOLLOW) != 0) access |= HL_HOST_FILE_NOFOLLOW;
    if ((flags & HL_LINUX_O_DIRECTORY) != 0) access |= HL_HOST_FILE_DIRECTORY;
    if ((flags & HL_LINUX_O_PATH) != 0) access |= HL_HOST_FILE_PATH_ONLY;
    if ((flags & HL_LINUX_O_CREAT) != 0) creation |= HL_HOST_FILE_CREATE;
    if ((flags & HL_LINUX_O_EXCL) != 0) creation |= HL_HOST_FILE_EXCLUSIVE;
    if ((flags & HL_LINUX_O_TRUNC) != 0) creation |= HL_HOST_FILE_TRUNCATE;
    files = hl_linux_files(linux_abi);
    if (files == NULL || files->open_relative == NULL) return -HL_LINUX_ENOSYS;

    if (direct_directory == HL_HOST_HANDLE_INVALID && directory_fd != HL_LINUX_AT_FDCWD) {
        if (directory_fd < 0) return -HL_LINUX_EBADF;
        hl_linux_lock(linux_abi);
        status = hl_linux_fd_get_unlocked(linux_abi, (hl_linux_fd)directory_fd, NULL, &found);
        if (status != HL_STATUS_OK) {
            hl_linux_unlock(linux_abi);
            return -HL_LINUX_EBADF;
        }
        directory_ofd = &linux_abi->ofds[(size_t)(found - linux_abi->ofds)];
        directory_ofd->active_operations++;
        hl_linux_unlock(linux_abi);
        hl_linux_ofd_lock(linux_abi, directory_ofd);
        directory = directory_ofd->host_handle;
    }

    opened = files->open_relative(linux_abi->host->context, directory, path, path_size, access, creation, mode);
    if (directory_ofd != NULL) {
        hl_linux_lock(linux_abi);
        directory_ofd->active_operations--;
        hl_linux_unlock(linux_abi);
        hl_linux_ofd_unlock(linux_abi, directory_ofd);
    }
    if (opened.status != HL_STATUS_OK)
        return opened.status == HL_STATUS_NOT_FOUND ? -HL_LINUX_ENOENT : hl_linux_error((hl_status)opened.status);
    if (reservation != NULL) {
        status = hl_linux_fd_commit(linux_abi, reservation, opened.value, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                    (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
        installed = reservation->fd;
    } else {
        status = hl_linux_fd_install(linux_abi, opened.value, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                     (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0, &installed);
    }
    if (status != HL_STATUS_OK) {
        if (files->close != NULL) (void)files->close(linux_abi->host->context, opened.value);
        return status == HL_STATUS_RESOURCE_LIMIT ? -HL_LINUX_EMFILE : hl_linux_error(status);
    }
    return (int64_t)installed;
}

int64_t hl_linux_openat(hl_linux_abi *linux_abi, int32_t directory_fd, const char *path, size_t path_size,
                        uint32_t flags, uint32_t mode) {
    return hl_linux_openat_install(linux_abi, NULL, directory_fd, HL_HOST_HANDLE_INVALID, path, path_size, flags, mode);
}

int64_t hl_linux_openat_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                 int32_t directory_fd, const char *path, size_t path_size, uint32_t flags,
                                 uint32_t mode) {
    if (reservation == NULL) return -HL_LINUX_EINVAL;
    return hl_linux_openat_install(linux_abi, reservation, directory_fd, HL_HOST_HANDLE_INVALID, path, path_size, flags,
                                   mode);
}

int64_t hl_linux_openat_handle_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                        hl_host_handle directory, const char *path, size_t path_size, uint32_t flags,
                                        uint32_t mode) {
    if (reservation == NULL || directory == HL_HOST_HANDLE_INVALID) return -HL_LINUX_EINVAL;
    return hl_linux_openat_install(linux_abi, reservation, HL_LINUX_AT_FDCWD, directory, path, path_size, flags, mode);
}

int64_t hl_linux_file_adopt_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                     hl_host_handle file, uint32_t flags) {
    hl_status status;
    if (linux_abi == NULL || reservation == NULL || file == HL_HOST_HANDLE_INVALID) return -HL_LINUX_EINVAL;
    status = hl_linux_fd_commit(linux_abi, reservation, file, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
    if (status == HL_STATUS_OK) return (int64_t)reservation->fd;
    return status == HL_STATUS_RESOURCE_LIMIT ? -HL_LINUX_EMFILE : hl_linux_error(status);
}
