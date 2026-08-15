static int bound_route_platform(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source, int source_bound) {
    int64_t result;
#if defined(_WIN32)
    if (!g_bound_source_native && bound_socket_route(c, nr, a0, a1, a2, a3)) return 1;
    /* eventfd2 over the typed counter provider.
     *
     * Windows only. The emulated eventfd is a host pipe pair plus a counter in a
     * shared arena, and on a host with no pipe there is nothing under it; the
     * typed provider is a real counter object with the kernel's own semantics.
     * Every other host keeps the emulation, which is mature and cross-process,
     * so this arm is deliberately not taken there.
     *
     * The shadow-descriptor reservation is the same one inotify_init1 does below
     * and for the same reason: the object lives only in the typed box table, so
     * without a real kernel descriptor holding the identical number a later
     * non-bound open is handed that number and silently aliases the eventfd.
     *
     * Flag validation matches fs/eventfd.c -- only EFD_SEMAPHORE, EFD_CLOEXEC
     * and EFD_NONBLOCK, everything else EINVAL. glibc's EventFD probe calls
     * eventfd2(0, ~0) and REQUIRES it to fail, so a permissive mask here is a
     * feature-detection lie rather than a harmless leniency. */
    if (nr == 19 && g_linux_box != NULL) {
        const uint64_t semaphore = UINT64_C(0x1), nonblock = UINT64_C(0x800), cloexec = UINT64_C(0x80000);
        struct fdvis_reservation fdvis;
        hl_linux_fd_reservation reservation;
        hl_status status;
        int shadow;
        if ((a1 & ~(semaphore | nonblock | cloexec)) != 0) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            return 1;
        }
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            G_RET(c) = (uint64_t)(int64_t)-(int64_t)errno;
            return 1;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
            return 1;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        /* The token only proves the slot is free; the installer publishes it. */
        (void)hl_linux_fd_cancel(g_linux_box, &reservation);
        result = hl_linux_eventfd_create_at(g_linux_box, (hl_linux_fd)shadow, a0,
                                            (uint32_t)(((a1 & semaphore) != 0 ? HL_LINUX_EVENTFD_SEMAPHORE : 0u) |
                                                       ((a1 & nonblock) != 0 ? HL_LINUX_EVENTFD_NONBLOCK : 0u)),
                                            (a1 & cloexec) != 0 ? HL_LINUX_FD_CLOEXEC : 0);
        if (result < 0) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            proc_fdvis_reservation_publish(&fdvis, (int)result, HL_HOST_FD_OTHER, 0, 0);
        }
        G_RET(c) = (uint64_t)result;
        return 1;
    }
#endif
    (void)a4;
    return 0;
}
static int bound_route_inotify(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source, int source_bound) {
    int64_t result;
    if (nr == 26 && g_linux_box != NULL) {
        bound_inotify_provider *provider;
        struct fdvis_reservation fdvis;
        hl_linux_fd_reservation reservation;
        hl_status status;
        int shadow;
        if ((a0 & ~(UINT64_C(0x800) | UINT64_C(0x80000))) != 0) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            return 1;
        }
        /* Hold the guest fd number in the host kernel fd space as well.  The
           inotify object lives only in the typed box table, so without a real
           descriptor reserving the same slot a later non-bound (absolute-path)
           open is handed the identical number by the kernel and silently
           clobbers the watch -- read/poll/select then fail with EBADF.  Mirror
           the bound-openat reservation so the box and host fd allocators agree. */
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            G_RET(c) = (uint64_t)(int64_t)-(int64_t)errno;
            return 1;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
            return 1;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-EMFILE;
            return 1;
        }
        /* The token only proves the slot is free; the object installer publishes
           the slot itself, so drop the token and install at the same number. */
        (void)hl_linux_fd_cancel(g_linux_box, &reservation);
        provider = bound_inotify_provider_create(g_host_services);
        if (provider == NULL) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
            return 1;
        }
        result = hl_linux_inotify_create_at(g_linux_box, (hl_linux_fd)shadow, &bound_inotify_ops, provider,
                                            (a0 & UINT64_C(0x80000)) != 0 ? HL_LINUX_FD_CLOEXEC : 0,
                                            (a0 & UINT64_C(0x800)) != 0 ? HL_LINUX_O_NONBLOCK : 0);
        if (result < 0) {
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            proc_fdvis_reservation_publish(&fdvis, (int)result, HL_HOST_FD_OTHER, 0, 0);
        }
        G_RET(c) = (uint64_t)result;
        return 1;
    }
    if (nr == 27 && source_bound) {
        char path[4200];
        char guest_path[HL_LINUX_PATH_MAX + 1];
        size_t guest_path_size;
        // EFAULT on an inaccessible path pointer BEFORE atpath dereferences it. inotify_add_watch(fd, NULL,
        // mask) and a wild/unmapped path return -EFAULT on Linux; without this guard atpath reads the
        // unmapped guest address and the engine child SIGSEGVs, killing the guest with signal 11 instead
        // (guest-triggerable crash). Mirrors the guarded sibling path syscalls (nr 78 below, fs.c openat).
        if (bound_path_copy(a1, guest_path, &guest_path_size) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            return 1;
        }
        const char *resolved = atpath(-100, guest_path, path, sizeof(path), 0);
        if (resolved == NULL)
            result = -errno;
        else
            result = hl_linux_inotify_add(g_linux_box, source.fd, resolved, strlen(resolved), (uint32_t)a2);
        G_RET(c) = (uint64_t)result;
        return 1;
    }
    if (nr == 28 && source_bound) {
        G_RET(c) = (uint64_t)hl_linux_inotify_remove(g_linux_box, source.fd, (int32_t)a1);
        return 1;
    }
    if (nr == 78 && a1 != 0 && a2 != 0 && (int64_t)a3 > 0) {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if (bound_path_copy(a1, path, &path_size) != 0) return 0;
        int guest_fd = procfd_num(path);
        hl_linux_fd_snapshot target;
        if (guest_fd >= 0 && bound_snapshot((uint64_t)(uint32_t)guest_fd, &target)) {
            char target_path[4200];
            int length = proc_fd_link_pid((int)getpid(), guest_fd, target_path, sizeof target_path);
            if (length < 0) return 0;
            size_t copied = (size_t)length > (size_t)a3 ? (size_t)a3 : (size_t)length;
            if (guest_copy_to(a2, target_path, copied) != (ssize_t)copied) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return 1;
            }
            G_RET(c) = (uint64_t)copied;
            return 1;
        }
    }
    (void)a4;
    return 0;
}

