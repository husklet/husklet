static int ckpt_restore_fds_dir(const char *procdir) {
    struct ckpt_fd *records = NULL;
    static unsigned char desired_pipe[HL_NFD];
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/fds", procdir);
    FILE *f = ckpt_source_fopen(pf);
    if (!f) return 0;
    // The object's length, asked of the source. fstat(fileno()) does not work here: a streamed object is a
    // memory stream with no descriptor behind it.
    int64_t record_bytes = ckpt_source_object_size(pf);
    if (record_bytes < 0 || record_bytes % (int64_t)sizeof *records != 0 ||
        (uint64_t)record_bytes / sizeof *records > HL_NFD) {
        ckpt_source_fclose(f);
        return -1;
    }
    int count = (int)((uint64_t)record_bytes / sizeof *records);
    records = calloc((size_t)(count ? count : 1), sizeof *records);
    if (records == NULL || (count != 0 && fread(records, sizeof *records, (size_t)count, f) != (size_t)count) ||
        fgetc(f) != EOF) {
        free(records);
        ckpt_source_fclose(f);
        return -1;
    }
    ckpt_source_fclose(f);
    ckpt_fd_terminate_all(records, (size_t)count);
    /* A restored child inherits its restorer parent's public eventfd descriptors and process-local routing
     * tables. They are not part of the child's saved fd table merely because the parent owned them. Drop
     * those public copies without closing the inherited hidden writer seeds, then rebuild exactly this
     * process's aliases below. The counter arena is shared across the whole restored tree and is deliberately
     * not modified here. */
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
    for (int i = 0; i < count; i++) {
        struct ckpt_fd r = records[i];
        /* Embedded/Rust launches seed stdio in the typed hl_linux_abi table.
         * A checkpoint may contain a later native dup2 replacement at the same
         * guest number.  Retire the fresh-launch typed binding before installing
         * serialized native state; otherwise typed syscall dispatch continues to
         * route to the launch object (typically /dev/null) and masks the restored
         * descriptor. */
        if ((r.kind == CKF_FILE || r.kind == CKF_PIPE || r.kind == CKF_BLOB || r.kind == CKF_MEMFD ||
             r.kind == CKF_EVENTFD || r.kind == CKF_TIMERFD || r.kind == CKF_INOTIFY || r.kind == CKF_EPOLL ||
             r.kind == CKF_SOCKETPAIR || r.kind == CKF_SOCKET || r.kind == CKF_SIGNALFD) &&
            g_linux_box != NULL &&
            hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)r.gfd, &(hl_linux_fd_snapshot){0}) == HL_STATUS_OK) {
            (void)hl_linux_close(g_linux_box, (hl_linux_fd)r.gfd);
            proc_fdvis_close(r.gfd);
            (void)close(r.gfd); /* retire the legacy same-number native shadow */
        }
        if (r.kind == CKF_EPOLL) continue;
        if (r.kind == CKF_SOCKETPAIR) {
            struct ckpt_restore_socket_endpoint *endpoint = ckpt_restore_socket_find(r.object_id);
            if (endpoint == NULL || endpoint->fd < 0 || r.gfd < 0 || r.gfd >= HL_NFD || dup2(endpoint->fd, r.gfd) < 0)
                return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            g_sock_object[r.gfd] = r.object_id;
            g_sock_peer_object[r.gfd] = r.auxiliary;
            const struct ckpt_socket_state *state = &endpoint->state;
            g_sock_fam[r.gfd] = endpoint->state_loaded ? (uint16_t)state->guest_family : AF_UNIX;
            g_sock_stream[r.gfd] = r.offset == SOCK_STREAM;
            g_sock_dgram[r.gfd] = r.offset == SOCK_DGRAM || r.offset == SOCK_SEQPACKET;
            g_sock_seqpacket[r.gfd] = r.offset == SOCK_SEQPACKET;
            g_sock_conn[r.gfd] = 1;
            if (endpoint->state_loaded) {
                g_tcp_listen[r.gfd] = state->listening != 0;
                g_sock_backlog[r.gfd] = state->backlog;
                g_lo_port[r.gfd] = state->lo_port;
                g_lo_v6[r.gfd] = state->lo_v6;
                g_lo_v6only[r.gfd] = state->lo_v6only;
                g_br_port[r.gfd] = state->br_port;
                g_br_ip[r.gfd] = state->br_ip;
                g_br_interface[r.gfd] = state->br_interface;
                g_tcp_lport[r.gfd] = state->tcp_local_port;
                g_tcp_laddr[r.gfd] = state->tcp_local_address;
                g_tcp_l6[r.gfd] = state->tcp_local_v6;
                memcpy(g_tcp_laddr6[r.gfd], state->tcp_local_address_v6, sizeof state->tcp_local_address_v6);
                g_so_error[r.gfd] = state->pending_error;
                g_so_reuseport[r.gfd] = state->shadow_reuse_port;
                memcpy(g_tcp_optval[r.gfd], state->tcp_option_value, sizeof state->tcp_option_value);
                memcpy(g_tcp_optset[r.gfd], state->tcp_option_set, sizeof state->tcp_option_set);
                memcpy(g_ipopt_val[r.gfd], state->ip_option_value, sizeof state->ip_option_value);
                memcpy(g_ipopt_set[r.gfd], state->ip_option_set, sizeof state->ip_option_set);
            }
            for (int peer_index = 0; peer_index < count; ++peer_index)
                if (records[peer_index].kind == CKF_SOCKETPAIR && records[peer_index].object_id == r.auxiliary) {
                    g_sock_pair_peer[r.gfd] = records[peer_index].gfd + 1;
                    break;
                }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_SOCKET) {
            struct ckpt_restore_socket *saved = ckpt_restore_socket_state_find(r.object_id);
            if (saved == NULL || saved->fd < 0 || r.gfd < 0 || r.gfd >= HL_NFD || dup2(saved->fd, r.gfd) < 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            const struct ckpt_socket_state *state = &saved->state;
            g_sock_object[r.gfd] = r.object_id;
            g_sock_peer_object[r.gfd] = 0;
            g_sock_fam[r.gfd] = (uint16_t)state->guest_family;
            g_sock_stream[r.gfd] = state->type == SOCK_STREAM;
            g_sock_dgram[r.gfd] = state->type == SOCK_DGRAM;
            g_sock_conn[r.gfd] = 0;
            g_tcp_listen[r.gfd] = state->listening != 0;
            g_sock_backlog[r.gfd] = state->backlog;
            g_lo_port[r.gfd] = state->lo_port;
            g_lo_v6[r.gfd] = state->lo_v6;
            g_lo_v6only[r.gfd] = state->lo_v6only;
            g_br_port[r.gfd] = state->br_port;
            g_br_ip[r.gfd] = state->br_ip;
            g_br_interface[r.gfd] = state->br_interface;
            g_tcp_lport[r.gfd] = state->tcp_local_port;
            g_tcp_laddr[r.gfd] = state->tcp_local_address;
            g_tcp_l6[r.gfd] = state->tcp_local_v6;
            memcpy(g_tcp_laddr6[r.gfd], state->tcp_local_address_v6, sizeof state->tcp_local_address_v6);
            g_so_error[r.gfd] = state->pending_error;
            g_so_reuseport[r.gfd] = state->shadow_reuse_port;
            memcpy(g_tcp_optval[r.gfd], state->tcp_option_value, sizeof state->tcp_option_value);
            memcpy(g_tcp_optset[r.gfd], state->tcp_option_set, sizeof state->tcp_option_set);
            memcpy(g_ipopt_val[r.gfd], state->ip_option_value, sizeof state->ip_option_value);
            memcpy(g_ipopt_set[r.gfd], state->ip_option_set, sizeof state->ip_option_set);
            g_udp_local_port[r.gfd] = (uint16_t)state->udp_local_port;
            g_udp_peer_port[r.gfd] = (uint16_t)state->udp_peer_port;
            g_udp_local_ip[r.gfd] = state->udp_local_ip;
            g_udp_peer_ip[r.gfd] = state->udp_peer_ip;
            g_udp_local_v6[r.gfd] = state->udp_local_v6;
            g_udp_peer_v6[r.gfd] = state->udp_peer_v6;
            g_udp_local_interface[r.gfd] = state->udp_local_interface;
            g_udp_peer_interface[r.gfd] = state->udp_peer_interface;
            if (state->udp_local_port != 0 && state->host_family == AF_UNIX) {
                int source = -1;
                for (int prior = 0; prior < i; ++prior)
                    if (records[prior].kind == CKF_SOCKET && records[prior].object_id == r.object_id) {
                        source = records[prior].gfd;
                        break;
                    }
                if (source >= 0) {
                    udp_ref_dup(r.gfd, source);
                } else {
                    const struct sockaddr_un *address = (const void *)&state->local;
                    if (udp_ref_create(r.gfd, address->sun_path) != 0) return -1;
                }
            }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_SIGNALFD) {
            struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(r.object_id);
            if (object == NULL || dup2(object->reader, r.gfd) < 0) return -1;
            int source = -1;
            for (int prior = 0; prior < i; ++prior)
                if (records[prior].kind == CKF_SIGNALFD && records[prior].object_id == r.object_id) {
                    source = records[prior].gfd;
                    break;
                }
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
                g_sfd[slot].rd = r.gfd;
                g_sfd[slot].wr = writer;
                g_sfd[slot].mask = object->mask;
            }
            g_sigfd_slot[r.gfd] = (uint8_t)(slot + 1);
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                return -1;
            g_ofd_id[r.gfd] = r.ofd_id;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_EVENTFD) {
            struct ckpt_restore_eventfd *object = ckpt_restore_eventfd_find(r.object_id);
            if (!object || object->slot < 0 || object->slot >= HL_NFD || r.gfd < 0 || r.gfd >= HL_NFD ||
                dup2(object->reader, r.gfd) < 0)
                return -1;
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, live_flags | O_NONBLOCK) != 0) return -1;
            g_eventfd_peer[r.gfd] = object->writer + 1;
            g_eventfd_cslot[r.gfd] = object->slot + 1;
            g_eventfd_sema[r.gfd] = object->semaphore;
            eventfd_guest_nb_set(r.gfd, object->guest_nonblock);
            g_eventfd_refs[object->slot]++;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_TIMERFD) {
            int clock_id = 0, first = 0;
            unsigned long long pending_value = 0;
            long long captured_ns = 0;
            if (sscanf(r.path, "%d %llu %u %lld", &clock_id, &pending_value, (unsigned *)&first, &captured_ns) != 4) {
                fprintf(stderr, "[restore] timerfd %d has invalid metadata '%s'\n", r.gfd, r.path);
                return -1;
            }
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].kind == CKF_TIMERFD && records[j].object_id == r.object_id) {
                    source = records[j].gfd;
                    break;
                }
            struct ckpt_restore_timerfd *restored = ckpt_restore_timerfd_find(r.object_id);
            if (!restored || !restored->state) return -1;
            int timer = source >= 0 ? dup(source) : kqueue();
            if (timer < 0) {
                fprintf(stderr, "[restore] timerfd %d create/dup failed: %s\n", r.gfd, strerror(errno));
                return -1;
            }
            if (source >= 0) hl_native_kqueue_duplicate(source, timer);
            if (timer != r.gfd) {
                if (dup2(timer, r.gfd) < 0) {
                    fprintf(stderr, "[restore] timerfd %d target dup failed: %s\n", r.gfd, strerror(errno));
                    close(timer);
                    return -1;
                }
                // dup2 moved the timer onto the guest's fd number; a shim kqueue keys its queue by descriptor
                // NUMBER, so it must be told or the arming kevent() below is EBADF.
                hl_native_kqueue_relocate(timer, r.gfd);
                close(timer);
            }
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) {
                fprintf(stderr, "[restore] timerfd %d flag restore failed: %s\n", r.gfd, strerror(errno));
                return -1;
            }
            int slot = source >= 0 ? timerfd_slot(source) : r.gfd;
            if (slot < 0 || slot >= HL_NFD) {
                fprintf(stderr, "[restore] timerfd %d invalid canonical slot %d\n", r.gfd, slot);
                return -1;
            }
            g_timerfd[r.gfd] = 1;
            g_epoll_family_seen = 1;
            g_tfd_cslot[r.gfd] = slot + 1;
            g_tfd_object[r.gfd] = r.object_id;
            g_tfd_clock[r.gfd] = clock_id;
            g_tfd_nb[r.gfd] = (r.flags & O_NONBLOCK) != 0;
            g_tfd_shared[r.gfd] = restored->state;
            g_tfd_refs[slot]++;
            if (source < 0) {
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
                if (pending != 0 || next > now_ns) {
                    struct kevent event;
                    int64_t delay = pending != 0 ? 1 : next - now_ns;
                    EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
                    if (kevent(r.gfd, &event, 1, NULL, 0, NULL) < 0) {
                        fprintf(stderr, "[restore] timerfd %d arm failed: %s\n", r.gfd, strerror(errno));
                        return -1;
                    }
                }
            } else {
                g_tfd_deadline[r.gfd] = g_tfd_deadline[slot];
                g_tfd_interval[r.gfd] = g_tfd_interval[slot];
                g_tfd_first_oneshot[r.gfd] = g_tfd_first_oneshot[slot];
            }
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) {
                fprintf(stderr, "[restore] timerfd %d publication failed\n", r.gfd);
                return -1;
            }
            continue;
        }
        if (r.kind == CKF_INOTIFY) {
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].kind == CKF_INOTIFY && records[j].object_id == r.object_id) {
                    source = records[j].gfd;
                    break;
                }
            if (r.path[0] != 0) {
                if (source >= 0) {
                    if (dup2(source, r.gfd) < 0 ||
                        hl_linux_dup3(g_linux_box, (hl_linux_fd)source, (hl_linux_fd)r.gfd,
                                      (r.descriptor_flags & FD_CLOEXEC) ? HL_LINUX_O_CLOEXEC : 0) < 0)
                        return -1;
                } else {
                    char image_path[1400];
                    snprintf(image_path, sizeof image_path, "%s/%s", procdir, r.path);
                    int64_t stored = ckpt_source_object_size(image_path);
                    if (stored <= 0) {
                        fprintf(stderr, "[restore] inotify %d image %s is invalid: %s\n", r.gfd, image_path,
                                strerror(errno));
                        return -1;
                    }
                    size_t image_size = (size_t)stored;
                    void *image = malloc(image_size);
                    if (image == NULL || ckpt_source_load(image_path, image, image_size) != 0) {
                        fprintf(stderr, "[restore] inotify %d cannot load %s\n", r.gfd, image_path);
                        free(image);
                        return -1;
                    }
                    int shadow =
                        open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | ((r.descriptor_flags & FD_CLOEXEC) ? O_CLOEXEC : 0));
                    if (shadow < 0 || (shadow != r.gfd && dup2(shadow, r.gfd) < 0)) {
                        fprintf(stderr, "[restore] inotify %d cannot reserve native shadow: %s\n", r.gfd,
                                strerror(errno));
                        if (shadow >= 0) close(shadow);
                        free(image);
                        return -1;
                    }
                    if (shadow != r.gfd) close(shadow);
                    void *provider = bound_inotify_provider_create(g_host_services);
                    int64_t imported = provider == NULL
                                           ? -HL_LINUX_ENOMEM
                                           : hl_linux_inotify_import_at(
                                                 g_linux_box, (hl_linux_fd)r.gfd, &bound_inotify_ops, provider,
                                                 (uint32_t)r.descriptor_flags, (uint32_t)r.flags, image, image_size);
                    free(image);
                    if (imported < 0) {
                        fprintf(stderr, "[restore] inotify %d typed import failed: %lld\n", r.gfd, (long long)imported);
                        close(r.gfd);
                        return -1;
                    }
                }
                if (proc_fdvis_publish(r.gfd, HL_HOST_FD_OTHER, 0, 0) != 0) {
                    fprintf(stderr, "[restore] inotify %d fd visibility publication failed\n", r.gfd);
                    return -1;
                }
                continue;
            }
