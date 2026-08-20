#include "hl/linux_abi.h"
#if defined(HL_EMBEDDED_BUILD)
#include "../engine/provider/files.h"
#endif
#include "object.h"
#include "mapping_output.h"

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define HL_LINUX_FD_RESERVED UINT32_MAX

static _Atomic uint32_t g_linux_ofd_token_counter;

static uint64_t hl_linux_new_ofd_token(void) {
    uint64_t token = ((uint64_t)(uint32_t)getpid() << 32) |
                     (uint64_t)(atomic_fetch_add_explicit(&g_linux_ofd_token_counter, 1, memory_order_relaxed) + 1u);
    return token != 0 ? token : 1;
}

static hl_status hl_linux_fd_get_unlocked(const hl_linux_abi *linux_abi, hl_linux_fd fd,
                                          const hl_linux_fd_entry **fd_entry, const hl_linux_ofd_entry **ofd_entry);
static const hl_host_file_services *hl_linux_files(const hl_linux_abi *linux_abi);
static const hl_host_stream_services *hl_linux_streams(const hl_linux_abi *linux_abi);
static hl_status hl_linux_ofd_finalize(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd_entry,
                                       hl_host_handle *final_handle);

static hl_status hl_linux_ofd_finalize_owned(hl_linux_abi *linux_abi, hl_linux_ofd_entry *entry) {
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status status = hl_linux_ofd_finalize(linux_abi, entry, &handle);
    if (handle != HL_HOST_HANDLE_INVALID) {
        const hl_host_file_services *files = hl_linux_files(linux_abi);
        if (files == NULL || files->close == NULL) return status == HL_STATUS_OK ? HL_STATUS_NOT_SUPPORTED : status;
        hl_host_result closed = files->close(linux_abi->host->context, handle);
        if (status == HL_STATUS_OK && closed.status != HL_STATUS_OK) status = (hl_status)closed.status;
    }
    return status;
}

static void hl_linux_lock(hl_linux_abi *linux_abi) {
    while (atomic_flag_test_and_set_explicit(&linux_abi->table_lock, memory_order_acquire)) {}
}

static void hl_linux_unlock(hl_linux_abi *linux_abi) {
    atomic_flag_clear_explicit(&linux_abi->table_lock, memory_order_release);
}

static const hl_host_sync_services *hl_linux_sync(const hl_linux_abi *linux_abi) {
    const hl_host_services *host = linux_abi == NULL ? NULL : linux_abi->host;
    if (host == NULL || (host->capabilities & HL_HOST_CAP_SYNC) == 0 || host->sync == NULL ||
        host->sync->abi != HL_HOST_SYNC_ABI || host->sync->size < sizeof(*host->sync))
        return NULL;
    return host->sync;
}

static void hl_linux_fork_unpin(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    for (uint32_t index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        int finalize = 0;
        if (record->snapshot_pin == 0 || record->ofd >= linux_abi->ofd_capacity) continue;
        hl_linux_lock(linux_abi);
        hl_linux_ofd_entry *entry = &linux_abi->ofds[record->ofd];
        if (entry->generation == record->generation && entry->active_operations != 0) {
            entry->active_operations--;
            finalize = entry->active_operations == 0 && entry->references == 0 && entry->closing != 0;
        }
        record->snapshot_pin = 0;
        hl_linux_unlock(linux_abi);
        if (finalize) (void)hl_linux_ofd_finalize_owned(linux_abi, entry);
    }
}

/* After fork only the snapshot pin belongs to the child.  Counts for operations
 * running in other parent threads are copied memory, not live child owners. */
static void hl_linux_fork_child_abort(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    hl_linux_lock(linux_abi);
    for (uint32_t index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        if (record->snapshot_pin == 0 || record->ofd >= linux_abi->ofd_capacity) continue;
        hl_linux_ofd_entry *entry = &linux_abi->ofds[record->ofd];
        if (entry->generation == record->generation) entry->active_operations = 1;
    }
    hl_linux_unlock(linux_abi);
    hl_linux_fork_unpin(linux_abi, plan);
}

static void hl_linux_fork_discard_children(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    const hl_host_file_services *files = hl_linux_files(linux_abi);
    for (uint32_t index = plan->count; index != 0;) {
        hl_linux_fork_record *record = &plan->records[--index];
        if (record->object_ops != NULL)
            (void)record->object_ops->close(record->child_context);
        else if (files != NULL && files->close != NULL)
            (void)files->close(linux_abi->host->context, record->child_handle);
    }
    plan->count = 0;
}

