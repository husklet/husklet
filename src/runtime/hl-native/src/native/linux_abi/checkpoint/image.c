enum ckpt_fd_capture_result {
    CKPT_FD_CAPTURE_ERROR = -1,
    CKPT_FD_CAPTURE_NEXT = 0,
    CKPT_FD_CAPTURED = 1,
};

static int ckpt_fd_was_captured(const struct ckpt_fd *records, int count, int fd) {
    for (int prior = 0; prior < count; ++prior)
        if (records[prior].gfd == fd) return 1;
    return 0;
}

static int ckpt_capture_early_emulated_fd(struct ckpt_fd *records, int *count, int fd) {
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    const char *early_emulated = ckpt_guest_kernel_fd(fd);
    if (early_emulated && strcmp(early_emulated, "socket") == 0 && fd >= 0 && fd < HL_NFD && g_sock_object[fd] != 0) {
        if (g_sock_peer_object[fd] == 0) {
            r.kind = CKF_SOCKET;
            r.flags = fcntl(fd, F_GETFL);
            r.descriptor_flags = fcntl(fd, F_GETFD);
            r.object_id = g_sock_object[fd];
            r.ofd_id = r.object_id;
            snprintf(r.path, sizeof r.path, "socket-state.%016llx", (unsigned long long)r.object_id);
            if (r.flags < 0 || r.descriptor_flags < 0 || ckpt_capture_socket_state(fd, r.object_id, 1) != 0) return -1;
            records[(*count)++] = r;
            return CKPT_FD_CAPTURED;
        }
        int type = g_sock_seqpacket[fd] ? SOCK_SEQPACKET : g_sock_dgram[fd] ? SOCK_DGRAM : SOCK_STREAM;
        r.kind = CKF_SOCKETPAIR;
        r.flags = fcntl(fd, F_GETFL);
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = g_sock_object[fd];
        r.ofd_id = r.object_id;
        r.auxiliary = g_sock_peer_object[fd];
        r.offset = type;
        snprintf(r.path, sizeof r.path, "socket.%016llx", (unsigned long long)r.object_id);
        if (r.flags < 0 || r.descriptor_flags < 0 || r.auxiliary == 0 ||
            ckpt_capture_socket_state(fd, r.object_id, 0) != 0 ||
            ckpt_capture_socket_queue(fd, r.object_id, (uint32_t)type) != 0)
            return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (early_emulated && strcmp(early_emulated, "epoll") == 0) {
        r.kind = CKF_EPOLL;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = ckpt_epoll_identity(fd);
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        snprintf(r.path, sizeof r.path, "epoll.%016llx", (unsigned long long)r.object_id);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (early_emulated && strcmp(early_emulated, "inotify") == 0) {
        inotify_object_assign(fd);
        r.kind = CKF_INOTIFY;
        r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = g_inotify_object[fd];
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    return CKPT_FD_CAPTURE_NEXT;
}

static int ckpt_capture_typed_fd(struct ckpt_fd *records, int *count, int fd) {
    hl_linux_fd_snapshot snapshot;
    if (g_linux_box == NULL || hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) != HL_STATUS_OK)
        return CKPT_FD_CAPTURE_NEXT;
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    hl_host_file_metadata metadata;
    if (snapshot.kind == HL_LINUX_OBJECT_INOTIFY) {
        r.kind = CKF_INOTIFY;
        r.flags = (int32_t)snapshot.status_flags;
        r.descriptor_flags = (int32_t)snapshot.descriptor_flags;
        r.object_id = UINT64_C(0x9000000000000000) | (uint64_t)snapshot.ofd;
        r.ofd_id = r.object_id;
        snprintf(r.path, sizeof r.path, "inotify.%016llx", (unsigned long long)r.object_id);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (g_linux_box->host == NULL || g_linux_box->host->file == NULL || g_linux_box->host->file->metadata == NULL ||
        g_linux_box->host->file->metadata(g_linux_box->host->context, snapshot.host_handle, &metadata).status !=
            HL_STATUS_OK) {
        if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
            proc_fdvis_close(fd);
            return CKPT_FD_CAPTURED;
        }
        fprintf(stderr, "[ckpt] refuse: cannot inspect typed guest fd %d (inotify=%u owner=%d watch=%s)\n", fd,
                (unsigned)((fd >= 0 && fd < HL_NFD) ? g_inotify[fd] : 0),
                (fd >= 0 && fd < HL_NFD) ? g_inotify_owner[fd] : 0,
                (fd >= 0 && fd < HL_NFD && g_inotify_wpath[fd][0]) ? g_inotify_wpath[fd] : "-");
        return -1;
    }
    r.flags = (int32_t)snapshot.status_flags;
    r.descriptor_flags = (int32_t)snapshot.descriptor_flags;
    r.offset = (int64_t)snapshot.offset;
    r.object_id = metadata.stable_object ? metadata.stable_object : (uint64_t)snapshot.host_handle;
    r.ofd_id = UINT64_C(0x8000000000000000) | (uint64_t)snapshot.ofd;
    if (snapshot.kind == HL_LINUX_OBJECT_PIPE || metadata.type == HL_HOST_FILE_TYPE_FIFO) {
        fprintf(stderr, "[ckpt] refuse: guest fd %d is a pipe -- shared pipe restore is not yet supported\n", fd);
        return -1;
    }
    if (metadata.type == HL_HOST_FILE_TYPE_SOCKET) {
        fprintf(stderr, "[ckpt] refuse: guest fd %d is a socket -- socket restore is not yet supported\n", fd);
        return -1;
    }
    if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER || metadata.type == HL_HOST_FILE_TYPE_BLOCK) {
        char fp[512];
        hl_host_result device_path = g_linux_box->host->file->path(g_linux_box->host->context, snapshot.host_handle,
                                                                   (hl_host_bytes){fp, sizeof(fp) - 1});
        if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER && isatty(fd)) {
            r.kind = CKF_TTY;
            r.offset = 0;
        } else if (device_path.status == HL_STATUS_OK && device_path.value < sizeof fp) {
            fp[device_path.value] = 0;
            if (metadata.type == HL_HOST_FILE_TYPE_CHARACTER && ckpt_path_is_ctty(fp)) {
                r.kind = CKF_TTY;
                r.offset = 0;
            } else {
                r.kind = CKF_DEVICE;
                if (path_copy(r.path, sizeof r.path, fp) != 0) return -1;
            }
        } else {
            fprintf(stderr, "[ckpt] refuse: device fd %d has no recoverable path\n", fd);
            return -1;
        }
    } else if (metadata.type == HL_HOST_FILE_TYPE_REGULAR || metadata.type == HL_HOST_FILE_TYPE_DIRECTORY) {
        char fp[512];
        hl_host_result path = g_linux_box->host->file->path(g_linux_box->host->context, snapshot.host_handle,
                                                            (hl_host_bytes){fp, sizeof(fp) - 1});
        if (path.status != HL_STATUS_OK || path.value >= sizeof fp) {
            fprintf(stderr, "[ckpt] refuse: fd %d has no recoverable path\n", fd);
            return -1;
        }
        fp[path.value] = '\0';
        if (ckpt_normalize_reopen_path(fp) != 0 ||
            (metadata.type == HL_HOST_FILE_TYPE_REGULAR && access(fp, F_OK) != 0)) {
            if (metadata.type != HL_HOST_FILE_TYPE_REGULAR || ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0) {
                fprintf(stderr, "[ckpt] refuse: cannot persist deleted fd %d\n", fd);
                return -1;
            }
            r.kind = CKF_BLOB;
        } else {
            r.kind = CKF_FILE;
            if (metadata.type == HL_HOST_FILE_TYPE_DIRECTORY) r.auxiliary |= CKFA_DIRECTORY;
            if (path_copy(r.path, sizeof r.path, fp) != 0) return -1;
        }
    } else {
        fprintf(stderr, "[ckpt] refuse: typed guest fd %d has unsupported type %u\n", fd, metadata.type);
        return -1;
    }
    records[(*count)++] = r;
    return CKPT_FD_CAPTURED;
}