#if defined(__linux__)
            int instance =
                source >= 0 ? dup(source)
                            : inotify_init1((r.flags & O_NONBLOCK) | ((r.descriptor_flags & FD_CLOEXEC) ? 0x80000 : 0));
#else
            int instance = source >= 0 ? dup(source) : kqueue();
#endif
            if (instance < 0) return -1;
            if (source >= 0) hl_native_kqueue_duplicate(source, instance);
            if (instance != r.gfd) {
                if (dup2(instance, r.gfd) < 0) {
                    close(instance);
                    return -1;
                }
                if (source >= 0) hl_native_kqueue_duplicate(source, r.gfd);
                close(instance);
            }
            if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
            g_inotify[r.gfd] = 1;
            g_inotify_nb[r.gfd] = (r.flags & O_NONBLOCK) != 0;
            g_inotify_object[r.gfd] = r.object_id;
            g_epoll_family_seen = 1;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            continue;
        }
        if (r.ofd_id != 0 && r.kind != CKF_PIPE && r.kind != CKF_TTY) {
            int source = -1;
            for (int j = 0; j < i; j++)
                if (records[j].ofd_id == r.ofd_id) {
                    source = records[j].gfd;
                    break;
                }
            if (source >= 0) {
                if (dup2(source, r.gfd) < 0) return -1;
                if (r.descriptor_flags & FD_CLOEXEC)
                    fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                else
                    fcntl(r.gfd, F_SETFD, 0);
                if (r.kind == CKF_MEMFD && r.gfd >= 0 && r.gfd < HL_NFD) {
                    g_memfd_is[r.gfd] = 1;
                    g_memfd_seal[r.gfd] = (int)r.auxiliary;
                    memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
                }
                if (r.gfd >= 0 && r.gfd < HL_NFD) g_ofd_id[r.gfd] = r.ofd_id;
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
                continue;
            }
        }
        if ((r.kind == CKF_FILE || r.kind == CKF_BLOB || r.kind == CKF_MEMFD) && r.ofd_id != 0) {
            struct ckpt_restore_right *right = ckpt_restore_right_find(r.ofd_id);
            if (right != NULL) {
                if (right->object_id != r.object_id || dup2(right->fd, r.gfd) < 0 ||
                    fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0)
                    return -1;
                g_ofd_id[r.gfd] = r.ofd_id;
                if (r.kind == CKF_MEMFD) {
                    g_memfd_is[r.gfd] = 1;
                    g_memfd_seal[r.gfd] = (int)r.auxiliary;
                    memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
                }
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
                continue;
            }
        }
        if (r.kind == CKF_TTY) {
            // 0/1/2 are inherited from the launcher pty. But an interactive shell also keeps a HIGH-fd dup of
            // the controlling terminal for job control (bash uses fd 255); the launcher doesn't provide it, so
            // recreate it by duping the ctty onto that number -- else the shell's tcsetattr/tcgetattr on it
            // fails EBADF after restore ("tcsetattr: Bad file descriptor" when a foreground job finishes).
            if (r.gfd > 2) {
                int ct = ckpt_ctty_open();
                if (ct >= 0 && r.gfd != ct && dup2(ct, r.gfd) >= 0 && (r.flags & FD_CLOEXEC))
                    fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                ckpt_ctty_close(ct);
            }
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            continue;
        }
        if (r.kind == CKF_PIPE) {
            uint64_t identity = (uint64_t)r.offset;
            struct ckpt_restore_pipe *pipe = ckpt_restore_pipe_find(identity);
            int source = ((r.flags & O_ACCMODE) == O_WRONLY) ? (pipe ? pipe->writer : -1) : (pipe ? pipe->reader : -1);
            if (source < 0 || dup2(source, r.gfd) < 0) return -1;
            int live_flags = fcntl(r.gfd, F_GETFL);
            if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0)
                return -1;
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            g_pipe_identity[r.gfd] = identity;
            g_pipesz[r.gfd] = pipe->size;
            if (proc_fdvis_publish(r.gfd, HL_HOST_FD_PIPE, 1, identity) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_BLOB) {
            if (ckpt_restore_file_blob(procdir, &r) != 0) return -1;
            continue;
        }
        if (r.kind == CKF_MEMFD) {
            int seed = ckpt_restore_backing_find(r.object_id);
            if (seed >= 0) {
                if (dup2(seed, r.gfd) < 0) return -1;
                int live_flags = fcntl(r.gfd, F_GETFL);
                if (live_flags < 0 || fcntl(r.gfd, F_SETFL, (live_flags & ~O_NONBLOCK) | (r.flags & O_NONBLOCK)) != 0 ||
                    lseek(r.gfd, (off_t)r.offset, SEEK_SET) < 0)
                    return -1;
                if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
                if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
            } else if (ckpt_restore_file_blob(procdir, &r) != 0) {
                return -1;
            }
            if (r.gfd < 0 || r.gfd >= HL_NFD) return -1;
            g_memfd_is[r.gfd] = 1;
            g_memfd_seal[r.gfd] = (int)r.auxiliary;
            memfd_reg_set_fd(r.gfd, g_memfd_seal[r.gfd]);
            continue;
        }
        if (r.kind == CKF_FILE) {
            int flags = r.flags & ~(O_CREAT | O_EXCL | O_TRUNC);
            int hf = open(r.path, flags);
            if (hf < 0) {
                fprintf(stderr, "[restore] cannot reopen fd %d (%s): %s\n", r.gfd, r.path, strerror(errno));
                return -1;
            }
            if (hf != r.gfd) {
                dup2(hf, r.gfd);
                close(hf);
            }
            if (r.offset > 0) lseek(r.gfd, (off_t)r.offset, SEEK_SET);
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            if (r.gfd >= 0 && r.gfd < 1024 && path_copy(g_fdpath[r.gfd], sizeof g_fdpath[r.gfd], r.path) != 0)
                g_fdpath[r.gfd][0] = 0;
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
        }
        if (r.kind == CKF_DEVICE) {
            int flags = r.flags & ~(O_CREAT | O_EXCL | O_TRUNC);
            int host_fd = open(r.path, flags);
            if (host_fd < 0 || (host_fd != r.gfd && dup2(host_fd, r.gfd) < 0)) {
                if (host_fd >= 0) close(host_fd);
                return -1;
            }
            if (host_fd != r.gfd) close(host_fd);
            if (r.descriptor_flags & FD_CLOEXEC) fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
        }
    }
    for (int i = 0; i < count; ++i) {
        struct ckpt_fd r = records[i];
        if (r.kind != CKF_EPOLL) continue;
        int source = -1;
        for (int prior = 0; prior < i; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == r.object_id) {
                source = records[prior].gfd;
                break;
            }
        int instance = source >= 0 ? dup(source) : kqueue();
        if (instance < 0) return -1;
        if (source >= 0) hl_native_kqueue_duplicate(source, instance);
        if (instance != r.gfd) {
            if (dup2(instance, r.gfd) < 0) {
                close(instance);
                return -1;
            }
            // Same shim-kqueue descriptor-number identity as the timerfd path above.
            hl_native_kqueue_relocate(instance, r.gfd);
            close(instance);
        }
        if (fcntl(r.gfd, F_SETFD, (r.descriptor_flags & FD_CLOEXEC) ? FD_CLOEXEC : 0) != 0) return -1;
        g_ep_provider_generations[r.gfd] = ep_provider_next(g_ep_provider_generations[r.gfd]);
        g_epoll[r.gfd] = 1;
        g_ep_cslot[r.gfd] = (uint16_t)((source >= 0 ? epoll_slot(source) : r.gfd) + 1);
        g_ep_dupd[r.gfd] = source >= 0;
        if (source >= 0) {
            g_ep_dupd[source] = 1;
            ofd_link_dup(r.gfd, source);
        }
        g_epoll_family_seen = 1;
        ep_mem_clear(r.gfd);
        if (proc_fdvis_publish_native_fd(r.gfd) != 0) return -1;
    }
    for (int i = 0; i < count; ++i) {
        if (records[i].kind != CKF_EPOLL) continue;
        int duplicate = 0;
        for (int prior = 0; prior < i; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == records[i].object_id) duplicate = 1;
        if (!duplicate && ckpt_restore_epoll_watches(procdir, &records[i]) != 0) return -1;
    }
    int restored = ckpt_restore_inotify_sidecar(procdir);
    free(records);
    return restored;
}