static void hl_linux_ofd_lock(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd) {
    (void)hl_linux_sync(linux_abi)->mutex_lock(linux_abi->host->context, ofd->io_mutex);
}

static void hl_linux_ofd_unlock(hl_linux_abi *linux_abi, hl_linux_ofd_entry *ofd) {
    (void)hl_linux_sync(linux_abi)->mutex_unlock(linux_abi->host->context, ofd->io_mutex);
}

static int64_t hl_linux_error(hl_status status) {
    switch (status) {
    case HL_STATUS_OK: return 0;
    case HL_STATUS_INTERRUPTED: return -HL_LINUX_EINTR;
    case HL_STATUS_NOT_FOUND: return -HL_LINUX_EBADF;
    case HL_STATUS_WOULD_BLOCK: return -HL_LINUX_EAGAIN;
    case HL_STATUS_OUT_OF_MEMORY: return -HL_LINUX_ENOMEM;
    case HL_STATUS_PERMISSION_DENIED: return -HL_LINUX_EACCES;
    case HL_STATUS_BUSY: return -HL_LINUX_EBUSY;
    case HL_STATUS_NOT_DIRECTORY: return -HL_LINUX_ENOTDIR;
    case HL_STATUS_IS_DIRECTORY: return -HL_LINUX_EISDIR;
    case HL_STATUS_NAME_TOO_LONG: return -HL_LINUX_ENAMETOOLONG;
    case HL_STATUS_SYMLINK_LOOP: return -HL_LINUX_ELOOP;
    case HL_STATUS_READ_ONLY: return -HL_LINUX_EROFS;
    case HL_STATUS_ALREADY_EXISTS: return -HL_LINUX_EEXIST;
    case HL_STATUS_RESOURCE_LIMIT: return -HL_LINUX_ENFILE;
    case HL_STATUS_PROCESS_LIMIT: return -HL_LINUX_EMFILE;
    case HL_STATUS_DISCONNECTED: return -HL_LINUX_EPIPE;
    case HL_STATUS_CROSS_DEVICE: return -HL_LINUX_EXDEV;
    case HL_STATUS_NOT_EMPTY: return -HL_LINUX_ENOTEMPTY;
    case HL_STATUS_NO_SPACE: return -HL_LINUX_ENOSPC;
    case HL_STATUS_QUOTA: return -HL_LINUX_EDQUOT;
    case HL_STATUS_FILE_TOO_LARGE: return -HL_LINUX_EFBIG;
    case HL_STATUS_TIMED_OUT: return -HL_LINUX_ETIMEDOUT;
    case HL_STATUS_CONNECTION_REFUSED: return -HL_LINUX_ECONNREFUSED;
    case HL_STATUS_CONNECTION_RESET: return -HL_LINUX_ECONNRESET;
    case HL_STATUS_NETWORK_UNREACHABLE: return -HL_LINUX_ENETUNREACH;
    case HL_STATUS_ADDRESS_IN_USE: return -HL_LINUX_EADDRINUSE;
    case HL_STATUS_INVALID_ARGUMENT:
    case HL_STATUS_ABI_MISMATCH:
    case HL_STATUS_CORRUPT: return -HL_LINUX_EINVAL;
    case HL_STATUS_NOT_SUPPORTED: return -HL_LINUX_ENOSYS;
    case HL_STATUS_IO:
    case HL_STATUS_PLATFORM_FAILURE:
    default: return -HL_LINUX_EIO;
    }
}

static const hl_host_file_services *hl_linux_files(const hl_linux_abi *linux_abi) {
    const hl_host_services *host = linux_abi == NULL ? NULL : linux_abi->host;
    if (host == NULL || (host->capabilities & HL_HOST_CAP_FILE) == 0 || host->file == NULL ||
        host->file->abi != HL_HOST_FILE_ABI || host->file->size < sizeof(*host->file))
        return NULL;
    return host->file;
}

static const hl_host_stream_services *hl_linux_streams(const hl_linux_abi *linux_abi) {
    const hl_host_services *host = linux_abi == NULL ? NULL : linux_abi->host;
    if (host == NULL || (host->capabilities & HL_HOST_CAP_STREAM) == 0 || host->stream == NULL ||
        host->stream->abi != HL_HOST_STREAM_ABI || host->stream->size < sizeof(*host->stream) ||
        host->stream->set_status_flags == NULL)
        return NULL;
    return host->stream;
}

