#include "descriptor_output.h"

hl_status hl_linux_fd_dup(hl_linux_abi *linux_abi, hl_linux_fd source, uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    const hl_linux_fd_entry *source_entry;
    hl_linux_fd fd;
    hl_status status;
    status = hl_linux_fd_output_validate_context(linux_abi, out_fd);
    if (status != HL_STATUS_OK) return status;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, source, &source_entry, NULL);
    if (status != HL_STATUS_OK) goto done;
    status = hl_linux_find_fd(linux_abi, &fd);
    if (status != HL_STATUS_OK) goto done;
    linux_abi->fds[fd].ofd = source_entry->ofd;
    linux_abi->fds[fd].descriptor_flags = descriptor_flags;
    linux_abi->fds[fd].generation++;
    linux_abi->ofds[source_entry->ofd].references++;
    *out_fd = fd;
done:
    hl_linux_unlock(linux_abi);
    return status;
}

static int64_t hl_linux_fd_dup_at_least(hl_linux_abi *linux_abi, hl_linux_fd source, hl_linux_fd minimum,
                                        uint32_t descriptor_flags) {
    const hl_linux_fd_entry *source_entry;
    hl_linux_fd fd;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, source, &source_entry, NULL);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    status = hl_linux_find_fd_at_least(linux_abi, minimum, &fd);
    if (status == HL_STATUS_OK) {
        linux_abi->fds[fd].ofd = source_entry->ofd;
        linux_abi->fds[fd].descriptor_flags = descriptor_flags;
        linux_abi->fds[fd].generation++;
        linux_abi->ofds[source_entry->ofd].references++;
    }
    hl_linux_unlock(linux_abi);
    return status == HL_STATUS_OK ? (int64_t)fd : -HL_LINUX_EMFILE;
}

/* Complete a final reference removal which already marked the OFD closing. */
static hl_status hl_linux_ofd_finalize(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd_entry,
                                       hl_host_handle *last_host_handle) {
    const hl_host_sync_services *sync = hl_linux_sync(linux_abi);
    hl_host_handle host_handle;
    const hl_linux_object_ops *object_ops;
    void *object_context;
    hl_status close_status = HL_STATUS_OK;
    hl_host_handle mutex = ofd_entry->io_mutex;
    hl_host_result result;
    /* Wait only for this OFD. Operations drop their pin before releasing io_lock. */
    result = sync->mutex_lock(linux_abi->host->context, mutex);
    if (result.status != HL_STATUS_OK) close_status = (hl_status)result.status;
retry_active:
    hl_linux_lock(linux_abi);
    if (ofd_entry->references != 0 || ofd_entry->closing == 0) {
        hl_linux_unlock(linux_abi);
        if (result.status == HL_STATUS_OK) (void)sync->mutex_unlock(linux_abi->host->context, mutex);
        return HL_STATUS_CORRUPT;
    }
    if (ofd_entry->active_operations != 0) {
        hl_linux_unlock(linux_abi);
        if (result.status != HL_STATUS_OK) return close_status;
        (void)sync->mutex_unlock(linux_abi->host->context, mutex);
        result = sync->mutex_lock(linux_abi->host->context, mutex);
        if (result.status != HL_STATUS_OK) return (hl_status)result.status;
        goto retry_active;
    }
    host_handle = ofd_entry->host_handle;
    object_ops = ofd_entry->object_ops;
    object_context = ofd_entry->object_context;
    ofd_entry->host_handle = HL_HOST_HANDLE_INVALID;
    ofd_entry->offset = 0;
    ofd_entry->status_flags = 0;
    ofd_entry->kind = 0;
    ofd_entry->object_ops = NULL;
    ofd_entry->object_context = NULL;
    hl_linux_unlock(linux_abi);
    if (object_ops != NULL) close_status = object_ops->close(object_context);
    if (result.status == HL_STATUS_OK) {
        result = sync->mutex_unlock(linux_abi->host->context, mutex);
        if (result.status != HL_STATUS_OK && close_status == HL_STATUS_OK) close_status = (hl_status)result.status;
    }
    result = sync->mutex_close(linux_abi->host->context, mutex);
    if (result.status != HL_STATUS_OK && close_status == HL_STATUS_OK) close_status = (hl_status)result.status;
    hl_linux_lock(linux_abi);
    if (ofd_entry->references != 0 || ofd_entry->active_operations != 0 || ofd_entry->closing == 0 ||
        ofd_entry->io_mutex != mutex) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_CORRUPT;
    }
    ofd_entry->io_mutex = HL_HOST_HANDLE_INVALID;
    ofd_entry->closing = 0;
    ofd_entry->generation++;
    hl_linux_unlock(linux_abi);
    if (last_host_handle != NULL) *last_host_handle = host_handle;
    return close_status;
}