static int ckpt_restore_cpu_dir(const char *procdir, const struct ckpt_meta *m, struct cpu **out) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/cpu", procdir);
    if (m->n_threads > SIZE_MAX / sizeof(struct cpu)) return -1;
    size_t bytes = (size_t)m->n_threads * sizeof(struct cpu);
    if (bytes > SIZE_MAX - sizeof(struct ckpt_cpu_header)) return -1;
    size_t file_bytes = sizeof(struct ckpt_cpu_header) + bytes;
    struct ckpt_cpu_header *cpu_file = malloc(file_bytes);
    if (!cpu_file || ckpt_source_load(pf, cpu_file, file_bytes) != 0) {
        free(cpu_file);
        fprintf(stderr, "[restore] cannot read cpu state\n");
        return -1;
    }
    if (cpu_file->magic != CKPT_CPU_MAGIC || cpu_file->version != m->version || cpu_file->arch != G_CKPT_ARCH ||
        cpu_file->count != m->n_threads || cpu_file->payload_size != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu image version/architecture/layout mismatch\n");
        free(cpu_file);
        return -1;
    }
    struct cpu *images = malloc(bytes);
    if (!images) {
        free(cpu_file);
        return -1;
    }
    memcpy(images, cpu_file + 1, bytes);
    free(cpu_file);
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