static int bound_route_memory_poll(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source, int source_bound) {
    int64_t result;
    if (nr == 73 && bound_poll_references(a0, a1)) {
        G_RET(c) = (uint64_t)bound_ppoll(c, a0, a1, a2, a3);
        return 1;
    }
    if (nr == 72 && bound_fdsets_reference(a0, a1, a2, a3)) {
        G_RET(c) = (uint64_t)bound_pselect(c, a0, a1, a2, a3);
        return 1;
    }
    if (nr == 222 && (a3 & 0x20u) == 0) {
        hl_linux_fd_snapshot mapped;
        if (bound_snapshot(G_A4(c), &mapped)) {
            G_RET(c) = (uint64_t)bound_mmap_file(&mapped, a0, a1, (uint32_t)a2, (uint32_t)a3, G_A5(c));
            return 1;
        }
    }
    if (nr == 215 || nr == 226 || nr == 227) {
        pthread_mutex_lock(&g_bound_mapping_gate);
        pthread_mutex_lock(&g_bound_mapping_lock);
        bound_mapping *mapping = bound_mapping_find(a0, a1);
        if (mapping != NULL) {
            if (mapping->object->handle == HL_HOST_HANDLE_INVALID) {
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 0;
            }
            uint64_t offset = a0 - mapping->address;
            hl_host_result operation;
            /* Guest mprotect is modeled by the 4 KiB Linux VMA/SMC registries in svc_mem. Routing a
             * typed file mapping to host protect applies macOS's 16 KiB granularity and can protect
             * adjacent ELF segments, breaking ld.so RELRO. Keep the typed mapping ledger, but let the
             * common guest-logical path validate the range and update permissions. */
            if (nr == 226) {
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 0;
            }
            if (nr == 227 && (((a2 & ~(uint64_t)7u) != 0) || (a2 & 5u) == 0 || (a2 & 5u) == 5u)) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                pthread_mutex_unlock(&g_bound_mapping_lock);
                pthread_mutex_unlock(&g_bound_mapping_gate);
                return 1;
            }
            uint64_t operation_size = a1;
            if (nr == 215 && offset == 0 && a1 == hl_gmap_find_guest_length(a0)) operation_size = mapping->size;
            if (nr == 215)
                operation = g_host_services->memory->unmap_range(g_host_services->context, mapping->object->handle,
                                                                 mapping->object_offset + offset, operation_size);
            else
                operation = g_host_services->memory->sync(g_host_services->context, mapping->object->handle,
                                                          mapping->object_offset + offset, a1);
            if (operation.status == HL_STATUS_OK && nr == 215) {
                hl_host_handle retired = mapping->object->handle;
                mapping->object->handle = HL_HOST_HANDLE_INVALID;
                (void)g_host_services->memory->discard(g_host_services->context, retired);
                bound_mapping_retire(a0, operation_size);
                hl_gmap_unmap_range(a0, a0 + operation_size);
                gbus_clear(a0, a0 + operation_size);
            }
            G_RET(c) = (uint64_t)bound_host_error(operation.status);
            pthread_mutex_unlock(&g_bound_mapping_lock);
            pthread_mutex_unlock(&g_bound_mapping_gate);
            return 1;
        }
        pthread_mutex_unlock(&g_bound_mapping_lock);
        pthread_mutex_unlock(&g_bound_mapping_gate);
    }
    (void)a4;
    return 0;
}

