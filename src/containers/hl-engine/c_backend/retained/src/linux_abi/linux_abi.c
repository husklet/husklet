#include "hl/linux_abi.h"
#if defined(HL_EMBEDDED_BUILD)
#include "../core/provider/files.h"
#endif
#include "object.h"

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

int64_t hl_linux_map_file(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t address, uint64_t offset, uint64_t size,
                          uint32_t protection, uint32_t flags, hl_host_file_mapping *mapping) {
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    const hl_host_memory_services *memory;
    hl_host_result result;
    hl_status status;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || linux_abi->size < sizeof(*linux_abi) ||
        mapping == NULL || mapping->abi != HL_HOST_FILE_MAPPING_ABI || mapping->size < sizeof(*mapping))
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
       (its sole caller calloc()s them, engine.c). Re-zeroing here would write
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

hl_status hl_linux_abi_fork_prepare(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    const hl_host_file_services *files;
    const hl_host_sync_services *sync;
    uint32_t index;
    int topology_changed;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || plan == NULL ||
        plan->abi != HL_LINUX_ABI_VERSION || plan->size < sizeof(*plan) ||
        (plan->capacity != 0 && plan->records == NULL))
        return HL_STATUS_INVALID_ARGUMENT;
    files = hl_linux_files(linux_abi);
    sync = hl_linux_sync(linux_abi);
    if (files == NULL || files->clone_for_fork == NULL || files->close == NULL || sync == NULL ||
        sync->fork_prepare == NULL)
        return HL_STATUS_NOT_SUPPORTED;
retry_snapshot:
    plan->count = 0;
    plan->armed = 0;
    plan->host_completed = 0;
    topology_changed = 0;
    hl_linux_lock(linux_abi);
    if (linux_abi->reserved_fds != 0) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_BUSY;
    }
    for (index = 1; index < linux_abi->ofd_watermark; ++index) {
        hl_linux_ofd_entry *entry = &linux_abi->ofds[index];
        if (entry->references == 0) continue;
        /* A live operation retains the OFD and its host handle.  Host fork
           clones duplicate/share that same open-file description, so a
           blocked read is not a snapshot conflict and must not make fork
           spuriously fail EAGAIN.  Closing still changes ownership. */
        if (entry->active_operations != 0 && entry->object_ops != NULL &&
            entry->object_ops->fork_while_active_safe == 0) {
            hl_linux_unlock(linux_abi);
            plan->count = 0;
            return HL_STATUS_BUSY;
        }
        if (entry->closing != 0) {
            hl_linux_unlock(linux_abi);
            plan->count = 0;
            return HL_STATUS_BUSY;
        }
        if (plan->count == plan->capacity) {
            hl_linux_unlock(linux_abi);
            hl_linux_fork_unpin(linux_abi, plan);
            plan->count = 0;
            return HL_STATUS_RESOURCE_LIMIT;
        }
        entry->active_operations++; /* lifetime pin across the unlocked clone phase */
        plan->records[plan->count++] = (hl_linux_fork_record){
            .ofd = index,
            .generation = entry->generation,
            .parent_handle = entry->host_handle,
            .child_handle = HL_HOST_HANDLE_INVALID,
            .child_mutex = HL_HOST_HANDLE_INVALID,
            .object_ops = entry->object_ops,
            .parent_context = entry->object_context,
            .snapshot_pin = 1,
        };
    }
    hl_linux_unlock(linux_abi);
    /* External quiescence keeps snapshots stable; host callbacks run without the ABI table lock. */
    for (index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        hl_host_result cloned = {HL_STATUS_OK, 0, HL_HOST_HANDLE_INVALID, 0};
        hl_status clone_status = HL_STATUS_OK;
        if (record->object_ops != NULL) {
            if (record->object_ops->clone == NULL)
                clone_status = HL_STATUS_NOT_SUPPORTED;
            else
                clone_status = record->object_ops->clone(record->parent_context, &record->child_context);
            if (clone_status == HL_STATUS_OK && record->child_context == NULL)
                clone_status = HL_STATUS_PLATFORM_FAILURE;
        } else {
            cloned = files->clone_for_fork(linux_abi->host->context, record->parent_handle);
            clone_status = cloned.status == HL_STATUS_OK && cloned.value == HL_HOST_HANDLE_INVALID
                               ? HL_STATUS_PLATFORM_FAILURE
                               : (hl_status)cloned.status;
            record->child_handle = cloned.value;
        }
        if (clone_status != HL_STATUS_OK) {
            while (index != 0) {
                hl_linux_fork_record *rollback = &plan->records[--index];
                if (rollback->object_ops != NULL)
                    (void)rollback->object_ops->close(rollback->child_context);
                else
                    (void)files->close(linux_abi->host->context, rollback->child_handle);
            }
            hl_linux_fork_unpin(linux_abi, plan);
            plan->count = 0;
            return clone_status;
        }
    }
    hl_linux_lock(linux_abi);
    /* Require a bijection: no live OFD may have appeared during the unlocked clone phase. */
    for (index = 1; index < linux_abi->ofd_capacity; ++index) {
        hl_linux_ofd_entry *entry = &linux_abi->ofds[index];
        uint32_t record_index;
        uint32_t matches = 0;
        if (entry->references == 0) continue;
        for (record_index = 0; record_index < plan->count; ++record_index) {
            hl_linux_fork_record *record = &plan->records[record_index];
            if (record->ofd == index && record->generation == entry->generation &&
                record->parent_handle == entry->host_handle && record->object_ops == entry->object_ops &&
                record->parent_context == entry->object_context)
                matches++;
        }
        if (entry->active_operations > 1 && entry->object_ops != NULL &&
            entry->object_ops->fork_while_active_safe == 0) {
            goto arm_failed;
        }
        if (matches != 1 || entry->closing != 0) {
            topology_changed = 1;
            goto arm_failed;
        }
    }
    for (index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        if (record->ofd >= linux_abi->ofd_capacity || linux_abi->ofds[record->ofd].references == 0 ||
            linux_abi->ofds[record->ofd].generation != record->generation ||
            linux_abi->ofds[record->ofd].host_handle != record->parent_handle ||
            linux_abi->ofds[record->ofd].object_ops != record->object_ops ||
            linux_abi->ofds[record->ofd].object_context != record->parent_context) {
            topology_changed = 1;
            goto arm_failed;
        }
    }
    {
        hl_host_result armed = sync->fork_prepare(linux_abi->host->context);
        if (armed.status != HL_STATUS_OK) goto arm_failed;
    }
    plan->armed = 1;
    return HL_STATUS_OK;
