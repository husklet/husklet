#include "fork_output.h"

hl_status hl_linux_abi_fork_prepare(hl_linux_abi *linux_abi, hl_linux_fork_plan *plan) {
    const hl_host_file_services *files;
    const hl_host_sync_services *sync;
    uint32_t index;
    int topology_changed;
    if (!hl_linux_fork_plan_output_prepare(plan)) return HL_STATUS_INVALID_ARGUMENT;
    if (linux_abi == NULL || linux_abi->abi != HL_LINUX_ABI_VERSION || (plan->capacity != 0 && plan->records == NULL))
        return HL_STATUS_INVALID_ARGUMENT;
    files = hl_linux_files(linux_abi);
    sync = hl_linux_sync(linux_abi);
    if (files == NULL || files->clone_for_fork == NULL || files->close == NULL || sync == NULL ||
        sync->fork_prepare == NULL)
        return HL_STATUS_NOT_SUPPORTED;
retry_snapshot:
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