static int ckpt_capture_native_fd(struct ckpt_fd *records, int *count, const struct fdvis_view *view) {
    int fd = view->guest_fd;
    struct ckpt_fd r;
    memset(&r, 0, sizeof r);
    r.gfd = fd;
    hl_host_process_fd detail;
    char path[512];
    size_t path_size = 0;
    if (!hl_host_process_fd_read(getpid(), fd, &detail, path, sizeof(path) - 1, &path_size) ||
        (detail.flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0) {
        if (fcntl(fd, F_GETFD) < 0 && errno == EBADF) {
            proc_fdvis_close(fd);
            return CKPT_FD_CAPTURED;
        }
        fprintf(stderr, "[ckpt] refuse: cannot inspect native guest fd %d\n", fd);
        return -1;
    }
    const char *emulated = ckpt_guest_kernel_fd(fd);
    if (emulated && strcmp(emulated, "signalfd") == 0) {
        int slot = g_sigfd_slot[fd] - 1;
        uint64_t identity = ofd_identity_ensure(fd);
        if (slot < 0 || slot >= HL_SFD_MAX || !identity) return -1;
        r.kind = CKF_SIGNALFD;
        r.flags = fcntl(fd, F_GETFL);
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = identity;
        r.ofd_id = identity;
        r.auxiliary = g_sfd[slot].mask;
        snprintf(r.path, sizeof r.path, "signalfd.%016llx", (unsigned long long)identity);
        if (r.flags < 0 || r.descriptor_flags < 0 || ckpt_capture_signalfd(fd, identity) != 0) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (emulated && strcmp(emulated, "eventfd") == 0) {
        int slot = eventfd_counter_slot(fd);
        if (slot < 0 || slot >= HL_NFD || !g_eventfd_count) return -1;
        r.kind = CKF_EVENTFD;
        r.flags = eventfd_guest_nb(fd) ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        if (r.descriptor_flags < 0) return -1;
        r.object_id = UINT64_C(0x2000000000000000) | (uint64_t)(unsigned)(slot + 1);
        r.ofd_id = r.object_id;
        r.auxiliary = g_eventfd_count[slot];
        r.offset = g_eventfd_sema[fd] ? 1 : 0;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (emulated && strcmp(emulated, "timerfd") == 0) {
        int slot = timerfd_slot(fd);
        if (slot < 0 || slot >= HL_NFD) return -1;
        timerfd_object_assign(fd);
        r.kind = CKF_TIMERFD;
        r.flags = g_tfd_nb[fd] ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        if (r.flags < 0 || r.descriptor_flags < 0 || !g_tfd_object[fd]) return -1;
        r.object_id = g_tfd_object[fd];
        r.ofd_id = r.object_id;
        r.offset = g_tfd_deadline[slot];
        r.auxiliary = (uint64_t)g_tfd_interval[slot];
        uint64_t pending = g_tfd_pending[slot];
        int copied = 0;
        for (int prior = 0; prior < *count; prior++)
            if (records[prior].kind == CKF_TIMERFD && records[prior].object_id == r.object_id) {
                pending = strtoull(records[prior].path + strcspn(records[prior].path, " ") + 1, NULL, 10);
                copied = 1;
                break;
            }
        if (!copied) {
            struct kevent event;
            struct timespec zero = {0, 0};
            int ready = kevent(fd, NULL, 0, &event, 1, &zero);
            if (ready < 0) return -1;
            if (ready > 0) pending += g_tfd_interval[slot] == 0 ? 1 : (uint64_t)event.data;
        }
        struct timespec captured;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &captured);
        int64_t captured_ns = (int64_t)captured.tv_sec * 1000000000LL + captured.tv_nsec;
        snprintf(r.path, sizeof r.path, "%d %llu %u %lld", g_tfd_clock[slot], (unsigned long long)pending,
                 (unsigned)g_tfd_first_oneshot[slot], (long long)captured_ns);
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (emulated && strcmp(emulated, "inotify") == 0) {
        inotify_object_assign(fd);
        r.kind = CKF_INOTIFY;
        r.flags = g_inotify_nb[fd] ? O_NONBLOCK : 0;
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.object_id = g_inotify_object[fd];
        r.ofd_id = r.object_id;
        if (r.descriptor_flags < 0 || !r.object_id) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (detail.kind == HL_HOST_FD_PIPE) {
        int flags = fcntl(fd, F_GETFL);
        int descriptor_flags = fcntl(fd, F_GETFD);
        uint64_t identity = view->object ? view->object : g_pipe_identity[fd];
        if (flags < 0 || descriptor_flags < 0 || identity == 0) return -1;
        if ((flags & O_ACCMODE) != O_WRONLY && ckpt_capture_pipe(fd, identity) != 0) return -1;
        r.kind = CKF_PIPE;
        r.flags = flags;
        r.descriptor_flags = descriptor_flags;
        r.offset = (int64_t)identity;
        snprintf(r.path, sizeof r.path, "%d", (fd >= 0 && fd < HL_NFD) ? g_pipesz[fd] : 0);
        if ((r.flags & O_ACCMODE) == O_RDONLY && ckpt_capture_pipe(fd, identity) != 0) return -1;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (detail.kind == HL_HOST_FD_SOCKET) {
        fprintf(stderr, "[ckpt] refuse: guest fd %d is a socket -- socket restore is not yet supported\n", fd);
        return -1;
    }
    if (emulated && strcmp(emulated, "memfd") == 0) {
        struct stat status;
        if (fstat(fd, &status) != 0) return -1;
        r.flags = fcntl(fd, F_GETFL);
        r.descriptor_flags = fcntl(fd, F_GETFD);
        r.offset = lseek(fd, 0, SEEK_CUR);
        if (r.flags < 0 || r.descriptor_flags < 0 || r.offset < 0 ||
            ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0)
            return -1;
        r.kind = CKF_MEMFD;
        r.object_id = ckpt_backing_id(&status);
        r.ofd_id = ckpt_native_ofd_id(records, *count, fd, r.object_id);
        int seals = g_memfd_seal[fd];
        (void)memfd_reg_get_fd(fd, &seals);
        r.auxiliary = (uint64_t)(unsigned)seals;
        records[(*count)++] = r;
        return CKPT_FD_CAPTURED;
    }
    if (emulated) {
        fprintf(stderr, "[ckpt] refuse: guest fd %d is a %s -- restore is not yet supported\n", fd, emulated);
        return -1;
    }
    struct stat status;
    if (fstat(fd, &status) != 0) return -1;
    r.flags = fcntl(fd, F_GETFL);
    r.descriptor_flags = fcntl(fd, F_GETFD);
    if (r.flags < 0 || r.descriptor_flags < 0) return -1;
    r.offset = lseek(fd, 0, SEEK_CUR);
    r.object_id = ckpt_backing_id(&status);
    r.ofd_id = ckpt_native_ofd_id(records, *count, fd, r.object_id);
    if (S_ISCHR(status.st_mode) && isatty(fd)) {
        r.kind = CKF_TTY;
        r.offset = 0;
    } else if (S_ISCHR(status.st_mode) || S_ISBLK(status.st_mode)) {
        if (path_size >= sizeof path) return -1;
        path[path_size] = '\0';
        if (S_ISCHR(status.st_mode) && ckpt_path_is_ctty(path)) {
            r.kind = CKF_TTY;
            r.offset = 0;
        } else {
            r.kind = CKF_DEVICE;
            if (path_copy(r.path, sizeof r.path, path) != 0) return -1;
        }
    } else if (S_ISREG(status.st_mode) || S_ISDIR(status.st_mode)) {
        if (path_size >= sizeof path) return -1;
        path[path_size] = '\0';
        if (ckpt_normalize_reopen_path(path) != 0 || (S_ISREG(status.st_mode) && access(path, F_OK) != 0)) {
            if (!S_ISREG(status.st_mode) || ckpt_capture_file_blob(fd, r.path, sizeof r.path) != 0) {
                fprintf(stderr, "[ckpt] refuse: cannot persist deleted fd %d\n", fd);
                return -1;
            }
            r.kind = CKF_BLOB;
        } else {
            r.kind = CKF_FILE;
            if (S_ISDIR(status.st_mode)) r.auxiliary |= CKFA_DIRECTORY;
            if (path_copy(r.path, sizeof r.path, path) != 0) return -1;
        }
    } else {
        fprintf(stderr, "[ckpt] refuse: native guest fd %d has unsupported mode %o\n", fd, (unsigned)status.st_mode);
        return -1;
    }
    records[(*count)++] = r;
    return CKPT_FD_CAPTURED;
}

static int ckpt_scan_fds(struct ckpt_fd *recs, int cap, int *out_n) {
    static struct fdvis_view views[HL_NFD];
    int n = 0;
    size_t visible = proc_fdvis_list((int)getpid(), NULL, 0);
    if (visible > sizeof views / sizeof views[0] || visible > (size_t)cap) {
        fprintf(stderr, "[ckpt] refuse: %zu guest descriptors exceed checkpoint limit %d\n", visible, cap);
        return -1;
    }
    if (proc_fdvis_list((int)getpid(), views, visible) != visible) {
        fprintf(stderr, "[ckpt] refuse: guest descriptor table changed during checkpoint\n");
        return -1;
    }
    for (size_t index = 0; index < visible; index++) {
        int fd = views[index].guest_fd;
        if (ckpt_fd_was_captured(recs, n, fd)) continue;
        int result = ckpt_capture_early_emulated_fd(recs, &n, fd);
        if (result == CKPT_FD_CAPTURE_ERROR) return -1;
        if (result == CKPT_FD_CAPTURED) continue;
        result = ckpt_capture_typed_fd(recs, &n, fd);
        if (result == CKPT_FD_CAPTURE_ERROR) return -1;
        if (result == CKPT_FD_CAPTURED) continue;
        if (ckpt_capture_native_fd(recs, &n, &views[index]) == CKPT_FD_CAPTURE_ERROR) return -1;
    }
    *out_n = n;
    return 0;
}

static uint32_t ckpt_inotify_fflags(uint32_t flags) {
    uint32_t mask = 0;
    if (flags & (NOTE_WRITE | NOTE_EXTEND)) mask |= 0x2;
    if (flags & NOTE_ATTRIB) mask |= 0x4;
    if (flags & NOTE_DELETE) mask |= 0x400;
    if (flags & NOTE_RENAME) mask |= 0x800;
    return mask;
}

static int ckpt_dump_inotify(struct ckpt_sink *sink, const char *group) {
    for (int instance = 0; instance < HL_NFD; instance++) {
        if (!g_inotify[instance]) continue;
#if defined(__linux__)
        int original_flags = fcntl(instance, F_GETFL);
        if (original_flags < 0 || fcntl(instance, F_SETFL, original_flags | O_NONBLOCK) != 0) return -1;
        for (;;) {
            uint8_t buffer[16384];
            ssize_t count = read(instance, buffer, sizeof buffer);
            if (count < 0 && errno == EAGAIN) break;
            if (count < 0) return -1;
            if (!count) break;
            size_t old = g_inotify_raw_len[instance];
            if ((size_t)count > SIZE_MAX - old) return -1;
            uint8_t *grown = realloc(g_inotify_raw[instance], old + (size_t)count);
            if (!grown) return -1;
            g_inotify_raw[instance] = grown;
            memcpy(grown + old, buffer, (size_t)count);
            g_inotify_raw_len[instance] = old + (size_t)count;
        }
        if (fcntl(instance, F_SETFL, original_flags) != 0) return -1;
#else
        for (;;) {
            struct kevent events[32];
            struct timespec zero = {0, 0};
            int count = kevent(instance, NULL, 0, events, 32, &zero);
            if (count < 0) return -1;
            if (!count) break;
            for (int index = 0; index < count; index++) {
                int wd = (int)events[index].ident;
                if (wd >= 0 && wd < HL_NFD && g_inotify_owner[wd] == instance)
                    g_inotify_pending[wd] |= g_inotify_isdir[wd] ? 1u : ckpt_inotify_fflags(events[index].fflags);
            }
        }
#endif
    }
    uint32_t watches = 0, moves = 0, raw_instances = 0;
    for (int wd = 0; wd < HL_NFD; wd++)
        if (g_inotify_owner[wd]) watches++;
    for (int index = 0; index < g_inomv_n; index++) {
        int wd = g_inomv[index].wd;
        if (wd >= 0 && wd < HL_NFD && g_inotify_owner[wd]) moves++;
    }
    for (int instance = 0; instance < HL_NFD; instance++)
        if (g_inotify_raw_len[instance] > g_inotify_raw_pos[instance]) raw_instances++;
    struct ckpt_sink_stream *file = NULL;
    if (ckpt_sink_begin(sink, group, "inotify", 0, &file) != 0) return -1;
    if (ckpt_sink_write(sink, file, &watches, sizeof watches) != 0 ||
        ckpt_sink_write(sink, file, &moves, sizeof moves) != 0 ||
        ckpt_sink_write(sink, file, &raw_instances, sizeof raw_instances) != 0)
        goto fail;
    for (int wd = 0; wd < HL_NFD; wd++) {
        if (!g_inotify_owner[wd]) continue;
        size_t snapshot_size = g_inotify_snap[wd] ? strlen(g_inotify_snap[wd]) + 1 : 0;
        if (snapshot_size > UINT32_MAX) goto fail;
        struct ckpt_inotify_watch watch = {
            .instance = g_inotify_owner[wd],
            .wd = wd,
            .mask = g_inotify_mask[wd],
            .pending = g_inotify_pending[wd],
            .snapshot_size = (uint32_t)snapshot_size,
            .is_directory = g_inotify_isdir[wd],
        };
        memcpy(watch.path, g_inotify_wpath[wd], sizeof watch.path);
        watch.path[sizeof watch.path - 1] = 0;
        if (ckpt_sink_write(sink, file, &watch, sizeof watch) != 0 ||
            (snapshot_size && ckpt_sink_write(sink, file, g_inotify_snap[wd], snapshot_size) != 0))
            goto fail;
    }
    for (int index = 0; index < g_inomv_n; index++) {
        int wd = g_inomv[index].wd;
        if (wd < 0 || wd >= HL_NFD || !g_inotify_owner[wd]) continue;
        struct ckpt_inotify_move move = {
            .wd = wd,
            .mask = g_inomv[index].mask,
            .cookie = g_inomv[index].cookie,
        };
        snprintf(move.name, sizeof move.name, "%s", g_inomv[index].name);
        if (ckpt_sink_write(sink, file, &move, sizeof move) != 0) goto fail;
    }
    for (int instance = 0; instance < HL_NFD; instance++) {
        size_t remaining = g_inotify_raw_len[instance] - g_inotify_raw_pos[instance];
        if (!remaining) continue;
        if (remaining > UINT32_MAX) goto fail;
        struct ckpt_inotify_raw raw = {.instance = instance, .size = (uint32_t)remaining};
        if (ckpt_sink_write(sink, file, &raw, sizeof raw) != 0 ||
            ckpt_sink_write(sink, file, g_inotify_raw[instance] + g_inotify_raw_pos[instance], remaining) != 0)
            goto fail;
    }
    return ckpt_sink_finish(sink, &file);
fail:
    ckpt_sink_abort(sink, &file);
    return -1;
}

static int ckpt_dump_epoll(struct ckpt_sink *sink, const char *group, const struct ckpt_fd *records, int count) {
    for (int record_index = 0; record_index < count; ++record_index) {
        const struct ckpt_fd *record = &records[record_index];
        if (record->kind != CKF_EPOLL) continue;
        int duplicate = 0;
        for (int prior = 0; prior < record_index; ++prior)
            if (records[prior].kind == CKF_EPOLL && records[prior].object_id == record->object_id) duplicate = 1;
        if (duplicate) continue;
        struct ckpt_epoll_watch watches[HL_NFD + EP_PROVIDER_WATCH_LIMIT + EP_OBJECT_WATCH_LIMIT];
        uint32_t used = 0;
        for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
            ep_native_watch *watch = &g_ep_native_watches[index];
            if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1 ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] = (struct ckpt_epoll_watch){watch->logical_descriptor, watch->events,
                                                        ((watch->events & 1u) ? HL_LINUX_READY_READ : 0u) |
                                                            ((watch->events & 4u) ? HL_LINUX_READY_WRITE : 0u),
                                                        watch->armed, watch->data};
        }
        for (uint32_t index = 0; index < EP_PROVIDER_WATCH_LIMIT; ++index) {
            ep_provider_watch *watch = &g_ep_provider_watches[index];
            if (atomic_load_explicit(&watch->state, memory_order_acquire) != EP_PROVIDER_ACTIVE ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] = (struct ckpt_epoll_watch){watch->descriptor, watch->events, watch->interests,
                                                        watch->interests != 0 ? 3u : 0u, watch->data};
        }
        for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
            ep_object_watch *watch = &g_ep_object_watches[index];
            if (atomic_load_explicit(&watch->active, memory_order_acquire) == 0 ||
                ckpt_epoll_identity(watch->epoll) != record->object_id)
                continue;
            watches[used++] =
                (struct ckpt_epoll_watch){watch->descriptor, watch->events, watch->interests, 3u, watch->data};
        }
        size_t bytes = sizeof(struct ckpt_epoll_header) + (size_t)used * sizeof(*watches);
        unsigned char *image = malloc(bytes);
        if (image == NULL) return -1;
        struct ckpt_epoll_header header = {CKPT_EPOLL_MAGIC, used, 0};
        memcpy(image, &header, sizeof header);
        memcpy(image + sizeof header, watches, (size_t)used * sizeof(*watches));
        int result = ckpt_sink_put(sink, group, record->path, 0, image, bytes);
        free(image);
        if (result != 0) return -1;
    }
    return 0;
}

static int ckpt_region_prot(uint64_t addr, uint64_t glen) {
    int p = anon_prot_if_contained(addr, glen ? glen : 1);
    return p >= 0 ? p : (PROT_READ | PROT_WRITE);
}

static int ckpt_logical_descriptor_compare(const void *left, const void *right) {
    const hl_logical_vma_descriptor *a = left;
    const hl_logical_vma_descriptor *b = right;
    if (a->guest_first < b->guest_first) return -1;
    if (a->guest_first > b->guest_first) return 1;
    return 0;
}

static int ckpt_dump_region_bytes(struct ckpt_sink *sink, struct ckpt_sink_stream *f, size_t pagesz,
                                  struct ckpt_region *reg) {
    static uint8_t zero[65536];
    uint8_t *logical_page = reg->logical ? malloc(pagesz) : NULL;
    if (reg->logical && logical_page == NULL) return -1;
    for (uint64_t off = 0; off < reg->len; off += pagesz) {
        uint64_t va = reg->addr + off;
        size_t n = (reg->len - off < pagesz) ? (size_t)(reg->len - off) : pagesz;
        const void *bytes = (const void *)(uintptr_t)va;
        if (reg->logical) {
            if (hl_logical_vma_global_copy_out(va, logical_page, n) != 0) {
                free(logical_page);
                return -1;
            }
            bytes = logical_page;
        } else if (!host_range_mapped((uintptr_t)va, n)) {
            continue;
        }
        if (n <= sizeof zero && memcmp(bytes, zero, n) == 0) continue;
        if (ckpt_sink_write(sink, f, &va, sizeof va) != 0 || ckpt_sink_write(sink, f, bytes, n) != 0) {
            free(logical_page);
            return -1;
        }
        reg->npages++;
    }
    free(logical_page);
    return 0;
}

static int ckpt_write_region(struct ckpt_sink *sink, struct ckpt_sink_stream *stream,
                             const struct ckpt_region *region) {
    return ckpt_sink_write(sink, stream, region, sizeof *region);
}

static int ckpt_write_region_at(struct ckpt_sink *sink, struct ckpt_sink_stream *stream, uint64_t offset,
                                const struct ckpt_region *region) {
    return ckpt_sink_write_at(sink, stream, offset, region, sizeof *region);
}

// Sparse-dump every tracked guest mapping (image/interp/heap/stack/anon/file mmap). Non-zero HOST pages only.
static int ckpt_dump_pages(struct ckpt_sink *sink, struct ckpt_sink_stream *f, size_t pagesz, uint64_t *out_n) {
    uint64_t nreg = 0;
    size_t mapping_count = hl_gmap_count();
    for (size_t i = 0; i < mapping_count; i++) {
        hl_gmap_entry mapping;
        if (!hl_gmap_get(i, &mapping)) continue;
        uint64_t addr = mapping.address, len = mapping.length, glen = mapping.guest_length;
        if (!addr || !len) continue;
        struct ckpt_region reg;
        memset(&reg, 0, sizeof reg);
        reg.format_version = CKPT_REGION_VERSION;
        reg.addr = addr;
        reg.len = len;
        reg.glen = glen;
        reg.prot = ckpt_region_prot(addr, glen);
        // is_gna is a WHOLE-REGION claim (restore gna_adds the whole region), so ask it as one: gna_hit's
        // first-page test is true of every glibc pthread stack guard, which poisoned whole stacks on restore
        // -> -EFAULT in pthread_join's futex -> abort.
        reg.is_gna = gna_all(addr, glen ? glen : 1);
        pthread_mutex_lock(&g_filemap_lock);
        for (int map_index = 0; map_index < g_nfilemap; map_index++) {
            struct guest_file_mapping *filemap = &g_filemap[map_index];
            if (addr < filemap->lo || addr + glen > filemap->hi) continue;
            reg.backing_object = ckpt_backing_values(filemap->device, filemap->inode);
            reg.backing_offset = filemap->offset + (addr - filemap->lo);
            reg.backing_shared = filemap->shared;
            reg.backing_emulated = filemap->emulated;
            break;
        }
        pthread_mutex_unlock(&g_filemap_lock);
        hl_logical_vma_descriptor logical;
        int is_logical = hl_logical_vma_global_describe(addr, &logical);
        if (is_logical < 0) return -1;
        if (is_logical == 1) {
            /*
             * gmap tracks the original mmap while mprotect may split the
             * logical ledger. Emit every descriptor in this gmap separately;
             * the next outer entry (if any) skips descriptors it does not own.
             */
            size_t descriptor_count = hl_logical_vma_global_export(NULL, 0);
            hl_logical_vma_descriptor *descriptors =
                descriptor_count ? malloc(descriptor_count * sizeof(*descriptors)) : NULL;
            if (descriptor_count && descriptors == NULL) return -1;
            if (hl_logical_vma_global_export(descriptors, descriptor_count) != descriptor_count) {
                free(descriptors);
                errno = EAGAIN;
                return -1;
            }
            qsort(descriptors, descriptor_count, sizeof(*descriptors), ckpt_logical_descriptor_compare);
            for (size_t descriptor_index = 0; descriptor_index < descriptor_count; ++descriptor_index) {
                const hl_logical_vma_descriptor *descriptor = &descriptors[descriptor_index];
                if (descriptor->guest_first < addr || descriptor->guest_first >= addr + glen) continue;
                struct ckpt_region logical_region = {0};
                logical_region.addr = descriptor->guest_first;
                logical_region.len = descriptor->length;
                logical_region.glen = descriptor->length;
                logical_region.prot = (int32_t)descriptor->protection;
                logical_region.backing_object = ckpt_backing_values(descriptor->device, descriptor->inode);
                logical_region.backing_offset = descriptor->backing_offset;
                logical_region.backing_shared = 1;
                logical_region.format_version = CKPT_REGION_VERSION;
                logical_region.logical = 1;
                int64_t logical_header = ckpt_sink_tell(sink, f);
                if (logical_header < 0 || ckpt_write_region(sink, f, &logical_region) != 0 ||
                    ckpt_dump_region_bytes(sink, f, pagesz, &logical_region) != 0 ||
                    ckpt_write_region_at(sink, f, (uint64_t)logical_header, &logical_region) != 0) {
                    free(descriptors);
                    return -1;
                }
                nreg++;
            }
            free(descriptors);
            continue;
        }
        int64_t header_offset = ckpt_sink_tell(sink, f);
        if (header_offset < 0) return -1;
        if (ckpt_write_region(sink, f, &reg) != 0) return -1;
        if (ckpt_dump_region_bytes(sink, f, pagesz, &reg) != 0) return -1;
        // Patch the region header in place now that npages is known (the streaming equivalent of the
        // old seek-back-and-rewrite).
        if (ckpt_write_region_at(sink, f, (uint64_t)header_offset, &reg) != 0) return -1;
        nreg++;
    }
    *out_n = nreg;
    return 0;
}

// This process's guest identity (pid / parent / group / session), mapped from host ids to guest space (the
// container init's real host pid/group/session all read back as 1). getppid()==g_init_hostpid means "child
// of init"; a host pgid/sid equal to g_init_hostpid is the container's own group/session (guest 1).
static void ckpt_self_identity(struct ckpt_meta *m) {
    hl_host_process_info process;
    int self = container_pid();
    m->self_gpid = self;
    if (self == 1) {
        m->ppid_gpid = 0;
    } else {
        int pp = getppid();
        m->ppid_gpid = (g_init_hostpid && pp == g_init_hostpid) ? 1 : pp;
    }
    int pg = getpgid(0);
    m->pgid_gpid = (g_init_hostpid && pg == g_init_hostpid) ? 1 : pg;
    int sd = hl_host_process_read(getpid(), &process) ? (int)process.session : getsid(0);
    m->sid_gpid = (g_init_hostpid && sd == g_init_hostpid) ? 1 : sd;
}

// Dump THIS process (RAM + cpu + fds) into `procdir` (temp dir + rename). Returns 0 on success, -1 on any
// failure or P3 refusal (nothing published on failure).
static struct cpu *g_ckpt_cpu_images;
static int g_ckpt_cpu_count;

static int ckpt_dump_self_locked(struct cpu *c, const char *group);

static int ckpt_dump_self(struct cpu *c, const char *procdir) {
    struct cpu *live[THREAD_REG_MAX];
    atomic_store_explicit(&g_ckpt_barrier_active, 1, memory_order_release);
    uint64_t request = stw_checkpoint_arm();
    ckpt_interrupt_threads(c);
    if (stw_checkpoint_wait(request) != 0) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    int count = stw_checkpoint_cpus(live, THREAD_REG_MAX);
    if (count < 1 || count > THREAD_REG_MAX) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    struct cpu *images = malloc((size_t)count * sizeof *images);
    if (!images) {
        stw_checkpoint_end();
        atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
        return -1;
    }
    for (int i = 0; i < count; i++)
        images[i] = *live[i];
    g_ckpt_cpu_images = images;
    g_ckpt_cpu_count = count;
    int result = ckpt_dump_self_locked(c, procdir);
    g_ckpt_cpu_images = NULL;
    g_ckpt_cpu_count = 0;
    free(images);
    atomic_store_explicit(&g_ckpt_barrier_active, 0, memory_order_release);
    stw_checkpoint_end();
    return result;
}

static int ckpt_dump_self_locked(struct cpu *c, const char *group) {
    if (g_untrusted) {
        fprintf(stderr, "[ckpt] refuse: sentry/untrusted split is P3\n");
        return -1;
    }
    struct ckpt_sink *sink = ckpt_sink_current();
    struct ckpt_fd *fdrecs = calloc(HL_NFD, sizeof *fdrecs);
    int nfd = 0;
    if (fdrecs == NULL || ckpt_scan_fds(fdrecs, HL_NFD, &nfd) != 0) {
        free(fdrecs);
        return -1; // P3 refusal already reported
    }

    // Open this process's image group. The sink stages it; nothing is visible until group_commit.
    fprintf(stderr, "[ckpt] %s: begin (pid %d)\n", group, (int)getpid());
    if (ckpt_sink_group_begin(sink, group) != 0) {
        free(fdrecs);
        return -1;
    }
    struct ckpt_sink_stream *fp = NULL, *ff = NULL;
    int ok = 0;
    size_t pagesz = hl_linux_host_map_granularity();

    struct ckpt_meta m;
    memset(&m, 0, sizeof m);
    m.magic = CKPT_MAGIC;
    m.version = CKPT_VERSION;
    m.arch = G_CKPT_ARCH;
    m.engine_id = pcache_engine_id();
    m.cpu_sz = sizeof(struct cpu);
    m.pagesz = pagesz;
    m.n_threads = (uint64_t)g_ckpt_cpu_count;
    m.brk_lo = brk_lo;
    m.brk_cur = brk_cur;
    m.brk_hi = brk_hi;
    m.nonpie_lo = g_nonpie_lo;
    m.nonpie_hi = g_nonpie_hi;
    m.nonpie_bias = g_nonpie_bias;
    m.stack_lo = g_stack_lo;
    m.stack_hi = g_stack_hi;
    m.n_fds = (uint64_t)nfd;
    ckpt_self_identity(&m);
    snprintf(m.exe_path, sizeof m.exe_path, "%s", g_exe_path ? g_exe_path : "");
    for (int s = 0; s < 65; s++) { // capture this process's guest signal dispositions (restored on thaw)
        m.sig_handler[s] = g_sigact[s].handler;
        m.sig_flags[s] = g_sigact[s].flags;
        m.sig_mask[s] = g_sigact[s].mask;
    }

    if (ckpt_sink_begin(sink, group, "pages", 0, &fp) != 0) goto done;
    if (ckpt_dump_pages(sink, fp, pagesz, &m.n_regions) != 0) goto done;
    if (ckpt_sink_finish(sink, &fp) != 0) goto done;

    {
        size_t payload = (size_t)g_ckpt_cpu_count * sizeof *g_ckpt_cpu_images;
        size_t total = sizeof(struct ckpt_cpu_header) + payload;
        struct ckpt_cpu_header *cpu_file = malloc(total);
        if (!cpu_file) goto done;
        *cpu_file = (struct ckpt_cpu_header){CKPT_CPU_MAGIC, CKPT_VERSION, G_CKPT_ARCH, (uint64_t)g_ckpt_cpu_count,
                                             sizeof(struct cpu)};
        memcpy(cpu_file + 1, g_ckpt_cpu_images, payload);
        int cpu_rc = ckpt_sink_put(sink, group, "cpu", 0, cpu_file, total);
        free(cpu_file);
        if (cpu_rc != 0) goto done;
    }

    if (ckpt_sink_begin(sink, group, "fds", 0, &ff) != 0) goto done;
    for (int i = 0; i < nfd; i++)
        if (ckpt_sink_write(sink, ff, &fdrecs[i], sizeof fdrecs[i]) != 0) goto done;
    if (ckpt_sink_finish(sink, &ff) != 0) goto done;

    for (int i = 0; i < nfd; i++) {
        if (fdrecs[i].kind != CKF_INOTIFY || fdrecs[i].path[0] == 0) continue;
        int duplicate = 0;
        for (int j = 0; j < i; j++)
            if (fdrecs[j].kind == CKF_INOTIFY && fdrecs[j].ofd_id == fdrecs[i].ofd_id) duplicate = 1;
        if (duplicate) continue;
        size_t bytes = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, NULL, 0, &bytes) != HL_STATUS_OK)
            goto done;
        void *image = malloc(bytes);
        if (image == NULL) goto done;
        size_t actual = 0;
        if (hl_linux_inotify_export(g_linux_box, (hl_linux_fd)fdrecs[i].gfd, image, bytes, &actual) != HL_STATUS_OK ||
            actual != bytes) {
            free(image);
            goto done;
        }
        int stored = ckpt_sink_put(sink, group, fdrecs[i].path, 0, image, bytes);
        free(image);
        if (stored != 0) goto done;
    }

    if (ckpt_dump_epoll(sink, group, fdrecs, nfd) != 0) goto done;
    if (ckpt_dump_inotify(sink, group) != 0) goto done;
    if (ckpt_dump_signal_state(sink, group) != 0) goto done;

    // meta written LAST within the group (it carries the section counts).
    if (ckpt_sink_put(sink, group, "meta", 0, &m, sizeof m) != 0) goto done;
    ok = 1;

done:
    if (fp) ckpt_sink_abort(sink, &fp);
    if (ff) ckpt_sink_abort(sink, &ff);
    free(fdrecs);
    if (!ok) {
        fprintf(stderr, "[ckpt] %s: ABORT -- see the refusal above; nothing from this process is published\n", group);
        ckpt_sink_group_abort(sink, group);
        return -1;
    }
    fprintf(stderr, "[ckpt] %s: commit\n", group);
    return ckpt_sink_group_commit(sink, group);
}