arm_failed:
    hl_linux_unlock(linux_abi);
    for (uint32_t rollback_index = plan->count; rollback_index != 0;) {
        hl_linux_fork_record *rollback = &plan->records[--rollback_index];
        if (rollback->object_ops != NULL)
            (void)rollback->object_ops->close(rollback->child_context);
        else
            (void)files->close(linux_abi->host->context, rollback->child_handle);
    }
    hl_linux_fork_unpin(linux_abi, plan);
    plan->count = 0;
    if (topology_changed) goto retry_snapshot;
    return HL_STATUS_BUSY;
}

hl_status hl_linux_abi_fork_host_completed(hl_linux_fork_plan *plan) {
    if (plan == NULL || plan->abi != HL_LINUX_ABI_VERSION || plan->size < sizeof(*plan) || plan->armed == 0 ||
        plan->host_completed != 0)
        return HL_STATUS_INVALID_ARGUMENT;
    plan->host_completed = 1;
    return HL_STATUS_OK;
}

hl_status hl_linux_abi_fork_parent(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    const hl_host_file_services *files;
    const hl_host_sync_services *sync;
    hl_status status = HL_STATUS_OK;
    if (linux_abi == NULL || plan == NULL || plan->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    files = hl_linux_files(linux_abi);
    sync = hl_linux_sync(linux_abi);
    if (files == NULL || files->close == NULL || sync == NULL || sync->fork_parent == NULL)
        return HL_STATUS_NOT_SUPPORTED;
    if (plan->armed == 0) return HL_STATUS_INVALID_ARGUMENT;
    {
        hl_host_result completed = {HL_STATUS_OK, 1, 0, 0};
        if (plan->host_completed == 0) completed = sync->fork_parent(linux_abi->host->context);
        plan->armed = 0;
        plan->host_completed = 0;
        for (uint32_t index = 0; index < plan->count; ++index) {
            hl_linux_fork_record *record = &plan->records[index];
            if (record->snapshot_pin != 0 && record->ofd < linux_abi->ofd_capacity) {
                hl_linux_ofd_entry *entry = &linux_abi->ofds[record->ofd];
                if (entry->generation == record->generation && entry->active_operations != 0) {
                    entry->active_operations--;
                    /* Preserve a finalize request across the unlock.  Generation
                     * prevents a recycled slot from being finalized below. */
                    record->snapshot_pin =
                        entry->active_operations == 0 && entry->references == 0 && entry->closing != 0 ? 2 : 0;
                } else {
                    record->snapshot_pin = 0;
                }
            }
        }
        hl_linux_unlock(linux_abi);
        for (uint32_t index = 0; index < plan->count; ++index) {
            hl_linux_fork_record *record = &plan->records[index];
            if (record->snapshot_pin != 2 || record->ofd >= linux_abi->ofd_capacity) continue;
            hl_linux_ofd_entry *entry = &linux_abi->ofds[record->ofd];
            if (entry->generation == record->generation) (void)hl_linux_ofd_finalize_owned(linux_abi, entry);
            record->snapshot_pin = 0;
        }
        if (completed.status != HL_STATUS_OK) status = (hl_status)completed.status;
    }
    while (plan->count != 0) {
        hl_linux_fork_record *record = &plan->records[--plan->count];
        hl_status closed;
        if (record->object_ops != NULL)
            closed = record->object_ops->close(record->child_context);
        else
            closed = (hl_status)files->close(linux_abi->host->context, record->child_handle).status;
        if (closed != HL_STATUS_OK && status == HL_STATUS_OK) status = closed;
    }
    return status;
}

hl_status hl_linux_abi_fork_child(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    const hl_host_file_services *files;
    const hl_host_sync_services *sync;
    uint32_t index;
    if (linux_abi == NULL || plan == NULL || plan->abi != HL_LINUX_ABI_VERSION) return HL_STATUS_INVALID_ARGUMENT;
    files = hl_linux_files(linux_abi);
    sync = hl_linux_sync(linux_abi);
    if (files == NULL || files->close == NULL || sync == NULL || sync->mutex_create == NULL ||
        sync->mutex_close == NULL || sync->fork_child == NULL || plan->armed == 0)
        return HL_STATUS_NOT_SUPPORTED;
    {
        hl_host_result completed = {HL_STATUS_OK, 1, 0, 0};
        if (plan->host_completed == 0) completed = sync->fork_child(linux_abi->host->context);
        if (completed.status != HL_STATUS_OK) {
            hl_status status = (hl_status)completed.status;
            plan->armed = 0;
            plan->host_completed = 0;
            atomic_flag_clear(&linux_abi->table_lock);
            hl_linux_fork_child_abort(linux_abi, plan);
            hl_linux_fork_discard_children(linux_abi, plan);
            return status;
        }
    }
    plan->armed = 0;
    plan->host_completed = 0;
    atomic_flag_clear(&linux_abi->table_lock);
    /* Phase one validates every record and allocates every replacement lock without mutating an OFD. */
    for (index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        hl_host_result created;
        if (record->ofd >= linux_abi->ofd_capacity || linux_abi->ofds[record->ofd].generation != record->generation ||
            linux_abi->ofds[record->ofd].host_handle != record->parent_handle ||
            linux_abi->ofds[record->ofd].object_ops != record->object_ops ||
            linux_abi->ofds[record->ofd].object_context != record->parent_context)
            goto corrupt;
        created = sync->mutex_create(linux_abi->host->context);
        if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID) {
            hl_status status = created.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)created.status;
            while (index != 0)
                (void)sync->mutex_close(linux_abi->host->context, plan->records[--index].child_mutex);
            hl_linux_fork_child_abort(linux_abi, plan);
            hl_linux_fork_discard_children(linux_abi, plan);
            return status;
        }
        record->child_mutex = created.value;
    }
    /* Phase two cannot fail: swap validated handles/locks, then release this child's inherited copies. */
    for (index = 0; index < plan->count; ++index) {
        hl_linux_fork_record *record = &plan->records[index];
        hl_linux_ofd_entry *entry = &linux_abi->ofds[record->ofd];
        entry->active_operations = 0; /* parent peer operations do not survive fork */
        record->snapshot_pin = 0;
        (void)sync->mutex_close(linux_abi->host->context, entry->io_mutex);
        entry->io_mutex = record->child_mutex;
        if (record->object_ops != NULL) {
            entry->object_context = record->child_context;
            (void)record->object_ops->close(record->parent_context);
        } else {
            entry->host_handle = record->child_handle;
            (void)files->close(linux_abi->host->context, record->parent_handle);
        }
    }
    plan->count = 0;
    return HL_STATUS_OK;