static int bound_route_transfer(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source, int source_bound) {
    int64_t result;
    if (nr == 71) {
        hl_linux_fd_snapshot second;
        int second_bound = !g_bound_second_native && bound_snapshot(a1, &second);
        if (source_bound || second_bound) {
            G_RET(c) = (uint64_t)bound_sendfile(source_bound ? &source : NULL, (int)a0, second_bound ? &second : NULL,
                                                (int)a1, a2, a3);
            return 1;
        }
    }
    if (nr == 76) {
        hl_linux_fd_snapshot second;
        int second_bound = bound_snapshot(a2, &second);
        if (source_bound || second_bound) {
            G_RET(c) = (uint64_t)bound_splice(source_bound ? &source : NULL, (int)a0, a1, second_bound ? &second : NULL,
                                              (int)a2, a3, G_A4(c), G_A5(c));
            return 1;
        }
    }
    if ((nr == 75 || nr == 77) && source_bound) {
        /* vmsplice and tee require pipe endpoints. Typed descriptors currently name ordinary files. */
        G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        return 1;
    }
    if (nr == 77) {
        hl_linux_fd_snapshot second;
        if (bound_snapshot(a1, &second)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            return 1;
        }
    }
    (void)a4;
    return 0;
}

static int bound_route_paths(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source, int source_bound) {
    int64_t result;
    if (nr == 36) {
        hl_linux_fd_snapshot directory;
        if (bound_snapshot(a1, &directory)) {
            char target[HL_LINUX_PATH_MAX + 1], path[HL_LINUX_PATH_MAX + 1];
            size_t target_size, path_size;
            result = bound_path_copy(a0, target, &target_size);
            if (result == 0) result = bound_path_copy(a2, path, &path_size);
            if (result == 0 && path[0] == '/') return 0;
            if (result == 0 && (!bound_file_abi14() || g_host_services->file->make_symlink == NULL)) result = -ENOSYS;
            if (result == 0)
                result = bound_host_error(g_host_services->file
                                              ->make_symlink(g_host_services->context, target, target_size,
                                                             directory.host_handle, path, path_size)
                                              .status);
            if (result == 0) bound_evict_relative(directory.host_handle, path);
            G_RET(c) = (uint64_t)result;
            return 1;
        }
    }
    if (nr == 37 || nr == 38 || nr == 276) {
        hl_linux_fd_snapshot destination;
        int destination_bound = bound_snapshot(a2, &destination);
        if (source_bound || destination_bound) {
            char old_path[HL_LINUX_PATH_MAX + 1], new_path[HL_LINUX_PATH_MAX + 1];
            size_t old_size, new_size;
            result = bound_path_copy(a1, old_path, &old_size);
            if (result == 0) result = bound_path_copy(a3, new_path, &new_size);
            if (result == 0 && source_bound && old_path[0] != '/') {
                int error = bound_handle_dirfd_error((int)a0);
                if (error != -EACCES) result = error;
            }
            if (result == 0 && destination_bound && new_path[0] != '/') {
                int error = bound_handle_dirfd_error((int)a2);
                if (error != -EACCES) result = error;
            }
            if (result != 0) {
                G_RET(c) = (uint64_t)result;
                return 1;
            }
            /* Absolute operands ignore their corresponding dirfd.  Let the
               ordinary jailed path resolver handle mixed bound/native fds in
               that case instead of rejecting a descriptor Linux never reads. */
            if (old_path[0] == '/' || new_path[0] == '/') return 0;
            if (!source_bound || !destination_bound) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
                return 1;
            }
            if (nr == 37) {
                uint64_t flags = G_A4(c);
                if ((flags & ~UINT64_C(0x400)) != 0)
                    result = -EINVAL;
                else if (!bound_file_abi14() || g_host_services->file->make_link == NULL)
                    result = -ENOSYS;
                else if (result == 0)
                    result = bound_host_error(g_host_services->file
                                                  ->make_link(g_host_services->context, source.host_handle, old_path,
                                                              old_size, destination.host_handle, new_path, new_size,
                                                              (flags & UINT64_C(0x400)) != 0 ? 1u : 0u)
                                                  .status);
            } else {
                uint64_t flags = nr == 276 ? G_A4(c) : 0;
                /* RENAME_NOREPLACE (0x1) and RENAME_EXCHANGE (0x2) are honored;
                   they are mutually exclusive and no other flag is supported. */
                if ((flags & ~UINT64_C(0x3)) != 0 || (flags & UINT64_C(0x3)) == UINT64_C(0x3))
                    result = -EINVAL;
                else if (g_host_services->file->rename_relative == NULL)
                    result = -ENOSYS;
                else if (result != 0) {
                    /* preserve earlier path-copy error */
                } else if ((flags & UINT64_C(0x2)) != 0) {
                    result = (int)bound_rename_exchange(source.host_handle, old_path, old_size, destination.host_handle,
                                                        new_path, new_size);
                } else if ((flags & UINT64_C(0x1)) != 0) {
                    hl_host_result probe = g_host_services->file->open_relative(
                        g_host_services->context, destination.host_handle, new_path, new_size,
                        HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
                    if (probe.status == HL_STATUS_OK) {
                        (void)g_host_services->file->close(g_host_services->context, probe.value);
                        result = -EEXIST;
                    } else if (probe.status != HL_STATUS_NOT_FOUND) {
                        result = (int)bound_host_error(probe.status);
                    } else {
                        result = (int)bound_host_error(
                            g_host_services->file
                                ->rename_relative(g_host_services->context, source.host_handle, old_path, old_size,
                                                  destination.host_handle, new_path, new_size)
                                .status);
                    }
                } else {
                    result =
                        bound_host_error(g_host_services->file
                                             ->rename_relative(g_host_services->context, source.host_handle, old_path,
                                                               old_size, destination.host_handle, new_path, new_size)
                                             .status);
                }
            }
            if (result == 0) {
                bound_evict_relative(source.host_handle, old_path);
                bound_evict_relative(destination.host_handle, new_path);
            }
            G_RET(c) = (uint64_t)result;
            return 1;
        }
    }
    (void)a4;
    return 0;
}

