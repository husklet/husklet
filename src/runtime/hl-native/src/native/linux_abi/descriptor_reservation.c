#include "reservation_output.h"

hl_status hl_linux_fd_reserve_at(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_fd_reservation *reservation) {
    if (!hl_linux_fd_reservation_output_prepare(reservation)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || fd >= linux_abi->fd_capacity)
        return HL_STATUS_INVALID_ARGUMENT;
    hl_linux_lock(linux_abi);
    if (linux_abi->fds[fd].ofd != 0) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_ALREADY_EXISTS;
    }
    linux_abi->fds[fd].generation++;
    linux_abi->fds[fd].ofd = HL_LINUX_FD_RESERVED;
    linux_abi->fds[fd].descriptor_flags = 0;
    linux_abi->reserved_fds++;
    *reservation = (hl_linux_fd_reservation){fd, linux_abi->fds[fd].generation};
    hl_linux_unlock(linux_abi);
    return HL_STATUS_OK;
}

hl_status hl_linux_fd_cancel(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation) {
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || reservation == NULL ||
        reservation->fd >= linux_abi->fd_capacity)
        return HL_STATUS_INVALID_ARGUMENT;
    hl_linux_lock(linux_abi);
    if (linux_abi->fds[reservation->fd].ofd != HL_LINUX_FD_RESERVED ||
        linux_abi->fds[reservation->fd].generation != reservation->generation) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_NOT_FOUND;
    }
    linux_abi->fds[reservation->fd].ofd = 0;
    if (linux_abi->reserved_fds != 0) linux_abi->reserved_fds--;
    hl_linux_unlock(linux_abi);
    return HL_STATUS_OK;
}

static hl_status hl_linux_fd_commit(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                    hl_host_handle host_handle, uint32_t status_flags, uint32_t descriptor_flags) {
    const hl_host_sync_services *sync;
    hl_host_result created;
    hl_linux_ofd ofd;
    hl_status status;
    if (linux_abi == NULL || reservation == NULL || host_handle == HL_HOST_HANDLE_INVALID)
        return HL_STATUS_INVALID_ARGUMENT;
    sync = hl_linux_sync(linux_abi);
    if (sync == NULL || sync->mutex_create == NULL || sync->mutex_close == NULL) return HL_STATUS_NOT_SUPPORTED;
    created = sync->mutex_create(linux_abi->host->context);
    if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID)
        return created.status == HL_STATUS_OK ? HL_STATUS_RESOURCE_LIMIT : (hl_status)created.status;
    hl_linux_lock(linux_abi);
    if (reservation->fd >= linux_abi->fd_capacity || linux_abi->fds[reservation->fd].ofd != HL_LINUX_FD_RESERVED ||
        linux_abi->fds[reservation->fd].generation != reservation->generation) {
        status = HL_STATUS_NOT_FOUND;
        goto done;
    }
    status = hl_linux_find_ofd(linux_abi, &ofd);
    if (status != HL_STATUS_OK) goto done;
    linux_abi->ofds[ofd].host_handle = host_handle;
    linux_abi->ofds[ofd].status_flags = status_flags;
    linux_abi->ofds[ofd].io_mutex = created.value;
    linux_abi->ofds[ofd].references = 1;
    linux_abi->ofds[ofd].generation++;
    linux_abi->ofds[ofd].flock_token = hl_linux_new_ofd_token();
    linux_abi->fds[reservation->fd].ofd = ofd;
    linux_abi->fds[reservation->fd].descriptor_flags = descriptor_flags;
    if (linux_abi->reserved_fds != 0) linux_abi->reserved_fds--; /* RESERVED -> committed */
done:
    hl_linux_unlock(linux_abi);
    if (status != HL_STATUS_OK) (void)sync->mutex_close(linux_abi->host->context, created.value);
    return status;
}

#include "snapshot_output.h"

hl_status hl_linux_fd_snapshot_get(const hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_fd_snapshot *snapshot) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *ofd_entry;
    hl_status status;
    if (!hl_linux_fd_snapshot_output_prepare(snapshot)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL) return HL_STATUS_INVALID_ARGUMENT;
    hl_linux_lock((hl_linux_abi *)linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &ofd_entry);
    if (status == HL_STATUS_OK) {
        snapshot->fd = fd;
        snapshot->ofd = fd_entry->ofd;
        snapshot->host_handle = ofd_entry->host_handle;
        snapshot->offset = ofd_entry->offset;
        snapshot->status_flags = ofd_entry->status_flags;
        snapshot->descriptor_flags = fd_entry->descriptor_flags;
        snapshot->descriptor_generation = fd_entry->generation;
        snapshot->ofd_generation = ofd_entry->generation;
        snapshot->descriptor_references = ofd_entry->references;
        snapshot->kind = ofd_entry->kind;
        snapshot->flock_token = ofd_entry->flock_token;
    }
    hl_linux_unlock((hl_linux_abi *)linux_abi);
    return status;
}