corrupt:
    while (index != 0)
        (void)sync->mutex_close(linux_abi->host->context, plan->records[--index].child_mutex);
    hl_linux_fork_child_abort(linux_abi, plan);
    hl_linux_fork_discard_children(linux_abi, plan);
    return HL_STATUS_CORRUPT;
}

typedef struct hl_linux_spawn_context {
    hl_linux_abi *linux_abi;
    hl_linux_fork_plan *plan;
    hl_host_process_entry entry;
    void *entry_context;
} hl_linux_spawn_context;

static int32_t hl_linux_spawn_entry(void *opaque) {
    hl_linux_spawn_context *context = opaque;
    if (hl_linux_abi_fork_host_completed(context->plan) != HL_STATUS_OK ||
        hl_linux_abi_fork_child(context->linux_abi, context->plan) != HL_STATUS_OK)
        return 255;
    return context->entry(context->entry_context);
}

hl_status hl_linux_abi_spawn(hl_linux_abi *linux_abi, hl_host_process_entry entry, void *entry_context,
                             hl_host_handle *out_process) {
    const hl_host_process_services *processes;
    hl_linux_fork_plan plan = {0};
    hl_linux_spawn_context context;
    hl_host_result spawned;
    hl_status completed;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || entry == NULL || out_process == NULL)
        return HL_STATUS_INVALID_ARGUMENT;
    processes = linux_abi->host == NULL ? NULL : linux_abi->host->process;
    if (linux_abi->host == NULL || (linux_abi->host->capabilities & HL_HOST_CAP_PROCESS) == 0 || processes == NULL ||
        processes->abi != HL_HOST_PROCESS_ABI || processes->size < sizeof(*processes) ||
        processes->spawn_prepared == NULL)
        return HL_STATUS_NOT_SUPPORTED;
    *out_process = HL_HOST_HANDLE_INVALID;
    plan.abi = HL_LINUX_ABI_VERSION;
    plan.size = sizeof(plan);
    plan.capacity = linux_abi->ofd_capacity;
    plan.records = calloc(plan.capacity, sizeof(*plan.records));
    if (plan.records == NULL) return HL_STATUS_OUT_OF_MEMORY;
    completed = hl_linux_abi_fork_prepare(linux_abi, &plan);
    if (completed != HL_STATUS_OK) {
        free(plan.records);
        return completed;
    }
    context = (hl_linux_spawn_context){linux_abi, &plan, entry, entry_context};
    spawned = processes->spawn_prepared(linux_abi->host->context, hl_linux_spawn_entry, &context);
    completed = hl_linux_abi_fork_host_completed(&plan);
    if (completed == HL_STATUS_OK) completed = hl_linux_abi_fork_parent(linux_abi, &plan);
    free(plan.records);
    if (completed != HL_STATUS_OK) return completed;
    if (spawned.status != HL_STATUS_OK) return (hl_status)spawned.status;
    if (spawned.value == HL_HOST_HANDLE_INVALID) return HL_STATUS_PLATFORM_FAILURE;
    *out_process = spawned.value;
    return HL_STATUS_OK;
}

