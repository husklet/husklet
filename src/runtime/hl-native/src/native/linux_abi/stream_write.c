int64_t hl_linux_pwrite64(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size, uint64_t offset) {
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (size > (size_t)INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, NULL, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[(size_t)(found - linux_abi->ofds)];
    if ((ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    result = hl_linux_write_owned(linux_abi, ofd, buffer, size, offset);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_write(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size) {
    const hl_host_file_services *files;
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    int append;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (size > (size_t)INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if ((ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    append = (ofd->status_flags & HL_LINUX_O_APPEND) != 0;
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (size != 0 && buffer == NULL)
        result = -HL_LINUX_EINVAL;
    else if (ofd->object_ops != NULL)
        result = ofd->object_ops->write == NULL ? -HL_LINUX_ENOSYS
                                                : ofd->object_ops->write(ofd->object_context, buffer, size);
    else if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        hl_host_result host_result;
        if (append)
            host_result =
                files->append(linux_abi->host->context, ofd->host_handle, (hl_host_const_bytes){buffer, size});
        else
            host_result = files->write(linux_abi->host->context, ofd->host_handle, buffer, (uint64_t)size);
        result = host_result.status == HL_STATUS_OK ? (int64_t)host_result.value
                                                    : hl_linux_error((hl_status)host_result.status);
        if (host_result.status == HL_STATUS_OK && (host_result.value > size || host_result.value > INT64_MAX))
            result = -HL_LINUX_EIO;
        else if (result > 0 && !append && ofd->offset <= UINT64_MAX - (uint64_t)result)
            ofd->offset += (uint64_t)result;
        else if (result > 0 && append && files->seek != NULL) {
            hl_host_result end = files->seek(linux_abi->host->context, ofd->host_handle, 0, HL_LINUX_SEEK_END);
            if (end.status == HL_STATUS_OK) ofd->offset = end.value;
        }
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

static int64_t hl_linux_vector(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                               uint64_t offset, uint32_t operation) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    uint64_t total = 0;
    uint32_t index;
    int writing = operation == 1 || operation == 3;
    int positioned = operation >= 2;
    int append;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (count > HL_LINUX_IOV_MAX || (count != 0 && vectors == NULL)) return -HL_LINUX_EINVAL;
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > (uint64_t)INT64_MAX - total ||
            (vectors[index].size != 0 && vectors[index].address == 0))
            return -HL_LINUX_EINVAL;
        total += vectors[index].size;
    }
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if (!writing && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_WRONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    if (writing && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    append = !positioned && writing && (ofd->status_flags & HL_LINUX_O_APPEND) != 0;
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        switch (operation) {
        case 0:
            host_result = files->readv == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->readv(linux_abi->host->context, ofd->host_handle, vectors, count);
            break;
        case 1:
            if (append)
                host_result = files->appendv == NULL
                                  ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                  : files->appendv(linux_abi->host->context, ofd->host_handle, vectors, count);
            else
                host_result = files->writev == NULL
                                  ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                  : files->writev(linux_abi->host->context, ofd->host_handle, vectors, count);
            break;
        case 2:
            host_result = files->readv_at == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->readv_at(linux_abi->host->context, ofd->host_handle, vectors, count, offset);
            break;
        default:
            host_result = files->writev_at == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->writev_at(linux_abi->host->context, ofd->host_handle, vectors, count, offset);
            break;
        }
        if (host_result.status != HL_STATUS_OK)
            result = hl_linux_error((hl_status)host_result.status);
        else if (host_result.value > total || host_result.value > INT64_MAX)
            result = -HL_LINUX_EIO;
        else
            result = (int64_t)host_result.value;
        if (result > 0 && !positioned && !append && ofd->offset <= UINT64_MAX - (uint64_t)result)
            ofd->offset += (uint64_t)result;
        else if (result > 0 && append && files->seek != NULL) {
            hl_host_result end = files->seek(linux_abi->host->context, ofd->host_handle, 0, HL_LINUX_SEEK_END);
            if (end.status == HL_STATUS_OK) ofd->offset = end.value;
        }
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_readv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count) {
    return hl_linux_vector(linux_abi, fd, vectors, count, 0, 0);
}

int64_t hl_linux_writev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count) {
    return hl_linux_vector(linux_abi, fd, vectors, count, 0, 1);
}

int64_t hl_linux_preadv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                        uint64_t offset) {
    return hl_linux_vector(linux_abi, fd, vectors, count, offset, 2);
}

int64_t hl_linux_pwritev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                         uint64_t offset) {
    return hl_linux_vector(linux_abi, fd, vectors, count, offset, 3);
}

