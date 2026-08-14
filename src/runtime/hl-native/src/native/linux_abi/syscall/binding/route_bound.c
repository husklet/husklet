static int bound_route_io(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 57: /* close */
        ep_provider_retire_endpoint((int)source.fd);
        ep_object_retire_endpoint((int)source.fd);
        flock_broker_detach(&source);
        if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
            hl_host_file_metadata metadata;
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status == HL_STATUS_OK && metadata.type == HL_HOST_FILE_TYPE_REGULAR)
                flock_on_close_identity((int)source.fd, metadata.stable_device, metadata.stable_object);
            if (status.status == HL_STATUS_OK && metadata.type == HL_HOST_FILE_TYPE_REGULAR)
                poslk_on_close_identity(metadata.stable_device, metadata.stable_object);
        }
        result = hl_linux_close(g_linux_box, source.fd);
        proc_fdvis_close((int)source.fd);
        (void)close((int)source.fd);
        break;
    case 62: result = hl_linux_lseek(g_linux_box, source.fd, (int64_t)a1, (int32_t)a2); break;
    case 63: result = bound_guest_read(&source, a1, (size_t)a2, 0, 0); break;
    case 64: {
        int64_t allowed = bound_fsize_gate(c, &source, source.offset, a2); // RLIMIT_FSIZE -> SIGXFSZ/EFBIG
        result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, 0, 0);
    } break;
    case 67: result = bound_guest_read(&source, a1, (size_t)a2, a3, 1); break;
    case 68:
        if (source.status_flags & HL_LINUX_O_APPEND) {
            // Linux quirk: pwrite() on an O_APPEND fd IGNORES the supplied offset and appends at EOF (the
            // append is atomic, driven by the file's O_APPEND status flag, not the position argument). The
            // typed path honored a3 and overwrote, so route an O_APPEND pwrite through the appending write.
            int64_t allowed = bound_fsize_gate(c, &source, source.offset, a2);
            result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, 0, 0);
            if (result > 0) bound_mapping_file_written(&source, source.offset, (uint64_t)result);
        } else {
            int64_t allowed = bound_fsize_gate(c, &source, a3, a2); // RLIMIT_FSIZE at the explicit pwrite offset
            result = allowed < 0 ? allowed : bound_guest_write(&source, a1, (size_t)allowed, a3, 1);
            if (result > 0) bound_mapping_file_written(&source, a3, (uint64_t)result);
        }
        break;
    case 65:
    case 66:
    case 69:
    case 70: {
        static _Thread_local hl_host_iovec vectors[HL_LINUX_IOV_MAX];
        result = bound_vectors_prepare(&source, nr == 65 || nr == 69, a1, a2, vectors);
        if (result != 0) {
            // do_readv/do_writev test the access mode at fdget_pos, ahead of import_iovec.
            if (result == -HL_LINUX_EFAULT && bound_access_rejects(&source, nr == 65 || nr == 69)) result = -EBADF;
            break;
        }
        result = bound_vector_io(&source, vectors, (uint32_t)a2, nr == 65 || nr == 69, nr == 69 || nr == 70, a3);
        break;
    }
    case 213: {
        hl_host_file_metadata metadata;
        if ((int64_t)a1 < 0)
            result = -EINVAL;
        else if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL)
            result = -95; /* Linux EOPNOTSUPP; this route bypasses the native-to-Linux errno mapper. */
        else {
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            result = status.status != HL_STATUS_OK                               ? bound_host_error(status.status)
                     : metadata.type != HL_HOST_FILE_TYPE_REGULAR                ? -EINVAL
                     : hl_linux_pread64(g_linux_box, source.fd, NULL, 0, a1) < 0 ? -EBADF
                                                                                 : 0;
        }
        break;
    }
    case 286:
    case 287: {
        static _Thread_local hl_host_iovec vectors[HL_LINUX_IOV_MAX];
#ifdef CANON_X86ONLY
        uint64_t vector_offset = a3; /* x86-64 passes the complete 64-bit offset in argument 4. */
#else
        uint64_t vector_offset =
            (uint64_t)(uint32_t)a3 | ((uint64_t)(uint32_t)G_A4(c) << 32); /* AArch64 split offset. */
#endif
        result = bound_vectors_prepare(&source, nr == 286, a1, a2, vectors);
        if (result != 0) {
            if (result == -HL_LINUX_EFAULT && bound_access_rejects(&source, nr == 286)) result = -EBADF;
            break;
        }
        /* Flags are semantic requirements, not hints. Do not silently erase RWF_NOWAIT/APPEND/SYNC. */
        // RWF_APPEND is a semantic requirement: pwritev2 must ignore the supplied offset and land the
        // write at end-of-file, without moving the file position. The typed box takes no flags, so
        // resolve end-of-file from the file's own metadata and issue a positioned write there.
        uint64_t vector_flags = G_A5(c);
        if (nr == 287 && (vector_flags & 0x10u) != 0 && g_host_services != NULL && g_host_services->file != NULL &&
            g_host_services->file->metadata != NULL) {
            hl_host_file_metadata metadata;
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status != HL_STATUS_OK) {
                result = bound_host_error(status.status);
                break;
            }
            vector_offset = metadata.size;
            vector_flags &= ~UINT64_C(0x10);
        }
        if (vector_flags != 0) {
            result = -95; /* Linux EOPNOTSUPP; macOS's native value is 102. */
            break;
        }
        result = bound_vector_io(&source, vectors, (uint32_t)a2, nr == 286, vector_offset != UINT64_MAX, vector_offset);
        if (nr == 287 && result > 0)
            bound_mapping_file_written(&source, vector_offset == UINT64_MAX ? source.offset : vector_offset,
                                       (uint64_t)result);
        break;
    }
    case 46: {
        // RLIMIT_FSIZE: Linux (do_sys_ftruncate) raises SIGXFSZ and returns -EFBIG when the target length
        // exceeds the soft file-size limit, before touching the filesystem. The bound-descriptor path used
        // to skip this (only the bound write path enforced it), so an ftruncate grow past the limit on a
        // /tmp-backed fd silently succeeded. No-op for an infinite limit (the common case).
        {
            uint64_t fslim = guest_fsize_cur();
            if (fslim != ~UINT64_C(0) && a1 > fslim) {
                raise_guest_signal(c, 25); // SIGXFSZ
                result = -EFBIG;
                break;
            }
        }
        hl_host_file_metadata metadata = {0};
        int have_metadata = 0, prepared = 0;
        if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
            hl_host_result status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            have_metadata = status.status == HL_STATUS_OK;
        }
        if (have_metadata && a1 < metadata.size) {
            gbus_prepare();
            prepared = 1;
        }
        result = hl_linux_ftruncate(g_linux_box, source.fd, a1);
        if (result == 0 && have_metadata) {
            /* The local truncate is authoritative. Publish its generation
             * before releasing the BUS transition so the host watcher drops
             * the matching notification instead of replaying the shrink. */
            bound_watch_publish_size(metadata.stable_device, metadata.stable_object, a1);
            pthread_mutex_lock(&g_bound_mapping_gate);
            pthread_mutex_lock(&g_bound_mapping_lock);
            bound_mapping_file_size_changed(&source, &metadata, 1, metadata.size, a1, NULL);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            hl_linux_file_event_publish(HL_LINUX_FILE_EVENT_RESIZE, metadata.stable_device, metadata.stable_object,
                                        metadata.size, a1);
        }
        if (prepared) { gbus_prepare_release(); }
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route_sync(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 82: /* fsync */
    case 83: /* fdatasync */
        /* An O_PATH descriptor names a file but is not open for I/O; Linux
           rejects the sync family through it with EBADF (fs/sync.c). */
        result = (source.status_flags & HL_LINUX_O_PATH) != 0
                     ? -EBADF
                     : (nr == 82 ? hl_linux_fsync(g_linux_box, source.fd) : hl_linux_fdatasync(g_linux_box, source.fd));
        break;
    case 84:
        if ((G_A3(c) & ~(uint64_t)7u) != 0)
            result = -EINVAL;
        else
            result = hl_linux_sync_range(g_linux_box, source.fd, a1, a2, (uint32_t)G_A3(c));
        break;
    case 267: result = hl_linux_sync_filesystem(g_linux_box, source.fd); break;
    case 80: {
        hl_linux_file_status status;
        result = hl_linux_fstat(g_linux_box, source.fd, &status);
        if (result == 0 &&
            guest_accessible_prefix(a1, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES)
            result = -EFAULT;
        if (result == 0) bound_virtualize_namespace(source.fd, &status);
        if (result == 0) bound_virtualize_owner(&source, &status);
        if (result == 0) {
            uint8_t encoded[GUEST_LINUX_STAT_BYTES];
            fill_linux_bound_stat(encoded, &status);
            if (guest_copy_to(a1, encoded, sizeof encoded) != sizeof encoded) result = -EFAULT;
        }
        break;
    }
    case 291: {
        uint64_t flags = a2;
        uint64_t mask = a3;
        uint64_t output = G_A4(c);
        char path_byte;
        if ((flags & ~UINT64_C(0x7900)) != 0 || (flags & UINT64_C(0x6000)) == UINT64_C(0x6000) ||
            (mask & UINT64_C(0x80000000)) != 0) {
            result = -EINVAL;
            break;
        }
        if (a1 == 0 || guest_copy_from(&path_byte, a1, 1) != 1) {
            result = -EFAULT;
            break;
        }
        if (path_byte != 0 || (flags & UINT64_C(0x1000)) == 0) return 0;
        if (guest_accessible_prefix(output, 256, HL_LOGICAL_VMA_WRITE) != 256) {
            result = -EFAULT;
            break;
        }
        hl_linux_file_status status;
        result = hl_linux_fstat(g_linux_box, source.fd, &status);
        if (result == 0) {
            bound_virtualize_namespace(source.fd, &status);
            bound_virtualize_owner(&source, &status);
            uint8_t encoded[256];
            bound_fill_statx(encoded, &status);
            if (guest_copy_to(output, encoded, sizeof encoded) != sizeof encoded) result = -EFAULT;
        }
        break;
    }
    case 44: {
        hl_host_filesystem_metadata metadata;
        hl_host_result status;
        if (!bound_file_abi14() || g_host_services->file->filesystem_metadata == NULL) {
            result = -ENOSYS;
            break;
        }
        status = g_host_services->file->filesystem_metadata(g_host_services->context, source.host_handle, &metadata);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        if (guest_accessible_prefix(a1, 120, HL_LOGICAL_VMA_WRITE) != 120) {
            result = -EFAULT;
            break;
        }
        uint8_t encoded[120];
        bound_fill_statfs(encoded, &metadata);
        result = guest_copy_to(a1, encoded, sizeof encoded) == sizeof encoded ? 0 : -EFAULT;
        break;
    }
    case 47: {
        hl_host_file_metadata before = {0}, after = {0};
        hl_host_result status;
        uint32_t mode = (uint32_t)a1;
        int prepared = 0;
        if (a1 > UINT32_MAX || a2 > INT64_MAX || a3 == 0 || a3 > INT64_MAX) {
            result = -EINVAL;
            break;
        }
        // A mode bit outside FALLOC_FL_SUPPORTED_MASK (KEEP_SIZE|PUNCH_HOLE|COLLAPSE_RANGE|ZERO_RANGE|
        // INSERT_RANGE|UNSHARE_RANGE == 0x7b) is -EOPNOTSUPP on Linux, not -EINVAL (do_fallocate returns
        // -EOPNOTSUPP for anything it does not implement). Mirrors the native path in fs.c case 47.
        if (mode & ~0x7bu) {
            result = -95; // Linux EOPNOTSUPP; hard-coded because this result is not run through the
                          // macOS->Linux errno map (a host EOPNOTSUPP is 102 on Darwin). Cf. time.c case 115.
            break;
        }
        if (a2 > INT64_MAX - a3) {
            result = -EFBIG;
            break;
        }
        // RLIMIT_FSIZE: a fallocate that reserves past the soft file-size limit raises SIGXFSZ/-EFBIG (Linux
        // gates it even with FALLOC_FL_KEEP_SIZE). Only the size-extending modes are bounded -- PUNCH_HOLE /
        // COLLAPSE_RANGE / INSERT_RANGE never place data beyond the current end, so they are exempt.
        {
            uint64_t fslim = guest_fsize_cur();
            if (fslim != ~UINT64_C(0) && (a2 + a3) > fslim &&
                (mode & (HL_HOST_FILE_ALLOC_PUNCH_HOLE | HL_HOST_FILE_ALLOC_COLLAPSE_RANGE |
                         HL_HOST_FILE_ALLOC_INSERT_RANGE)) == 0) {
                raise_guest_signal(c, 25); // SIGXFSZ
                result = -EFBIG;
                break;
            }
        }
        if (!bound_file_abi14() || g_host_services->file->allocate_range == NULL) {
            result = -ENOSYS;
            break;
        }
        status = g_host_services->file->metadata(g_host_services->context, source.host_handle, &before);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        if ((mode & HL_HOST_FILE_ALLOC_COLLAPSE_RANGE) != 0) {
            gbus_prepare();
            prepared = 1;
        }
        status = g_host_services->file->allocate_range(g_host_services->context, source.host_handle, mode, a2, a3);
        result = bound_host_error(status.status);
        if (status.status == HL_STATUS_OK &&
            g_host_services->file->metadata(g_host_services->context, source.host_handle, &after).status ==
                HL_STATUS_OK) {
            bound_watch_publish_size(after.stable_device, after.stable_object, after.size);
            pthread_mutex_lock(&g_bound_mapping_gate);
            pthread_mutex_lock(&g_bound_mapping_lock);
            if (before.size != after.size)
                bound_mapping_file_size_changed(&source, &after, 1, before.size, after.size, NULL);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            bound_mapping_file_data_changed(&source, after.stable_device, after.stable_object);
        }
        if (prepared) gbus_prepare_release();
        break;
    }
    case 52: {
        if (g_host_services->file->set_permissions == NULL) {
            result = -ENOSYS;
            break;
        }
        hl_host_result status =
            g_host_services->file->set_permissions(g_host_services->context, source.host_handle, (uint32_t)a1 & 07777u);
        result = bound_host_error(status.status);
        if (result == 0) {
            char path[HL_LINUX_PATH_MAX + 1];
            hl_host_result named = g_host_services->file->path(g_host_services->context, source.host_handle,
                                                               (hl_host_bytes){path, HL_LINUX_PATH_MAX});
            if (named.status == HL_STATUS_OK && named.value <= HL_LINUX_PATH_MAX) {
                path[named.value] = 0;
                mode_xattr_set_path(path, (mode_t)a1);
                hl_fdcache_evict_path(path);
            }
        }
        break;
    }
    case 55: {
        char path[HL_LINUX_PATH_MAX + 1];
        hl_host_result status = g_host_services->file->path(g_host_services->context, source.host_handle,
                                                            (hl_host_bytes){path, HL_LINUX_PATH_MAX});
        if (status.status != HL_STATUS_OK || status.value > HL_LINUX_PATH_MAX) {
            result = bound_host_error(status.status);
            break;
        }
        path[status.value] = 0;
        hl_owner_set_path(path, dac_requested_id(a1), dac_requested_id(a2), 0);
        result = 0;
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route_metadata(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 88: {
        hl_host_file_time times[2];
        struct timespec guest_times[2];
        const struct timespec *guest = a2 ? guest_times : NULL;
        char relative[HL_LINUX_PATH_MAX + 1];
        size_t relative_size = 0;
        hl_host_handle target = source.host_handle;
        int close_target = 0;
        if (a3 & ~UINT64_C(0x100)) {
            result = -EINVAL;
            break;
        }
        if (a1 != 0) {
            result = bound_path_copy(a1, relative, &relative_size);
            if (result != 0) { break; }
            /* Absolute paths ignore dirfd and remain on the common namespace route. Relative paths
             * resolve beneath the opaque directory and update the independently opened target. */
            if (relative[0] == '/') return 0;
            if (g_host_services->file->open_relative == NULL) {
                result = -ENOSYS;
                break;
            }
            uint32_t access = HL_HOST_FILE_PATH_ONLY;
            if (a3 & UINT64_C(0x100)) access |= HL_HOST_FILE_NOFOLLOW;
            hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle,
                                                                         relative, relative_size, access, 0, 0);
            if (opened.status != HL_STATUS_OK) {
                result = bound_host_error(opened.status);
                break;
            }
            target = opened.value;
            close_target = 1;
        }
        if (g_host_services->file->set_times == NULL) {
            result = -ENOSYS;
            goto bound_set_times_done;
        }
        if (guest != NULL && guest_copy_from(guest_times, a2, sizeof guest_times) != sizeof guest_times) {
            result = -EFAULT;
            goto bound_set_times_done;
        }
        for (int index = 0; index < 2; ++index) {
            int64_t nanoseconds = guest == NULL ? INT64_C(0x3fffffff) : (int64_t)guest[index].tv_nsec;
            times[index].seconds = guest == NULL ? 0 : (int64_t)guest[index].tv_sec;
            times[index].nanoseconds = 0;
            if (nanoseconds == INT64_C(0x3fffffff))
                times[index].mode = HL_HOST_FILE_TIME_NOW;
            else if (nanoseconds == INT64_C(0x3ffffffe))
                times[index].mode = HL_HOST_FILE_TIME_OMIT;
            else if (nanoseconds >= 0 && nanoseconds < INT64_C(1000000000)) {
                times[index].nanoseconds = (uint32_t)nanoseconds;
                times[index].mode = HL_HOST_FILE_TIME_EXPLICIT;
            } else {
                result = -EINVAL;
                goto bound_set_times_done;
            }
        }
        result = bound_host_error(g_host_services->file->set_times(g_host_services->context, target, times).status);
    bound_set_times_done:
        if (close_target) (void)g_host_services->file->close(g_host_services->context, target);
        break;
    }
    case 32: {
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
        if (status.status != HL_STATUS_OK) {
            result = bound_host_error(status.status);
            break;
        }
        result = hl_flock_identity(&source, metadata.stable_device, metadata.stable_object, (int)a1) < 0
                     ? -(int64_t)(errno == EWOULDBLOCK ? 11 : errno)
                     : 0;
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}


static int64_t bound_provider_control(hl_linux_fd_snapshot source, uint32_t request, uint64_t guest_argument) {
    int64_t result;
    do {
        uint32_t direction = request >> 30;
        uint32_t argument_size = (request >> 16) & 0x3fffu;
        if (argument_size > 16384) {
            result = -E2BIG;
            break;
        }
        unsigned char *provider_argument = argument_size == 0 ? NULL : calloc(argument_size, 1);
        if (argument_size != 0 && provider_argument == NULL) {
            result = -ENOMEM;
            break;
        }
        if (argument_size != 0 && guest_argument == 0) {
            free(provider_argument);
            result = -EFAULT;
            break;
        }
        if ((direction & 1u) != 0 &&
            guest_copy_from(provider_argument, guest_argument, argument_size) != argument_size) {
            free(provider_argument);
            result = -EFAULT;
            break;
        }
        hl_provider_ioctl_result ioctl_result = {0};
        hl_host_result called =
            hl_provider_files_ioctl(source.host_handle, request, provider_argument, argument_size, &ioctl_result);
        if (called.status != HL_STATUS_OK)
            result = called.detail != 0 ? -(int64_t)called.detail : bound_host_error(called.status);
        else
            result = (int64_t)called.value;
        if (result >= 0 && (direction & 2u) != 0 &&
            guest_copy_to(guest_argument, provider_argument, argument_size) != argument_size)
            result = -EFAULT;
        if (result >= 0) {
            for (uint32_t i = 0; i < ioctl_result.write_count; ++i) {
                hl_provider_ioctl_write *write = &ioctl_result.writes[i];
                if (write->address == 0 ||
                    guest_copy_to(write->address, write->bytes, write->size) != write->size) {
                    result = -EFAULT;
                    break;
                }
            }
        }
        hl_provider_files_ioctl_result_destroy(&ioctl_result);
        free(provider_argument);
        break;
    } while (0);
    return result;
}

static int64_t bound_native_control(hl_linux_fd_snapshot source, uint32_t request, uint64_t guest_argument) {
    int64_t result;
    do {
    uint8_t argument[44] = {0};
    size_t argument_size = 0;
    int argument_input = 0, argument_output = 0;
    if (request == 0x5401u) argument_size = 36, argument_output = 1;
    if (request >= 0x5402u && request <= 0x5404u) argument_size = 36, argument_input = 1;
    if (request == 0x802c542au) argument_size = 44, argument_output = 1;
    if (request >= 0x402c542bu && request <= 0x402c542du) argument_size = 44, argument_input = 1;
    if (request == 0x5413u) argument_size = sizeof(struct winsize), argument_output = 1;
    if (request == 0x5414u) argument_size = sizeof(struct winsize), argument_input = 1;
    if (request == 0x5421u || request == 0x5410u) argument_size = sizeof(int), argument_input = 1;
    if (request == 0x541bu || request == 0x540fu) argument_size = sizeof(int), argument_output = 1;
    if (argument_input && guest_copy_from(argument, guest_argument, argument_size) != (ssize_t)argument_size) {
        result = -EFAULT;
        break;
    }
    if (request == 0x5451u || request == 0x5450u) { /* FIOCLEX / FIONCLEX */
        result =
            hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_SETFD, request == 0x5451u ? HL_LINUX_FD_CLOEXEC : 0);
    } else if (request == 0x5421u) { /* FIONBIO */
        int enabled = 0;
        memcpy(&enabled, argument, sizeof(enabled));
        int64_t flags = hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_GETFL, 0);
        if (flags < 0) {
            result = flags;
            break;
        }
        if (enabled)
            flags |= HL_LINUX_O_NONBLOCK;
        else
            flags &= ~(int64_t)HL_LINUX_O_NONBLOCK;
        result = hl_linux_fcntl(g_linux_box, source.fd, HL_LINUX_F_SETFL, (uint64_t)flags);
    } else if (request == 0x541bu) { /* FIONREAD */
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
        int64_t offset = hl_linux_lseek(g_linux_box, source.fd, 0, SEEK_CUR);
        if (status.status != HL_STATUS_OK)
            result = bound_host_error(status.status);
        else if (metadata.type != HL_HOST_FILE_TYPE_REGULAR || offset < 0)
            result = metadata.type != HL_HOST_FILE_TYPE_REGULAR ? -ENOTTY : offset;
        else {
            uint64_t available = metadata.size > (uint64_t)offset ? metadata.size - (uint64_t)offset : 0;
            int encoded = available > INT_MAX ? INT_MAX : (int)available;
            memcpy(argument, &encoded, sizeof(encoded));
            result = 0;
        }
    } else if (request == 0x5401u || request == 0x5402u || request == 0x5403u || request == 0x5404u ||
               request == 0x5413u || request == 0x5414u || request == 0x540fu || request == 0x5410u ||
               request == 0x540eu || request == 0x802c542au || request == 0x402c542bu || request == 0x402c542cu ||
               request == 0x402c542du) {
        int native_fd = -1;
        int borrowed = bound_attachment_borrow((int)source.fd, &native_fd);
        if (borrowed < 0) {
            result = borrowed;
            break;
        }
        if (request == 0x5401u) { /* TCGETS */
            struct termios native;
            if (tcgetattr(native_fd, &native) != 0)
                result = -errno;
            else {
#if defined(__linux__)
                memcpy(argument, &native, 36);
#else
                termios_m2l(&native, argument);
#endif
                result = 0;
            }
        } else if (request == 0x802c542au) { /* TCGETS2 */
            /* Linux termios2 has an encoded 44-byte payload. On the Linux/aarch64 host its ABI is
             * byte-identical to the aarch64 guest ABI, so preserve the extended speed fields and
             * forward the complete request. macOS has no termios2 request, so translate its native
             * termios and explicitly populate the two Linux speed fields. */
#if defined(__linux__)
            result = ioctl(native_fd, request, argument) == 0 ? 0 : -errno;
#else
            {
                struct termios native;
                if (tcgetattr(native_fd, &native) != 0)
                    result = -errno;
                else {
                    uint32_t input_speed = (uint32_t)cfgetispeed(&native);
                    uint32_t output_speed = (uint32_t)cfgetospeed(&native);
                    termios_m2l(&native, argument);
                    memcpy(argument + 36, &input_speed, sizeof(input_speed));
                    memcpy(argument + 40, &output_speed, sizeof(output_speed));
                    result = 0;
                }
            }
#endif
        } else if (request >= 0x402c542bu && request <= 0x402c542du) { /* TCSETS2/W2/F2 */
#if defined(__linux__)
            result = ioctl(native_fd, request, argument) == 0 ? 0 : -errno;
#else
            {
                struct termios native;
                uint32_t input_speed, output_speed;
                termios_l2m(argument, &native);
                memcpy(&input_speed, argument + 36, sizeof(input_speed));
                memcpy(&output_speed, argument + 40, sizeof(output_speed));
                (void)cfsetispeed(&native, input_speed);
                (void)cfsetospeed(&native, output_speed);
                int action = request == 0x402c542bu ? TCSANOW : request == 0x402c542cu ? TCSADRAIN : TCSAFLUSH;
                result = tcsetattr(native_fd, action, &native) == 0 ? 0 : -errno;
            }
#endif
        } else if (request >= 0x5402u && request <= 0x5404u) { /* TCSETS{,W,F} */

            struct termios native;
            {
#if defined(__linux__)
                memset(&native, 0, sizeof(native));
                memcpy(&native, argument, 36);
#else
                termios_l2m(argument, &native);
#endif
                int action = request == 0x5402u ? TCSANOW : request == 0x5403u ? TCSADRAIN : TCSAFLUSH;
                result = tcsetattr(native_fd, action, &native) == 0 ? 0 : -errno;
            }
        } else if (request == 0x5413u || request == 0x5414u) { /* TIOCGWINSZ/TIOCSWINSZ */
            result = ioctl(native_fd, request == 0x5413u ? TIOCGWINSZ : TIOCSWINSZ, argument) == 0 ? 0 : -errno;
        } else if (request == 0x540fu) { /* TIOCGPGRP */
            {
                pid_t group = tcgetpgrp(native_fd);
                if (group < 0)
                    result = -errno;
                else {
                    int encoded = group == g_init_hostpid ? 1 : (int)group;
                    memcpy(argument, &encoded, sizeof(encoded));
                    result = 0;
                }
            }
        } else if (request == 0x5410u) { /* TIOCSPGRP */
            {
                int encoded;
                memcpy(&encoded, argument, sizeof(encoded));
                pid_t group = encoded;
                if (group == 1 && g_init_hostpid) group = g_init_hostpid;
                result = tcsetpgrp(native_fd, group) == 0 ? 0 : -errno;
            }
        } else { /* TIOCSCTTY */
            result = ioctl(native_fd, TIOCSCTTY, 0) == 0 || errno == EPERM ? 0 : -errno;
        }
        if (borrowed > 0) bound_attachment_release(native_fd);
    } else {
        result = -ENOTTY;
    }
    if (result >= 0 && argument_output &&
        guest_copy_to(guest_argument, argument, argument_size) != (ssize_t)argument_size)
        result = -EFAULT;
    } while (0);
    return result;
}

static int bound_route_control(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    if (nr != 29) return 0;
    result = hl_provider_files_is_handle(source.host_handle)
                 ? bound_provider_control(source, (uint32_t)a1, a2)
                 : bound_native_control(source, (uint32_t)a1, a2);
    G_RET(c) = (uint64_t)result;
    (void)a0; (void)a3; (void)a4;
    return 1;
}

static int bound_route_directory(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 61: {
        uint64_t byte_capacity = a2 > UINT32_C(1 << 20) ? UINT32_C(1 << 20) : a2;
        if (a2 < 24) {
            result = -EINVAL;
            break;
        }
        if (a1 == 0 || byte_capacity > SIZE_MAX ||
            guest_accessible_prefix(a1, (size_t)byte_capacity, HL_LOGICAL_VMA_WRITE) != byte_capacity) {
            result = -EFAULT;
            break;
        }
        uint32_t capacity = (uint32_t)(byte_capacity / 24);
        hl_host_file_entry *entries = calloc(capacity, sizeof(*entries));
        if (entries == NULL) {
            result = -ENOMEM;
            break;
        }
        hl_host_result read = g_host_services->file->read_directory(g_host_services->context, source.host_handle,
                                                                    entries, capacity, (uint32_t)byte_capacity);
        if (read.status != HL_STATUS_OK) {
            result = bound_host_error(read.status);
            free(entries);
            break;
        }
        if (read.value > capacity) {
            result = -EIO;
            free(entries);
            break;
        }
        uint8_t *output = calloc(1, (size_t)byte_capacity);
        if (output == NULL) {
            free(entries);
            result = -ENOMEM;
            break;
        }
        size_t used = 0;
        result = 0;
        for (uint32_t index = 0; index < (uint32_t)read.value; ++index) {
            size_t record_size = (19u + entries[index].name_size + 1u + 7u) & ~(size_t)7u;
            if (entries[index].name_size > 255 || record_size > byte_capacity - used) {
                result = -EIO;
                break;
            }
            uint8_t *record = output + used;
            memset(record, 0, record_size);
            *(uint64_t *)(record + 0) = entries[index].object;
            *(uint64_t *)(record + 8) = entries[index].next_offset;
            *(uint16_t *)(record + 16) = (uint16_t)record_size;
            record[18] = (uint8_t)entries[index].type;
            memcpy(record + 19, entries[index].name, entries[index].name_size);
            used += record_size;
        }
        if (result == 0 && guest_copy_to(a1, output, used) != (ssize_t)used)
            result = -EFAULT;
        else if (result == 0)
            result = (int64_t)used;
        free(output);
        free(entries);
        break;
    }
    case 23: result = bound_dup_at_least(source.fd, 0, 0); break;
    case 24: {
        struct fdvis_reservation fdvis;
        uint32_t flags = (uint32_t)a2;
        int is_dup2 = G_IS_DUP2_COMPAT();
        int target = (int)a1;
        if (source.fd == (hl_linux_fd)target) {
            result = is_dup2 ? (int64_t)source.fd : -EINVAL;
        } else if ((!is_dup2 && (flags & ~HL_LINUX_O_CLOEXEC) != 0) || target < 0 || target >= guest_nofile_cur()) {
            result = target < 0 || target >= guest_nofile_cur() ? -EBADF : -EINVAL;
        } else {
            hl_linux_fd_snapshot target_snapshot;
            int target_bound = bound_snapshot((uint64_t)(uint32_t)target, &target_snapshot);
            int shadow;
            if (proc_fdvis_reserve_at(target, &fdvis) != 0) {
                result = -ENOSPC;
                break;
            }
            if (target_bound) {
                shadow = target;
            } else {
                engine_fd_vacate(target);
                fd_reset_emul(target);
                shadow = bound_shadow_dup2(target);
                if (shadow < 0) {
                    proc_fdvis_reservation_cancel(&fdvis);
                    result = -(int64_t)errno;
                    break;
                }
                (void)fcntl(target, F_SETFD, FD_CLOEXEC);
            }
            result = hl_linux_dup3(g_linux_box, source.fd, (hl_linux_fd)target,
                                   flags & HL_LINUX_O_CLOEXEC ? HL_LINUX_O_CLOEXEC : 0);
            if (result < 0) {
                proc_fdvis_reservation_cancel(&fdvis);
                if (!target_bound) close(shadow);
            } else {
                hl_linux_fd_snapshot duplicate;
                hl_host_file_metadata metadata = {0};
                uint32_t kind = HL_HOST_FD_OTHER;
                if (bound_snapshot((uint64_t)target, &duplicate) && g_host_services != NULL &&
                    g_host_services->file != NULL && g_host_services->file->metadata != NULL &&
                    g_host_services->file->metadata(g_host_services->context, duplicate.host_handle, &metadata)
                            .status == HL_STATUS_OK) {
                    if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                        metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                        kind = HL_HOST_FD_FILE;
                    else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                        kind = HL_HOST_FD_PIPE;
                    else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                        kind = HL_HOST_FD_SOCKET;
                }
                proc_fdvis_reservation_publish(&fdvis, target, kind, metadata.stable_device, metadata.stable_object);
                bound_path_duplicate(source.fd, result);
            }
        }
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route_duplication(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 25:
        if ((int32_t)a1 == HL_LINUX_F_DUPFD || (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC) {
            if (a2 > INT_MAX)
                result = -EINVAL;
            else
                result = bound_dup_at_least(source.fd, (int)a2,
                                            (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC ? HL_LINUX_FD_CLOEXEC : 0);
        } else if (a1 == 5 || a1 == 6 || a1 == 7) {
            uint8_t lock[32];
            hl_host_file_metadata metadata;
            hl_host_result status;
            int64_t current = 0;
            int lock_result = 0;
            if (guest_copy_from(lock, a2, sizeof(lock)) != (ssize_t)sizeof(lock)) {
                result = -EFAULT;
                break;
            }
            short whence;
            memcpy(&whence, lock + 2, sizeof(whence));
            if (whence != SEEK_SET && whence != SEEK_CUR && whence != SEEK_END) {
                result = -EINVAL;
                break;
            }
            if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL) {
                result = -ENOSYS;
                break;
            }
            status = g_host_services->file->metadata(g_host_services->context, source.host_handle, &metadata);
            if (status.status != HL_STATUS_OK) {
                result = bound_host_error(status.status);
                break;
            }
            if (metadata.type != HL_HOST_FILE_TYPE_REGULAR) {
                result = -EBADF;
                break;
            }
            if (whence == SEEK_CUR) {
                current = hl_linux_lseek(g_linux_box, source.fd, 0, SEEK_CUR);
                if (current < 0) {
                    result = current;
                    break;
                }
            }
            for (;;) {
                (void)poslk_op_identity(metadata.stable_device, metadata.stable_object, current, metadata.size, (int)a1,
                                        lock, &lock_result);
                if (a1 != 7 || lock_result != -EAGAIN) break;
                int interrupted = 0;
                for (int signal_number = 1; signal_number <= 64; ++signal_number)
                    if ((process_pending_test(signal_number) || thread_pending_test(c, signal_number)) &&
                        !(c->sigmask & (UINT64_C(1) << (signal_number - 1)))) {
                        interrupted = 1;
                        break;
                    }
                if (interrupted) {
                    lock_result = -EINTR;
                    break;
                }
                struct timespec delay = {0, 1000000};
                nanosleep(&delay, NULL);
            }
            /* poslk_apply is shared with the legacy Darwin syscall path and therefore reports native
             * errno numbers. This typed route bypasses svc_done(), so translate at this boundary. */
            result = lock_result < 0 ? -hl_linux_errno_from_macos(-lock_result) : lock_result;
            if (result == 0 && a1 == 5 && guest_copy_to(a2, lock, sizeof(lock)) != (ssize_t)sizeof(lock))
                result = -EFAULT;
        } else if ((int32_t)a1 == HL_LINUX_F_SETFL) {
            // O_DIRECT is a settable status flag whose bit is arch-specific (G_O_DIRECT: aarch64 0x10000 --
            // which aliases HL_LINUX_O_DIRECTORY -- vs x86-64 0x4000). Normalize the guest's arch bit to the
            // canonical arch-neutral HL_LINUX_O_DIRECT before the arch-neutral core stores it, so F_GETFL
            // reflects it (fcntl-cmds direct.consistent) instead of it being silently dropped.
            uint64_t normalized = a2 & ~(uint64_t)G_O_DIRECT;
            if (a2 & G_O_DIRECT) normalized |= HL_LINUX_O_DIRECT;
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, normalized);
        } else if ((int32_t)a1 == HL_LINUX_F_GETFL) {
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, a2);
            if (result >= 0 && (result & HL_LINUX_O_DIRECT)) // map the canonical bit back to the guest arch bit
                result = (result & ~(int64_t)HL_LINUX_O_DIRECT) | (int64_t)(uint64_t)G_O_DIRECT;
        } else if ((int32_t)a1 == HL_LINUX_F_GETFD || (int32_t)a1 == HL_LINUX_F_SETFD) {
            // Descriptor flags (FD_CLOEXEC) live in the virtual descriptor table, not the backing host fd.
            result = hl_linux_fcntl(g_linux_box, source.fd, (int32_t)a1, a2);
        } else {
            // Every remaining fcntl command -- F_SETOWN/F_GETOWN, F_SETSIG/F_GETSIG, the F_OFD_* open-file
            // description locks, F_SETLEASE/F_GETLEASE, F_NOTIFY, F_SET/GETPIPE_SZ, F_ADD/GET_SEALS, the
            // RW_HINT family -- operates on the real open file description, not the virtual fd table (which
            // only knows FD/FL/DUPFD/POSIX-lock). The arch-neutral core answered EINVAL for all of them, so
            // a bound (typed) descriptor lost OFD locks, memfd seals, owner/signal ownership, etc. The host
            // is Linux and the bound descriptor has a same-number native shadow, so forward the command
            // verbatim to that real host fd; pointer args (OFD flock, rw_hint u64) are already host-mapped.
            int native_fd = -1;
            int borrowed = bound_attachment_borrow((int)source.fd, &native_fd);
            if (borrowed < 0) {
                result = borrowed;
                break;
            }
            long r = fcntl(native_fd, (int)a1, (unsigned long)a2);
            result = r < 0 ? -(int64_t)errno : (int64_t)r;
        }
        break;
    case 285: {
        hl_linux_fd_snapshot output;
        off_t input_value = 0, output_value = 0;
        off_t *input_offset = a1 != 0 ? &input_value : NULL;
        off_t *output_offset = a3 != 0 ? &output_value : NULL;
        size_t done = 0;
        char buffer[8192];
        result = 0;
        // copy_file_range defines NO flags: Linux rejects a non-zero `flags` with -EINVAL before copying
        // anything. Mirrors the native path in io.c case 285.
        if (G_A5(c)) {
            result = -EINVAL;
            break;
        }
        if (!bound_snapshot(a2, &output)) {
            result = -ENOSYS;
            break;
        }
        if ((input_offset &&
             guest_copy_from(input_offset, a1, sizeof(*input_offset)) != (ssize_t)sizeof(*input_offset)) ||
            (output_offset &&
             guest_copy_from(output_offset, a3, sizeof(*output_offset)) != (ssize_t)sizeof(*output_offset))) {
            result = -EFAULT;
            break;
        }
        // Linux rejects a same-file copy whose ranges overlap (EINVAL) instead of copying through the
        // overlap.  Mirrors the native path in io.c case 285, using the typed identity for sameness.
        if (G_A4(c) > 0 && g_host_services != NULL && g_host_services->file != NULL &&
            g_host_services->file->metadata != NULL) {
            hl_host_file_metadata in_meta, out_meta;
            hl_host_result in_status =
                g_host_services->file->metadata(g_host_services->context, source.host_handle, &in_meta);
            hl_host_result out_status =
                g_host_services->file->metadata(g_host_services->context, output.host_handle, &out_meta);
            if (in_status.status == HL_STATUS_OK && out_status.status == HL_STATUS_OK &&
                in_meta.stable_device == out_meta.stable_device && in_meta.stable_object == out_meta.stable_object) {
                off_t in_start = input_offset ? *input_offset : (off_t)source.offset;
                off_t out_start = output_offset ? *output_offset : (off_t)output.offset;
                off_t length = (off_t)G_A4(c);
                if (in_start >= 0 && out_start >= 0 && in_start < out_start + length && out_start < in_start + length) {
                    result = -EINVAL;
                    break;
                }
            }
        }
        while (done < (size_t)G_A4(c)) {
            size_t chunk = (size_t)G_A4(c) - done;
            if (chunk > sizeof(buffer)) chunk = sizeof(buffer);
            int64_t nr_read = input_offset
                                  ? hl_linux_pread64(g_linux_box, source.fd, buffer, chunk, (uint64_t)*input_offset)
                                  : hl_linux_read(g_linux_box, source.fd, buffer, chunk);
            if (nr_read <= 0) {
                if (!done) result = nr_read;
                break;
            }
            int64_t nr_written = output_offset ? hl_linux_pwrite64(g_linux_box, output.fd, buffer, (size_t)nr_read,
                                                                   (uint64_t)*output_offset)
                                               : hl_linux_write(g_linux_box, output.fd, buffer, (size_t)nr_read);
            if (nr_written < 0) {
                if (!done) result = nr_written;
                break;
            }
            done += (size_t)nr_written;
            if (input_offset) *input_offset += (off_t)nr_written;
            if (output_offset) *output_offset += (off_t)nr_written;
            result = (int64_t)done;
            if (nr_written < nr_read) break;
        }
        if (result >= 0 && ((input_offset && guest_copy_to(a1, input_offset, sizeof(*input_offset)) !=
                                                 (ssize_t)sizeof(*input_offset)) ||
                            (output_offset && guest_copy_to(a3, output_offset, sizeof(*output_offset)) !=
                                                  (ssize_t)sizeof(*output_offset))))
            result = done != 0 ? (int64_t)done : -EFAULT;
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route_unsupported(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 20: return 0; /* epoll_create1: a0 is flags, not an fd */
    case 21:           /* epoll_ctl */
    case 22:           /* epoll_pwait */
    case 71:           /* sendfile */
    case 75:           /* vmsplice */
    case 76:           /* splice */
    case 77:           /* tee */
    case 200:          /* bind */
    case 201:          /* listen */
    case 202:          /* accept */
    case 203:          /* connect */
    case 204:          /* getsockname */
    case 205:          /* getpeername */
    case 206:          /* sendto */
    case 207:          /* recvfrom */
    case 208:          /* setsockopt */
    case 209:          /* getsockopt */
    case 210:          /* shutdown */
    case 211:          /* sendmsg */
    case 212:          /* recvmsg */
        /* A bound slot is never a native descriptor. Unsupported fd operations cannot touch its shadow. */
        result = -ENOSYS;
        break;
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4) {
    hl_linux_fd_snapshot source = {0};
    int source_bound = !g_bound_source_native && bound_snapshot(a0, &source);
    if (bound_route_platform(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (bound_route_inotify(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (bound_route_memory_poll(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (bound_route_transfer(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (bound_route_paths(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (bound_route_descriptor_special(c, nr, a0, a1, a2, a3, a4, source, source_bound)) return 1;
    if (!source_bound) return 0;
    if (bound_route_attributes(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_paths_bound(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_io(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_sync(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_metadata(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_control(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_directory(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    if (bound_route_duplication(c, nr, a0, a1, a2, a3, a4, source)) return 1;
    return bound_route_unsupported(c, nr, a0, a1, a2, a3, a4, source);
}