hl_status hl_linux_fd_install(hl_linux_abi *linux_abi, hl_host_handle host_handle, uint32_t status_flags,
                              uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    hl_linux_fd fd;
    hl_linux_ofd ofd;
    hl_host_handle mutex;
    hl_host_result created;
    const hl_host_sync_services *sync;
    hl_status status;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || host_handle == HL_HOST_HANDLE_INVALID ||
        out_fd == NULL)
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

hl_status hl_linux_object_pin_fd(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_object_pin *pin) {
    const hl_linux_fd_entry *descriptor;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    hl_status status;
    if (linux_abi == NULL || pin == NULL) return HL_STATUS_INVALID_ARGUMENT;
    memset(pin, 0, sizeof(*pin));
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &descriptor, &found);
    if (status == HL_STATUS_OK && found->object_ops == NULL) status = HL_STATUS_NOT_SUPPORTED;
    if (status == HL_STATUS_OK) {
        ofd = &linux_abi->ofds[descriptor->ofd];
        ofd->active_operations++;
        pin->linux_abi = linux_abi;
        pin->ofd = descriptor->ofd;
        pin->generation = ofd->generation;
        pin->ops = ofd->object_ops;
        pin->context = ofd->object_context;
    }
    hl_linux_unlock(linux_abi);
    if (status == HL_STATUS_OK) {
        hl_host_result locked = hl_linux_sync(linux_abi)->mutex_lock(linux_abi->host->context, ofd->io_mutex);
        if (locked.status != HL_STATUS_OK) {
            hl_linux_lock(linux_abi);
            ofd->active_operations--;
            hl_linux_unlock(linux_abi);
            memset(pin, 0, sizeof(*pin));
            return (hl_status)locked.status;
        }
    }
    return status;
}

hl_status hl_linux_object_pin_ofd(hl_linux_abi *linux_abi, hl_linux_ofd ofd_index, uint32_t generation,
                                  hl_linux_object_pin *pin) {
    hl_linux_ofd_entry *ofd;
    hl_host_result locked;
    if (linux_abi == NULL || pin == NULL || ofd_index == 0 || ofd_index >= linux_abi->ofd_capacity)
        return HL_STATUS_INVALID_ARGUMENT;
    memset(pin, 0, sizeof(*pin));
    hl_linux_lock(linux_abi);
    ofd = &linux_abi->ofds[ofd_index];
    if (ofd->generation != generation || ofd->references == 0 || ofd->closing != 0 || ofd->object_ops == NULL) {
        hl_linux_unlock(linux_abi);
        return HL_STATUS_NOT_FOUND;
    }
    ofd->active_operations++;
    pin->linux_abi = linux_abi;
    pin->ofd = ofd_index;
    pin->generation = generation;
    pin->ops = ofd->object_ops;
    pin->context = ofd->object_context;
    hl_linux_unlock(linux_abi);
    locked = hl_linux_sync(linux_abi)->mutex_lock(linux_abi->host->context, ofd->io_mutex);
    if (locked.status == HL_STATUS_OK) return HL_STATUS_OK;
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    memset(pin, 0, sizeof(*pin));
    return (hl_status)locked.status;
}

