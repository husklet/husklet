static uint32_t hl_linux_mode_type(uint32_t host_type) {
    switch (host_type) {
    case HL_HOST_FILE_TYPE_REGULAR: return HL_LINUX_S_IFREG;
    case HL_HOST_FILE_TYPE_DIRECTORY: return HL_LINUX_S_IFDIR;
    case HL_HOST_FILE_TYPE_SYMLINK: return HL_LINUX_S_IFLNK;
    case HL_HOST_FILE_TYPE_CHARACTER: return HL_LINUX_S_IFCHR;
    case HL_HOST_FILE_TYPE_BLOCK: return HL_LINUX_S_IFBLK;
    case HL_HOST_FILE_TYPE_FIFO: return HL_LINUX_S_IFIFO;
    case HL_HOST_FILE_TYPE_SOCKET: return HL_LINUX_S_IFSOCK;
    default: return 0;
    }
}

static int64_t hl_linux_metadata_owned(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd,
                                       hl_host_file_metadata *metadata) {
    const hl_host_file_services *files = hl_linux_files(linux_abi);
    hl_host_result result;
    if (files == NULL || files->metadata == NULL) return -HL_LINUX_ENOSYS;
    memset(metadata, 0, sizeof(*metadata));
    result = files->metadata(linux_abi->host->context, ofd->host_handle, metadata);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)result.status);
}

/* A typed object -- eventfd, pipe, epoll, inotify -- owns no host FILE handle.
 * Its ofd->host_handle is not a file, so asking the file group to describe it
 * either fails or describes something else entirely; the object's own status
 * callback is the only source of truth for what fstat should report. Answering
 * from the adapter is not an optimisation, it is the difference between
 * fstat(eventfd) reporting a zero-length regular file the way the kernel's
 * anonymous inode does and reporting EBADF-ish nonsense that libc treats as a
 * broken descriptor. */
static int hl_linux_status_from_object(const hl_linux_ofd_entry *ofd, hl_linux_file_status *output, int64_t *result) {
    if (ofd->object_ops == NULL || ofd->object_ops->status == NULL) return 0;
    *result = ofd->object_ops->status(ofd->object_context, output);
    return 1;
}

int64_t hl_linux_fstat(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_file_status *output) {
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    hl_host_file_metadata metadata;
    hl_status status;
    int64_t result;
    int metadata_from_object = 0;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (output == NULL) return -HL_LINUX_EINVAL;
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
    if (!hl_linux_status_from_object(ofd, output, &result))
        result = hl_linux_metadata_owned(linux_abi, ofd, &metadata);
    else
        metadata_from_object = 1;
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    if (result != 0 || metadata_from_object) return result;
    output->device = metadata.stable_device;
    output->object = metadata.stable_object;
    output->size = metadata.size;
    output->blocks_512 = metadata.allocated_size / 512u;
    output->modified_ns = metadata.modified_ns;
    output->accessed_ns = metadata.accessed_ns;
    output->changed_ns = metadata.changed_ns;
    output->created_ns = metadata.created_ns;
    output->special_device = metadata.device;
    output->link_count = metadata.link_count;
    output->user = metadata.user;
    output->group = metadata.group;
    output->mode = hl_linux_mode_type(metadata.type) | (metadata.permissions & 07777u);
    return 0;
}

int64_t hl_linux_lseek(hl_linux_abi *linux_abi, hl_linux_fd fd, int64_t offset, int32_t whence) {
    const hl_host_file_services *files;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    hl_host_file_metadata metadata;
    hl_host_result host_result;
    hl_status status;
    int64_t result;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
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
    files = hl_linux_files(linux_abi);
    if (whence < HL_LINUX_SEEK_SET || whence > HL_LINUX_SEEK_HOLE)
        result = -HL_LINUX_EINVAL;
    else if (offset < 0 && (whence == HL_LINUX_SEEK_DATA || whence == HL_LINUX_SEEK_HOLE))
        /* Linux rejects negative sparse-seek offsets with ENXIO before asking the filesystem.  Keeping
           this ABI range check here also prevents a provider which represents positions as unsigned
           values from accepting -1 as an implementation-defined cursor (and returning offset zero). */
        result = -HL_LINUX_ENXIO;
    else if (files == NULL || files->seek == NULL)
        result = -HL_LINUX_ENOSYS;
    else if (hl_linux_metadata_owned(linux_abi, ofd, &metadata) != 0)
        result = -HL_LINUX_EIO;
    else if (metadata.type != HL_HOST_FILE_TYPE_REGULAR && metadata.type != HL_HOST_FILE_TYPE_BLOCK &&
             metadata.type != HL_HOST_FILE_TYPE_DIRECTORY)
        result = -HL_LINUX_ESPIPE;
    else {
        host_result = files->seek(linux_abi->host->context, ofd->host_handle, offset, (uint32_t)whence);
        result = host_result.status == HL_STATUS_OK ? (int64_t)host_result.value
                                                    : hl_linux_error((hl_status)host_result.status);
        /* Linux reports an out-of-range SEEK_DATA/SEEK_HOLE offset -- negative, or at/past EOF -- as
           ENXIO. No hl_status models ENXIO, so hl_linux_status_from_errno collapsed it to
           HL_STATUS_IO and the guest saw EIO instead (core/syscall/lseekhole `negative`). The host
           services carry the raw errno in `detail`, and ENXIO is 6 on both hosts. */
        if (host_result.status == HL_STATUS_IO && host_result.detail == (uint64_t)HL_LINUX_ENXIO &&
            (whence == HL_LINUX_SEEK_DATA || whence == HL_LINUX_SEEK_HOLE))
            result = -HL_LINUX_ENXIO;
        if (host_result.status == HL_STATUS_OK && host_result.value > INT64_MAX) result = -HL_LINUX_EOVERFLOW;
        if (host_result.status == HL_STATUS_OK && host_result.value <= INT64_MAX) ofd->offset = host_result.value;
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}
