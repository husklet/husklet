// Write the bytes capture drained out of this pipe back into the freshly created one. This is the second
// half of the closed round trip ckpt_capture_pipe_reason depends on: it consumes exactly the object that
// the winner of the image-wide "pipe.<identity>" election published, in the order it was published, and it
// runs before any guest process is reforked. It is a named function rather than an inline loop so that the
// two halves of the round trip can be driven against each other by a fixture.
//
// A missing object is not an error: it means no holder of this pipe could read it -- every holder was a
// write end -- so there were no bytes any guest could ever have observed, and an empty pipe is the faithful
// restoration.
static int ckpt_refill_restore_pipe(int writer, uint64_t identity) {
    char data_path[1300];
    snprintf(data_path, sizeof data_path, "pipe.%016llx", (unsigned long long)identity);
    FILE *data = ckpt_source_fopen(data_path);
    if (!data) return 0;
    unsigned char buffer[65536];
    size_t count;
    while ((count = fread(buffer, 1, sizeof buffer, data)) != 0) {
        size_t offset = 0;
        while (offset < count) {
            ssize_t written = write(writer, buffer + offset, count - offset);
            if (written > 0) {
                offset += (size_t)written;
                continue;
            }
            if (written < 0 && errno == EINTR) continue;
            ckpt_source_fclose(data);
            return -1;
        }
    }
    int failed = ferror(data) != 0;
    ckpt_source_fclose(data);
    return failed ? -1 : 0;
}

static int ckpt_prepare_restore_pipes(void) {
    g_nrestore_pipes = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue; // a stopped process must not fail the restore it was pruned from
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_PIPE) continue;
            uint64_t identity = (uint64_t)record.offset;
            struct ckpt_restore_pipe *pipe = ckpt_restore_pipe_find(identity);
            if (!pipe) {
                if (ckpt_vector_reserve((void **)&g_restore_pipes, &g_restore_pipes_capacity, sizeof *g_restore_pipes,
                                        g_nrestore_pipes + 1) != 0) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                pipe = &g_restore_pipes[g_nrestore_pipes++];
                *pipe = (struct ckpt_restore_pipe){.identity = identity, .reader = -1, .writer = -1};
            }
            size_t parsed;
            if (ckpt_decimal_capacity(record.path, 65536, INT_MAX, &parsed) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            int size = (int)parsed;
            if (size > pipe->size) pipe->size = size;
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int i = 0; i < g_nrestore_pipes; i++) {
        char data_path[1300];
        snprintf(data_path, sizeof data_path, "pipe.%016llx", (unsigned long long)g_restore_pipes[i].identity);
        int64_t stored = ckpt_source_object_size(data_path);
        size_t data_size;
        if (stored >= 0 && ckpt_capacity_object_size(stored, (size_t)g_restore_pipes[i].size, &data_size) != 0)
            return -1;
    }
    for (int i = 0; i < g_nrestore_pipes; i++) {
        int pair[2];
        if (pipe(pair) != 0) return -1;
#ifdef F_SETPIPE_SZ
        if (g_restore_pipes[i].size > 0) (void)fcntl(pair[1], F_SETPIPE_SZ, g_restore_pipes[i].size);
#endif
        int reader = hl_host_process_fd_private_adopt(pair[0]);
        if (reader < 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
        int writer = hl_host_process_fd_private_adopt(pair[1]);
        if (writer < 0) {
            hl_host_process_fd_private_remove(reader);
            close(reader);
            close(pair[1]);
            return -1;
        }
        g_restore_pipes[i].reader = reader;
        g_restore_pipes[i].writer = writer;
        int flags = fcntl(writer, F_GETFL);
        if (flags < 0 || fcntl(writer, F_SETFL, flags | O_NONBLOCK) != 0) return -1;
        if (ckpt_refill_restore_pipe(writer, g_restore_pipes[i].identity) != 0) return -1;
    }
    return 0;
}