void hl_linux_object_unpin(hl_linux_object_pin *pin) {
    hl_linux_ofd_entry *ofd;
    int finalize = 0;
    if (pin == NULL || pin->linux_abi == NULL) return;
    hl_linux_lock(pin->linux_abi);
    ofd = &pin->linux_abi->ofds[pin->ofd];
    if (ofd->generation == pin->generation && ofd->active_operations != 0) {
        ofd->active_operations--;
        finalize = ofd->active_operations == 0 && ofd->references == 0 && ofd->closing != 0;
    }
    hl_linux_unlock(pin->linux_abi);
    (void)hl_linux_sync(pin->linux_abi)->mutex_unlock(pin->linux_abi->host->context, ofd->io_mutex);
    if (finalize) (void)hl_linux_ofd_finalize(pin->linux_abi, ofd, NULL);
    memset(pin, 0, sizeof(*pin));
}

hl_status hl_linux_object_unlock(hl_linux_object_pin *pin) {
    hl_host_result result;
    if (pin == NULL || pin->linux_abi == NULL) return HL_STATUS_INVALID_ARGUMENT;
    result = hl_linux_sync(pin->linux_abi)
                 ->mutex_unlock(pin->linux_abi->host->context, pin->linux_abi->ofds[pin->ofd].io_mutex);
    return (hl_status)result.status;
}

hl_status hl_linux_object_relock(hl_linux_object_pin *pin) {
    hl_host_result result;
    if (pin == NULL || pin->linux_abi == NULL) return HL_STATUS_INVALID_ARGUMENT;
    result = hl_linux_sync(pin->linux_abi)
                 ->mutex_lock(pin->linux_abi->host->context, pin->linux_abi->ofds[pin->ofd].io_mutex);
    return (hl_status)result.status;
}

void hl_linux_object_abandon(hl_linux_object_pin *pin) {
    hl_linux_ofd_entry *ofd;
    int finalize = 0;
    if (pin == NULL || pin->linux_abi == NULL) return;
    hl_linux_lock(pin->linux_abi);
    ofd = &pin->linux_abi->ofds[pin->ofd];
    if (ofd->generation == pin->generation && ofd->active_operations != 0) {
        ofd->active_operations--;
        finalize = ofd->active_operations == 0 && ofd->references == 0 && ofd->closing != 0;
    }
    hl_linux_unlock(pin->linux_abi);
    if (finalize) (void)hl_linux_ofd_finalize(pin->linux_abi, ofd, NULL);
    memset(pin, 0, sizeof(*pin));
}

int hl_linux_object_retired(hl_linux_object_pin *pin) {
    int retired;
    if (pin == NULL || pin->linux_abi == NULL) return 1;
    hl_linux_lock(pin->linux_abi);
    retired = pin->ofd >= pin->linux_abi->ofd_capacity ||
              pin->linux_abi->ofds[pin->ofd].generation != pin->generation ||
              pin->linux_abi->ofds[pin->ofd].references == 0 || pin->linux_abi->ofds[pin->ofd].closing != 0;
    hl_linux_unlock(pin->linux_abi);
    return retired;
}

uint32_t hl_linux_object_ready(hl_linux_object_pin *pin, uint32_t interests) {
    if (pin == NULL || pin->ops == NULL || pin->ops->readiness == NULL) return 0;
    return pin->ops->readiness(pin->context, interests) & (interests | HL_LINUX_READY_ERROR | HL_LINUX_READY_HANGUP);
}