hl_status hl_linux_fd_close(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle *last_host_handle) {
    hl_linux_ofd ofd;
    hl_linux_ofd_entry *ofd_entry;
    int final_reference;
    int defer_finalization;
    const hl_linux_object_ops *retire_ops = NULL;
    void *retire_context = NULL;
    if (last_host_handle != NULL) *last_host_handle = HL_HOST_HANDLE_INVALID;
    if (linux_abi == NULL) return HL_STATUS_NOT_FOUND;
    hl_linux_lock(linux_abi);
    if (fd >= linux_abi->fd_capacity || linux_abi->fds[fd].ofd == 0 || linux_abi->fds[fd].ofd == HL_LINUX_FD_RESERVED) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_NOT_FOUND;
    }
    ofd = linux_abi->fds[fd].ofd;
    if (ofd >= linux_abi->ofd_capacity) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_CORRUPT;
    }
    ofd_entry = &linux_abi->ofds[ofd];
    if (ofd_entry->references == 0) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_CORRUPT;
    }
    linux_abi->fds[fd].ofd = 0;
    linux_abi->fds[fd].descriptor_flags = 0;
    linux_abi->fds[fd].generation++;
    ofd_entry->references--;
    final_reference = ofd_entry->references == 0;
    if (final_reference) {
        ofd_entry->closing = 1;
        retire_ops = ofd_entry->object_ops;
        retire_context = ofd_entry->object_context;
    }
    defer_finalization = final_reference && ofd_entry->active_operations != 0;
    hl_linux_unlock(linux_abi);
    if (retire_ops != NULL && retire_ops->retire != NULL) retire_ops->retire(retire_context);
    return final_reference && !defer_finalization ? hl_linux_ofd_finalize(linux_abi, ofd_entry, last_host_handle)
                                                  : HL_STATUS_OK;
}

#include "count_output.h"

hl_status hl_linux_fd_exec(hl_linux_abi *linux_abi, hl_linux_fd fd, uint32_t *out_closed) {
    const hl_host_file_services *files;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status status;
    hl_host_result result;
    if (!hl_linux_count_output_prepare(out_closed)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    hl_linux_lock(linux_abi);
    if (fd >= linux_abi->fd_capacity || linux_abi->fds[fd].ofd == 0 || linux_abi->fds[fd].ofd == HL_LINUX_FD_RESERVED) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_NOT_FOUND;
    }
    if ((linux_abi->fds[fd].descriptor_flags & HL_LINUX_FD_CLOEXEC) == 0) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_OK;
    }
    hl_linux_unlock(linux_abi);
    status = hl_linux_fd_close(linux_abi, fd, &handle);
    *out_closed = 1;
    if (status != HL_STATUS_OK) return status;
    if (handle == HL_HOST_HANDLE_INVALID) return HL_STATUS_OK;
    files = hl_linux_files(linux_abi);
    if (files == NULL || files->close == NULL) return HL_STATUS_NOT_SUPPORTED;
    result = files->close(linux_abi->host->context, handle);
    return (hl_status)result.status;
}