static int bound_route_native_dup(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    hl_linux_fd_snapshot target;
    if (!bound_snapshot(a1, &target)) return 0;
    unsigned flags = (unsigned)a2;
    int is_dup2 = G_IS_DUP2_COMPAT();
    int source_fd = (int)a0;
    int target_fd = (int)a1;
    if (source_fd == target_fd) {
        G_RET(c) = is_dup2 ? (uint64_t)(unsigned)target_fd : (uint64_t)(int64_t)-EINVAL;
        return 1;
    }
    if ((!is_dup2 && (flags & ~HL_LINUX_O_CLOEXEC) != 0) || target_fd < 0 || target_fd >= guest_nofile_cur()) {
        G_RET(c) = (uint64_t)(int64_t)(target_fd < 0 || target_fd >= guest_nofile_cur() ? -EBADF : -EINVAL);
        return 1;
    }
    if (source_fd < 0 || fcntl(source_fd, F_GETFD) < 0) {
        G_RET(c) = (uint64_t)(int64_t)-EBADF;
        return 1;
    }
    flock_broker_detach(&target);
    (void)hl_linux_close(g_linux_box, target.fd);
    proc_fdvis_close(target_fd);
    (void)close(target_fd);
    return 0;
}

static int bound_route_epoll_watched(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                     hl_linux_fd_snapshot watched) {
    int64_t epoll_result = -ENOSYS;
    if ((int)a0 >= 0 && (int)a0 < HL_NFD && (int)a2 >= 0 && (int)a2 < HL_NFD &&
        hl_provider_files_is_handle(watched.host_handle)) {
        int registry_ep = epoll_slot((int)a0);
        uint32_t epoll_generation = g_ep_provider_generations[registry_ep];
        ep_provider_watch *watch = ep_provider_find(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT, registry_ep,
                                                    epoll_generation, (int)a2, watched.descriptor_generation);
        if (a1 == HL_LINUX_EPOLL_DELETE) {
            if (watch == NULL)
                epoll_result = -ENOENT;
            else {
                ep_provider_retire(watch);
                epoll_result = 0;
            }
        } else if ((a1 == HL_LINUX_EPOLL_ADD || a1 == HL_LINUX_EPOLL_MODIFY) && a3 != 0) {
            uint8_t encoded[G_EPEV_DOFF + sizeof(uint64_t)];
            uint32_t events = 0;
            uint64_t data = 0;
            if (guest_copy_from(encoded, a3, sizeof(encoded)) != (ssize_t)sizeof(encoded)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return 1;
            }
            memcpy(&events, encoded, sizeof(events));
            memcpy(&data, encoded + G_EPEV_DOFF, sizeof(data));
            if (a1 == HL_LINUX_EPOLL_ADD && watch != NULL)
                epoll_result = -EEXIST;
            else if (a1 == HL_LINUX_EPOLL_MODIFY && watch == NULL)
                epoll_result = -ENOENT;
            else {
                ep_provider_watch *previous = watch;
                ep_provider_watch *replacement =
                    ep_provider_alloc(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT);
                if (replacement == NULL) {
                    G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
                    return 1;
                }
                uint32_t serial = g_ep_provider_serial = ep_provider_next(g_ep_provider_serial);
                uint32_t interests =
                    ((events & 1u) ? HL_LINUX_READY_READ : 0u) | ((events & 4u) ? HL_LINUX_READY_WRITE : 0u);
                ep_provider_activate(replacement, registry_ep, epoll_generation, (int)a2,
                                     watched.descriptor_generation, serial, watched.host_handle, events,
                                     interests, data);
                ep_wake_arm((int)a0);
                epoll_result = hl_provider_files_subscribe(replacement->handle, replacement->interests,
                                                           bound_epoll_provider_ready, replacement,
                                                           atomic_load(&replacement->serial)) == 0
                                   ? 0
                                   : -EIO;
                if (epoll_result != 0)
                    ep_provider_retire(replacement);
                else if (previous != NULL)
                    ep_provider_retire(previous);
            }
        } else
            epoll_result = -EINVAL;
        G_RET(c) = (uint64_t)epoll_result;
        return 1;
    }
    /* Typed box objects (an inotify watch is the canonical case) own host
       observation and expose readiness only through their object adapter,
       never as a host descriptor -- watched.host_handle is INVALID.  Route
       them through the object-sampling epoll registry, the same way
       poll()/select() observe these objects (hl_linux_object_poll). */
    if ((int)a0 >= 0 && (int)a0 < HL_NFD && (int)a2 >= 0 && (int)a2 < HL_NFD) {
        hl_linux_object_pin pin;
        int object_epollable = 0;
        if (hl_linux_object_pin_fd(g_linux_box, (hl_linux_fd)a2, &pin) == HL_STATUS_OK) {
            object_epollable = pin.ops != NULL && pin.ops->readiness != NULL;
            hl_linux_object_unpin(&pin);
        }
        if (object_epollable) {
            int registry_ep = epoll_slot((int)a0);
            uint32_t epoll_generation = g_ep_provider_generations[registry_ep];
            ep_object_watch *watch =
                ep_object_find(registry_ep, epoll_generation, (int)a2, watched.descriptor_generation);
            if (a1 == HL_LINUX_EPOLL_DELETE) {
                if (watch == NULL)
                    epoll_result = -ENOENT;
                else {
                    ep_object_free(watch);
                    epoll_result = 0;
                }
            } else if ((a1 == HL_LINUX_EPOLL_ADD || a1 == HL_LINUX_EPOLL_MODIFY) && a3 != 0) {
                uint8_t encoded[G_EPEV_DOFF + sizeof(uint64_t)];
                uint32_t events = 0;
                uint64_t data = 0;
                if (guest_copy_from(encoded, a3, sizeof(encoded)) != (ssize_t)sizeof(encoded)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    return 1;
                }
                memcpy(&events, encoded, sizeof(events));
                memcpy(&data, encoded + G_EPEV_DOFF, sizeof(data));
                if (a1 == HL_LINUX_EPOLL_ADD && watch != NULL)
                    epoll_result = -EEXIST;
                else if (a1 == HL_LINUX_EPOLL_MODIFY && watch == NULL)
                    epoll_result = -ENOENT;
                else {
                    uint32_t interests = ((events & 0x1u) ? HL_LINUX_READY_READ : 0u) |
                                         ((events & 0x4u) ? HL_LINUX_READY_WRITE : 0u);
                    if (watch == NULL) {
                        watch = ep_object_alloc();
                        if (watch == NULL) {
                            G_RET(c) = (uint64_t)(int64_t)-ENOSPC;
                            return 1;
                        }
                        watch->epoll = registry_ep;
                        watch->epoll_generation = epoll_generation;
                        watch->descriptor = (int)a2;
                        watch->descriptor_generation = watched.descriptor_generation;
                        g_ep_object_count[registry_ep]++;
                    }
                    watch->events = events;
                    watch->interests = interests;
                    watch->data = data;
                    ep_wake_arm((int)a0);
                    epoll_result = 0;
                }
            } else
                epoll_result = -EINVAL;
            G_RET(c) = (uint64_t)epoll_result;
            return 1;
        }
    }
    if (a1 == HL_LINUX_EPOLL_ADD && g_host_services != NULL && g_host_services->file != NULL &&
        g_host_services->file->metadata != NULL) {
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, watched.host_handle, &metadata);
        if (status.status == HL_STATUS_OK &&
            (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY))
            epoll_result = -EPERM;
    }
    G_RET(c) = (uint64_t)epoll_result;
    return 1;
}

