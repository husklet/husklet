static int ckpt_restore_prior_kind(const struct ckpt_fd *records, int index, int kind, uint64_t object_id) {
    for (int prior = 0; prior < index; ++prior)
        if (records[prior].kind == kind && records[prior].object_id == object_id) return records[prior].gfd;
    return -1;
}

static int ckpt_restore_prior_ofd(const struct ckpt_fd *records, int index, uint64_t ofd_id) {
    for (int prior = 0; prior < index; ++prior)
        if (records[prior].ofd_id == ofd_id) return records[prior].gfd;
    return -1;
}

static void ckpt_restore_reset_inherited_fds(const struct ckpt_fd *records, int count) {
    static unsigned char desired_pipe[HL_NFD];
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (!g_eventfd_peer[fd]) continue;
        proc_fdvis_close(fd);
        close(fd);
        g_eventfd_peer[fd] = 0;
        g_eventfd_cslot[fd] = 0;
        g_eventfd_sema[fd] = 0;
        g_eventfd_gnb[fd] = 0;
    }
    memset(g_eventfd_refs, 0, sizeof g_eventfd_refs);
    memset(desired_pipe, 0, sizeof desired_pipe);
    for (int i = 0; i < count; i++)
        if (records[i].kind == CKF_PIPE && records[i].gfd >= 0 && records[i].gfd < HL_NFD)
            desired_pipe[records[i].gfd] = 1;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (g_pipe_identity[fd] == 0 || desired_pipe[fd]) continue;
        proc_fdvis_close(fd);
        g_pipe_identity[fd] = 0;
        close(fd);
    }
}

static int ckpt_restore_record_replaces_typed_fd(int kind) {
    switch (kind) {
        case CKF_FILE:
        case CKF_PIPE:
        case CKF_BLOB:
        case CKF_MEMFD:
        case CKF_EVENTFD:
        case CKF_TIMERFD:
        case CKF_INOTIFY:
        case CKF_EPOLL:
        case CKF_SOCKETPAIR:
        case CKF_SOCKET:
        case CKF_SIGNALFD:
        case CKF_DEVICE:
            return 1;
        default:
            return 0;
    }
}

static int ckpt_restore_retire_typed_fd(const struct ckpt_fd *record) {
    if (!ckpt_restore_record_replaces_typed_fd(record->kind) || g_linux_box == NULL ||
        hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)record->gfd, &(hl_linux_fd_snapshot){0}) != HL_STATUS_OK)
        return 0;
    /* The typed table owns the provider handle while the process descriptor table owns the numeric shadow.
     * A restore that replaces the object must retire both before publishing the native replacement.  In
     * particular, CKF_DEVICE at fd 0 is commonly /dev/null; leaving its typed entry alive makes the next
     * checkpoint inspect a stale provider handle instead of the newly opened native descriptor. */
    if (hl_linux_close(g_linux_box, (hl_linux_fd)record->gfd) < 0) return -1;
    proc_fdvis_close(record->gfd);
    if (close(record->gfd) != 0 && errno != EBADF) return -1;
    return 0;
}

static void ckpt_restore_socket_state(int fd, const struct ckpt_socket_state *state) {
    if (state->guest_family == AF_UNIX && state->host_family == AF_UNIX) {
        const struct sockaddr_un *local = (const void *)&state->local;
        if (local->sun_path[0] == '/') unix_bind_note(fd, local->sun_path);
    }
    g_tcp_listen[fd] = state->listening != 0;
    g_sock_backlog[fd] = state->backlog;
    g_lo_port[fd] = state->lo_port;
    g_lo_v6[fd] = state->lo_v6;
    g_lo_v6only[fd] = state->lo_v6only;
    g_br_port[fd] = state->br_port;
    g_br_ip[fd] = state->br_ip;
    g_br_interface[fd] = state->br_interface;
    g_tcp_lport[fd] = state->tcp_local_port;
    g_tcp_laddr[fd] = state->tcp_local_address;
    g_tcp_l6[fd] = state->tcp_local_v6;
    memcpy(g_tcp_laddr6[fd], state->tcp_local_address_v6, sizeof state->tcp_local_address_v6);
    g_so_error[fd] = state->pending_error;
    g_so_reuseport[fd] = state->shadow_reuse_port;
    memcpy(g_tcp_optval[fd], state->tcp_option_value, sizeof state->tcp_option_value);
    memcpy(g_tcp_optset[fd], state->tcp_option_set, sizeof state->tcp_option_set);
    memcpy(g_ipopt_val[fd], state->ip_option_value, sizeof state->ip_option_value);
    memcpy(g_ipopt_set[fd], state->ip_option_set, sizeof state->ip_option_set);
}