int64_t hl_linux_map_file(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t address, uint64_t offset, uint64_t size,
                          uint32_t protection, uint32_t flags, hl_host_file_mapping *mapping) {
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    const hl_host_memory_services *memory;
    hl_host_result result;
    hl_status status;
    if (!hl_linux_file_mapping_output_prepare(mapping)) return -HL_LINUX_EINVAL;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || linux_abi->size < sizeof(*linux_abi))
        return -HL_LINUX_EINVAL;
    memory = linux_abi->host != NULL ? linux_abi->host->memory : NULL;
    if (memory == NULL || memory->map_file == NULL) return -HL_LINUX_ENOSYS;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, NULL, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[(size_t)(found - linux_abi->ofds)];
    if (ofd->object_ops != NULL) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EINVAL;
    }
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    result =
        memory->map_file(linux_abi->host->context, ofd->host_handle, address, offset, size, protection, flags, mapping);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    return result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)result.status);
}

int hl_linux_writable_identity_open(hl_linux_abi *linux_abi, uint64_t device, uint64_t object) {
    if (linux_abi == NULL || linux_abi->host == NULL || linux_abi->host->file == NULL ||
        linux_abi->host->file->metadata == NULL)
        return 0;
    for (uint32_t index = 1; index < linux_abi->ofd_watermark; ++index) {
        hl_host_handle handle = HL_HOST_HANDLE_INVALID;
        uint32_t generation = 0;
        hl_linux_lock(linux_abi);
        hl_linux_ofd_entry *ofd = &linux_abi->ofds[index];
        if (ofd->references != 0 && ofd->object_ops == NULL &&
            (ofd->status_flags & HL_LINUX_O_ACCMODE) != HL_LINUX_O_RDONLY) {
            ofd->active_operations++;
            handle = ofd->host_handle;
            generation = ofd->generation;
        }
        hl_linux_unlock(linux_abi);
        if (handle == HL_HOST_HANDLE_INVALID) continue;
        hl_host_file_metadata metadata;
        hl_host_result result = linux_abi->host->file->metadata(linux_abi->host->context, handle, &metadata);
        int finalize = 0;
        hl_linux_lock(linux_abi);
        if (ofd->generation == generation && ofd->active_operations != 0) {
            ofd->active_operations--;
            finalize = ofd->active_operations == 0 && ofd->references == 0 && ofd->closing != 0;
        }
        hl_linux_unlock(linux_abi);
        if (finalize) (void)hl_linux_ofd_finalize_owned(linux_abi, ofd);
        if (result.status == HL_STATUS_OK && metadata.stable_device == device && metadata.stable_object == object)
            return 1;
    }
    return 0;
}

/* All helpers below through hl_linux_fd_get_unlocked require table_lock. */
static hl_status hl_linux_find_fd(const hl_linux_abi *linux_abi, hl_linux_fd *out_fd) {
    uint32_t fd;
    for (fd = 0; fd < linux_abi->fd_capacity; ++fd) {
        if (linux_abi->fds[fd].ofd == 0) {
            *out_fd = fd;
            return HL_STATUS_OK;
        }
    }
    return HL_STATUS_RESOURCE_LIMIT;
}

static hl_status hl_linux_find_fd_at_least(const hl_linux_abi *linux_abi, hl_linux_fd minimum, hl_linux_fd *out_fd) {
    uint32_t fd;
    if (minimum >= linux_abi->fd_capacity) return HL_STATUS_RESOURCE_LIMIT;
    for (fd = minimum; fd < linux_abi->fd_capacity; ++fd) {
        if (linux_abi->fds[fd].ofd == 0) {
            *out_fd = fd;
            return HL_STATUS_OK;
        }
    }
    return HL_STATUS_RESOURCE_LIMIT;
}

static hl_status hl_linux_find_ofd(hl_linux_abi *linux_abi, hl_linux_ofd *out_ofd) {
    uint32_t ofd;
    for (ofd = 1; ofd < linux_abi->ofd_capacity; ++ofd) {
        if (linux_abi->ofds[ofd].references == 0 && linux_abi->ofds[ofd].active_operations == 0 &&
            linux_abi->ofds[ofd].closing == 0 && linux_abi->ofds[ofd].io_mutex == HL_HOST_HANDLE_INVALID) {
            *out_ofd = ofd;
            /* Extend the live-OFD high-water mark so fork_prepare's snapshot scan reaches this slot. */
            if (ofd + 1 > linux_abi->ofd_watermark) linux_abi->ofd_watermark = ofd + 1;
            return HL_STATUS_OK;
        }
    }
    return HL_STATUS_RESOURCE_LIMIT;
}

