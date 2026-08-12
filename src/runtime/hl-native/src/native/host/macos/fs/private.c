static hl_host_result hl_macos_file_validate_private_regular(void *context, hl_host_handle file) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, file, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISREG(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}

static hl_host_result hl_macos_file_store_private_atomic(void *context, hl_host_handle directory, const char *path,
                                                         size_t path_size, hl_host_const_bytes input,
                                                         uint32_t permissions) {
    static _Atomic uint64_t sequence;
    hl_host_macos *host = context;
    char name[PATH_MAX], temporary[PATH_MAX];
    int directory_fd = AT_FDCWD, descriptor = -1;
    if (path == NULL || path_size == 0 || path_size >= sizeof(name) || memchr(path, '\0', path_size) != NULL ||
        (permissions & ~0777u) != 0 || (input.size != 0 && input.data == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memcpy(name, path, path_size);
    name[path_size] = '\0';
    if (directory != HL_HOST_HANDLE_CWD) {
        directory_fd = hl_macos_file_descriptor(host, directory, 0);
        if (directory_fd >= 0) directory_fd = fcntl(directory_fd, F_DUPFD_CLOEXEC, 0);
        if (directory_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
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
        return hl_macos_errno();
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
    return ok ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_validate_private_directory(void *context, hl_host_handle directory) {
    hl_host_macos *host = context;
    struct stat st;
    int descriptor = hl_macos_file_descriptor(host, directory, 0);
    if (descriptor >= 0) descriptor = fcntl(descriptor, F_DUPFD_CLOEXEC, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    int status = fstat(descriptor, &st);
    int saved = errno;
    close(descriptor);
    errno = saved;
    if (status != 0) return hl_macos_errno();
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 022) == 0
               ? hl_macos_result(HL_STATUS_OK, 0, 0)
               : hl_macos_result(HL_STATUS_PERMISSION_DENIED, 0, 0);
}