int64_t hl_linux_object_poll(hl_linux_abi *linux_abi, hl_linux_poll_entry *entries, uint32_t count,
                             uint64_t deadline_ns) {
    const hl_host_clock_services *clock;
    uint32_t index;
    if (linux_abi == NULL || (count != 0 && entries == NULL)) return -HL_LINUX_EINVAL;
    clock = linux_abi->host->clock;
    if (deadline_ns != 0 && ((linux_abi->host->capabilities & HL_HOST_CAP_CLOCK) == 0 || clock == NULL ||
                             clock->monotonic_ns == NULL || clock->sleep_until == NULL))
        return -HL_LINUX_ENOSYS;
    for (;;) {
        int64_t count_ready = 0;
        for (index = 0; index < count; ++index) {
            hl_linux_object_pin pin;
            hl_status status;
            entries[index].readiness = 0;
            status = hl_linux_object_pin_fd(linux_abi, entries[index].fd, &pin);
            if (status == HL_STATUS_NOT_FOUND) {
                entries[index].readiness = HL_LINUX_READY_ERROR;
                count_ready++;
            } else if (status == HL_STATUS_NOT_SUPPORTED) {
                /* A NULL object adapter denotes an ordinary opaque host file. The
                 * typed layer deliberately has no native descriptor to poll; Linux
                 * regular files are immediately readable/writable, so readiness is
                 * derived from the logical request rather than host fd numbering. */
                hl_linux_fd_snapshot snapshot;
                status = hl_linux_fd_snapshot_get(linux_abi, entries[index].fd, &snapshot);
                if (status == HL_STATUS_NOT_FOUND) {
                    entries[index].readiness = HL_LINUX_READY_ERROR;
                    count_ready++;
                } else if (status != HL_STATUS_OK) {
                    return hl_linux_error(status);
                }
#if defined(HL_EMBEDDED_BUILD)
                else if (hl_provider_files_is_handle(snapshot.host_handle)) {
                    entries[index].readiness =
                        hl_provider_files_readiness(snapshot.host_handle, entries[index].interests);
                    if (entries[index].readiness != 0) count_ready++;
                }
#endif
                else {
                    uint32_t host_interests = 0;
                    uint32_t ready = 0;
                    hl_host_result observed = {.status = HL_STATUS_NOT_SUPPORTED};
                    if ((entries[index].interests & HL_LINUX_READY_READ) != 0) host_interests |= HL_HOST_READY_READ;
                    if ((entries[index].interests & HL_LINUX_READY_WRITE) != 0) host_interests |= HL_HOST_READY_WRITE;
                    if ((entries[index].interests & HL_LINUX_READY_ERROR) != 0) host_interests |= HL_HOST_READY_ERROR;
                    if ((entries[index].interests & HL_LINUX_READY_HANGUP) != 0) host_interests |= HL_HOST_READY_HANGUP;
                    if ((linux_abi->host->capabilities & HL_HOST_CAP_STREAM) != 0 && linux_abi->host->stream != NULL &&
                        linux_abi->host->stream->readiness != NULL)
                        observed = linux_abi->host->stream->readiness(linux_abi->host->context, snapshot.host_handle,
                                                                      host_interests);
                    if (observed.status == HL_STATUS_OK) {
                        if ((observed.value & HL_HOST_READY_READ) != 0) ready |= HL_LINUX_READY_READ;
                        if ((observed.value & HL_HOST_READY_WRITE) != 0) ready |= HL_LINUX_READY_WRITE;
                        if ((observed.value & HL_HOST_READY_ERROR) != 0) ready |= HL_LINUX_READY_ERROR;
                        if ((observed.value & HL_HOST_READY_HANGUP) != 0) ready |= HL_LINUX_READY_HANGUP;
                        entries[index].readiness = ready;
                    } else {
                        entries[index].readiness =
                            entries[index].interests & (HL_LINUX_READY_READ | HL_LINUX_READY_WRITE);
                    }
                    if (entries[index].readiness != 0) count_ready++;
                }
            } else if (status != HL_STATUS_OK) {
                return hl_linux_error(status);
            } else {
                entries[index].readiness = hl_linux_object_ready(&pin, entries[index].interests);
                hl_linux_object_unpin(&pin);
                if (entries[index].readiness != 0) count_ready++;
            }
        }
        if (count_ready != 0 || deadline_ns == 0) return count_ready;
        {
            hl_host_result now = clock->monotonic_ns(linux_abi->host->context);
            uint64_t slice;
            hl_host_result slept;
            if (now.status != HL_STATUS_OK) return hl_linux_error((hl_status)now.status);
            if (now.value >= deadline_ns) return 0;
            slice = now.value > UINT64_MAX - UINT64_C(1000000) ? deadline_ns : now.value + UINT64_C(1000000);
            if (slice > deadline_ns) slice = deadline_ns;
            slept = clock->sleep_until(linux_abi->host->context, HL_HOST_CLOCK_MONOTONIC, slice);
            if (slept.status != HL_STATUS_OK) return hl_linux_error((hl_status)slept.status);
        }
    }
}

hl_status hl_linux_fd_reserve_at(hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_fd_reservation *reservation) {
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || reservation == NULL ||
        fd >= linux_abi->fd_capacity)
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

hl_status hl_linux_fd_snapshot_get(const hl_linux_abi *linux_abi, hl_linux_fd fd, hl_linux_fd_snapshot *snapshot) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *ofd_entry;
    hl_status status;
    if (linux_abi == NULL || snapshot == NULL) return HL_STATUS_INVALID_ARGUMENT;
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

hl_status hl_linux_fd_dup(hl_linux_abi *linux_abi, hl_linux_fd source, uint32_t descriptor_flags, hl_linux_fd *out_fd) {
    const hl_linux_fd_entry *source_entry;
    hl_linux_fd fd;
    hl_status status;
    if (linux_abi == NULL || out_fd == NULL) return HL_STATUS_INVALID_ARGUMENT;
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

hl_status hl_linux_fd_exec(hl_linux_abi *linux_abi, hl_linux_fd fd, uint32_t *out_closed) {
    const hl_host_file_services *files;
    hl_host_handle handle = HL_HOST_HANDLE_INVALID;
    hl_status status;
    hl_host_result result;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || out_closed == NULL)
        return HL_STATUS_INVALID_ARGUMENT;
    *out_closed = 0;
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
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || out_closed == NULL)
        return HL_STATUS_INVALID_ARGUMENT;
    *out_closed = 0;
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

