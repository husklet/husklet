#include "descriptor_output.h"

hl_status hl_linux_fd_install(hl_linux_abi *linux_abi, hl_host_handle host_handle, uint32_t status_flags,
                              uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    hl_linux_fd fd;
    hl_linux_ofd ofd;
    hl_host_handle mutex;
    hl_host_result created;
    const hl_host_sync_services *sync;
    hl_status status;
    if (!hl_linux_fd_output_prepare(out_fd)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || host_handle == HL_HOST_HANDLE_INVALID)
        return HL_STATUS_INVALID_ARGUMENT;
    sync = hl_linux_sync(linux_abi);
    if (sync == NULL || sync->mutex_create == NULL || sync->mutex_close == NULL) return HL_STATUS_NOT_SUPPORTED;
    created = sync->mutex_create(linux_abi->host->context);
    if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID)
        return created.status == HL_STATUS_OK ? HL_STATUS_RESOURCE_LIMIT : (hl_status)created.status;
    mutex = created.value;
    hl_linux_lock(linux_abi);
    status = hl_linux_find_fd(linux_abi, &fd);
    if (status != HL_STATUS_OK) goto done;
    status = hl_linux_find_ofd(linux_abi, &ofd);
    if (status != HL_STATUS_OK) goto done;
    linux_abi->ofds[ofd].host_handle = host_handle;
    linux_abi->ofds[ofd].status_flags = status_flags;
    linux_abi->ofds[ofd].io_mutex = mutex;
    linux_abi->ofds[ofd].references = 1;
    linux_abi->ofds[ofd].generation++;
    linux_abi->ofds[ofd].flock_token = hl_linux_new_ofd_token();
    linux_abi->fds[fd].ofd = ofd;
    linux_abi->fds[fd].descriptor_flags = descriptor_flags;
    linux_abi->fds[fd].generation++;
    *out_fd = fd;
done:
    hl_linux_unlock(linux_abi);
    if (status != HL_STATUS_OK) (void)sync->mutex_close(linux_abi->host->context, mutex);
    return status;
}

hl_status hl_linux_fd_install_at(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_host_handle host_handle,
                                 uint32_t status_flags, uint32_t descriptor_flags) {
    hl_linux_ofd ofd;
    hl_host_handle mutex;
    hl_host_result created;
    const hl_host_sync_services *sync;
    hl_status status;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || fd >= linux_abi->fd_capacity ||
        host_handle == HL_HOST_HANDLE_INVALID)
        return HL_STATUS_INVALID_ARGUMENT;
    sync = hl_linux_sync(linux_abi);
    if (sync == NULL || sync->mutex_create == NULL || sync->mutex_close == NULL) return HL_STATUS_NOT_SUPPORTED;
    created = sync->mutex_create(linux_abi->host->context);
    if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID)
        return created.status == HL_STATUS_OK ? HL_STATUS_RESOURCE_LIMIT : (hl_status)created.status;
    mutex = created.value;
    hl_linux_lock(linux_abi);
    if (linux_abi->fds[fd].ofd != 0) {
        status = HL_STATUS_ALREADY_EXISTS;
        goto done;
    }
    status = hl_linux_find_ofd(linux_abi, &ofd);
    if (status != HL_STATUS_OK) goto done;
    linux_abi->ofds[ofd].host_handle = host_handle;
    linux_abi->ofds[ofd].status_flags = status_flags;
    linux_abi->ofds[ofd].io_mutex = mutex;
    linux_abi->ofds[ofd].references = 1;
    linux_abi->ofds[ofd].generation++;
    linux_abi->ofds[ofd].flock_token = hl_linux_new_ofd_token();
    linux_abi->fds[fd].ofd = ofd;
    linux_abi->fds[fd].descriptor_flags = descriptor_flags;
    linux_abi->fds[fd].generation++;
done:
    hl_linux_unlock(linux_abi);
    if (status != HL_STATUS_OK) (void)sync->mutex_close(linux_abi->host->context, mutex);
    return status;
}

static hl_status hl_linux_object_install_common(hl_linux_abi *linux_abi, hl_linux_fd requested,
                                                const hl_linux_object_ops *ops, void *context, uint32_t kind,
                                                uint32_t status_flags, uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    const hl_host_sync_services *sync;
    hl_host_result created;
    hl_linux_ofd_entry candidate = {0};
    hl_linux_ofd ofd;
    hl_linux_fd fd;
    hl_status status;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || ops == NULL || ops->close == NULL ||
        context == NULL || out_fd == NULL || (requested != UINT32_MAX && requested >= linux_abi->fd_capacity))
        return HL_STATUS_INVALID_ARGUMENT;
    sync = hl_linux_sync(linux_abi);
    if (sync == NULL || sync->mutex_create == NULL || sync->mutex_close == NULL) return HL_STATUS_NOT_SUPPORTED;
    created = sync->mutex_create(linux_abi->host->context);
    if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID)
        return created.status == HL_STATUS_OK ? HL_STATUS_RESOURCE_LIMIT : (hl_status)created.status;

    /* Build the complete immutable adapter away from the table. Publication is one fd-store under ownership. */
    candidate.host_handle = HL_HOST_HANDLE_INVALID;
    candidate.status_flags = status_flags;
    candidate.references = 1;
    candidate.kind = kind;
    candidate.io_mutex = created.value;
    candidate.object_ops = ops;
    candidate.object_context = context;
    candidate.flock_token = hl_linux_new_ofd_token();
    hl_linux_lock(linux_abi);
    if (requested == UINT32_MAX)
        status = hl_linux_find_fd(linux_abi, &fd);
    else if (linux_abi->fds[requested].ofd != 0)
        status = HL_STATUS_ALREADY_EXISTS;
    else {
        fd = requested;
        status = HL_STATUS_OK;
    }
    if (status == HL_STATUS_OK) status = hl_linux_find_ofd(linux_abi, &ofd);
    if (status == HL_STATUS_OK) {
        candidate.generation = linux_abi->ofds[ofd].generation + 1;
        linux_abi->ofds[ofd] = candidate;
        linux_abi->fds[fd].descriptor_flags = descriptor_flags;
        linux_abi->fds[fd].generation++;
        linux_abi->fds[fd].ofd = ofd;
        *out_fd = fd;
    }
    hl_linux_unlock(linux_abi);
    if (status != HL_STATUS_OK) (void)sync->mutex_close(linux_abi->host->context, created.value);
    return status;
}

hl_status hl_linux_object_install(hl_linux_abi *linux_abi, const hl_linux_object_ops *ops, void *context, uint32_t kind,
                                  uint32_t status_flags, uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    return hl_linux_object_install_common(linux_abi, UINT32_MAX, ops, context, kind, status_flags, descriptor_flags,
                                          out_fd);
}

hl_status hl_linux_object_install_at(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_linux_object_ops *ops,
                                     void *context, uint32_t kind, uint32_t status_flags, uint32_t descriptor_flags) {
    hl_linux_fd installed = UINT32_MAX;
    return hl_linux_object_install_common(linux_abi, fd, ops, context, kind, status_flags, descriptor_flags,
                                          &installed);
}

static hl_status hl_linux_ofd_finalize(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd_entry,
                                       hl_host_handle *last_host_handle);