static hl_status hl_linux_fd_get_unlocked(const hl_linux_abi *linux_abi, hl_linux_fd fd,
                                          const hl_linux_fd_entry **fd_entry, const hl_linux_ofd_entry **ofd_entry) {
    hl_linux_ofd ofd;
    if (fd >= linux_abi->fd_capacity || linux_abi->fds[fd].ofd == 0 || linux_abi->fds[fd].ofd == HL_LINUX_FD_RESERVED)
        return HL_STATUS_NOT_FOUND;
    ofd = linux_abi->fds[fd].ofd;
    if (ofd >= linux_abi->ofd_capacity || linux_abi->ofds[ofd].references == 0) return HL_STATUS_CORRUPT;
    if (fd_entry != NULL) *fd_entry = &linux_abi->fds[fd];
    if (ofd_entry != NULL) *ofd_entry = &linux_abi->ofds[ofd];
    return HL_STATUS_OK;
}

hl_status hl_linux_abi_init(hl_linux_abi *linux_abi, const hl_host_services *host, hl_linux_fd_entry *fd_storage,
                            uint32_t fd_capacity, hl_linux_ofd_entry *ofd_storage, uint32_t ofd_capacity) {
    if (linux_abi == NULL || host == NULL || fd_storage == NULL || ofd_storage == NULL || fd_capacity == 0 ||
        fd_capacity > HL_LINUX_FD_LIMIT || ofd_capacity < 2 || ofd_capacity > HL_LINUX_OFD_LIMIT)
        return HL_STATUS_INVALID_ARGUMENT;
    memset(linux_abi, 0, sizeof(*linux_abi));
    /* fd_storage/ofd_storage must be supplied zero-initialized by the caller
       (its sole caller calloc()s them, runtime.c). Re-zeroing here would write
       every byte of the ~5MB descriptor tables, faulting in all HL_LINUX_FD_LIMIT
       pages at startup even though a typical guest touches only a handful of fds;
       leaving them demand-zero keeps untouched slots off the resident set. */
    linux_abi->abi = HL_LINUX_ABI_VERSION;
    linux_abi->size = sizeof(*linux_abi);
    linux_abi->host = host;
    linux_abi->fds = fd_storage;
    linux_abi->fd_capacity = fd_capacity;
    linux_abi->ofds = ofd_storage;
    linux_abi->ofd_capacity = ofd_capacity;
    const hl_host_sync_services *sync = hl_linux_sync(linux_abi);
    if (sync == NULL || sync->mutex_create == NULL || sync->mutex_lock == NULL || sync->mutex_unlock == NULL ||
        sync->mutex_close == NULL) {
        linux_abi->abi = 0;
        return HL_STATUS_NOT_SUPPORTED;
    }
    atomic_flag_clear(&linux_abi->table_lock);
    return HL_STATUS_OK;
}

hl_status hl_linux_abi_destroy(hl_linux_abi *linux_abi) {
    uint32_t fd;
    uint32_t ofd;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    hl_linux_lock(linux_abi);
    for (fd = 0; fd < linux_abi->fd_capacity; ++fd) {
        if (linux_abi->fds[fd].ofd == HL_LINUX_FD_RESERVED) {
            hl_linux_unlock(linux_abi);
            return HL_STATUS_BUSY;
        }
    }
    for (ofd = 1; ofd < linux_abi->ofd_capacity; ++ofd) {
        if (linux_abi->ofds[ofd].references != 0 || linux_abi->ofds[ofd].active_operations != 0 ||
            linux_abi->ofds[ofd].closing != 0) {
            hl_linux_unlock(linux_abi);
            return HL_STATUS_BUSY;
        }
    }
    hl_linux_unlock(linux_abi);
    linux_abi->abi = 0;
    return HL_STATUS_OK;
}


#include "fork_snapshot.c"
#include "fork_completion.c"
#include "descriptor_install.c"
#include "object_access.c"
#include "descriptor_reservation.c"
#include "descriptor_lifecycle.c"
#include "stream_read.c"
#include "stream_write.c"
#include "file_control.c"
#include "descriptor_control.c"
#include "file_metadata.c"