static int bound_route_descriptor_special(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2,
                                          uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source,
                                          int source_bound) {
    hl_linux_fd_snapshot watched;
    (void)a4;
    (void)source;
    if (nr == 24 && !source_bound) return bound_route_native_dup(c, a0, a1, a2);
    if (nr != 21 || source_bound || !bound_snapshot(a2, &watched)) return 0;
    return bound_route_epoll_watched(c, a0, a1, a2, a3, watched);
}

static int bound_route_attributes(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 7:    /* fsetxattr */
    case 10:   /* fgetxattr */
    case 13:   /* flistxattr */
    case 16: { /* fremovexattr */
        char path[HL_LINUX_PATH_MAX + 1];
        hl_host_result named;
        if (g_host_services->file->path == NULL) {
            result = -ENOSYS;
            break;
        }
        named = g_host_services->file->path(g_host_services->context, source.host_handle,
                                            (hl_host_bytes){path, HL_LINUX_PATH_MAX});
        if (named.status != HL_STATUS_OK || named.value > HL_LINUX_PATH_MAX) {
            result = bound_host_error(named.status);
            break;
        }
        path[named.value] = 0;
        if (nr == 7)
            result = guest_xattr_set(path, (const char *)a1, (const void *)a2, (size_t)a3, a4, 0);
        else if (nr == 10)
            result = guest_xattr_get(path, (const char *)a1, (void *)a2, (size_t)a3, 0);
        else if (nr == 13)
            result = guest_xattr_list(path, (char *)a1, (size_t)a2, 0);
        else
            result = guest_xattr_remove(path, (const char *)a1, 0);
        if (result < 0) result = -hl_linux_errno_from_host((int)-result);
        break;
    }
    case 33: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if (((mode_t)a2 & S_IFMT) != S_IFIFO || a3 != 0) return 0;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (g_host_services->file->abi != HL_HOST_FILE_ABI ||
            g_host_services->file->size < sizeof(*g_host_services->file) || g_host_services->file->make_fifo == NULL) {
            result = -ENOSYS;
            break;
        }
        result = bound_host_error(
            g_host_services->file
                ->make_fifo(g_host_services->context, source.host_handle, path, path_size, (uint32_t)a2 & 07777u)
                .status);
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 34: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (!bound_file_abi14() || g_host_services->file->make_directory == NULL) {
            result = -ENOSYS;
            break;
        }
        result = bound_host_error(
            g_host_services->file
                ->make_directory(g_host_services->context, source.host_handle, path, path_size, (uint32_t)a2 & 07777u)
                .status);
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 35: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if ((a2 & ~UINT64_C(0x200)) != 0) {
            result = -EINVAL;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if ((a2 & UINT64_C(0x200)) != 0 && g_host_services->file->remove_directory == NULL) {
            result = -ENOSYS;
            break;
        }
        if ((a2 & UINT64_C(0x200)) != 0) {
            result = bound_host_error(
                g_host_services->file->remove_directory(g_host_services->context, source.host_handle, path, path_size)
                    .status);
        } else if (g_host_services->file->unlink_relative == NULL) {
            result = -ENOSYS;
        } else {
            result = bound_host_error(
                g_host_services->file->unlink_relative(g_host_services->context, source.host_handle, path, path_size)
                    .status);
        }
        if (result == 0) bound_evict_relative(source.host_handle, path);
        break;
    }
    case 53:
    case 452: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        uint64_t flags = nr == 452 ? a3 : 0;
        if ((flags & ~UINT64_C(0x1100)) != 0) {
            result = -EINVAL;
            break;
        }
        char first_path_byte;
        if (nr == 452 && (flags & UINT64_C(0x1000)) != 0 && a1 != 0 && guest_copy_from(&first_path_byte, a1, 1) == 1 &&
            first_path_byte == '\0') {
            if (g_host_services->file->set_permissions == NULL) {
                result = -ENOSYS;
                break;
            }
            result = bound_host_error(
                g_host_services->file
                    ->set_permissions(g_host_services->context, source.host_handle, (uint32_t)a2 & 07777u)
                    .status);
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if (g_host_services->file->open_relative == NULL || g_host_services->file->set_permissions == NULL) {
            result = -ENOSYS;
            break;
        }
        uint32_t access = HL_HOST_FILE_PATH_ONLY;
        if ((flags & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
        hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle, path,
                                                                     path_size, access, 0, 0);
        if (opened.status != HL_STATUS_OK) {
            result = bound_host_error(opened.status);
            break;
        }
        result = bound_host_error(
            g_host_services->file->set_permissions(g_host_services->context, opened.value, (uint32_t)a2 & 07777u)
                .status);
        (void)g_host_services->file->close(g_host_services->context, opened.value);
        break;
    }
    case 48:
    case 439: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        uint64_t flags = nr == 439 ? a3 : 0;
        if (a2 > 7 || (flags & ~UINT64_C(0x1300)) != 0) {
            result = -EINVAL;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        uint32_t access = a2 == 0 ? HL_HOST_FILE_PATH_ONLY : 0;
        if ((a2 & 4u) != 0) access |= HL_HOST_FILE_READ;
        if ((a2 & 2u) != 0) access |= HL_HOST_FILE_WRITE;
        if ((a2 & 1u) != 0) access |= HL_HOST_FILE_PATH_ONLY;
        /* AT_SYMLINK_NOFOLLOW checks the link itself instead of its target. */
        if ((flags & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
        hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle, path,
                                                                     path_size, access, 0, 0);
        result = bound_host_error(opened.status);
        if (opened.status == HL_STATUS_OK) {
            if ((a2 & 1u) != 0) {
                hl_host_file_metadata metadata;
                hl_host_result measured =
                    g_host_services->file->metadata(g_host_services->context, opened.value, &metadata);
                if (measured.status != HL_STATUS_OK)
                    result = bound_host_error(measured.status);
                else if (metadata.type != HL_HOST_FILE_TYPE_DIRECTORY && (metadata.permissions & 0111u) == 0)
                    result = -EACCES;
            }
            (void)g_host_services->file->close(g_host_services->context, opened.value);
        }
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}

static int bound_route_paths_bound(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, hl_linux_fd_snapshot source) {
    int64_t result;
    switch (nr) {
    case 79: {
        char path[HL_LINUX_PATH_MAX + 1];
        size_t path_size;
        if ((a3 & ~UINT64_C(0x1900)) != 0) {
            result = -EINVAL;
            break;
        }
        if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES) {
            result = -EFAULT;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        int empty = result == -HL_LINUX_ENOENT && (a3 & UINT64_C(0x1000)) != 0;
        if (result != 0 && !empty) break;
        if (!empty && path[0] == '/') return 0;
        if (!empty) {
            char backing[HL_LINUX_PATH_MAX + 1];
            if (bound_handle_host_path(source.host_handle, backing, sizeof backing) == 0 &&
                strstr(backing, "/.hl-proc-fd") != NULL) {
                char synthetic[HL_LINUX_PATH_MAX + 1];
                struct stat status;
                int written = snprintf(synthetic, sizeof synthetic, "/proc/self/fd/%s", path);
                int measured = written > 0 && (size_t)written < sizeof synthetic
                                   ? ((a3 & UINT64_C(0x100)) != 0 ? synth_stat_raw(synthetic, &status)
                                                                  : procfd_follow_stat(synthetic, &status))
                                   : 0;
                if (measured <= 0) {
                    result = -ENOENT;
                } else {
                    result = guest_fill_linux_stat(a2, &status, NULL, -1);
                }
                break;
            }
        }
        hl_host_handle target = source.host_handle;
        int close_target = 0;
        if (!empty) {
            uint32_t access = HL_HOST_FILE_PATH_ONLY;
            if ((a3 & UINT64_C(0x100)) != 0) access |= HL_HOST_FILE_NOFOLLOW;
            hl_host_result opened = g_host_services->file->open_relative(g_host_services->context, source.host_handle,
                                                                         path, path_size, access, 0, 0);
            if (opened.status != HL_STATUS_OK) {
                result = bound_host_error(opened.status);
                break;
            }
            target = opened.value;
            close_target = 1;
        }
        hl_host_file_metadata metadata;
        hl_host_result measured = g_host_services->file->metadata(g_host_services->context, target, &metadata);
        if (measured.status != HL_STATUS_OK) {
            result = bound_host_error(measured.status);
        } else {
            hl_linux_file_status status;
            hl_linux_fd_snapshot target_snapshot = {.host_handle = target};
            bound_status_from_metadata(&status, &metadata);
            bound_virtualize_owner(&target_snapshot, &status);
            uint8_t encoded[GUEST_LINUX_STAT_BYTES];
            fill_linux_bound_stat(encoded, &status);
            result = guest_copy_to(a2, encoded, sizeof encoded) == sizeof encoded ? 0 : -EFAULT;
        }
        if (close_target) (void)g_host_services->file->close(g_host_services->context, target);
        break;
    }
    case 78: {
        char path[HL_LINUX_PATH_MAX + 1];
        char output[HL_LINUX_PATH_MAX];
        size_t path_size;
        size_t capacity = a3 < sizeof output ? (size_t)a3 : sizeof output;
        if (a3 == 0 || a3 > SIZE_MAX || guest_accessible_prefix(a2, capacity, HL_LOGICAL_VMA_WRITE) != capacity) {
            result = a3 == 0 ? -EINVAL : -EFAULT;
            break;
        }
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        hl_host_result opened =
            g_host_services->file->open_relative(g_host_services->context, source.host_handle, path, path_size,
                                                 HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0);
        if (opened.status != HL_STATUS_OK) {
            result = bound_host_error(opened.status);
            break;
        }
        hl_host_result read =
            g_host_services->file->readlink(g_host_services->context, opened.value, (hl_host_bytes){output, capacity});
        result = read.status == HL_STATUS_OK ? (int64_t)read.value : bound_host_error(read.status);
        if (result > 0 && guest_copy_to(a2, output, (size_t)result) != result) result = -EFAULT;
        (void)g_host_services->file->close(g_host_services->context, opened.value);
        break;
    }
    case 56: {
        struct fdvis_reservation fdvis;
        const uint32_t supported = HL_LINUX_O_ACCMODE | HL_LINUX_O_CREAT | HL_LINUX_O_EXCL | HL_LINUX_O_TRUNC |
                                   HL_LINUX_O_APPEND | HL_LINUX_O_NONBLOCK | HL_LINUX_O_NOFOLLOW |
                                   HL_LINUX_O_DIRECTORY | HL_LINUX_O_PATH | HL_LINUX_O_CLOEXEC;
        uint32_t flags = typed_open_flags(a2);
        size_t path_size;
        char path[HL_LINUX_PATH_MAX + 1];
        int shadow;
        hl_linux_fd_reservation reservation;
        hl_status status;
        result = bound_path_copy(a1, path, &path_size);
        if (result != 0) break;
        if (path[0] == '/') return 0;
        if ((flags & ~supported) != 0) {
            result = -HL_LINUX_EINVAL;
            break;
        }
        shadow = bound_shadow_reserve(0);
        if (shadow < 0) {
            result = -(int64_t)errno;
            break;
        }
        if (shadow >= guest_nofile_cur()) {
            close(shadow);
            result = -HL_LINUX_EMFILE;
            break;
        }
        if (proc_fdvis_reserve(&fdvis) != 0) {
            close(shadow);
            result = -HL_LINUX_ENOSPC;
            break;
        }
        for (;;) {
            status = hl_linux_fd_reserve_at(g_linux_box, (hl_linux_fd)shadow, &reservation);
            if (status != HL_STATUS_ALREADY_EXISTS) break;
            close(shadow);
            shadow = bound_shadow_reserve(shadow + 1);
            if (shadow < 0 || shadow >= guest_nofile_cur()) break;
        }
        if (status != HL_STATUS_OK || shadow < 0 || shadow >= guest_nofile_cur()) {
            if (shadow >= 0) close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
            result = -HL_LINUX_EMFILE;
            break;
        }
        result = hl_linux_openat_reserved(g_linux_box, &reservation, (int32_t)source.fd, path, path_size, flags,
                                          (uint32_t)a3);
        HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "openat-bound path=%s flags=%#x result=%lld", path, flags,
                (long long)result);
        if (result < 0) {
            (void)hl_linux_fd_cancel(g_linux_box, &reservation);
            close(shadow);
            proc_fdvis_reservation_cancel(&fdvis);
        } else {
            hl_linux_fd_snapshot opened;
            hl_host_file_metadata metadata = {0};
            uint32_t kind = HL_HOST_FD_OTHER;
            if (bound_snapshot((uint64_t)result, &opened) && g_host_services != NULL && g_host_services->file != NULL &&
                g_host_services->file->metadata != NULL &&
                g_host_services->file->metadata(g_host_services->context, opened.host_handle, &metadata).status ==
                    HL_STATUS_OK) {
                if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY ||
                    metadata.type == HL_HOST_FILE_TYPE_SYMLINK)
                    kind = HL_HOST_FD_FILE;
                else if (metadata.type == HL_HOST_FILE_TYPE_FIFO)
                    kind = HL_HOST_FD_PIPE;
                else if (metadata.type == HL_HOST_FILE_TYPE_SOCKET)
                    kind = HL_HOST_FD_SOCKET;
            }
            proc_fdvis_reservation_publish(&fdvis, (int)result, kind, metadata.stable_device, metadata.stable_object);
        }
        break;
    }
    default: return 0;
    }
    G_RET(c) = (uint64_t)result;
    return 1;
}