static int ckpt_restore_right_epoll(const struct ckpt_fd *record) {
    // Any open descriptor will do as the placeholder, but it must not stay where open(2) put it: it is
    // closed AFTER the guest's own descriptors are dup2'd into place, so a low number destroys whichever
    // guest descriptor now owns it (it landed on 4, a socketpair endpoint). Hoist it into the private
    // high band, like every other queued right.
    int placeholder = open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | O_CLOEXEC);
    if (placeholder < 0) return -1;
    int fd = hl_host_process_fd_private_adopt(placeholder);
    if (fd < 0) {
        close(placeholder);
        return -1;
    }
    g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, fd, 1};
    return fd;
}

static int ckpt_restore_right_inotify(const struct ckpt_fd *record) {
    char image_path[1400];
    snprintf(image_path, sizeof image_path, "%s", record->path);
    int64_t stored = ckpt_source_object_size(image_path);
    size_t size;
    if (g_linux_box == NULL || ckpt_inotify_object_size(stored, &size) != 0) return -1;
    void *image = malloc(size);
    int shadow = bound_shadow_reserve(0);
    if (image == NULL || shadow < 0 || ckpt_source_load(image_path, image, size) != 0) {
        free(image);
        if (shadow >= 0) close(shadow);
        return -1;
    }
    void *provider = bound_inotify_provider_create(g_host_services);
    int64_t imported =
        provider == NULL
            ? -HL_LINUX_ENOMEM
            : hl_linux_inotify_import_at(g_linux_box, (hl_linux_fd)shadow, &bound_inotify_ops, provider,
                                         (uint32_t)record->descriptor_flags, (uint32_t)record->flags, image, size);
    free(image);
    if (imported < 0) {
        close(shadow);
        return -1;
    }
    hl_linux_fd_snapshot snapshot;
    if (!bound_snapshot((uint64_t)(uint32_t)shadow, &snapshot) ||
        bound_fdvis_publish_snapshot(shadow, &snapshot) != 0) {
        (void)hl_linux_close(g_linux_box, (hl_linux_fd)shadow);
        close(shadow);
        return -1;
    }
    g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, shadow, 2};
    return shadow;
}

static int ckpt_restore_right_signalfd(const struct ckpt_fd *record) {
    struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(record->object_id);
    if (object == NULL) {
        if (!record->object_id || ckpt_vector_reserve((void **)&g_restore_signalfds, &g_restore_signalfds_capacity,
                                                      sizeof *g_restore_signalfds, g_nrestore_signalfds + 1) != 0)
            return -1;
        int pair[2];
        if (pipe(pair) != 0) return -1;
        int seed_reader = fcntl(pair[0], F_DUPFD_CLOEXEC, HL_NFD);
        int seed_writer = fcntl(pair[1], F_DUPFD_CLOEXEC, HL_NFD);
        close(pair[0]);
        close(pair[1]);
        if (seed_reader < 0 || seed_writer < 0) {
            if (seed_reader >= 0) close(seed_reader);
            if (seed_writer >= 0) close(seed_writer);
            return -1;
        }
        int reader = hl_host_process_fd_private_adopt(seed_reader);
        int writer = reader >= 0 ? hl_host_process_fd_private_adopt(seed_writer) : -1;
        if (reader < 0 || writer < 0) {
            if (reader >= 0) {
                hl_host_process_fd_private_remove(reader);
                close(reader);
            } else
                close(seed_reader);
            if (writer >= 0) {
                hl_host_process_fd_private_remove(writer);
                close(writer);
            } else
                close(seed_writer);
            return -1;
        }
        int writer_flags = fcntl(writer, F_GETFL);
        if (writer_flags < 0 || fcntl(writer, F_SETFL, writer_flags | O_NONBLOCK) != 0) return -1;
        object = &g_restore_signalfds[g_nrestore_signalfds++];
        *object = (struct ckpt_restore_signalfd){record->object_id, record->auxiliary, reader, writer};
        char queue_path[1300];
        snprintf(queue_path, sizeof queue_path, "%s", record->path);
        FILE *queue = ckpt_source_fopen(queue_path);
        if (queue != NULL) {
            unsigned char bytes[4096];
            size_t count;
            while ((count = fread(bytes, 1, sizeof bytes, queue)) != 0) {
                size_t offset = 0;
                while (offset < count) {
                    ssize_t written = write(writer, bytes + offset, count - offset);
                    if (written > 0)
                        offset += (size_t)written;
                    else if (written < 0 && errno == EINTR)
                        continue;
                    else {
                        ckpt_source_fclose(queue);
                        return -1;
                    }
                }
            }
            if (ferror(queue)) {
                ckpt_source_fclose(queue);
                return -1;
            }
            ckpt_source_fclose(queue);
        }
    } else if (object->mask != record->auxiliary) {
        return -1;
    }
    g_restore_rights[g_nrestore_rights++] =
        (struct ckpt_restore_right){record->ofd_id, record->object_id, object->reader, 0};
    return object->reader;
}

