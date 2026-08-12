static int hl_macos_file_directory(hl_host_macos *host, hl_host_handle directory) {
    int descriptor = AT_FDCWD;
    if (directory == HL_HOST_HANDLE_CWD) return descriptor;
    pthread_mutex_lock(&host->lock);
    hl_macos_file *file = hl_macos_file_lookup(host, directory);
    descriptor = file != NULL ? file->descriptor : -1;
    pthread_mutex_unlock(&host->lock);
    return descriptor;
}

static hl_host_result hl_macos_file_rename(void *context, hl_host_handle old_directory, const char *old_path,
                                           size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                           size_t new_path_size) {
    hl_host_macos *host = context;
    char old_local[PATH_MAX];
    char new_local[PATH_MAX];
    int old_fd;
    int new_fd;
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = '\0';
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = '\0';
    old_fd = hl_macos_file_directory(host, old_directory);
    new_fd = hl_macos_file_directory(host, new_directory);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (renameat(old_fd, old_local, new_fd, new_local) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_unlink(void *context, hl_host_handle directory, const char *path,
                                           size_t path_size) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, 0) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_rmdir(void *context, hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    int directory_fd;
    if (path == NULL || path_size == 0 || path_size >= sizeof(local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
    directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (unlinkat(directory_fd, local, AT_REMOVEDIR) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_mkdir(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                          uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkdirat(directory_fd, local, (mode_t)permissions) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_macos_errno();
}

static hl_host_result hl_macos_file_fifo(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t permissions) {
    hl_host_macos *host = context;
    char local[PATH_MAX];
    if (path == NULL || path_size == 0 || path_size >= sizeof(local) || (permissions & ~07777u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return mkfifoat(directory_fd, local, (mode_t)permissions) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                   : hl_macos_errno();
}

static hl_host_result hl_macos_file_symlink(void *context, const char *target, size_t target_size,
                                            hl_host_handle directory, const char *path, size_t path_size) {
    hl_host_macos *host = context;
    char target_local[PATH_MAX], path_local[PATH_MAX];
    if (target == NULL || path == NULL || target_size == 0 || path_size == 0 || target_size >= sizeof(target_local) ||
        path_size >= sizeof(path_local))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(target_local, target, target_size);
    target_local[target_size] = 0;
    memcpy(path_local, path, path_size);
    path_local[path_size] = 0;
    int directory_fd = hl_macos_file_directory(host, directory);
    if (directory_fd < 0 && directory != HL_HOST_HANDLE_CWD) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return symlinkat(target_local, directory_fd, path_local) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                  : hl_macos_errno();
}

static hl_host_result hl_macos_file_link(void *context, hl_host_handle old_directory, const char *old_path,
                                         size_t old_path_size, hl_host_handle new_directory, const char *new_path,
                                         size_t new_path_size, uint32_t flags) {
    hl_host_macos *host = context;
    char old_local[PATH_MAX], new_local[PATH_MAX];
    if (old_path == NULL || new_path == NULL || old_path_size == 0 || new_path_size == 0 ||
        old_path_size >= sizeof(old_local) || new_path_size >= sizeof(new_local) || (flags & ~1u) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(old_local, old_path, old_path_size);
    old_local[old_path_size] = 0;
    memcpy(new_local, new_path, new_path_size);
    new_local[new_path_size] = 0;
    int old_fd = hl_macos_file_directory(host, old_directory);
    int new_fd = hl_macos_file_directory(host, new_directory);
    if ((old_fd < 0 && old_directory != HL_HOST_HANDLE_CWD) || (new_fd < 0 && new_directory != HL_HOST_HANDLE_CWD))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int native_flags = (flags & 1u) != 0 ? AT_SYMLINK_FOLLOW : 0;
    return linkat(old_fd, old_local, new_fd, new_local, native_flags) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                           : hl_macos_errno();
}

static hl_host_result hl_macos_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset) {
    return hl_macos_file_vector(context, file, vectors, count, offset, 2);
}

static hl_host_result hl_macos_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count, uint64_t offset) {
    return hl_macos_file_vector(context, file, vectors, count, offset, 3);
}

static hl_host_result hl_macos_file_metadata_get(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    struct stat status;
    if (descriptor < 0 || output == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstat(descriptor, &status) != 0) return hl_macos_errno();
    memset(output, 0, sizeof(*output));
    output->stable_device = (uint64_t)status.st_dev;
    output->stable_object = (uint64_t)status.st_ino;
    output->size = (uint64_t)status.st_size;
    output->allocated_size = (uint64_t)status.st_blocks * 512u;
    output->modified_ns =
        (uint64_t)status.st_mtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_mtimespec.tv_nsec;
    output->accessed_ns =
        (uint64_t)status.st_atimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_atimespec.tv_nsec;
    output->changed_ns =
        (uint64_t)status.st_ctimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_ctimespec.tv_nsec;
    output->created_ns =
        (uint64_t)status.st_birthtimespec.tv_sec * UINT64_C(1000000000) + (uint64_t)status.st_birthtimespec.tv_nsec;
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
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_resolve_beneath(void *context, hl_host_handle root, const char *path,
                                                    size_t path_size, uint32_t policy,
                                                    hl_host_file_resolution *output) {
    hl_host_macos *host = context;
    hl_host_resolved_path resolved = {0};
    hl_host_result parent;
    hl_host_result target = {HL_STATUS_OK, 0, HL_HOST_HANDLE_INVALID, 0};
    char local[PATH_MAX];
    int root_fd = hl_macos_file_descriptor(host, root, 0);
    /* Resolution is a metadata probe, never an I/O open.  O_NONBLOCK prevents a FIFO/device final
     * component from stalling the resolver before the Linux ABI can apply its actual open flags. */
    int target_flags = O_RDONLY | O_NONBLOCK;
    if (root_fd < 0 || output == NULL || path == NULL || path_size == 0 || path_size >= sizeof(local) ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS |
                              HL_HOST_RESOLVE_ALLOW_MISSING)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(local, path, path_size);
    local[path_size] = '\0';
#ifdef O_SYMLINK
    if ((policy & HL_HOST_RESOLVE_NOFOLLOW_FINAL) != 0) target_flags = O_SYMLINK;
#endif
    if ((policy & HL_HOST_RESOLVE_ALLOW_MISSING) != 0) target_flags = -1;
    if (hl_host_resolve_beneath(root_fd, local, policy & (HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS),
                                target_flags, &resolved) != 0)
        return hl_macos_errno();
    if (strlen(resolved.leaf) >= sizeof(output->final)) {
        hl_host_resolved_path_destroy(&resolved);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    parent = hl_macos_file_register(host, resolved.parent_fd, -1, 0);
    if (parent.status != HL_STATUS_OK) {
        hl_host_resolved_path_destroy(&resolved);
        return parent;
    }
    resolved.parent_fd = -1;
    if (resolved.target_fd >= 0) {
        target = hl_macos_file_register(host, resolved.target_fd, -1, 0);
        if (target.status != HL_STATUS_OK) {
            (void)hl_macos_file_close(host, parent.value);
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
        hl_host_file_metadata metadata = {0};
        if (hl_macos_file_metadata_get(host, output->target, &metadata).status == HL_STATUS_OK)
            output->target_type = metadata.type;
    }
    hl_host_resolved_path_destroy(&resolved);
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_open_beneath(void *context, hl_host_handle root, const char *path, size_t path_size,
                                                 uint32_t access, uint32_t creation, uint32_t permissions,
                                                 uint32_t policy) {
    hl_host_file_resolution resolved = {0};
    hl_host_result result;
    if (path == NULL || path_size == 0 || path[0] == '/' || memchr(path, '\0', path_size) != NULL ||
        (policy & ~(uint32_t)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    uint32_t resolve_policy = policy | HL_HOST_RESOLVE_ALLOW_MISSING;
    if ((creation & (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE)) == (HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE))
        resolve_policy |= HL_HOST_RESOLVE_NOFOLLOW_FINAL;
    result = hl_macos_file_resolve_beneath(context, root, path, path_size, resolve_policy, &resolved);
    if (result.status != HL_STATUS_OK) return result;
    result = hl_macos_file_open(context, resolved.parent, resolved.final, resolved.final_size,
                                access | HL_HOST_FILE_NOFOLLOW, creation, permissions);
    if (resolved.target != HL_HOST_HANDLE_INVALID) (void)hl_macos_file_close(context, resolved.target);
    (void)hl_macos_file_close(context, resolved.parent);
    return result;
}

static hl_host_result hl_macos_file_path(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_macos *host = context;
    char path[PATH_MAX];
    hl_macos_file *file;
    int error = 0;
    size_t length;
    if ((output.size != 0 && output.data == 0) || output.size > SIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    if (file == NULL)
        error = EBADF;
    else if (fcntl(file->descriptor, F_GETPATH, path) != 0)
        error = errno;
    pthread_mutex_unlock(&host->lock);
    if (error != 0) {
        errno = error;
        return hl_macos_errno();
    }
    length = strnlen(path, sizeof path);
    if (length > output.size) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, length, 0);
    if (length != 0) memcpy(output.data, path, length);
    return hl_macos_result(HL_STATUS_OK, length, 0);
}

static hl_host_result hl_macos_file_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_file *file;
    int descriptor;
    int append_descriptor;
    hl_macos_stream_shared *stream;
    DIR *directory;
    hl_macos_directory_shared *directory_shared;
    pthread_mutex_lock(&host->lock);
    file = hl_macos_file_lookup(host, handle);
    if (file == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = file->descriptor;
    append_descriptor = file->append_descriptor;
    stream = file->stream;
    directory = file->directory;
    directory_shared = file->directory_shared;
    hl_host_process_fd_private_remove(descriptor);
    hl_host_process_fd_private_remove(append_descriptor);
    file->active = 0;
    file->shared = 0;
    file->descriptor = -1;
    file->append_descriptor = -1;
    file->stream = NULL;
    file->stream_endpoint = 0;
    file->directory = NULL;
    file->directory_position = 0;
    file->directory_shared = NULL;
    pthread_mutex_unlock(&host->lock);
    int saved_error = close(descriptor) != 0 ? errno : 0;
    if (directory != NULL && closedir(directory) != 0 && saved_error == 0) saved_error = errno;
    if (append_descriptor >= 0 && close(append_descriptor) != 0 && saved_error == 0) saved_error = errno;
    hl_macos_stream_release(stream);
    hl_macos_directory_shared_release(directory_shared);
    if (saved_error != 0) {
        errno = saved_error;
        return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