static int ckpt_restore_socketpair_fd(const struct ckpt_fd *records, int count, const struct ckpt_fd *record) {
    struct ckpt_restore_socket_endpoint *endpoint = ckpt_restore_socket_find(record->object_id);
    int fd = record->gfd;
    if (endpoint == NULL || endpoint->fd < 0 || fd < 0 || fd >= HL_NFD || dup2(endpoint->fd, fd) < 0) return -1;
    int live_flags = fcntl(fd, F_GETFL);
    if (live_flags < 0 || fcntl(fd, F_SETFL, (live_flags & ~O_NONBLOCK) | (record->flags & O_NONBLOCK)) != 0 ||
        fcntl(fd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
        return -1;
    const struct ckpt_socket_state *state = &endpoint->state;
    g_sock_object[fd] = record->object_id;
    g_sock_peer_object[fd] = record->auxiliary;
    g_sock_fam[fd] = endpoint->state_loaded ? (uint16_t)state->guest_family : AF_UNIX;
    g_sock_stream[fd] = record->offset == SOCK_STREAM;
    g_sock_dgram[fd] = record->offset == SOCK_DGRAM || record->offset == SOCK_SEQPACKET;
    g_sock_seqpacket[fd] = record->offset == SOCK_SEQPACKET;
    g_sock_conn[fd] = 1;
    if (endpoint->state_loaded) ckpt_restore_socket_state(fd, state);
    int peer = ckpt_restore_prior_kind(records, count, CKF_SOCKETPAIR, record->auxiliary);
    if (peer >= 0) g_sock_pair_peer[fd] = peer + 1;
    return proc_fdvis_publish_native_fd(fd);
}

static int ckpt_restore_bound_socket_fd(const struct ckpt_fd *records, int index, const struct ckpt_fd *record) {
    struct ckpt_restore_socket *saved = ckpt_restore_socket_state_find(record->object_id);
    int fd = record->gfd;
    if (saved == NULL || saved->fd < 0 || fd < 0 || fd >= HL_NFD || dup2(saved->fd, fd) < 0) return -1;
    int live_flags = fcntl(fd, F_GETFL);
    if (live_flags < 0 || fcntl(fd, F_SETFL, (live_flags & ~O_NONBLOCK) | (record->flags & O_NONBLOCK)) != 0 ||
        fcntl(fd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
        return -1;
    const struct ckpt_socket_state *state = &saved->state;
    g_sock_object[fd] = record->object_id;
    g_sock_peer_object[fd] = 0;
    g_sock_fam[fd] = (uint16_t)state->guest_family;
    g_sock_stream[fd] = state->type == SOCK_STREAM;
    g_sock_dgram[fd] = state->type == SOCK_DGRAM;
    g_sock_conn[fd] = 0;
    ckpt_restore_socket_state(fd, state);
    g_udp_local_port[fd] = (uint16_t)state->udp_local_port;
    g_udp_peer_port[fd] = (uint16_t)state->udp_peer_port;
    g_udp_local_ip[fd] = state->udp_local_ip;
    g_udp_peer_ip[fd] = state->udp_peer_ip;
    g_udp_local_v6[fd] = state->udp_local_v6;
    g_udp_peer_v6[fd] = state->udp_peer_v6;
    g_udp_local_interface[fd] = state->udp_local_interface;
    g_udp_peer_interface[fd] = state->udp_peer_interface;
    if (state->udp_local_port != 0 && state->host_family == AF_UNIX) {
        int source = ckpt_restore_prior_kind(records, index, CKF_SOCKET, record->object_id);
        if (source >= 0) {
            udp_ref_dup(fd, source);
        } else {
            const struct sockaddr_un *address = (const void *)&state->local;
            if (udp_ref_create(fd, address->sun_path) != 0) return -1;
        }
    }
    return proc_fdvis_publish_native_fd(fd);
}

static int ckpt_restore_signalfd_fd(const struct ckpt_fd *records, int index, const struct ckpt_fd *record) {
    struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(record->object_id);
    int fd = record->gfd;
    if (object == NULL || dup2(object->reader, fd) < 0) return -1;
    int source = ckpt_restore_prior_kind(records, index, CKF_SIGNALFD, record->object_id);
    int slot;
    if (source >= 0) {
        slot = g_sigfd_slot[source] - 1;
        if (slot < 0 || slot >= HL_SFD_MAX) return -1;
        g_sfd[slot].refs++;
    } else {
        slot = sfd_alloc();
        int writer = slot >= 0 ? fcntl(object->writer, F_DUPFD, 1 << 20) : -1;
        if (writer < 0 && slot >= 0) writer = fcntl(object->writer, F_DUPFD, 64);
        if (slot < 0 || writer < 0 || hl_host_process_fd_private_adopt(writer) < 0) {
            if (writer >= 0) close(writer);
            if (slot >= 0) g_sfd[slot].refs = 0;
            return -1;
        }
        g_sfd[slot].rd = fd;
        g_sfd[slot].wr = writer;
        g_sfd[slot].mask = object->mask;
    }
    g_sigfd_slot[fd] = (uint8_t)(slot + 1);
    int live_flags = fcntl(fd, F_GETFL);
    if (live_flags < 0 || fcntl(fd, F_SETFL, (live_flags & ~O_NONBLOCK) | (record->flags & O_NONBLOCK)) != 0 ||
        fcntl(fd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
        return -1;
    g_ofd_id[fd] = record->ofd_id;
    return proc_fdvis_publish_native_fd(fd);
}

static int ckpt_restore_eventfd_fd(const struct ckpt_fd *record) {
    struct ckpt_restore_eventfd *object = ckpt_restore_eventfd_find(record->object_id);
    int fd = record->gfd;
    if (object == NULL || object->slot < 0 || object->slot >= HL_NFD || fd < 0 || fd >= HL_NFD ||
        dup2(object->reader, fd) < 0)
        return -1;
    if (fcntl(fd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
    int live_flags = fcntl(fd, F_GETFL);
    if (live_flags < 0 || fcntl(fd, F_SETFL, live_flags | O_NONBLOCK) != 0) return -1;
    g_eventfd_peer[fd] = object->writer + 1;
    g_eventfd_cslot[fd] = object->slot + 1;
    g_eventfd_sema[fd] = object->semaphore;
    eventfd_guest_nb_set(fd, object->guest_nonblock);
    g_eventfd_refs[object->slot]++;
    return proc_fdvis_publish_native_fd(fd);
}

static int ckpt_restore_timerfd_state(const struct ckpt_fd *record, int source, int slot, int first,
                                      struct ckpt_restore_timerfd *restored) {
    if (source >= 0) {
        g_tfd_deadline[record->gfd] = g_tfd_deadline[slot];
        g_tfd_interval[record->gfd] = g_tfd_interval[slot];
        g_tfd_first_oneshot[record->gfd] = g_tfd_first_oneshot[slot];
        return 0;
    }
    struct timespec now;
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
    int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
    timerfd_shared_lock(restored->state);
    int64_t next = restored->state->deadline;
    int64_t interval = restored->state->interval;
    uint64_t pending = restored->state->pending;
    timerfd_shared_unlock(restored->state);
    g_tfd_deadline[slot] = next;
    g_tfd_interval[slot] = interval;
    g_tfd_pending[slot] = pending;
    g_tfd_first_oneshot[slot] = interval > 0 ? 1 : (uint8_t)first;
    if (pending == 0 && next <= now_ns) return 0;
    struct kevent event;
    int64_t delay = pending != 0 ? 1 : next - now_ns;
    EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
    if (kevent(record->gfd, &event, 1, NULL, 0, NULL) >= 0) return 0;
    fprintf(stderr, "[restore] timerfd %d arm failed: %s\n", record->gfd, strerror(errno));
    return -1;
}

static int ckpt_restore_timerfd_fd(const struct ckpt_fd *records, int index, const struct ckpt_fd *record) {
    int clock_id = 0, first = 0;
    unsigned long long pending_value = 0;
    long long captured_ns = 0;
    if (sscanf(record->path, "%d %llu %u %lld", &clock_id, &pending_value, (unsigned *)&first, &captured_ns) != 4) {
        fprintf(stderr, "[restore] timerfd %d has invalid metadata '%s'\n", record->gfd, record->path);
        return -1;
    }
    int source = ckpt_restore_prior_kind(records, index, CKF_TIMERFD, record->object_id);
    struct ckpt_restore_timerfd *restored = ckpt_restore_timerfd_find(record->object_id);
    if (restored == NULL || restored->state == NULL) return -1;
    int timer = source >= 0 ? dup(source) : kqueue();
    if (timer < 0) {
        fprintf(stderr, "[restore] timerfd %d create/dup failed: %s\n", record->gfd, strerror(errno));
        return -1;
    }
    if (source >= 0) hl_native_kqueue_duplicate(source, timer);
    if (timer != record->gfd) {
        if (dup2(timer, record->gfd) < 0) {
            fprintf(stderr, "[restore] timerfd %d target dup failed: %s\n", record->gfd, strerror(errno));
            close(timer);
            return -1;
        }
        hl_native_kqueue_relocate(timer, record->gfd);
        close(timer);
    }
    if (fcntl(record->gfd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) {
        fprintf(stderr, "[restore] timerfd %d flag restore failed: %s\n", record->gfd, strerror(errno));
        return -1;
    }
    int slot = source >= 0 ? timerfd_slot(source) : record->gfd;
    if (slot < 0 || slot >= HL_NFD) {
        fprintf(stderr, "[restore] timerfd %d invalid canonical slot %d\n", record->gfd, slot);
        return -1;
    }
    g_timerfd[record->gfd] = 1;
    g_epoll_family_seen = 1;
    g_tfd_cslot[record->gfd] = slot + 1;
    g_tfd_object[record->gfd] = record->object_id;
    g_tfd_clock[record->gfd] = clock_id;
    g_tfd_nb[record->gfd] = (record->flags & O_NONBLOCK) != 0;
    g_tfd_shared[record->gfd] = restored->state;
    g_tfd_refs[slot]++;
    if (ckpt_restore_timerfd_state(record, source, slot, first, restored) != 0) return -1;
    if (proc_fdvis_publish_native_fd(record->gfd) == 0) return 0;
    fprintf(stderr, "[restore] timerfd %d publication failed\n", record->gfd);
    return -1;
}

static int ckpt_restore_typed_inotify(const char *procdir, const struct ckpt_fd *record, int source) {
    if (source >= 0) {
        if (dup2(source, record->gfd) < 0 ||
            hl_linux_dup3(g_linux_box, (hl_linux_fd)source, (hl_linux_fd)record->gfd,
                          (record->descriptor_flags & FD_CLOEXEC) ? HL_LINUX_O_CLOEXEC : 0) < 0)
            return -1;
    } else {
        char image_path[1400];
        snprintf(image_path, sizeof image_path, "%s/%s", procdir, record->path);
        int64_t stored = ckpt_source_object_size(image_path);
        size_t image_size;
        if (ckpt_inotify_object_size(stored, &image_size) != 0) {
            fprintf(stderr, "[restore] inotify %d image %s is invalid: %s\n", record->gfd, image_path, strerror(errno));
            return -1;
        }
        void *image = malloc(image_size);
        if (image == NULL || ckpt_source_load(image_path, image, image_size) != 0) {
            fprintf(stderr, "[restore] inotify %d cannot load %s\n", record->gfd, image_path);
            free(image);
            return -1;
        }
        int flags = O_RDONLY | ((record->descriptor_flags & FD_CLOEXEC) ? O_CLOEXEC : 0);
        int shadow = open(HL_LINUX_HOST_NULL_DEVICE, flags);
        if (shadow < 0 || (shadow != record->gfd && dup2(shadow, record->gfd) < 0)) {
            fprintf(stderr, "[restore] inotify %d cannot reserve native shadow: %s\n", record->gfd, strerror(errno));
            if (shadow >= 0) close(shadow);
            free(image);
            return -1;
        }
        if (shadow != record->gfd) close(shadow);
        void *provider = bound_inotify_provider_create(g_host_services);
        int64_t imported = provider == NULL
                               ? -HL_LINUX_ENOMEM
                               : hl_linux_inotify_import_at(g_linux_box, (hl_linux_fd)record->gfd, &bound_inotify_ops,
                                                            provider, (uint32_t)record->descriptor_flags,
                                                            (uint32_t)record->flags, image, image_size);
        free(image);
        if (imported < 0) {
            fprintf(stderr, "[restore] inotify %d typed import failed: %lld\n", record->gfd, (long long)imported);
            close(record->gfd);
            return -1;
        }
    }
    if (proc_fdvis_publish(record->gfd, HL_HOST_FD_OTHER, 0, 0) == 0) return 0;
    fprintf(stderr, "[restore] inotify %d fd visibility publication failed\n", record->gfd);
    return -1;
}

static int ckpt_restore_native_inotify(const struct ckpt_fd *record, int source) {
#if defined(__linux__)
    int instance =
        source >= 0
            ? dup(source)
            : inotify_init1((record->flags & O_NONBLOCK) | ((record->descriptor_flags & FD_CLOEXEC) ? 0x80000 : 0));
#else
    int instance = source >= 0 ? dup(source) : kqueue();
#endif
    if (instance < 0) return -1;
    if (source >= 0) hl_native_kqueue_duplicate(source, instance);
    if (instance != record->gfd) {
        if (dup2(instance, record->gfd) < 0) {
            close(instance);
            return -1;
        }
        if (source >= 0) hl_native_kqueue_duplicate(source, record->gfd);
        close(instance);
    }
    if (fcntl(record->gfd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
    g_inotify[record->gfd] = 1;
    g_inotify_nb[record->gfd] = (record->flags & O_NONBLOCK) != 0;
    g_inotify_object[record->gfd] = record->object_id;
    g_epoll_family_seen = 1;
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_inotify_fd(const char *procdir, const struct ckpt_fd *records, int index,
                                   const struct ckpt_fd *record) {
    int source = ckpt_restore_prior_kind(records, index, CKF_INOTIFY, record->object_id);
    return record->path[0] != 0 ? ckpt_restore_typed_inotify(procdir, record, source)
                                : ckpt_restore_native_inotify(record, source);
}

static int ckpt_restore_existing_ofd(const struct ckpt_fd *records, int index, const struct ckpt_fd *record) {
    if (record->ofd_id == 0 || record->kind == CKF_PIPE || record->kind == CKF_TTY) return 0;
    int source = ckpt_restore_prior_ofd(records, index, record->ofd_id);
    if (source < 0) return 0;
    if (dup2(source, record->gfd) < 0) return -1;
    fcntl(record->gfd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0);
    if (record->kind == CKF_MEMFD && record->gfd >= 0 && record->gfd < HL_NFD) {
        g_memfd_is[record->gfd] = 1;
        g_memfd_seal[record->gfd] = (int)record->auxiliary;
        memfd_reg_set_fd(record->gfd, g_memfd_seal[record->gfd]);
    }
    if (record->gfd >= 0 && record->gfd < HL_NFD) g_ofd_id[record->gfd] = record->ofd_id;
    return proc_fdvis_publish_native_fd(record->gfd) == 0 ? 1 : -1;
}

static int ckpt_restore_saved_ofd(const struct ckpt_fd *record) {
    if ((record->kind != CKF_FILE && record->kind != CKF_BLOB && record->kind != CKF_MEMFD) || record->ofd_id == 0)
        return 0;
    struct ckpt_restore_right *right = ckpt_restore_right_find(record->ofd_id);
    if (right == NULL) return 0;
    if (right->object_id != record->object_id || dup2(right->fd, record->gfd) < 0 ||
        fcntl(record->gfd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
        return -1;
    g_ofd_id[record->gfd] = record->ofd_id;
    if (record->kind == CKF_MEMFD) {
        g_memfd_is[record->gfd] = 1;
        g_memfd_seal[record->gfd] = (int)record->auxiliary;
        memfd_reg_set_fd(record->gfd, g_memfd_seal[record->gfd]);
    }
    return proc_fdvis_publish_native_fd(record->gfd) == 0 ? 1 : -1;
}

static int ckpt_restore_tty_fd(const struct ckpt_fd *record) {
    if (record->gfd > 2) {
        int ctty = ckpt_ctty_open();
        if (ctty >= 0 && record->gfd != ctty && dup2(ctty, record->gfd) >= 0 && (record->flags & FD_CLOEXEC))
            fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
        ckpt_ctty_close(ctty);
    }
    if (record->descriptor_flags & FD_CLOEXEC) fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
    return 0;
}

static int ckpt_restore_pipe_fd(const struct ckpt_fd *record) {
    uint64_t identity = (uint64_t)record->offset;
    struct ckpt_restore_pipe *pipe = ckpt_restore_pipe_find(identity);
    int source = ((record->flags & O_ACCMODE) == O_WRONLY) ? (pipe ? pipe->writer : -1) : (pipe ? pipe->reader : -1);
    if (source < 0 || dup2(source, record->gfd) < 0) return -1;
    int live_flags = fcntl(record->gfd, F_GETFL);
    if (live_flags < 0 || fcntl(record->gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (record->flags & O_NONBLOCK)) != 0)
        return -1;
    if (record->descriptor_flags & FD_CLOEXEC) fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
    g_pipe_identity[record->gfd] = identity;
    g_pipesz[record->gfd] = pipe->size;
    return proc_fdvis_publish(record->gfd, HL_HOST_FD_PIPE, 1, identity);
}

static int ckpt_restore_memfd_fd(const char *procdir, const struct ckpt_fd *record) {
    int seed = ckpt_restore_backing_find(record->object_id);
    if (seed >= 0) {
        if (dup2(seed, record->gfd) < 0) return -1;
        int live_flags = fcntl(record->gfd, F_GETFL);
        if (live_flags < 0 ||
            fcntl(record->gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (record->flags & O_NONBLOCK)) != 0 ||
            lseek(record->gfd, (off_t)record->offset, SEEK_SET) < 0)
            return -1;
        if (record->descriptor_flags & FD_CLOEXEC) fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
        if (proc_fdvis_publish_native_fd(record->gfd) != 0) return -1;
    } else if (ckpt_restore_file_blob(procdir, record) != 0) {
        return -1;
    }
    if (record->gfd < 0 || record->gfd >= HL_NFD) return -1;
    g_memfd_is[record->gfd] = 1;
    g_memfd_seal[record->gfd] = (int)record->auxiliary;
    memfd_reg_set_fd(record->gfd, g_memfd_seal[record->gfd]);
    return 0;
}

static int ckpt_restore_file_fd(const struct ckpt_fd *record) {
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int host_fd = open(record->path, flags);
    if (host_fd < 0) {
        fprintf(stderr, "[restore] cannot reopen fd %d (%s): %s\n", record->gfd, record->path, strerror(errno));
        return -1;
    }
    if (host_fd != record->gfd) {
        dup2(host_fd, record->gfd);
        close(host_fd);
    }
    if (record->offset > 0) lseek(record->gfd, (off_t)record->offset, SEEK_SET);
    if (record->descriptor_flags & FD_CLOEXEC) fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
    if (record->gfd >= 0 && record->gfd < HL_NFD &&
        path_copy(g_fdpath[record->gfd], sizeof g_fdpath[record->gfd], record->path) != 0)
        g_fdpath[record->gfd][0] = 0;
    if (record->gfd >= 0 && record->gfd < HL_NFD) g_fdpath_guest[record->gfd] = 0;
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_device_fd(const struct ckpt_fd *record) {
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int host_fd = open(record->path, flags);
    if (host_fd < 0 || (host_fd != record->gfd && dup2(host_fd, record->gfd) < 0)) {
        if (host_fd >= 0) close(host_fd);
        return -1;
    }
    if (host_fd != record->gfd) close(host_fd);
    if (record->descriptor_flags & FD_CLOEXEC) fcntl(record->gfd, F_SETFD, FD_CLOEXEC);
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_fd_record(const char *procdir, const struct ckpt_fd *records, int count, int index) {
    const struct ckpt_fd *record = &records[index];
    if (ckpt_restore_retire_typed_fd(record) != 0) return -1;
    if (record->kind == CKF_EPOLL) return 0;
    if (record->kind == CKF_SOCKETPAIR) return ckpt_restore_socketpair_fd(records, count, record);
    if (record->kind == CKF_SOCKET) return ckpt_restore_bound_socket_fd(records, index, record);
    if (record->kind == CKF_SIGNALFD) return ckpt_restore_signalfd_fd(records, index, record);
    if (record->kind == CKF_EVENTFD) return ckpt_restore_eventfd_fd(record);
    if (record->kind == CKF_TIMERFD) return ckpt_restore_timerfd_fd(records, index, record);
    if (record->kind == CKF_INOTIFY) return ckpt_restore_inotify_fd(procdir, records, index, record);
    int restored = ckpt_restore_existing_ofd(records, index, record);
    if (restored != 0) return restored < 0 ? -1 : 0;
    restored = ckpt_restore_saved_ofd(record);
    if (restored != 0) return restored < 0 ? -1 : 0;
    if (record->kind == CKF_TTY) return ckpt_restore_tty_fd(record);
    if (record->kind == CKF_PIPE) return ckpt_restore_pipe_fd(record);
    if (record->kind == CKF_BLOB) return ckpt_restore_file_blob(procdir, record);
    if (record->kind == CKF_MEMFD) return ckpt_restore_memfd_fd(procdir, record);
    if (record->kind == CKF_FILE) return ckpt_restore_file_fd(record);
    if (record->kind == CKF_DEVICE) return ckpt_restore_device_fd(record);
    return 0;
}

static int ckpt_restore_epoll_fd(const struct ckpt_fd *records, int index) {
    const struct ckpt_fd *record = &records[index];
    int source = ckpt_restore_prior_kind(records, index, CKF_EPOLL, record->object_id);
    int instance = source >= 0 ? dup(source) : kqueue();
    if (instance < 0) return -1;
    if (source >= 0) hl_native_kqueue_duplicate(source, instance);
    if (instance != record->gfd) {
        if (dup2(instance, record->gfd) < 0) {
            close(instance);
            return -1;
        }
        hl_native_kqueue_relocate(instance, record->gfd);
        close(instance);
    }
    if (fcntl(record->gfd, F_SETFD, (record->descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
    g_ep_provider_generations[record->gfd] = ep_provider_next(g_ep_provider_generations[record->gfd]);
    g_epoll[record->gfd] = 1;
    g_ep_cslot[record->gfd] = (uint16_t)((source >= 0 ? epoll_slot(source) : record->gfd) + 1);
    g_ep_dupd[record->gfd] = source >= 0;
    if (source >= 0) {
        g_ep_dupd[source] = 1;
        ofd_link_dup(record->gfd, source);
    }
    g_epoll_family_seen = 1;
    ep_mem_clear(record->gfd);
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_epoll_fds(const char *procdir, const struct ckpt_fd *records, int count) {
    for (int index = 0; index < count; ++index)
        if (records[index].kind == CKF_EPOLL && ckpt_restore_epoll_fd(records, index) != 0) return -1;
    for (int index = 0; index < count; ++index) {
        if (records[index].kind != CKF_EPOLL) continue;
        int source = ckpt_restore_prior_kind(records, index, CKF_EPOLL, records[index].object_id);
        if (source < 0 && ckpt_restore_epoll_watches(procdir, &records[index]) != 0) return -1;
    }
    return 0;
}

static int ckpt_restore_fds_dir(const char *procdir) {
    struct ckpt_fd *records = NULL;
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/fds", procdir);
    // The object's length, asked of the source. fstat(fileno()) does not work here: a streamed object is a
    // memory stream with no descriptor behind it. Validate before ckpt_source_fopen materializes the object,
    // so an invalid record count cannot drive an otherwise unnecessary allocation.
    int64_t record_bytes = ckpt_source_object_size(pf);
    size_t image_size, record_count;
    if (record_bytes < 0) return 0;
    if (ckpt_record_object_size(record_bytes, sizeof *records, HL_NFD, &image_size, &record_count) != 0) return -1;
    if (image_size != record_count * sizeof *records) return -1;
    FILE *f = ckpt_source_fopen(pf);
    if (!f) return -1;
    int count = (int)record_count;
    records = calloc(record_count ? record_count : 1, sizeof *records);
    if (records == NULL || (record_count != 0 && fread(records, sizeof *records, record_count, f) != record_count) ||
        fgetc(f) != EOF) {
        free(records);
        ckpt_source_fclose(f);
        return -1;
    }
    ckpt_source_fclose(f);
    ckpt_fd_terminate_all(records, (size_t)count);
    ckpt_restore_reset_inherited_fds(records, count);
    for (int index = 0; index < count; ++index)
        if (ckpt_restore_fd_record(procdir, records, count, index) != 0) {
            free(records);
            return -1;
        }
    if (ckpt_restore_epoll_fds(procdir, records, count) != 0) {
        free(records);
        return -1;
    }
    int restored = ckpt_restore_inotify_sidecar(procdir);
    free(records);
    return restored;
}

static int ckpt_restore_cpu_dir(const char *procdir, const struct ckpt_meta *m, struct cpu **out) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/cpu", procdir);
    *out = NULL;
    size_t bytes;
    if (ckpt_fixed_payload_object_size(ckpt_source_object_size(pf), sizeof(struct ckpt_cpu_header), m->n_threads,
                                       sizeof(struct cpu), THREAD_REG_MAX, &bytes) != 0)
        return -1;
    FILE *file = ckpt_source_fopen(pf);
    struct ckpt_cpu_header header;
    if (file == NULL || ckpt_rd_all(file, &header, sizeof header) != 0) {
        if (file != NULL) ckpt_source_fclose(file);
        fprintf(stderr, "[restore] cannot read cpu state\n");
        return -1;
    }
    if (header.magic != CKPT_CPU_MAGIC || header.version != m->version || header.arch != G_CKPT_ARCH ||
        header.count != m->n_threads || header.payload_size != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu image version/architecture/layout mismatch\n");
        ckpt_source_fclose(file);
        return -1;
    }
    struct cpu *images = malloc(bytes);
    if (!images || ckpt_rd_all(file, images, bytes) != 0 || fgetc(file) != EOF) {
        free(images);
        ckpt_source_fclose(file);
        return -1;
    }
    ckpt_source_fclose(file);
    // Zero host-transient fields (meaningful only WHILE a block runs; run_block re-populates them). The
    // architectural state (x[],sp,pc,tls,nzcv,v[],sigmask,tpending,alt_*,tid,ctid) + shadow stack are verbatim.
    for (uint64_t i = 0; i < m->n_threads; i++) {
        struct cpu *c = &images[i];
        memset(c->host_save, 0, sizeof c->host_save);
        memset(c->host_v, 0, sizeof c->host_v);
        c->host_sp = 0;
        c->reason = 0;
        c->irq = 0;
        c->in_service = 0;
        c->exited = 0;
        c->redirect = 0;
        /* Filter nodes are engine addresses, not checkpoint data.  The former
           host-TLS model also did not serialize seccomp state. */
        c->seccomp_filters = NULL;
        c->seccomp_mode = 0;
        G_CKPT_CPU_SANITIZE(c);
    }
    *out = images;
    return 0;
}

static int ckpt_restore_leader(const struct cpu *images, uint64_t count, struct cpu *leader) {
    for (uint64_t i = 0; i < count; i++) {
        if (images[i].tid != 0) continue;
        *leader = images[i];
        return 0;
    }
    fprintf(stderr, "[restore] checkpoint thread group has no leader\n");
    return -1;
}

// Re-install this process's guest signal DISPOSITIONS after a restore, from the table carried in `m`.
// g_sigact (the guest handler table) is engine C state, not guest RAM, so it is NOT in the page dump; a
// freshly re-launched/-forked restore process starts with an all-SIG_DFL table and DEFAULT host
// dispositions -- so a ^C (SIGINT) at a restored bash prompt hit the host default action (terminate) and
// KILLED the shell instead of running bash's interrupt handler (the "restore + Ctrl-C closes the tab" bug).
// This restores g_sigact and replays the exact host-side installation rt_sigaction(case 134) performs, so
// async signals route back through the engine's host_sigh and reach the guest handler.
static void ckpt_reinstall_sigacts(const struct ckpt_meta *m) {
    for (int s = 1; s <= 64; s++) {
        uint64_t h = m->sig_handler[s];
        g_sigact[s].handler = h;
        g_sigact[s].flags = m->sig_flags[s];
        g_sigact[s].mask = m->sig_mask[s];
        if (s == 9 || s == 19) continue; // SIGKILL/SIGSTOP: unmaskable, no host disposition to forward
        // SIGSEGV(11)/SIGBUS(7) keep the engine's permanent hardware-fault guard -- never overwrite it with a
        // plain disposition (that is exactly what rt_sigaction refuses to do). Only ILL/TRAP/FPE(4/5/8), whose
        // POSIX handler only ever sees an EXTERNAL kill, and the async signals forward to the host here.
        if (s == 7 || s == 11) continue;
        int ms = sig_l2m(s);
        if (sig_host_is_engine_control(ms)) continue;
        if (h == 0) {
            signal(ms, SIG_DFL);
        } else if (h == 1) {
            signal(ms, SIG_IGN);
        } else {
            struct sigaction sa;
            memset(&sa, 0, sizeof sa);
            sa.sa_sigaction = (s == 4 || s == 5 || s == 8) ? host_sigh_sync : host_sigh_si;
            sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
            sigfillset(&sa.sa_mask);
            sigaction(ms, &sa, NULL);
        }
    }
}

// Survive a terminal signal typed while the tree is still being rebuilt.
//
// A restoring process is not the guest yet: until it has replayed the guest's dispositions
// (ckpt_reinstall_sigacts) it carries the HOST defaults, so a ^C at the pty -- which the embedder's user can
// type at any moment, and which an acceptance test aims at the restored foreground job -- terminates the
// restore driver itself and takes the whole machine with it. Ignore the terminal-generated signals for that
// window; reinstall_sigacts then overwrites every disposition, SIG_DFL included. Each re-forked process does
// this again on entry, because it inherits the init's already-replayed dispositions.
static void ckpt_restore_hold_tty_signals(void) {
    static const int tty[] = {SIGINT, SIGQUIT, SIGHUP, SIGTSTP, SIGTTIN, SIGTTOU};
    for (size_t i = 0; i < sizeof tty / sizeof tty[0]; ++i)
        (void)signal(tty[i], SIG_IGN);
}

// The process table read from the checkpoint (one entry per proc.<gpid>/meta), used to rebuild the tree.
struct ckpt_proc {
    int gpid, ppid, pgid, sid;
    uint64_t version;
    int viable;
    char reason[192];
};
static struct ckpt_proc *g_rprocs;
static int g_nrprocs;
static int g_rprocs_capacity;

// Enumerate the captured process images. This is the restore-side counterpart of the coordinator's group
// count: "which processes are in this image" is answered by the store, over the same channel.
static int ckpt_scan_procs(void) {
    char names[64 * 1024];
    g_nrprocs = 0;
    int listed = ckpt_source_list("proc.", names, sizeof names);
    if (listed < 0) return -1;
    const char *cursor = names;
    for (int index = 0; index < listed; ++index, cursor += strlen(cursor) + 1) {
        if (strstr(cursor, ".tmp.")) continue;
        int gpid = atoi(cursor + 5);
        if (gpid <= 0) continue;
        char pd[1200];
        snprintf(pd, sizeof pd, "%.200s", cursor);
        struct ckpt_meta m;
        if (ckpt_read_meta_dir(pd, &m) != 0) continue;
        if (ckpt_vector_reserve((void **)&g_rprocs, &g_rprocs_capacity, sizeof *g_rprocs, g_nrprocs + 1) != 0)
            return -1;
        g_rprocs[g_nrprocs].gpid = gpid;
        g_rprocs[g_nrprocs].ppid = m.ppid_gpid;
        g_rprocs[g_nrprocs].pgid = m.pgid_gpid;
        g_rprocs[g_nrprocs].sid = m.sid_gpid;
        g_rprocs[g_nrprocs].version = m.version;
        g_rprocs[g_nrprocs].viable = 1;
        g_rprocs[g_nrprocs].reason[0] = 0;
        g_nrprocs++;
    }
    return g_nrprocs > 0 ? 0 : -1;
}

static int ckpt_proc_index(int gpid) {
    for (int i = 0; i < g_nrprocs; ++i)
        if (g_rprocs[i].gpid == gpid) return i;
    return -1;
}

static int ckpt_validate_proc_tree(const struct ckpt_manifest *man) {
    // This table is copied into the fixed-capacity guest/host pid map after fork. Refuse an image which
    // cannot fit before creating even the first child; silently truncating it would make wait/kill target
    // unrelated host identities after restore.
    if (man->n_procs == 0 || man->n_procs > HL_LINUX_PIDMAP_CAPACITY ||
        (uint64_t)g_nrprocs != man->n_procs || man->root_gpid != 1)
        return -1;

    int *relations = malloc((size_t)g_nrprocs * 3 * sizeof *relations);
    if (!relations) return -1;
    int *parents = relations;
    int *groups = relations + g_nrprocs;
    int *sessions = relations + 2 * g_nrprocs;
    int roots = 0;
    for (int i = 0; i < g_nrprocs; i++) {
        const struct ckpt_proc *process = &g_rprocs[i];
        if (process->version != man->version || process->gpid <= 0 || process->pgid <= 0 || process->sid <= 0)
            goto invalid;
        for (int j = 0; j < i; ++j)
            if (g_rprocs[j].gpid == process->gpid) goto invalid;

        if (process->gpid == man->root_gpid) {
            if (process->ppid != 0) goto invalid;
            roots++;
        } else if (process->ppid <= 0) {
            goto invalid;
        }
    }
    if (roots != 1) goto invalid;

    for (int i = 0; i < g_nrprocs; ++i) {
        const struct ckpt_proc *process = &g_rprocs[i];
        parents[i] = process->ppid == 0 ? -1 : ckpt_proc_index(process->ppid);
        groups[i] = -1;
        for (int j = 0; j < g_nrprocs; ++j) {
            if (g_rprocs[j].pgid != process->pgid) continue;
            if (g_rprocs[j].sid != process->sid) goto invalid;
            if (groups[i] < 0) groups[i] = j;
        }
        sessions[i] = ckpt_proc_index(process->sid);
        if (process->gpid != man->root_gpid && parents[i] < 0) goto invalid;
    }

    for (int i = 0; i < g_nrprocs; ++i) {
        const struct ckpt_proc *process = &g_rprocs[i];
        // Walking at most n indexed parent links proves both reachability and acyclicity without recursion.
        int ancestor = i;
        for (int depth = 0;; ++depth) {
            if (ancestor < 0 || depth >= g_nrprocs) goto invalid;
            if (g_rprocs[ancestor].gpid == man->root_gpid) break;
            ancestor = parents[ancestor];
        }

        int group_index = groups[i];
        int session_index = sessions[i];
        if (group_index < 0 || session_index < 0 || g_rprocs[session_index].sid != process->sid ||
            g_rprocs[session_index].pgid != process->sid)
            goto invalid;

        // setsid creates a process which is simultaneously the session and process-group leader. Every other
        // process inherits its saved parent's session; accepting any other transition creates a leaderless or
        // cross-session group which the host cannot reconstruct faithfully.
        if (process->gpid == process->sid) {
            if (process->pgid != process->gpid) goto invalid;
        } else if (parents[i] < 0 || process->sid != g_rprocs[parents[i]].sid) {
            goto invalid;
        }
    }

    if (man->fg_pgid_gpid < 0) goto invalid;
    if (man->fg_pgid_gpid > 0) {
        int foreground = -1;
        for (int i = 0; i < g_nrprocs; ++i)
            if (g_rprocs[i].pgid == man->fg_pgid_gpid) {
                foreground = i;
                break;
            }
        if (foreground < 0) goto invalid;
    }
    free(relations);
    return 0;

invalid:
    free(relations);
    return -1;
}

// The requested policy, or -1 when the caller never asked for one (unset, or the DEFAULT wire value).
static int ckpt_recovery_policy_requested(void) {
    const char *value = hl_option_get("HL_CHECKPOINT_POLICY");
    if (value && value[0] > '0' && value[0] <= '3' && value[1] == 0) return value[0] - '0';
    return -1;
}

// No policy asked for means "restore whatever can be restored", so the default is the most permissive one.
static int ckpt_recovery_policy(void) {
    int requested = ckpt_recovery_policy_requested();
    return requested < 0 ? CKPT_RECOVERY_DISCARD_OPTIONAL : requested;
}

// Capture only relaxes for an explicitly permissive policy; the permissive restore default must not reach it.
static int ckpt_recovery_permissive_requested(void) {
    int requested = ckpt_recovery_policy_requested();
    return requested == CKPT_RECOVERY_RECONNECT || requested == CKPT_RECOVERY_DISCARD_OPTIONAL;
}

static int ckpt_recovery_policy_set(const char *value) {
    const char *encoded = NULL;
    if (!strcmp(value, "refuse"))
        encoded = "3";
    else if (!strcmp(value, "reconnect"))
        encoded = "1";
    else if (!strcmp(value, "discard-optional"))
        encoded = "2";
    else if (!strcmp(value, "default"))
        encoded = "0";
    if (!encoded) return -1;
    return hl_option_set("HL_CHECKPOINT_POLICY", encoded, 1);
}

static struct ckpt_proc *ckpt_proc_find(int gpid) {
    for (int i = 0; i < g_nrprocs; ++i)
        if (g_rprocs[i].gpid == gpid) return &g_rprocs[i];
    return NULL;
}

static void ckpt_process_stop(struct ckpt_proc *process, const char *reason) {
    if (!process || !process->viable) return;
    process->viable = 0;
    snprintf(process->reason, sizeof process->reason, "%s", reason);
    for (int changed = 1; changed;) {
        changed = 0;
        for (int i = 0; i < g_nrprocs; ++i) {
            struct ckpt_proc *child = &g_rprocs[i];
            struct ckpt_proc *parent = ckpt_proc_find(child->ppid);
            if (child->viable && parent && !parent->viable) {
                child->viable = 0;
                snprintf(child->reason, sizeof child->reason, "ancestor %d was stopped", parent->gpid);
                changed = 1;
            }
        }
    }
}

static void ckpt_json_string(FILE *file, const char *value, size_t capacity) {
    size_t length = value != NULL ? strnlen(value, capacity) : 0;
    fputc('"', file);
    for (size_t index = 0; index < length; ++index) {
        unsigned char byte = (unsigned char)value[index];
        if (byte == '"' || byte == '\\')
            fprintf(file, "\\%c", byte);
        else if (byte == '\n')
            fputs("\\n", file);
        else if (byte == '\r')
            fputs("\\r", file);
        else if (byte == '\t')
            fputs("\\t", file);
        else if (byte < 0x20)
            fprintf(file, "\\u%04x", byte);
        else
            fputc(byte, file);
    }
    fputc('"', file);
}

static int ckpt_recovery_report_queue(FILE *report, const struct ckpt_proc *process,
                                      const struct ckpt_fd *socket_record) {
    char path[1400];
    snprintf(path, sizeof path, "%s", socket_record->path);
    FILE *queue = ckpt_source_fopen(path);
    if (!queue) return -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(queue, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC) {
        ckpt_source_fclose(queue);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t received = fread(&frame, 1, sizeof frame, queue);
        if (received == 0 && feof(queue)) break;
        if (received != sizeof frame || frame.size > (1u << 20) || frame.rights_count > 253) {
            ckpt_source_fclose(queue);
            return -1;
        }
        unsigned char payload[4096];
        uint32_t remaining = frame.size;
        while (remaining != 0) {
            size_t chunk = remaining < sizeof payload ? remaining : sizeof payload;
            if (ckpt_rd_all(queue, payload, chunk) != 0) {
                ckpt_source_fclose(queue);
                return -1;
            }
            remaining -= (uint32_t)chunk;
        }
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            struct ckpt_fd right;
            if (ckpt_rd_fd(queue, &right) != 0) {
                ckpt_source_fclose(queue);
                return -1;
            }
            const char *outcome = !process->viable                                       ? "stopped"
                                  : (right.kind == CKF_FILE || right.kind == CKF_DEVICE) ? "reconnected"
                                                                                         : "reconstructed";
            fprintf(
                report,
                "{\"type\":\"resource\",\"gpid\":%d,\"fd\":-1,\"kind\":%d,\"queued\":true,\"outcome\":\"%s\",\"path\":",
                process->gpid, right.kind, outcome);
            if (right.kind == CKF_FILE || right.kind == CKF_DEVICE)
                ckpt_json_string(report, right.path, sizeof right.path);
            else
                fputs("\"\"", report);
            fputs("}\n", report);
        }
    }
    ckpt_source_fclose(queue);
    return 0;
}

// The restore-side recovery journal. It is the one thing restore WRITES, and it is written back into the
// image next to what it describes: assembled in memory, then handed to the store as the object
// "RECOVERY.jsonl" -- all at once, or not at all.
static int ckpt_recovery_report(int policy) {
    char *buffer = NULL;
    size_t buffered = 0;
    FILE *file = tmpfile();
    if (!file) return -1;
    fprintf(file, "{\"type\":\"summary\",\"format\":1,\"policy\":%d,\"processes\":%d}\n", policy, g_nrprocs);
    for (int i = 0; i < g_nrprocs; ++i) {
        struct ckpt_proc *process = &g_rprocs[i];
        fprintf(file, "{\"type\":\"process\",\"gpid\":%d,\"ppid\":%d,\"outcome\":\"%s\",\"reason\":", process->gpid,
                process->ppid, process->viable ? "restored" : "stopped");
        ckpt_json_string(file, process->reason, sizeof process->reason);
        fputs("}\n", file);
        char fd_path[1300];
        snprintf(fd_path, sizeof fd_path, "proc.%d/fds", process->gpid);
        FILE *fds = ckpt_source_fopen(fd_path);
        if (fds) {
            struct ckpt_fd record;
            while (ckpt_rd_fd(fds, &record) == 0) {
                const char *outcome = "reconstructed";
                if (!process->viable)
                    outcome = "stopped";
                else if (record.kind == CKF_FILE || record.kind == CKF_TTY || record.kind == CKF_DEVICE ||
                         record.kind == CKF_SOCKET)
                    outcome = "reconnected";
                fprintf(file, "{\"type\":\"resource\",\"gpid\":%d,\"fd\":%d,\"kind\":%d,\"outcome\":\"%s\",\"path\":",
                        process->gpid, record.gfd, record.kind, outcome);
                if (record.kind == CKF_FILE || record.kind == CKF_DEVICE)
                    ckpt_json_string(file, record.path, sizeof record.path);
                else
                    fputs("\"\"", file);
                fputs("}\n", file);
                if (record.kind == CKF_SOCKETPAIR && ckpt_recovery_report_queue(file, process, &record) != 0) {
                    ckpt_source_fclose(fds);
                    fclose(file);
                    free(buffer);
                    return -1;
                }
            }
            ckpt_source_fclose(fds);
        }
    }
    {
        long length = -1;
        int failed = fflush(file) != 0 || (length = ftell(file)) < 0 || fseek(file, 0, SEEK_SET) != 0;
        if (!failed) {
            buffered = (size_t)length;
            buffer = malloc(buffered == 0 ? 1 : buffered);
            failed = buffer == NULL || (buffered != 0 && fread(buffer, 1, buffered, file) != buffered);
        }
        if (fclose(file) != 0) failed = 1;
        if (!failed) {
            struct ckpt_sink_stream *object = NULL;
            failed = ckpt_sink_stream_begin(NULL, NULL, "RECOVERY.jsonl", 0, &object) != 0 ||
                     ckpt_sink_stream_write(object, buffer, buffered) != 0 || ckpt_sink_stream_finish(object) != 0;
        }
        free(buffer);
        return failed ? -1 : 0;
    }
}

static int ckpt_validate_process_image(const struct ckpt_proc *process, struct ckpt_meta *meta) {
    char procdir[1300], path[1400];
    snprintf(procdir, sizeof procdir, "proc.%d", process->gpid);
    if (ckpt_read_meta_dir(procdir, meta) != 0 || meta->self_gpid != process->gpid ||
        meta->ppid_gpid != process->ppid || meta->pagesz == 0 || meta->pagesz > UINT64_C(1048576) ||
        (meta->pagesz & (meta->pagesz - 1)) != 0 || meta->n_regions > UINT64_C(1048576) || meta->n_fds > HL_NFD)
        return -1;

    snprintf(path, sizeof path, "%s/pages", procdir);
    FILE *pages = ckpt_source_fopen(path);
    if (!pages) return -1;
    for (uint64_t index = 0; index < meta->n_regions; ++index) {
        struct ckpt_region region;
        if (ckpt_read_region(pages, &region) != 0 || region.addr == 0 || region.len == 0 ||
            region.addr > UINT64_MAX - region.len || region.glen > region.len ||
            region.npages > (region.len - 1) / meta->pagesz + 1 || !ckpt_region_valid(&region) ||
            (region.backing_object != 0 && (region.backing_offset > UINT64_MAX - region.glen ||
                                            region.backing_offset + region.glen > (uint64_t)INT64_MAX)) ||
            (region.logical && (region.backing_object == 0 || !region.backing_shared || region.backing_emulated ||
                                region.glen != region.len))) {
            ckpt_source_fclose(pages);
            return -1;
        }
        for (uint64_t page = 0; page < region.npages; ++page) {
            uint64_t address;
            if (ckpt_rd_all(pages, &address, sizeof address) != 0 || address < region.addr ||
                address >= region.addr + region.len || (address - region.addr) % meta->pagesz != 0) {
                ckpt_source_fclose(pages);
                return -1;
            }
            uint64_t remaining = region.addr + region.len - address;
            size_t size = (size_t)(remaining < meta->pagesz ? remaining : meta->pagesz);
            unsigned char buffer[4096];
            while (size != 0) {
                size_t chunk = size < sizeof buffer ? size : sizeof buffer;
                if (ckpt_rd_all(pages, buffer, chunk) != 0) {
                    ckpt_source_fclose(pages);
                    return -1;
                }
                size -= chunk;
            }
        }
    }
    int trailing = fgetc(pages);
    ckpt_source_fclose(pages);
    if (trailing != EOF) return -1;

    struct cpu *images = NULL, leader;
    if (ckpt_restore_cpu_dir(procdir, meta, &images) != 0 ||
        ckpt_restore_leader(images, meta->n_threads, &leader) != 0) {
        free(images);
        return -1;
    }
    free(images);

    snprintf(path, sizeof path, "%s/fds", procdir);
    int64_t fd_bytes = ckpt_source_object_size(path);
    size_t fd_size, fd_count;
    if (ckpt_record_object_size(fd_bytes, sizeof(struct ckpt_fd), HL_NFD, &fd_size, &fd_count) != 0 ||
        fd_count != meta->n_fds || fd_size != fd_count * sizeof(struct ckpt_fd))
        return -1;
    FILE *fds = ckpt_source_fopen(path);
    if (!fds) return -1;
    uint64_t descriptors = 0;
    struct ckpt_fd record;
    while (ckpt_rd_fd(fds, &record) == 0) {
        if (record.gfd < 0 || record.gfd >= HL_NFD || record.kind < CKF_TTY || record.kind > CKF_DEVICE) {
            ckpt_source_fclose(fds);
            return -1;
        }
        descriptors++;
    }
    int valid_fds = feof(fds) && descriptors == meta->n_fds;
    ckpt_source_fclose(fds);
    return valid_fds ? 0 : -1;
}

static int ckpt_external_unavailable(const struct ckpt_fd *record) {
    if (record->kind != CKF_FILE && record->kind != CKF_DEVICE) return 0;
    struct stat status;
    if (stat(record->path, &status) != 0) return 1;
    if (record->kind == CKF_DEVICE) {
        if (!S_ISCHR(status.st_mode) && !S_ISBLK(status.st_mode)) return 1;
    } else {
        int directory = (record->auxiliary & CKFA_DIRECTORY) != 0;
        if (directory ? !S_ISDIR(status.st_mode) : !S_ISREG(status.st_mode)) return 1;
    }
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int probe = open(record->path, flags);
    if (probe < 0) return 1;
    close(probe);
    return 0;
}

static int ckpt_preflight_socket_queue(const struct ckpt_fd *socket_record, struct ckpt_fd *unavailable) {
    char path[1400];
    snprintf(path, sizeof path, "%s", socket_record->path);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(file, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC) {
        ckpt_source_fclose(file);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t received = fread(&frame, 1, sizeof frame, file);
        if (received == 0 && feof(file)) break;
        if (received != sizeof frame || frame.size > (1u << 20) || frame.rights_count > 253) {
            ckpt_source_fclose(file);
            return -1;
        }
        unsigned char payload[4096];
        uint32_t remaining = frame.size;
        while (remaining != 0) {
            size_t chunk = remaining < sizeof payload ? remaining : sizeof payload;
            if (ckpt_rd_all(file, payload, chunk) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            remaining -= (uint32_t)chunk;
        }
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            struct ckpt_fd right;
            if (ckpt_rd_fd(file, &right) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            if (ckpt_external_unavailable(&right)) {
                *unavailable = right;
                ckpt_source_fclose(file);
                return 1;
            }
        }
    }
    ckpt_source_fclose(file);
    return 0;
}

static int ckpt_restore_preflight(int policy) {
    for (int i = 0; i < g_nrprocs; ++i) {
        struct ckpt_proc *process = &g_rprocs[i];
        struct ckpt_meta meta;
        if (ckpt_validate_process_image(process, &meta) != 0) {
            ckpt_process_stop(process, "checkpoint process image is invalid");
            continue;
        }
        char path[1300];
        snprintf(path, sizeof path, "proc.%d/fds", process->gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) {
            ckpt_process_stop(process, "descriptor image is missing");
            continue;
        }
        struct ckpt_fd record;
        while (process->viable && ckpt_rd_fd(file, &record) == 0) {
            if (record.kind == CKF_FILE || record.kind == CKF_DEVICE) {
                if (ckpt_external_unavailable(&record)) {
                    char reason[192];
                    snprintf(reason, sizeof reason, "required external %s is unavailable: %.130s",
                             record.kind == CKF_DEVICE ? "device" : "path", record.path);
                    ckpt_process_stop(process, reason);
                }
            }
            if (process->viable && record.kind == CKF_SOCKETPAIR) {
                struct ckpt_fd unavailable;
                int queue = ckpt_preflight_socket_queue(&record, &unavailable);
                if (queue < 0)
                    ckpt_process_stop(process, "socket queue image is corrupt");
                else if (queue > 0) {
                    char reason[192];
                    snprintf(reason, sizeof reason, "queued external %s is unavailable: %.130s",
                             unavailable.kind == CKF_DEVICE ? "device" : "path", unavailable.path);
                    ckpt_process_stop(process, reason);
                }
            }
        }
        if (!feof(file) && process->viable) ckpt_process_stop(process, "descriptor image is corrupt");
        ckpt_source_fclose(file);
    }
    // A permissive recovery may remove members of a group or session. A process group remains reconstructible
    // while any same-session member survives (restore elects a replacement host leader), but a session cannot
    // exist without its saved leader. Apply this after resource preflight and before any restore-side fork.
    for (int changed = 1; changed;) {
        changed = 0;
        for (int i = 0; i < g_nrprocs; ++i) {
            struct ckpt_proc *process = &g_rprocs[i];
            if (!process->viable) continue;
            struct ckpt_proc *session = ckpt_proc_find(process->sid);
            int group_viable = 0;
            for (int j = 0; j < g_nrprocs; ++j)
                group_viable |= g_rprocs[j].viable && g_rprocs[j].pgid == process->pgid &&
                                g_rprocs[j].sid == process->sid;
            if (!group_viable) {
                ckpt_process_stop(process, "process group is not recoverable");
                changed = 1;
            } else if (!session || !session->viable) {
                ckpt_process_stop(process, "session leader is not recoverable");
                changed = 1;
            }
        }
    }
    struct ckpt_proc *root = ckpt_proc_find(1);
    int stopped = 0;
    for (int i = 0; i < g_nrprocs; ++i)
        stopped += !g_rprocs[i].viable;
    if (ckpt_recovery_report(policy) != 0) {
        fprintf(stderr, "[restore] cannot publish recovery report\n");
        return -1;
    }
    if (stopped && policy == CKPT_RECOVERY_REFUSE) {
        fprintf(stderr, "[restore] preflight refused %d process(es); see RECOVERY.jsonl\n", stopped);
        return -1;
    }
    if (!root || !root->viable) {
        // Name the reason here too: RECOVERY.jsonl goes to the embedder's store, which a caller watching the
        // machine's own terminal cannot read, and "not recoverable" alone says nothing about what was missing.
        fprintf(stderr, "[restore] container init is not recoverable: %s (see RECOVERY.jsonl)\n",
                root ? root->reason : "no init image");
        return -1;
    }
    return 0;
}