static int ckpt_restore_right_pipe(const struct ckpt_fd *record) {
    uint64_t identity = (uint64_t)record->offset;
    struct ckpt_restore_pipe *pipe_object = ckpt_restore_pipe_find(identity);
    if (pipe_object == NULL) {
        if (!identity || ckpt_vector_reserve((void **)&g_restore_pipes, &g_restore_pipes_capacity,
                                             sizeof *g_restore_pipes, g_nrestore_pipes + 1) != 0)
            return -1;
        pipe_object = &g_restore_pipes[g_nrestore_pipes++];
        *pipe_object =
            (struct ckpt_restore_pipe){.identity = identity, .reader = -1, .writer = -1, .size = atoi(record->path)};
        int pair[2];
        if (pipe(pair) != 0) return -1;
        if (fcntl(pair[0], F_SETFD, FD_CLOEXEC) != 0 || fcntl(pair[1], F_SETFD, FD_CLOEXEC) != 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
#ifdef F_SETPIPE_SZ
        if (pipe_object->size > 0) (void)fcntl(pair[1], F_SETPIPE_SZ, pipe_object->size);
#endif
        pipe_object->reader = hl_host_process_fd_private_adopt(pair[0]);
        pipe_object->writer = pipe_object->reader >= 0 ? hl_host_process_fd_private_adopt(pair[1]) : -1;
        if (pipe_object->reader < 0 || pipe_object->writer < 0) {
            if (pipe_object->reader >= 0) {
                hl_host_process_fd_private_remove(pipe_object->reader);
                close(pipe_object->reader);
            } else
                close(pair[0]);
            if (pipe_object->writer >= 0) {
                hl_host_process_fd_private_remove(pipe_object->writer);
                close(pipe_object->writer);
            } else
                close(pair[1]);
            return -1;
        }
        int writer_flags = fcntl(pipe_object->writer, F_GETFL);
        if (writer_flags < 0 || fcntl(pipe_object->writer, F_SETFL, writer_flags | O_NONBLOCK) != 0) return -1;
        char data_path[1300];
        snprintf(data_path, sizeof data_path, "pipe.%016llx", (unsigned long long)identity);
        FILE *data = ckpt_source_fopen(data_path);
        if (data != NULL) {
            unsigned char buffer[65536];
            size_t count;
            while ((count = fread(buffer, 1, sizeof buffer, data)) != 0) {
                size_t offset = 0;
                while (offset < count) {
                    ssize_t written = write(pipe_object->writer, buffer + offset, count - offset);
                    if (written > 0)
                        offset += (size_t)written;
                    else if (written < 0 && errno == EINTR)
                        continue;
                    else {
                        ckpt_source_fclose(data);
                        return -1;
                    }
                }
            }
            if (ferror(data)) {
                ckpt_source_fclose(data);
                return -1;
            }
            ckpt_source_fclose(data);
        }
    }
    int fd = ((record->flags & O_ACCMODE) == O_WRONLY) ? pipe_object->writer : pipe_object->reader;
    if (fd < 0) return -1;
    g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, fd, 0};
    return fd;
}