// Enumerate the container's whole process tree = every ENGINE process in the init's session. hl runs each
// guest process as a real host process and the launcher setsid()s the container init, so every guest process
// (even a fork-without-exec bash subshell, even one orphaned to launchd after its parent exited) keeps the
// init's session id. The pid registry is unreliable here (a short-lived fork child inherits + unlinks the
// parent's registry entry on exit), so we scan the session table directly and filter to processes running
// OUR OWN executable -- excluding the launcher and any unrelated session member. The host contract returns
// peers only; native process-table details stay in the backend.
// The container INIT (guest pid 1) coordinates a whole-tree checkpoint at its safepoint: freeze + dump every
// peer, then itself, then publish the MANIFEST. Never returns (_exit frees init's RAM).
static void ckpt_coordinate_and_exit(struct cpu *c) {
    struct ckpt_sink *sink = ckpt_sink_current();

    size_t peer_capacity = 512;
    hl_host_process_peer *foll = malloc(peer_capacity * sizeof *foll);
    size_t observed = 0;
    if (foll == NULL) _exit(70);
    for (;;) {
        if (!hl_host_process_peers(foll, peer_capacity, &observed)) _exit(70);
        if (observed <= peer_capacity) break;
        if (observed > (size_t)INT_MAX || observed > SIZE_MAX / sizeof *foll) _exit(70);
        hl_host_process_peer *expanded = realloc(foll, observed * sizeof *foll);
        if (expanded == NULL) _exit(70);
        foll = expanded;
        peer_capacity = observed;
    }
    int nfoll = (int)observed;
    int descendants = 0;
    for (int i = 0; i < nfoll; i++)
        if (ckpt_is_descendant(foll[i].identity, getpid())) foll[descendants++] = foll[i];
    nfoll = descendants;
    fprintf(stderr, "[ckpt] coordinator pid=%d found %d peer(s)\n", getpid(), nfoll);

    // Freeze + dump every peer: the shared trigger generation is already advanced (the requester bumped it),
    // so KICK each peer with the guest-proof THREAD_INT_SIG to bounce it out of a blocked syscall / chained
    // in-cache loop to its safepoint, where ckpt_poll sees the new generation and dumps proc.<gpid> + _exit()s.
    for (int i = 0; i < nfoll; i++) {
        int kicked = hl_host_process_interrupt(foll[i]);
        fprintf(stderr, "[ckpt] participant %lld %s\n", (long long)foll[i].identity,
                kicked ? "interrupted" : "NOT interrupted (it cannot reach a safepoint)");
    }
    unsigned char *completed = calloc((size_t)(nfoll ? nfoll : 1), 1);
    if (completed == NULL) _exit(70);
    int ndone = 0;
    for (int t = 0; t < 500 && ndone != nfoll; t++) { // one whole-tree deadline: at most ~5s total
        for (int i = 0; i < nfoll; i++) {
            if (completed[i]) continue;
            char pd[64];
            snprintf(pd, sizeof pd, "proc.%d", ckpt_peer_gpid(foll[i].identity));
            // Rendezvous through the sink, not through the store: "that peer finished" is defined as
            // "its group was committed", which is exactly what group_commit means for every implementation.
            if (ckpt_sink_group_present(sink, pd) == 1) {
                completed[i] = 1;
                ndone++;
            }
        }
        int st;
        while (waitpid(-1, &st, WNOHANG) > 0) {} // reap so a peer zombie doesn't linger
        if (ndone != nfoll) usleep(10000);
    }
    if (ndone != nfoll) {
        // Name every participant still outstanding at the rendezvous deadline: "the group never committed"
        // is otherwise indistinguishable from "nothing was ever asked to commit".
        for (int i = 0; i < nfoll; i++)
            if (!completed[i])
                fprintf(stderr,
                        "[ckpt] participant %lld never committed proc.%d (it did not reach a checkpoint "
                        "safepoint, or its dump was refused); refusing incomplete manifest\n",
                        (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity));
        _exit(70);
    }

    // Dump ourselves (the init) last.
    if (ckpt_dump_self(c, "proc.1") != 0) {
        fprintf(stderr, "[ckpt] init dump FAILED -- checkpoint incomplete\n");
        _exit(70);
    }

    // Publish the MANIFEST last: its presence == a complete, restorable checkpoint.
    int nproc = 0;
    // A descendant can observe the shared generation and publish its image even if the host peer snapshot
    // raced its registration. Do not commit while that independently frozen process is still assembling its
    // group. A fixed quiescence window also covers the registration gap where neither the peer snapshot nor
    // the store contains the child yet.
    for (int settle = 0; settle < 200; settle++) {
        int complete = ckpt_sink_group_count(sink, "proc.");
        if (complete >= 0) nproc = complete;
        usleep(10000);
    }
    if (nproc < nfoll + 1) {
        fprintf(stderr, "[ckpt] process-count mismatch: expected at least %d, captured %d; refusing manifest\n",
                nfoll + 1, nproc);
        _exit(70);
    }
    struct ckpt_manifest man;
    memset(&man, 0, sizeof man);
    man.magic = CKPT_MANIFEST_MAGIC;
    man.version = CKPT_VERSION;
    man.arch = G_CKPT_ARCH;
    man.n_procs = (uint64_t)nproc;
    man.root_gpid = 1;
    // Record which group owns the controlling terminal's foreground (in guest terms). The init is the tty's
    // session leader here, so tcgetpgrp reads the real fg host pgid; child job groups pass through untranslated
    // (guest pgid == host pgid), only the init's own group folds to guest pgid 1.
    {
        int tf = ckpt_ctty_open();
        int fgh = (tf >= 0) ? tcgetpgrp(tf) : -1;
        struct termios tio;
        man.fg_pgid_gpid = (fgh <= 0) ? 0 : (g_init_hostpid && fgh == g_init_hostpid) ? 1 : fgh;
        if (tf >= 0 && tcgetattr(tf, &tio) == 0) {
            size_t cc = sizeof tio.c_cc < sizeof man.tty_cc ? sizeof tio.c_cc : sizeof man.tty_cc;
            man.tty_termios = 1;
            man.tty_iflag = (uint32_t)tio.c_iflag;
            man.tty_oflag = (uint32_t)tio.c_oflag;
            man.tty_cflag = (uint32_t)tio.c_cflag;
            man.tty_lflag = (uint32_t)tio.c_lflag;
            man.tty_ispeed = (uint32_t)cfgetispeed(&tio);
            man.tty_ospeed = (uint32_t)cfgetospeed(&tio);
            memcpy(man.tty_cc, tio.c_cc, cc);
        }
        ckpt_ctty_close(tf);
    }
    // The digest is asked of the sink: the server accumulated it while the bytes went past, so nothing
    // re-reads the embedder's store.
    if (ckpt_sink_digest(sink, &man.image_hash, &man.image_files, &man.image_bytes) != 0) {
        fprintf(stderr, "[ckpt] cannot hash checkpoint image: %s\n", strerror(errno));
        _exit(70);
    }
    // Explicit completion: the only signal that the image is complete.
    if (ckpt_sink_commit(sink, &man, sizeof man) != 0) {
        fprintf(stderr, "[ckpt] cannot publish checkpoint manifest: %s\n", strerror(errno));
        _exit(70);
    }
    fprintf(stderr, "[ckpt] checkpoint OK: %d process(es)\n", nproc);
    int st;
    while (waitpid(-1, &st, WNOHANG) > 0) {} // final reap
    hl_engine_child_result_publish(0, HL_STATUS_OK, 0);
    _exit(0);
}

// ================================= RESTORE =================================
