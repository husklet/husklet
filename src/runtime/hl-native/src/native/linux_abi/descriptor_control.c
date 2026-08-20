int64_t hl_linux_close(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    const hl_host_file_services *files;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status status = hl_linux_fd_close(linux_abi, fd, &handle);
    hl_host_result result;
    if (status != HL_STATUS_OK) return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    if (handle == HL_HOST_HANDLE_INVALID) return 0;
    files = hl_linux_files(linux_abi);
    if (files == NULL || files->close == NULL) return -HL_LINUX_ENOSYS;
    result = files->close(linux_abi->host->context, handle);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)result.status);
}

int64_t hl_linux_dup(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_fd_dup_at_least(linux_abi, fd, 0, 0);
}

/*
 * Publish target's new OFD while holding table ownership. If target displaced
 * the final reference to another OFD, drain that OFD only after publication.
 */
static int64_t hl_linux_fd_replace(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd target,
                                   uint32_t descriptor_flags, int reject_same) {
    const hl_linux_fd_entry *source_entry;
    hl_linux_ofd_entry *displaced = NULL;
    hl_linux_ofd source_ofd;
    hl_linux_ofd target_ofd;
    hl_host_handle displaced_handle = HL_HOST_HANDLE_INVALID;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (source == target && reject_same) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, source, &source_entry, NULL);
    if (status != HL_STATUS_OK || target >= linux_abi->fd_capacity) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    if (source == target) {
        hl_linux_unlock(linux_abi);
        return (int64_t)target;
    }
    source_ofd = source_entry->ofd;
    target_ofd = linux_abi->fds[target].ofd;
    if (target_ofd != 0) {
        if (target_ofd >= linux_abi->ofd_capacity || linux_abi->ofds[target_ofd].references == 0) {
            hl_linux_unlock(linux_abi);
            return -HL_LINUX_EIO;
        }
        displaced = &linux_abi->ofds[target_ofd];
        displaced->references--;
        if (displaced->references == 0) displaced->closing = 1;
    }
    linux_abi->fds[target].ofd = source_ofd;
    linux_abi->fds[target].descriptor_flags = descriptor_flags;
    linux_abi->fds[target].generation++;
    linux_abi->ofds[source_ofd].references++;
    if (displaced != NULL && displaced->references != 0) displaced = NULL;
    hl_linux_unlock(linux_abi);

    if (displaced != NULL) {
        const hl_host_file_services *files;
        status = hl_linux_ofd_finalize(linux_abi, displaced, &displaced_handle);
        files = hl_linux_files(linux_abi);
        /* Linux dup2/dup3 intentionally discard errors from closing target. */
        if (files != NULL && files->close != NULL) (void)files->close(linux_abi->host->context, displaced_handle);
        (void)status;
    }
    return (int64_t)target;
}

int64_t hl_linux_dup2(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd target) {
    return hl_linux_fd_replace(linux_abi, source, target, 0, 0);
}

int64_t hl_linux_dup3(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd target, uint32_t flags) {
    if ((flags & ~(uint32_t)HL_LINUX_O_CLOEXEC) != 0) return -HL_LINUX_EINVAL;
    return hl_linux_fd_replace(linux_abi, source, target, (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0,
                               1);
}

int64_t hl_linux_fcntl(hl_linux_abi *linux_abi, hl_linux_fd fd, int32_t command, uint64_t argument) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *ofd_entry;
    hl_status status;
    if (command == HL_LINUX_F_DUPFD || command == HL_LINUX_F_DUPFD_CLOEXEC) {
        if (linux_abi == NULL) return -HL_LINUX_EBADF;
        if (argument >= linux_abi->fd_capacity) return -HL_LINUX_EINVAL;
        return hl_linux_fd_dup_at_least(linux_abi, fd, (hl_linux_fd)argument,
                                        command == HL_LINUX_F_DUPFD_CLOEXEC ? HL_LINUX_FD_CLOEXEC : 0);
    }
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &ofd_entry);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    switch (command) {
    case HL_LINUX_F_GETFD: argument = fd_entry->descriptor_flags; break;
    case HL_LINUX_F_SETFD:
        linux_abi->fds[fd].descriptor_flags = (uint32_t)argument & HL_LINUX_FD_CLOEXEC;
        argument = 0;
        break;
    case HL_LINUX_F_GETFL: argument = ofd_entry->status_flags; break;
    case HL_LINUX_F_SETFL: {
        hl_linux_ofd_entry *ofd = &linux_abi->ofds[fd_entry->ofd];
        // Linux replaces exactly the settable status flags and preserves everything else, so the access
        // mode, O_LARGEFILE and O_PATH survive a set (fs/fcntl.c setfl). Masking down to the access mode
        // instead had erased them.
        uint32_t requested = (ofd_entry->status_flags & ~(uint32_t)HL_LINUX_O_SETFL) |
                             ((uint32_t)argument & (uint32_t)HL_LINUX_O_SETFL);
        // The engine records only what the object is actually carrying. A host that arms fewer flags than
        // it was given reports the difference, and a host that cannot arm them at all reports why -- so
        // F_GETFL never answers with a capability nothing is applying.
        uint32_t effective = requested & (uint32_t)HL_LINUX_O_SETFL_HOST;
        if (ofd->object_ops != NULL && ofd->object_ops->set_status_flags != NULL) {
            int64_t result;
            ofd->active_operations++;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_lock(linux_abi, ofd);
            result = ofd->object_ops->set_status_flags(ofd->object_context, requested, &effective);
            hl_linux_lock(linux_abi);
            if (result == 0)
                ofd->status_flags = (requested & ~(uint32_t)HL_LINUX_O_SETFL_HOST) |
                                    (effective & (uint32_t)HL_LINUX_O_SETFL_HOST);
            ofd->active_operations--;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_unlock(linux_abi, ofd);
            return result;
        }
        // A descriptor backed by a host handle -- every adopted stdio descriptor and every guest open --
        // reaches the real open file description through the stream seam. Without this the whole command
        // was a shadow write: F_SETFL(O_NONBLOCK) on an inherited descriptor was reported by F_GETFL and
        // the following read still blocked, and F_SETFL(O_DIRECT) on a device that rejects it invented a
        // success the kernel does not give.
        if (ofd->host_handle != HL_HOST_HANDLE_INVALID) {
            const hl_host_stream_services *streams = hl_linux_streams(linux_abi);
            hl_host_result result;
            if (streams == NULL) {
                hl_linux_unlock(linux_abi);
                return -HL_LINUX_ENOSYS;
            }
            ofd->active_operations++;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_lock(linux_abi, ofd);
            result = streams->set_status_flags(linux_abi->host->context, ofd->host_handle,
                                               hl_linux_host_stream_flags(requested));
            hl_linux_lock(linux_abi);
            if (result.status == HL_STATUS_OK)
                ofd->status_flags = (requested & ~(uint32_t)HL_LINUX_O_SETFL_HOST) |
                                    (hl_linux_status_flags_from_host_stream((uint32_t)result.value) &
                                     (uint32_t)HL_LINUX_O_SETFL_HOST);
            ofd->active_operations--;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_unlock(linux_abi, ofd);
            return result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)result.status);
        }
        ofd->status_flags = requested;
        argument = 0;
        break;
    }
    default: hl_linux_unlock(linux_abi); return -HL_LINUX_EINVAL;
    }
    hl_linux_unlock(linux_abi);
    return (int64_t)argument;
}

