int64_t hl_linux_pread64(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t size, uint64_t offset) {
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
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    result = hl_linux_pread64_owned(linux_abi, ofd, buffer, size, offset);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_read(hl_linux_abi *linux_abi, hl_linux_fd fd, void *buffer, size_t size) {
    const hl_host_file_services *files;
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (size > (size_t)INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        result = status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
        goto done;
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if ((ofd->status_flags & HL_LINUX_O_PATH) || (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_WRONLY)
        result = -HL_LINUX_EBADF;
    else if (size != 0 && buffer == NULL)
        result = -HL_LINUX_EINVAL;
    else if (ofd->object_ops != NULL)
        result =
            ofd->object_ops->read == NULL ? -HL_LINUX_ENOSYS : ofd->object_ops->read(ofd->object_context, buffer, size);
    else if (files == NULL || files->read == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        host_result = files->read(linux_abi->host->context, ofd->host_handle, buffer, (uint64_t)size);
        result = host_result.status == HL_STATUS_OK ? (int64_t)host_result.value
                                                    : hl_linux_error((hl_status)host_result.status);
        if (host_result.status == HL_STATUS_OK && (host_result.value > size || host_result.value > INT64_MAX))
            result = -HL_LINUX_EIO;
        else if (result > 0 && ofd->offset <= UINT64_MAX - (uint64_t)result)
            ofd->offset += (uint64_t)result;
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
done:
    hl_linux_unlock(linux_abi);
    return result;
}

static int64_t hl_linux_write_owned(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd, const void *buffer, size_t size,
                                    uint64_t offset) {
    const hl_host_file_services *files;
    hl_host_result result;
    if (ofd->status_flags & HL_LINUX_O_PATH) return -HL_LINUX_EBADF;
    if (size != 0 && buffer == NULL) return -HL_LINUX_EINVAL;
    files = hl_linux_files(linux_abi);
    if (files == NULL) return -HL_LINUX_ENOSYS;
    if (files->write_at == NULL) return -HL_LINUX_ENOSYS;
    result = files->write_at(linux_abi->host->context, ofd->host_handle, offset, (hl_host_const_bytes){buffer, size});
    if (result.status != HL_STATUS_OK) return hl_linux_error((hl_status)result.status);
    if (result.value > size || result.value > (uint64_t)INT64_MAX) return -HL_LINUX_EIO;
    return (int64_t)result.value;
}