hl_status hl_linux_fd_exec_all(hl_linux_abi *linux_abi, hl_linux_fd_exec_callback callback, void *context,
                               uint32_t *out_closed) {
    hl_status first = HL_STATUS_OK;
    hl_linux_fd cursor = 0;
    uint32_t count = 0;
    if (!hl_linux_count_output_prepare(out_closed)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    while (cursor < linux_abi->fd_capacity) {
        hl_linux_fd candidate;
        uint32_t removed = 0;
        hl_status status;
        hl_linux_lock(linux_abi);
        for (candidate = cursor; candidate < linux_abi->fd_capacity; ++candidate) {
            hl_linux_ofd ofd = linux_abi->fds[candidate].ofd;
            if (ofd != 0 && ofd != HL_LINUX_FD_RESERVED &&
                (linux_abi->fds[candidate].descriptor_flags & HL_LINUX_FD_CLOEXEC) != 0)
                break;
        }
        cursor = candidate < linux_abi->fd_capacity ? candidate + 1 : linux_abi->fd_capacity;
        hl_linux_unlock(linux_abi);
        if (candidate == linux_abi->fd_capacity) break;
        status = hl_linux_fd_exec(linux_abi, candidate, &removed);
        if (status != HL_STATUS_OK && first == HL_STATUS_OK) first = status;
        if (removed != 0) {
            count++;
            if (callback != NULL) callback(context, candidate);
        }
    }
    *out_closed = count;
    return first;
}

hl_status hl_linux_abi_validate_fds(const hl_linux_abi *linux_abi) {
    uint32_t *references;
    uint32_t fd;
    uint32_t ofd;
    hl_status status = HL_STATUS_OK;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    references = calloc(linux_abi->ofd_capacity, sizeof(*references));
    if (references == NULL) return HL_STATUS_OUT_OF_MEMORY;
    hl_linux_lock((hl_linux_abi *)linux_abi);
    for (fd = 0; fd < linux_abi->fd_capacity; ++fd) {
        ofd = linux_abi->fds[fd].ofd;
        if (ofd == 0 || ofd == HL_LINUX_FD_RESERVED) continue;
        if (ofd >= linux_abi->ofd_capacity || references[ofd] == UINT32_MAX) {
            status = HL_STATUS_CORRUPT;
            goto done;
        }
        references[ofd]++;
    }
    for (ofd = 1; ofd < linux_abi->ofd_capacity; ++ofd) {
        const hl_linux_ofd_entry *entry = &linux_abi->ofds[ofd];
        if (entry->references != references[ofd] ||
            (entry->references != 0 &&
             (((entry->object_ops == NULL) == (entry->host_handle == HL_HOST_HANDLE_INVALID)) ||
              (entry->object_ops != NULL && entry->object_context == NULL) ||
              entry->io_mutex == HL_HOST_HANDLE_INVALID || entry->closing != 0)) ||
            (entry->references == 0 && entry->active_operations == 0 &&
             (entry->host_handle != HL_HOST_HANDLE_INVALID || entry->io_mutex != HL_HOST_HANDLE_INVALID ||
              entry->closing != 0 || entry->object_ops != NULL || entry->object_context != NULL))) {
            status = HL_STATUS_CORRUPT;
            goto done;
        }
    }
done:
    hl_linux_unlock((hl_linux_abi *)linux_abi);
    free(references);
    return status;
}

static int64_t hl_linux_pread64_owned(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd, void *buffer, size_t size,
                                      uint64_t offset) {
    const hl_host_file_services *files;
    hl_host_result result;
    if (ofd->status_flags & HL_LINUX_O_PATH) return -HL_LINUX_EBADF;
    if ((ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_WRONLY) return -HL_LINUX_EBADF;
    if (size != 0 && buffer == NULL) return -HL_LINUX_EINVAL;
    files = hl_linux_files(linux_abi);
    if (files == NULL || files->read_at == NULL) return -HL_LINUX_ENOSYS;
    result = files->read_at(linux_abi->host->context, ofd->host_handle, offset, (hl_host_bytes){buffer, size});
    if (result.status != HL_STATUS_OK) return hl_linux_error((hl_status)result.status);
    if (result.value > size || result.value > (uint64_t)INT64_MAX) return -HL_LINUX_EIO;
    return (int64_t)result.value;
}