static int ckpt_restore_right_eventfd(const struct ckpt_fd *record) {
    struct ckpt_restore_eventfd *eventfd = ckpt_restore_eventfd_find(record->object_id);
    if (eventfd == NULL) {
        if (ckpt_vector_reserve((void **)&g_restore_eventfds, &g_restore_eventfds_capacity, sizeof *g_restore_eventfds,
                                g_nrestore_eventfds + 1) != 0)
            return -1;
        int slot = (int)((record->object_id & UINT64_C(0xffffffff)) - 1);
        if (slot < 0 || slot >= HL_NFD) return -1;
        int pair[2];
        if (pipe(pair) != 0) return -1;
        int flags = fcntl(pair[0], F_GETFL);
        if (flags < 0 || fcntl(pair[0], F_SETFL, flags | O_NONBLOCK) != 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
        int reader = hl_host_process_fd_private_adopt(pair[0]);
        int writer = reader >= 0 ? hl_host_process_fd_private_adopt(pair[1]) : -1;
        if (reader < 0 || writer < 0) {
            if (reader >= 0) {
                hl_host_process_fd_private_remove(reader);
                close(reader);
            } else
                close(pair[0]);
            if (writer >= 0) {
                hl_host_process_fd_private_remove(writer);
                close(writer);
            } else
                close(pair[1]);
            return -1;
        }
        eventfd = &g_restore_eventfds[g_nrestore_eventfds++];
        *eventfd = (struct ckpt_restore_eventfd){
            .identity = record->object_id,
            .count = record->auxiliary,
            .reader = reader,
            .writer = writer,
            .slot = slot,
            .semaphore = record->offset != 0,
            .guest_nonblock = (record->flags & O_NONBLOCK) != 0,
        };
        if (eventfd->count != 0) {
            char byte = 1;
            if (write(writer, &byte, 1) != 1) return -1;
        }
    } else if (eventfd->count != record->auxiliary || eventfd->semaphore != (record->offset != 0) ||
               eventfd->guest_nonblock != ((record->flags & O_NONBLOCK) != 0)) {
        return -1;
    }
    g_restore_rights[g_nrestore_rights++] =
        (struct ckpt_restore_right){record->ofd_id, record->object_id, eventfd->reader, 0};
    return eventfd->reader;
}

static int ckpt_restore_right_timerfd(const struct ckpt_fd *record) {
    struct ckpt_restore_timerfd *timerfd = ckpt_restore_timerfd_find(record->object_id);
    if (timerfd == NULL) {
        int clock_id = 0;
        unsigned first = 0;
        unsigned long long pending = 0;
        long long captured_ns = 0;
        if (!record->object_id ||
            ckpt_vector_reserve((void **)&g_restore_timerfds, &g_restore_timerfds_capacity, sizeof *g_restore_timerfds,
                                g_nrestore_timerfds + 1) != 0 ||
            sscanf(record->path, "%d %llu %u %lld", &clock_id, &pending, &first, &captured_ns) != 4)
            return -1;
        struct timerfd_shared_state *state =
            mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
        if (state == MAP_FAILED) return -1;
        memset(state, 0, sizeof *state);
        struct timespec now;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
        int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
        int64_t deadline = record->offset;
        int64_t interval = (int64_t)record->auxiliary;
        int64_t next = deadline;
        uint64_t accumulated = (uint64_t)pending;
        if (deadline > 0 && interval > 0) {
            if (next <= captured_ns) next += ((captured_ns - next) / interval + 1) * interval;
            if (now_ns >= next) {
                accumulated += 1 + (uint64_t)((now_ns - next) / interval);
                next += ((now_ns - next) / interval + 1) * interval;
            }
        } else if (deadline > 0 && now_ns >= deadline) {
            accumulated = 1;
            next = 0;
        }
        state->deadline = next;
        state->interval = interval;
        state->pending = accumulated;
        g_restore_timerfds[g_nrestore_timerfds++] = (struct ckpt_restore_timerfd){
            .identity = record->object_id,
            .state = state,
            .clock_id = clock_id,
            .fd = -1,
            .slot = -1,
            .first_oneshot = (uint8_t)(first != 0),
        };
        timerfd = &g_restore_timerfds[g_nrestore_timerfds - 1];
    }
    if (timerfd->state == NULL) return -1;
    if (timerfd->fd < 0) {
        int timer = kqueue();
        if (timer < 0) return -1;
        timerfd->fd = hl_host_process_fd_private_adopt(timer);
        if (timerfd->fd < 0) {
            hl_native_kqueue_close(timer);
            close(timer);
            return -1;
        }
        // adopt() moves the descriptor and closes the original, so a number-keyed shim kqueue must move
        // with it or the arming kevent() below is EBADF.
        hl_native_kqueue_relocate(timer, timerfd->fd);
        timerfd->slot = timerfd->fd;
        struct timespec now;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
        int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
        timerfd_shared_lock(timerfd->state);
        int64_t next = timerfd->state->deadline;
        uint64_t pending = timerfd->state->pending;
        timerfd_shared_unlock(timerfd->state);
        if (pending != 0 || next > now_ns) {
            struct kevent event;
            int64_t delay = pending != 0 ? 1 : next - now_ns;
            EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
            if (kevent(timerfd->fd, &event, 1, NULL, 0, NULL) < 0) return -1;
        }
    }
    g_restore_rights[g_nrestore_rights++] =
        (struct ckpt_restore_right){record->ofd_id, record->object_id, timerfd->fd, 0};
    return timerfd->fd;
}

static int ckpt_restore_right_prepare(const struct ckpt_fd *record) {
    struct ckpt_restore_right *existing = ckpt_restore_right_find(record->ofd_id);
    if (existing != NULL) return existing->object_id == record->object_id ? existing->fd : -1;
    if (!record->ofd_id ||
        (record->kind != CKF_FILE && record->kind != CKF_DEVICE && record->kind != CKF_BLOB &&
         record->kind != CKF_MEMFD && record->kind != CKF_PIPE && record->kind != CKF_SIGNALFD &&
         record->kind != CKF_INOTIFY && record->kind != CKF_EVENTFD && record->kind != CKF_TIMERFD &&
         record->kind != CKF_EPOLL) ||
        ckpt_vector_reserve((void **)&g_restore_rights, &g_restore_rights_capacity, sizeof *g_restore_rights,
                            g_nrestore_rights + 1) != 0)
        return fprintf(stderr, "[restore] invalid queued right kind=%d ofd=%llx\n", record->kind,
                       (unsigned long long)record->ofd_id),
               -1;
    int fd = -1;
    if (record->kind == CKF_EPOLL) return ckpt_restore_right_epoll(record);
    if (record->kind == CKF_INOTIFY) return ckpt_restore_right_inotify(record);
    if (record->kind == CKF_SIGNALFD) return ckpt_restore_right_signalfd(record);
    if (record->kind == CKF_PIPE) return ckpt_restore_right_pipe(record);
    if (record->kind == CKF_EVENTFD) return ckpt_restore_right_eventfd(record);
    if (record->kind == CKF_TIMERFD) return ckpt_restore_right_timerfd(record);
    if (record->kind == CKF_FILE || record->kind == CKF_DEVICE) {
        int open_flags = record->flags & (O_ACCMODE | O_APPEND | O_NONBLOCK);
        fd = open(record->path, open_flags);
        if (record->kind == CKF_FILE && fd < 0 && (open_flags & O_ACCMODE) == O_RDWR) fd = open(record->path, O_RDONLY);
    } else {
        char temporary[] = "/tmp/.hl-restore-rightXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || ckpt_source_copy_to_fd(record->path, fd) != 0) {
            if (fd >= 0) close(fd);
            return fprintf(stderr, "[restore] queued right blob %s copy failed: %s\n", record->path, strerror(errno)),
                   -1;
        }
        int live_flags = fcntl(fd, F_GETFL);
        if (live_flags < 0 || fcntl(fd, F_SETFL, (live_flags & O_ACCMODE) | (record->flags & ~O_ACCMODE)) != 0) {
            close(fd);
            return fprintf(stderr, "[restore] queued right flags failed: %s\n", strerror(errno)), -1;
        }
    }
    if (fd < 0 || (record->kind != CKF_DEVICE && lseek(fd, (off_t)record->offset, SEEK_SET) != (off_t)record->offset)) {
        if (fd >= 0) close(fd);
        return fprintf(stderr, "[restore] queued right open/seek kind=%d path=%s offset=%lld: %s\n", record->kind,
                       record->path, (long long)record->offset, strerror(errno)),
               -1;
    }
    int adopted = hl_host_process_fd_private_adopt(fd);
    if (adopted < 0) {
        close(fd);
        return fprintf(stderr, "[restore] queued right adopt failed: %s\n", strerror(errno)), -1;
    }
    if (record->kind == CKF_MEMFD) {
        g_memfd_is[adopted] = 1;
        g_memfd_seal[adopted] = (int)record->auxiliary;
        memfd_reg_set_fd(adopted, g_memfd_seal[adopted]);
    }
    g_restore_rights[g_nrestore_rights++] = (struct ckpt_restore_right){record->ofd_id, record->object_id, adopted, 1};
    return adopted;
}