static int ckpt_validate_proc_tree(const struct ckpt_manifest *man) {
    if ((uint64_t)g_nrprocs != man->n_procs) return -1;
    int roots = 0;
    for (int i = 0; i < g_nrprocs; i++) {
        if (g_rprocs[i].version != man->version) return -1;
        if (g_rprocs[i].gpid == man->root_gpid) {
            if (g_rprocs[i].ppid != 0) return -1;
            roots++;
            continue;
        }
        int ancestor = g_rprocs[i].ppid;
        int reached_root = 0;
        for (int depth = 0; depth < g_nrprocs; depth++) {
            if (ancestor == man->root_gpid) {
                reached_root = 1;
                break;
            }
            int parent = -1;
            for (int j = 0; j < g_nrprocs; j++)
                if (g_rprocs[j].gpid == ancestor) {
                    parent = g_rprocs[j].ppid;
                    break;
                }
            if (parent <= 0) break;
            ancestor = parent;
        }
        if (!reached_root) return -1; // missing parent or detached cycle
    }
    return roots == 1 ? 0 : -1;
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

static void ckpt_json_string(FILE *file, const char *value) {
    fputc('"', file);
    for (const unsigned char *p = (const unsigned char *)(value ? value : ""); *p; ++p) {
        if (*p == '"' || *p == '\\')
            fprintf(file, "\\%c", *p);
        else if (*p == '\n')
            fputs("\\n", file);
        else if (*p == '\r')
            fputs("\\r", file);
        else if (*p == '\t')
            fputs("\\t", file);
        else if (*p < 0x20)
            fprintf(file, "\\u%04x", *p);
        else
            fputc(*p, file);
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
            ckpt_json_string(report, (right.kind == CKF_FILE || right.kind == CKF_DEVICE) ? right.path : "");
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
    FILE *file = open_memstream(&buffer, &buffered);
    if (!file) return -1;
    fprintf(file, "{\"type\":\"summary\",\"format\":1,\"policy\":%d,\"processes\":%d}\n", policy, g_nrprocs);
    for (int i = 0; i < g_nrprocs; ++i) {
        struct ckpt_proc *process = &g_rprocs[i];
        fprintf(file, "{\"type\":\"process\",\"gpid\":%d,\"ppid\":%d,\"outcome\":\"%s\",\"reason\":", process->gpid,
                process->ppid, process->viable ? "restored" : "stopped");
        ckpt_json_string(file, process->reason);
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
                ckpt_json_string(file, (record.kind == CKF_FILE || record.kind == CKF_DEVICE) ? record.path : "");
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
        int failed = fclose(file) != 0;
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
            region.npages > (region.len - 1) / meta->pagesz + 1 || region.format_version != CKPT_REGION_VERSION ||
            region.logical > 1 ||
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