static int64_t hl_linux_file_control(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t size, uint32_t operation) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    hl_status status;
    int64_t result;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (operation == 0 && size > INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if (operation == 0 && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        if (operation == 0)
            host_result = files->truncate == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                                  : files->truncate(linux_abi->host->context, ofd->host_handle, size);
        else if (operation == 1)
            host_result = files->sync == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                              : files->sync(linux_abi->host->context, ofd->host_handle);
        else
            host_result = files->data_sync == NULL ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                                   : files->data_sync(linux_abi->host->context, ofd->host_handle);
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_ftruncate(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t size) {
    return hl_linux_file_control(linux_abi, fd, size, 0);
}

int64_t hl_linux_fsync(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_file_control(linux_abi, fd, 0, 1);
}

int64_t hl_linux_fdatasync(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_file_control(linux_abi, fd, 0, 2);
}

static int64_t hl_linux_extended_sync(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t offset, uint64_t size,
                                      uint32_t flags, int filesystem) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    hl_status status;
    int64_t result;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else if (filesystem)
        host_result = files->sync_filesystem == NULL
                          ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                          : files->sync_filesystem(linux_abi->host->context, ofd->host_handle),
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    else
        host_result = files->sync_range == NULL
                          ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                          : files->sync_range(linux_abi->host->context, ofd->host_handle, offset, size, flags),
        result = host_result.status == HL_STATUS_OK ? 0 : hl_linux_error((hl_status)host_result.status);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_sync_range(hl_linux_abi *linux_abi, hl_linux_fd fd, uint64_t offset, uint64_t size, uint32_t flags) {
    if ((flags & ~7u) != 0) return -HL_LINUX_EINVAL;
    return hl_linux_extended_sync(linux_abi, fd, offset, size, flags, 0);
}

int64_t hl_linux_sync_filesystem(hl_linux_abi *linux_abi, hl_linux_fd fd) {
    return hl_linux_extended_sync(linux_abi, fd, 0, 0, 0, 1);
}

static int64_t hl_linux_openat_install(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                       int32_t directory_fd, hl_host_handle direct_directory, const char *path,
                                       size_t path_size, uint32_t flags, uint32_t mode) {
    const uint32_t supported = HL_LINUX_O_ACCMODE | HL_LINUX_O_CREAT | HL_LINUX_O_EXCL | HL_LINUX_O_TRUNC |
                               HL_LINUX_O_APPEND | HL_LINUX_O_NONBLOCK | HL_LINUX_O_NOFOLLOW | HL_LINUX_O_DIRECTORY |
                               HL_LINUX_O_PATH | HL_LINUX_O_CLOEXEC;
    const hl_host_file_services *files;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *directory_ofd = NULL;
    hl_host_handle directory = direct_directory == HL_HOST_HANDLE_INVALID ? HL_HOST_HANDLE_CWD : direct_directory;
    hl_host_result opened;
    hl_linux_fd installed;
    uint32_t access;
    uint32_t creation = 0;
    hl_status status;
    if (linux_abi == NULL || path == NULL || path_size == 0 || (flags & ~supported) != 0) return -HL_LINUX_EINVAL;
    switch (flags & HL_LINUX_O_ACCMODE) {
    case HL_LINUX_O_RDONLY: access = HL_HOST_FILE_READ; break;
    case HL_LINUX_O_WRONLY: access = HL_HOST_FILE_WRITE; break;
    case HL_LINUX_O_RDWR: access = HL_HOST_FILE_READ | HL_HOST_FILE_WRITE; break;
    default: return -HL_LINUX_EINVAL;
    }
    if ((flags & HL_LINUX_O_APPEND) != 0) access |= HL_HOST_FILE_APPEND;
    if ((flags & HL_LINUX_O_NONBLOCK) != 0) access |= HL_HOST_FILE_NONBLOCK;
    if ((flags & HL_LINUX_O_NOFOLLOW) != 0) access |= HL_HOST_FILE_NOFOLLOW;
    if ((flags & HL_LINUX_O_DIRECTORY) != 0) access |= HL_HOST_FILE_DIRECTORY;
    if ((flags & HL_LINUX_O_PATH) != 0) access |= HL_HOST_FILE_PATH_ONLY;
    if ((flags & HL_LINUX_O_CREAT) != 0) creation |= HL_HOST_FILE_CREATE;
    if ((flags & HL_LINUX_O_EXCL) != 0) creation |= HL_HOST_FILE_EXCLUSIVE;
    if ((flags & HL_LINUX_O_TRUNC) != 0) creation |= HL_HOST_FILE_TRUNCATE;
    files = hl_linux_files(linux_abi);
    if (files == NULL || files->open_relative == NULL) return -HL_LINUX_ENOSYS;

    if (direct_directory == HL_HOST_HANDLE_INVALID && directory_fd != HL_LINUX_AT_FDCWD) {
        if (directory_fd < 0) return -HL_LINUX_EBADF;
        hl_linux_lock(linux_abi);
        status = hl_linux_fd_get_unlocked(linux_abi, (hl_linux_fd)directory_fd, NULL, &found);
        if (status != HL_STATUS_OK) {
            hl_linux_unlock(linux_abi);
            return -HL_LINUX_EBADF;
        }
        directory_ofd = &linux_abi->ofds[(size_t)(found - linux_abi->ofds)];
        directory_ofd->active_operations++;
        hl_linux_unlock(linux_abi);
        hl_linux_ofd_lock(linux_abi, directory_ofd);
        directory = directory_ofd->host_handle;
    }

    opened = files->open_relative(linux_abi->host->context, directory, path, path_size, access, creation, mode);
    if (directory_ofd != NULL) {
        hl_linux_lock(linux_abi);
        directory_ofd->active_operations--;
        hl_linux_unlock(linux_abi);
        hl_linux_ofd_unlock(linux_abi, directory_ofd);
    }
    if (opened.status != HL_STATUS_OK)
        return opened.status == HL_STATUS_NOT_FOUND ? -HL_LINUX_ENOENT : hl_linux_error((hl_status)opened.status);
    if (reservation != NULL) {
        status = hl_linux_fd_commit(linux_abi, reservation, opened.value, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                    (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
        installed = reservation->fd;
    } else {
        status = hl_linux_fd_install(linux_abi, opened.value, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                     (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0, &installed);
    }
    if (status != HL_STATUS_OK) {
        if (files->close != NULL) (void)files->close(linux_abi->host->context, opened.value);
        return status == HL_STATUS_RESOURCE_LIMIT ? -HL_LINUX_EMFILE : hl_linux_error(status);
    }
    return (int64_t)installed;
}

int64_t hl_linux_openat(hl_linux_abi *linux_abi, int32_t directory_fd, const char *path, size_t path_size,
                        uint32_t flags, uint32_t mode) {
    return hl_linux_openat_install(linux_abi, NULL, directory_fd, HL_HOST_HANDLE_INVALID, path, path_size, flags, mode);
}

int64_t hl_linux_openat_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                 int32_t directory_fd, const char *path, size_t path_size, uint32_t flags,
                                 uint32_t mode) {
    if (reservation == NULL) return -HL_LINUX_EINVAL;
    return hl_linux_openat_install(linux_abi, reservation, directory_fd, HL_HOST_HANDLE_INVALID, path, path_size, flags,
                                   mode);
}

int64_t hl_linux_openat_handle_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                        hl_host_handle directory, const char *path, size_t path_size, uint32_t flags,
                                        uint32_t mode) {
    if (reservation == NULL || directory == HL_HOST_HANDLE_INVALID) return -HL_LINUX_EINVAL;
    return hl_linux_openat_install(linux_abi, reservation, HL_LINUX_AT_FDCWD, directory, path, path_size, flags, mode);
}

int64_t hl_linux_file_adopt_reserved(hl_linux_abi *linux_abi, const hl_linux_fd_reservation *reservation,
                                     hl_host_handle file, uint32_t flags) {
    hl_status status;
    if (linux_abi == NULL || reservation == NULL || file == HL_HOST_HANDLE_INVALID) return -HL_LINUX_EINVAL;
    status = hl_linux_fd_commit(linux_abi, reservation, file, flags & ~(uint32_t)HL_LINUX_O_CLOEXEC,
                                (flags & HL_LINUX_O_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
    if (status == HL_STATUS_OK) return (int64_t)reservation->fd;
    return status == HL_STATUS_RESOURCE_LIMIT ? -HL_LINUX_EMFILE : hl_linux_error(status);
}

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
        // F_SETFL settable status flags: Linux lets a caller toggle O_APPEND, O_NONBLOCK and O_DIRECT (and
        // O_ASYNC/O_NOATIME) on an open description; accmode + creation flags are fixed. O_DIRECT was
        // previously dropped from the mask, so a F_SETFL(O_DIRECT) reported success yet F_GETFL never showed
        // it -- an inconsistent round-trip. Keep it so the get reflects the set (fcntl-cmds direct.consistent).
        uint32_t flags = (ofd_entry->status_flags & HL_LINUX_O_ACCMODE) |
                         ((uint32_t)argument & (HL_LINUX_O_APPEND | HL_LINUX_O_NONBLOCK | HL_LINUX_O_DIRECT));
        if (ofd->object_ops != NULL && ofd->object_ops->set_status_flags != NULL) {
            int64_t result;
            ofd->active_operations++;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_lock(linux_abi, ofd);
            result = ofd->object_ops->set_status_flags(ofd->object_context, flags);
            hl_linux_lock(linux_abi);
            if (result == 0) ofd->status_flags = flags;
            ofd->active_operations--;
            hl_linux_unlock(linux_abi);
            hl_linux_ofd_unlock(linux_abi, ofd);
            return result;
        }
        ofd->status_flags = flags;
        argument = 0;
        break;
    }
    default: hl_linux_unlock(linux_abi); return -HL_LINUX_EINVAL;
    }
    hl_linux_unlock(linux_abi);
    return (int64_t)argument;
}

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