static int ckpt_restore_socket_right_markers(const struct ckpt_fd *rights, const int *right_fds, uint32_t rights_count,
                                             int *combo, int *combo_count_out, unsigned char *payload, FILE *file) {
    int combo_count = *combo_count_out;
    for (uint32_t index = 0; index < rights_count; ++index) {
        if (rights[index].kind == CKF_EVENTFD) {
            struct ckpt_restore_eventfd *eventfd = ckpt_restore_eventfd_find(rights[index].object_id);
            if (eventfd == NULL || combo_count + 2 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            struct hl_cmsg_eventfd_meta metadata = {
                .magic = HL_CMSG_EVENTFD_MAGIC,
                .ordinal = index,
                .slot = (uint32_t)eventfd->slot,
                .sema = (uint32_t)eventfd->semaphore,
                .nb = (uint32_t)eventfd->guest_nonblock,
            };
            int writer_flags = fcntl(eventfd->writer, F_GETFL);
            if (writer_flags < 0 || fcntl(eventfd->writer, F_SETFL, writer_flags | O_NONBLOCK) != 0 ||
                fcntl(eventfd->writer, F_SETFD, FD_CLOEXEC) != 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            int marker = cmsg_eventfd_marker(&metadata);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = eventfd->writer;
            combo[combo_count++] = marker;
        }
        if (rights[index].kind == CKF_TIMERFD) {
            struct ckpt_restore_timerfd *timerfd = ckpt_restore_timerfd_find(rights[index].object_id);
            if (timerfd == NULL || timerfd->state == NULL || combo_count + 1 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            struct hl_cmsg_timerfd_meta metadata = {
                .magic = HL_CMSG_TIMERFD_MAGIC,
                .ordinal = index,
                .first_oneshot = timerfd->first_oneshot,
                .clock = timerfd->clock_id,
                .deadline = timerfd->state->deadline,
                .interval = timerfd->state->interval,
                .source_fd = timerfd->fd,
                .source_pid = (int32_t)getpid(),
                .nb = (rights[index].flags & O_NONBLOCK) != 0,
                .portable = 1,
                .restore_shared = 1,
                .object = timerfd->identity,
                .shared_state = (uint64_t)(uintptr_t)timerfd->state,
            };
            struct hl_cmsg_timerfd_meta placeholder_metadata;
            memset(&placeholder_metadata, 0, sizeof placeholder_metadata);
            int placeholder = cmsg_timerfd_marker(&placeholder_metadata);
            if (placeholder < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[index] = placeholder;
            int marker = cmsg_timerfd_marker(&metadata);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = marker;
        }
        if (rights[index].kind == CKF_PIPE) {
            uint64_t identity = (uint64_t)rights[index].offset;
            if (!identity || combo_count + 1 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            struct hl_cmsg_pipe_meta metadata = {
                .magic = UINT32_C(0x484c5049),
                .ordinal = index,
                .identity = identity,
                .size = atoi(rights[index].path),
            };
            int marker = cmsg_pipe_marker(&metadata);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = marker;
        }
        if (rights[index].kind == CKF_SIGNALFD) {
            struct ckpt_restore_signalfd *object = ckpt_restore_signalfd_find(rights[index].object_id);
            if (object == NULL || combo_count + 2 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            struct hl_cmsg_signalfd_meta metadata = {
                .magic = UINT32_C(0x484c5346),
                .ordinal = index,
                .source_pid = (int32_t)getpid(),
                .source_slot = -1,
                .mask = object->mask,
            };
            int marker = cmsg_signalfd_marker(&metadata);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = object->writer;
            combo[combo_count++] = marker;
        }
        if (rights[index].kind == CKF_INOTIFY) {
            if (combo_count + 1 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            struct hl_cmsg_kqueue_meta metadata = {
                .magic = UINT32_C(0x484c4b51),
                .ordinal = index,
                .source_pid = (int32_t)getpid(),
                .source_fd = right_fds[index],
                .kind = 3,
                .nonblock = (rights[index].flags & O_NONBLOCK) != 0,
                .object_id = rights[index].object_id,
                .descriptor_flags = (uint32_t)rights[index].descriptor_flags,
            };
            int placeholder = cmsg_kqueue_placeholder();
            if (placeholder < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[index] = placeholder;
            int marker = cmsg_kqueue_marker(&metadata);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = marker;
        }
        if (rights[index].kind == CKF_EPOLL) {
            if (combo_count + 1 > 253 * 4) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            int placeholder = cmsg_kqueue_placeholder();
            if (placeholder < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[index] = placeholder;
            int marker = ckpt_restore_epoll_marker(&rights[index], index);
            if (marker < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            combo[combo_count++] = marker;
        }
    }
    *combo_count_out = combo_count;
    return 0;
}

static int ckpt_restore_socket_queue_load(struct ckpt_restore_socket_endpoint *endpoint) {
    char path[1300];
    snprintf(path, sizeof path, "socket.%016llx", (unsigned long long)endpoint->identity);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return errno == ENOENT ? 0 : -1;
    struct ckpt_socket_queue_header header;
    if (ckpt_rd_all(file, &header, sizeof header) != 0 || header.magic != CKPT_SOCKET_QUEUE_MAGIC ||
        header.type != (uint32_t)endpoint->type) {
        ckpt_source_fclose(file);
        return -1;
    }
    endpoint->peer_closed = header.peer_closed != 0;
    struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
    if (peer == NULL || peer->fd < 0) {
        ckpt_source_fclose(file);
        return -1;
    }
    for (;;) {
        struct ckpt_socket_queue_frame frame;
        size_t frame_bytes = fread(&frame, 1, sizeof frame, file);
        if (frame_bytes == 0 && feof(file)) break;
        if (frame_bytes != sizeof frame || ferror(file) || frame.rights_count > 253 || frame.size > (1u << 20)) {
            ckpt_source_fclose(file);
            return -1;
        }
        unsigned char *payload = malloc(frame.size ? frame.size : 1u);
        if (payload == NULL || (frame.size != 0 && fread(payload, 1, frame.size, file) != frame.size)) {
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        struct ckpt_fd rights[253];
        int right_fds[253];
        if (frame.rights_count != 0 &&
            fread(rights, sizeof rights[0], frame.rights_count, file) != frame.rights_count) {
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_fd_terminate_all(rights, frame.rights_count);
        for (uint32_t index = 0; index < frame.rights_count; ++index) {
            right_fds[index] = ckpt_restore_right_prepare(&rights[index]);
            if (right_fds[index] < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
        }
        size_t offset = 0;
        if (frame.rights_count != 0) {
            int combo[253 * 4];
            int combo_count = 0;
            cmsg_tmpfds_close();
            for (uint32_t index = 0; index < frame.rights_count; ++index)
                combo[combo_count++] = right_fds[index];
            if (ckpt_restore_socket_right_markers(rights, right_fds, frame.rights_count, combo, &combo_count, payload,
                                                  file) != 0)
                return -1;
            for (uint32_t index = 0; index < frame.rights_count; ++index) {
                if (combo_count == (int)(sizeof combo / sizeof combo[0])) {
                    cmsg_tmpfds_close();
                    free(payload);
                    ckpt_source_fclose(file);
                    return -1;
                }
                struct hl_cmsg_ofd_meta metadata = {
                    .magic = HL_CMSG_OFD_MAGIC,
                    .ordinal = index,
                    .identity = rights[index].ofd_id,
                };
                int marker = cmsg_ofd_marker(&metadata, NULL);
                if (marker < 0) {
                    cmsg_tmpfds_close();
                    free(payload);
                    ckpt_source_fclose(file);
                    return -1;
                }
                combo[combo_count++] = marker;
            }
            unsigned char control[CMSG_SPACE(253 * 4 * sizeof(int))];
            struct iovec iov = {payload, frame.size};
            struct msghdr message;
            memset(&message, 0, sizeof message);
            message.msg_iov = &iov;
            message.msg_iovlen = 1;
            message.msg_control = control;
            message.msg_controllen = CMSG_SPACE((size_t)combo_count * sizeof(int));
            memset(control, 0, message.msg_controllen);
            struct cmsghdr *header = CMSG_FIRSTHDR(&message);
            if (header == NULL) {
                cmsg_tmpfds_close();
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            header->cmsg_level = SOL_SOCKET;
            header->cmsg_type = SCM_RIGHTS;
            header->cmsg_len = CMSG_LEN((size_t)combo_count * sizeof(int));
            memcpy(CMSG_DATA(header), combo, (size_t)combo_count * sizeof(int));
            ssize_t sent;
            do {
                sent = sendmsg(peer->fd, &message, 0);
            } while (sent < 0 && errno == EINTR);
            cmsg_tmpfds_close();
            if (sent < 0 || (endpoint->type != SOCK_STREAM && sent != (ssize_t)frame.size)) {
                fprintf(stderr, "[restore] queued rights send endpoint=%llx count=%u size=%u sent=%lld: %s\n",
                        (unsigned long long)endpoint->identity, frame.rights_count, frame.size, (long long)sent,
                        strerror(errno));
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
            offset = (size_t)sent;
        }
        if (frame.size == 0 && frame.rights_count == 0) {
            ssize_t sent;
            do {
                sent = send(peer->fd, "", 0, 0);
            } while (sent < 0 && errno == EINTR);
            if (sent < 0) {
                free(payload);
                ckpt_source_fclose(file);
                return -1;
            }
        }
        while (offset < frame.size) {
            ssize_t sent = send(peer->fd, payload + offset, frame.size - offset, 0);
            if (sent > 0) {
                offset += (size_t)sent;
                continue;
            }
            if (sent < 0 && errno == EINTR) continue;
            free(payload);
            ckpt_source_fclose(file);
            return -1;
        }
        free(payload);
    }
    ckpt_source_fclose(file);
    return 0;
}

// SO_RCVBUF/SO_SNDBUF do not round-trip on Linux: the kernel stores twice what setsockopt gets, getsockopt
// reports the doubled value, so replaying a capture doubles the buffer each generation. Halve on the way in
// (Linux only -- macOS stores what it is given).
static int ckpt_restore_socket_buffer(int fd, int name, int32_t captured) {
    int value = captured;
#if defined(__linux__)
    value = captured / 2;
#endif
    return setsockopt(fd, SOL_SOCKET, name, &value, sizeof value);
}

static int ckpt_restore_socket_options(int fd, const struct ckpt_socket_state *state) {
#define CKPT_RESTORE_SOCKET_OPTION(name, field)                                                                        \
    do {                                                                                                               \
        if (setsockopt(fd, SOL_SOCKET, name, &state->field, sizeof state->field) != 0) return -1;                      \
    } while (0)
    if (ckpt_restore_socket_buffer(fd, SO_RCVBUF, state->receive_buffer) != 0 ||
        ckpt_restore_socket_buffer(fd, SO_SNDBUF, state->send_buffer) != 0)
        return -1;
    CKPT_RESTORE_SOCKET_OPTION(SO_REUSEADDR, reuse_address);
    CKPT_RESTORE_SOCKET_OPTION(SO_REUSEPORT, reuse_port);
    CKPT_RESTORE_SOCKET_OPTION(SO_KEEPALIVE, keepalive);
    CKPT_RESTORE_SOCKET_OPTION(SO_BROADCAST, broadcast);
    CKPT_RESTORE_SOCKET_OPTION(SO_LINGER, linger);
#undef CKPT_RESTORE_SOCKET_OPTION
    return 0;
}
