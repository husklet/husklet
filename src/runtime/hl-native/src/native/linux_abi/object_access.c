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
