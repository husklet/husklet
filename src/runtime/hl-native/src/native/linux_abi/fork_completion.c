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

