static int ckpt_prepare_restore_sockets(void) {
    g_nrestore_socket_endpoints = 0;
    g_nrestore_rights = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_SOCKETPAIR) continue;
            struct ckpt_restore_socket_endpoint *endpoint = ckpt_restore_socket_find(record.object_id);
            if (endpoint != NULL) {
                if (endpoint->peer_identity != record.auxiliary || endpoint->type != record.offset) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                endpoint->guest_present = 1;
                continue;
            }
            if (!record.object_id || !record.auxiliary ||
                (record.offset != SOCK_STREAM && record.offset != SOCK_DGRAM && record.offset != SOCK_SEQPACKET) ||
                ckpt_vector_reserve((void **)&g_restore_socket_endpoints, &g_restore_socket_endpoints_capacity,
                                    sizeof *g_restore_socket_endpoints, g_nrestore_socket_endpoints + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            endpoint = &g_restore_socket_endpoints[g_nrestore_socket_endpoints++];
            *endpoint = (struct ckpt_restore_socket_endpoint){
                .identity = record.object_id,
                .peer_identity = record.auxiliary,
                .fd = -1,
                .type = (int)record.offset,
                .guest_present = 1,
            };
            char state_path[1400];
            snprintf(state_path, sizeof state_path, "socket-state.%016llx", (unsigned long long)record.object_id);
            if (ckpt_source_load(state_path, &endpoint->state, sizeof endpoint->state) != 0 ||
                endpoint->state.magic != CKPT_SOCKET_STATE_MAGIC ||
                endpoint->state.local_size > sizeof endpoint->state.local) {
                ckpt_source_fclose(file);
                return -1;
            }
            endpoint->state_loaded = 1;
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (ckpt_restore_socket_find(endpoint->peer_identity) != NULL) continue;
        uint64_t identity = endpoint->identity;
        uint64_t peer_identity = endpoint->peer_identity;
        int type = endpoint->type;
        if (ckpt_vector_reserve((void **)&g_restore_socket_endpoints, &g_restore_socket_endpoints_capacity,
                                sizeof *g_restore_socket_endpoints, g_nrestore_socket_endpoints + 1) != 0)
            return -1;
        struct ckpt_restore_socket_endpoint *peer = &g_restore_socket_endpoints[g_nrestore_socket_endpoints++];
        *peer = (struct ckpt_restore_socket_endpoint){
            .identity = peer_identity,
            .peer_identity = identity,
            .fd = -1,
            .type = type,
        };
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (endpoint->fd >= 0) continue;
        struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
        if (peer == NULL || peer->peer_identity != endpoint->identity || peer->type != endpoint->type) return -1;
        int pair[2];
        int host_type = endpoint->type == SOCK_SEQPACKET ? SOCK_DGRAM : endpoint->type;
        if (socketpair(AF_UNIX, host_type, 0, pair) != 0) return -1;
        endpoint->fd = hl_host_process_fd_private_adopt(pair[0]);
        peer->fd = hl_host_process_fd_private_adopt(pair[1]);
        if (endpoint->fd < 0 || peer->fd < 0) {
            if (endpoint->fd >= 0) {
                hl_host_process_fd_private_remove(endpoint->fd);
                close(endpoint->fd);
            } else
                close(pair[0]);
            if (peer->fd >= 0) {
                hl_host_process_fd_private_remove(peer->fd);
                close(peer->fd);
            } else
                close(pair[1]);
            return -1;
        }
        (void)hl_native_set_no_sigpipe(endpoint->fd);
        (void)hl_native_set_no_sigpipe(peer->fd);
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (endpoint->state_loaded && ckpt_restore_socket_options(endpoint->fd, &endpoint->state) != 0) return -1;
    }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index)
        if (g_restore_socket_endpoints[index].guest_present &&
            ckpt_restore_socket_queue_load(&g_restore_socket_endpoints[index]) != 0) {
            fprintf(stderr, "[restore] socket queue load failed endpoint=%016llx\n",
                    (unsigned long long)g_restore_socket_endpoints[index].identity);
            return -1;
        }
    for (int index = 0; index < g_nrestore_socket_endpoints; ++index) {
        struct ckpt_restore_socket_endpoint *endpoint = &g_restore_socket_endpoints[index];
        if (!endpoint->peer_closed) continue;
        struct ckpt_restore_socket_endpoint *peer = ckpt_restore_socket_find(endpoint->peer_identity);
        if (peer == NULL) return -1;
        if (peer->guest_present) {
            endpoint->peer_closed = 0;
            continue;
        }
        if (peer->fd >= 0) {
            hl_host_process_fd_private_remove(peer->fd);
            close(peer->fd);
            peer->fd = -1;
        }
    }
    return 0;
}

static int ckpt_socket_state_is_bound(const struct ckpt_socket_state *state) {
    if (state->host_family == AF_UNIX) {
        const struct sockaddr_un *address = (const void *)&state->local;
        return state->local_size > offsetof(struct sockaddr_un, sun_path) &&
               (address->sun_path[0] != 0 || state->local_size > offsetof(struct sockaddr_un, sun_path) + 1u);
    }
    if (state->host_family == AF_INET) return ((const struct sockaddr_in *)&state->local)->sin_port != 0;
    if (state->host_family == AF_INET6) return ((const struct sockaddr_in6 *)&state->local)->sin6_port != 0;
    return 0;
}

static int ckpt_prepare_restore_socket_states(void) {
    g_nrestore_sockets = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char records_path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(records_path, sizeof records_path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *records = ckpt_source_fopen(records_path);
        if (!records) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(records, &record) == 0) {
            if (record.kind != CKF_SOCKET || ckpt_restore_socket_state_find(record.object_id) != NULL) continue;
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_sockets, &g_restore_sockets_capacity,
                                                         sizeof *g_restore_sockets, g_nrestore_sockets + 1) != 0) {
                ckpt_source_fclose(records);
                return -1;
            }
            struct ckpt_restore_socket *socket_state = &g_restore_sockets[g_nrestore_sockets++];
            *socket_state = (struct ckpt_restore_socket){.identity = record.object_id, .fd = -1};
            char state_path[1400];
            snprintf(state_path, sizeof state_path, "%s", record.path);
            if (ckpt_source_load(state_path, &socket_state->state, sizeof socket_state->state) != 0 ||
                socket_state->state.magic != CKPT_SOCKET_STATE_MAGIC ||
                socket_state->state.local_size > sizeof socket_state->state.local)
                return -1;
        }
        if (!feof(records)) {
            ckpt_source_fclose(records);
            return -1;
        }
        ckpt_source_fclose(records);
    }
    for (int index = 0; index < g_nrestore_sockets; ++index) {
        struct ckpt_restore_socket *saved = &g_restore_sockets[index];
        struct ckpt_socket_state *state = &saved->state;
        if (state->host_family == AF_UNIX &&
            (state->udp_local_port != 0 || state->lo_port != 0 || state->br_port != 0)) {
            char virtual_path[200];
            if (state->udp_local_port != 0) {
                if (state->udp_local_interface != 0) {
                    if (br_path((int)state->udp_local_interface - 1, state->udp_local_ip,
                                (uint16_t)state->udp_local_port, virtual_path, sizeof virtual_path) != 0)
                        return -1;
                } else {
                    lo_path((uint16_t)state->udp_local_port, virtual_path, sizeof virtual_path);
                }
            } else if (state->br_port != 0) {
                if (br_path((int)state->br_interface - 1, state->br_ip, (uint16_t)state->br_port, virtual_path,
                            sizeof virtual_path) != 0)
                    return -1;
            } else {
                lo_tcp_path((uint16_t)state->lo_port, state->lo_v6only, virtual_path, sizeof virtual_path);
            }
            struct sockaddr_un address;
            if (unix_addr_set(&address, virtual_path) != 0) return -1;
            memset(&state->local, 0, sizeof state->local);
            memcpy(&state->local, &address, sizeof address);
            state->local_size = sizeof address;
        }
        int fd = socket((int)state->host_family, (int)state->type, (int)state->protocol);
        if (fd < 0) {
            fprintf(stderr, "[restore] socket %016llx create family=%u type=%u protocol=%u: %s\n",
                    (unsigned long long)saved->identity, state->host_family, state->type, state->protocol,
                    strerror(errno));
            return -1;
        }
        (void)hl_native_set_no_sigpipe(fd);
        if (ckpt_restore_socket_options(fd, state) != 0) {
            close(fd);
            return -1;
        }
        if (ckpt_socket_state_is_bound(state)) {
            if (state->host_family == AF_UNIX) {
                struct sockaddr_un *address = (void *)&state->local;
                if (address->sun_path[0] == '/' && unix_path_routed(address->sun_path)) {
                    char host[1024];
                    if (g_rootfs)
                        overlay_copyup(address->sun_path, host, sizeof host);
                    else
                        xlate(address->sun_path, host, sizeof host);
                    unlink(host);
                    if (unix_sock_at(fd, host, 0) != 0) {
                        close(fd);
                        return -1;
                    }
                    goto socket_bound;
                }
                if (address->sun_path[0] != 0) unlink(address->sun_path);
            }
            if (bind(fd, (struct sockaddr *)&state->local, (socklen_t)state->local_size) != 0) {
                fprintf(stderr, "[restore] socket %016llx bind failed: %s\n", (unsigned long long)saved->identity,
                        strerror(errno));
                close(fd);
                return -1;
            }
        socket_bound:;
        }
        if (state->listening && listen(fd, state->backlog) != 0) {
            fprintf(stderr, "[restore] socket %016llx listen backlog=%d failed: %s\n",
                    (unsigned long long)saved->identity, state->backlog, strerror(errno));
            close(fd);
            return -1;
        }
        saved->fd = hl_host_process_fd_private_adopt(fd);
        if (saved->fd < 0) {
            close(fd);
            return -1;
        }
    }
    return 0;
}

static int ckpt_prepare_restore_eventfds(void) {
    g_nrestore_eventfds = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_EVENTFD) continue;
            struct ckpt_restore_eventfd *object = ckpt_restore_eventfd_find(record.object_id);
            if (object) {
                if (object->count != record.auxiliary || object->semaphore != (record.offset != 0) ||
                    object->guest_nonblock != ((record.flags & O_NONBLOCK) != 0)) {
                    ckpt_source_fclose(file);
                    return -1;
                }
                continue;
            }
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_eventfds, &g_restore_eventfds_capacity,
                                                         sizeof *g_restore_eventfds, g_nrestore_eventfds + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            object = &g_restore_eventfds[g_nrestore_eventfds++];
            *object = (struct ckpt_restore_eventfd){
                .identity = record.object_id,
                .count = record.auxiliary,
                .reader = -1,
                .writer = -1,
                .slot = (int)((record.object_id & UINT64_C(0xffffffff)) - 1),
                .semaphore = record.offset != 0,
                .guest_nonblock = (record.flags & O_NONBLOCK) != 0,
            };
            if (object->slot < 0 || object->slot >= HL_NFD) {
                ckpt_source_fclose(file);
                return -1;
            }
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    for (int i = 0; i < g_nrestore_eventfds; i++) {
        int pair[2];
        if (pipe(pair) != 0) return -1;
        int flags = fcntl(pair[0], F_GETFL);
        if (flags < 0 || fcntl(pair[0], F_SETFL, flags | O_NONBLOCK) != 0) {
            close(pair[0]);
            close(pair[1]);
            return -1;
        }
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
        g_restore_eventfds[i].reader = reader;
        g_restore_eventfds[i].writer = writer;
        if (g_restore_eventfds[i].count != 0) {
            char byte = 1;
            if (write(writer, &byte, 1) != 1) return -1;
        }
    }
    return 0;
}

static int ckpt_prepare_restore_signalfds(void) {
    g_nrestore_signalfds = 0;
    for (int process = 0; process < g_nrprocs; ++process) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_SIGNALFD || ckpt_restore_signalfd_find(record.object_id)) continue;
            if (ckpt_restore_right_prepare(&record) < 0) {
                ckpt_source_fclose(file);
                return -1;
            }
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    g_nrestore_rights = 0;
    return 0;
}

static int ckpt_prepare_restore_timerfds(void) {
    g_nrestore_timerfds = 0;
    for (int process = 0; process < g_nrprocs; process++) {
        char path[1300];
        if (!g_rprocs[process].viable) continue;
        snprintf(path, sizeof path, "proc.%d/fds", g_rprocs[process].gpid);
        FILE *file = ckpt_source_fopen(path);
        if (!file) return -1;
        struct ckpt_fd record;
        while (ckpt_rd_fd(file, &record) == 0) {
            if (record.kind != CKF_TIMERFD || ckpt_restore_timerfd_find(record.object_id)) continue;
            if (!record.object_id || ckpt_vector_reserve((void **)&g_restore_timerfds, &g_restore_timerfds_capacity,
                                                         sizeof *g_restore_timerfds, g_nrestore_timerfds + 1) != 0) {
                ckpt_source_fclose(file);
                return -1;
            }
            int clock_id = 0;
            unsigned first = 0;
            unsigned long long pending = 0;
            long long captured_ns = 0;
            if (sscanf(record.path, "%d %llu %u %lld", &clock_id, &pending, &first, &captured_ns) != 4) {
                ckpt_source_fclose(file);
                return -1;
            }
            struct timerfd_shared_state *state =
                mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
            if (state == MAP_FAILED) {
                ckpt_source_fclose(file);
                return -1;
            }
            memset(state, 0, sizeof *state);
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t deadline = record.offset;
            int64_t interval = (int64_t)record.auxiliary;
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
                .identity = record.object_id,
                .state = state,
                .clock_id = clock_id,
                .fd = -1,
                .slot = -1,
                .first_oneshot = (uint8_t)(first != 0),
            };
        }
        if (!feof(file)) {
            ckpt_source_fclose(file);
            return -1;
        }
        ckpt_source_fclose(file);
    }
    return 0;
}

static int ckpt_restore_eventfds_initialize(void) {
    if (g_nrestore_eventfds != 0 && !g_eventfd_count) return -1;
    for (int i = 0; i < g_nrestore_eventfds; i++)
        g_eventfd_count[g_restore_eventfds[i].slot] = g_restore_eventfds[i].count;
    return 0;
}

// The guest group (guest pgid; 1 == init) that owned the controlling terminal's foreground at checkpoint,
// carried from the manifest so the restored init can publish the handoff after every group exists.
static int g_ckpt_fg_gpid = 0;
static pid_t ckpt_restore_live_pid(int guest);

static int ckpt_restore_ctty_open(void) {
    int fd = open("/dev/tty", O_RDWR | O_NOCTTY | O_CLOEXEC);
    return fd >= 0 ? fd : ckpt_ctty_open();
}

// The restored init owns the controlling terminal and performs the handoff only after every descendant has
// rebuilt its process group and reached the publication barrier. A descendant cannot reliably open the
// launcher's controlling tty during recovery; ignoring that failure leaves terminal SIGINT aimed at init.
static int ckpt_claim_tty_fg(int guest_group) {
    if (guest_group <= 0) return 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (hl_option_get("HL_CKPT_TEST_FAIL_TTY_MASK") != NULL) {
        errno = EIO;
        return -1;
    }
#endif
    int tf = ckpt_restore_ctty_open();
    if (tf < 0) return -1;
    sigset_t sv, bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGTTOU);
    if (sigprocmask(SIG_BLOCK, &bl, &sv) != 0) {
        int error = errno;
        ckpt_ctty_close(tf);
        errno = error;
        return -1;
    }
    pid_t group = ckpt_restore_live_pid(guest_group);
    int result = group > 0 && tcsetpgrp(tf, group) == 0 && tcgetpgrp(tf) == group ? 0 : -1;
    int error = result == 0 ? 0 : errno;
    if (result != 0 && error == 0) error = EIO;
    if (sigprocmask(SIG_SETMASK, &sv, NULL) != 0 && result == 0) {
        result = -1;
        error = errno;
    }
    ckpt_ctty_close(tf);
    if (result != 0) errno = error;
    return result;
}

// Replay the captured line discipline onto the fresh pty. The restored guest keeps its in-memory belief about
// the terminal (readline's "already prepared" flag), so a mode it set before the capture has to be there when
// it resumes. SIGTTOU is blocked: the init is not yet the foreground group at this point. Best effort -- a
// non-tty or an unsettable mode just leaves the launcher's default cooked terminal.
static int ckpt_restore_tty_mode(const struct ckpt_manifest *man) {
    struct termios tio;
    int tf = man->tty_termios ? ckpt_restore_ctty_open() : -1;
    if (tf < 0 || tcgetattr(tf, &tio) != 0) {
        ckpt_ctty_close(tf);
        return 0;
    }
    size_t cc = sizeof tio.c_cc < sizeof man->tty_cc ? sizeof tio.c_cc : sizeof man->tty_cc;
    tio.c_iflag = (tcflag_t)man->tty_iflag;
    tio.c_oflag = (tcflag_t)man->tty_oflag;
    tio.c_cflag = (tcflag_t)man->tty_cflag;
    tio.c_lflag = (tcflag_t)man->tty_lflag;
    memcpy(tio.c_cc, man->tty_cc, cc);
    (void)cfsetispeed(&tio, (speed_t)man->tty_ispeed);
    (void)cfsetospeed(&tio, (speed_t)man->tty_ospeed);
    sigset_t sv, bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGTTOU);
    if (sigprocmask(SIG_BLOCK, &bl, &sv) != 0) {
        ckpt_ctty_close(tf);
        return -1;
    }
    (void)tcsetattr(tf, TCSANOW, &tio);
    int result = sigprocmask(SIG_SETMASK, &sv, NULL);
    ckpt_ctty_close(tf);
    return result;
}

// Reconstruct this process's group/session relative to the LIVE (re-forked) tree. Idempotent verification
// accepts a relation already inherited from the parent, but a requested relation that cannot be established
// fails this process's atomic restore instead of publishing a tree with misdirected terminal signals.
static int ckpt_restore_pgrp(int gpid, int pgid_gpid, int sid_gpid);

struct ckpt_restore_commit {
    _Atomic int decision;
    _Atomic int ready;
    _Atomic int failed;
    _Atomic int released;
    int processes;
    _Atomic pid_t pids[];
};

enum { CKPT_FUTEX_WAIT = 0, CKPT_FUTEX_WAKE = 1 };

static struct ckpt_restore_commit *g_restore_commit;
static size_t g_restore_commit_size;
static int ckpt_restore_process_index(int gpid);

static pid_t ckpt_restore_live_pid(int guest) {
    if (guest == 1) return g_init_hostpid;
    int index = ckpt_restore_process_index(guest);
    if (g_restore_commit == NULL || index < 0) {
        errno = ESRCH;
        return -1;
    }
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) return -1;
    deadline.tv_sec += 10;
    for (;;) {
        pid_t host = atomic_load_explicit(&g_restore_commit->pids[index], memory_order_acquire);
        if (host > 0) {
            (void)hl_linux_pidmap_add(&g_pidmap, guest, (int)host);
            return host;
        }
        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 || now.tv_sec > deadline.tv_sec ||
            (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
            errno = ETIMEDOUT;
            return -1;
        }
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
        (void)nanosleep(&pause, NULL);
    }
}

static int ckpt_restore_pgrp(int gpid, int pgid_gpid, int sid_gpid) {
    if (sid_gpid == gpid && getsid(0) != getpid() && setsid() < 0 && getsid(0) != getpid()) return -1;
    if (pgid_gpid <= 0) return 0;
    pid_t leader = pgid_gpid == gpid ? getpid() : ckpt_restore_live_pid(pgid_gpid);
    if (leader <= 0) return -1;
    if (getpgrp() == leader) return 0;

    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) return -1;
    deadline.tv_sec += 10;
    for (;;) {
        if (setpgid(0, leader) == 0 || getpgrp() == leader) return 0;
        int error = errno;
        struct timespec now;
        if ((error != EPERM && error != EACCES && error != ESRCH) || clock_gettime(CLOCK_MONOTONIC, &now) != 0 ||
            now.tv_sec > deadline.tv_sec || (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
            errno = error;
            return -1;
        }
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
        (void)nanosleep(&pause, NULL);
    }
}

static int ckpt_restore_commit_futex(_Atomic int *word, int operation, int value, const struct timespec *timeout) {
#if defined(_WIN32)
    /* Checkpoint control is rejected by the Windows engine boundary.  Keep the
     * Linux guest translation unit portable without manufacturing a partial
     * restore implementation: creation below fails before this can be reached. */
    (void)word;
    (void)operation;
    (void)value;
    (void)timeout;
    errno = ENOSYS;
    return -1;
#elif defined(__linux__)
    return (int)syscall(SYS_futex, word, operation, value, timeout, NULL, 0);
#else
    /* The restore protocol always rechecks its shared atomic after a wait and
     * treats wake as a hint.  Hosts without futex can therefore poll at a
     * bounded interval without weakening the publication barrier. */
    (void)word;
    (void)value;
    if (operation == CKPT_FUTEX_WAIT) {
        const struct timespec interval = {.tv_sec = 0, .tv_nsec = 10000000};
        (void)nanosleep(timeout != NULL ? timeout : &interval, NULL);
    }
    return 0;
#endif
}

static int ckpt_restore_process_index(int gpid) {
    for (int index = 0; index < g_nrprocs; ++index)
        if (g_rprocs[index].gpid == gpid) return index;
    return -1;
}

static int ckpt_restore_commit_create(void) {
#if defined(_WIN32)
    errno = ENOTSUP;
    return -1;
#else
    if ((size_t)g_nrprocs > (SIZE_MAX - sizeof(struct ckpt_restore_commit)) / sizeof(pid_t)) return -1;
    g_restore_commit_size = sizeof(struct ckpt_restore_commit) + (size_t)g_nrprocs * sizeof(pid_t);
    g_restore_commit = mmap(NULL, g_restore_commit_size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (g_restore_commit == MAP_FAILED) {
        g_restore_commit = NULL;
        return -1;
    }
    g_restore_commit->processes = g_nrprocs;
    int root = ckpt_restore_process_index(1);
    if (root >= 0) atomic_store_explicit(&g_restore_commit->pids[root], getpid(), memory_order_release);
    return 0;
#endif
}

static void ckpt_restore_commit_destroy(void) {
    if (g_restore_commit != NULL) (void)munmap(g_restore_commit, g_restore_commit_size);
    g_restore_commit = NULL;
    g_restore_commit_size = 0;
}

static void ckpt_restore_commit_wake(void) {
    (void)ckpt_restore_commit_futex(&g_restore_commit->decision, CKPT_FUTEX_WAKE, INT_MAX, NULL);
}

static void ckpt_restore_commit_abort(void) {
    if (g_restore_commit == NULL) return;
    atomic_store_explicit(&g_restore_commit->decision, 2, memory_order_release);
    ckpt_restore_commit_wake();
    for (int index = 0; index < g_restore_commit->processes; ++index) {
        pid_t process = atomic_load_explicit(&g_restore_commit->pids[index], memory_order_acquire);
        if (process > 0 && process != getpid()) (void)kill(process, SIGKILL);
    }
    for (;;) {
        int status;
        pid_t child = waitpid(-1, &status, 0);
        if (child > 0) continue;
        if (child < 0 && errno == EINTR) continue;
        break;
    }
}

static void ckpt_restore_commit_failed(void) {
    if (g_restore_commit != NULL) {
        atomic_fetch_add_explicit(&g_restore_commit->failed, 1, memory_order_release);
        (void)ckpt_restore_commit_futex(&g_restore_commit->ready, CKPT_FUTEX_WAKE, INT_MAX, NULL);
    }
    _exit(70);
}

static void ckpt_restore_commit_wait(void) {
    atomic_fetch_add_explicit(&g_restore_commit->ready, 1, memory_order_release);
    (void)ckpt_restore_commit_futex(&g_restore_commit->ready, CKPT_FUTEX_WAKE, INT_MAX, NULL);
    for (;;) {
        int decision = atomic_load_explicit(&g_restore_commit->decision, memory_order_acquire);
        if (decision == 1) {
            atomic_fetch_add_explicit(&g_restore_commit->released, 1, memory_order_release);
            (void)ckpt_restore_commit_futex(&g_restore_commit->released, CKPT_FUTEX_WAKE, INT_MAX, NULL);
            return;
        }
        if (decision == 2) _exit(70);
        (void)ckpt_restore_commit_futex(&g_restore_commit->decision, CKPT_FUTEX_WAIT, 0, NULL);
    }
}

static int ckpt_restore_commit_publish(void) {
    int expected = -1; /* exclude the init process itself */
    for (int index = 0; index < g_nrprocs; ++index)
        expected += g_rprocs[index].viable;
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) return -1;
    deadline.tv_sec += 10;
    while (atomic_load_explicit(&g_restore_commit->ready, memory_order_acquire) < expected) {
        if (atomic_load_explicit(&g_restore_commit->failed, memory_order_acquire) != 0) return -1;
        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 || now.tv_sec > deadline.tv_sec ||
            (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec))
            return -1;
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 10000000};
        (void)ckpt_restore_commit_futex(&g_restore_commit->ready, CKPT_FUTEX_WAIT,
                                        atomic_load_explicit(&g_restore_commit->ready, memory_order_relaxed), &pause);
    }
    if (ckpt_claim_tty_fg(g_ckpt_fg_gpid) != 0) return -1;
    atomic_store_explicit(&g_restore_commit->decision, 1, memory_order_release);
    ckpt_restore_commit_wake();
    while (atomic_load_explicit(&g_restore_commit->released, memory_order_acquire) < expected) {
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 10000000};
        (void)ckpt_restore_commit_futex(&g_restore_commit->released, CKPT_FUTEX_WAIT,
                                        atomic_load_explicit(&g_restore_commit->released, memory_order_relaxed),
                                        &pause);
    }
    return 0;
}

static void ckpt_restore_proc_run(int gpid); // fwd

// Re-fork every child of `gpid` (per the checkpoint ppid table); each child restores its own subtree and
// resumes. Records the checkpoint-gpid -> live-hostpid mapping so this process's guest pids resolve.
static void ckpt_fork_children(int gpid, struct cpu *parent) {
    for (int i = 0; i < g_nrprocs; i++) {
        if (!g_rprocs[i].viable || g_rprocs[i].ppid != gpid || g_rprocs[i].gpid == gpid) continue;
        int cg = g_rprocs[i].gpid;
        int source = cpu_tid(parent);
        if (!hl_target_task_event(parent, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)source, 0)) {
            fprintf(stderr, "[restore] runtime refused fork preparation for gpid %d\n", cg);
            continue;
        }
        pid_t p = hl_host_process_clone_current();
        if (p < 0) {
            (void)hl_target_task_event(parent, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)source, 0);
        } else if (!hl_target_task_event(parent, HL_TASK_EVENT_FORK_PROCESS, (uint64_t)cg, (uint64_t)source, p == 0)) {
            if (p == 0) _exit(127);
            int status;
            kill(p, SIGKILL);
            while (waitpid(p, &status, 0) < 0 && errno == EINTR) {}
            fprintf(stderr, "[restore] runtime refused restored child gpid %d\n", cg);
            continue;
        }
        if (p == 0) {
            ckpt_restore_proc_run(cg); // never returns
            _exit(0);
        } else if (p > 0) {
            (void)hl_linux_pidmap_add(&g_pidmap, cg, (int)p);
            int index = ckpt_restore_process_index(cg);
            if (g_restore_commit != NULL && index >= 0)
                atomic_store_explicit(&g_restore_commit->pids[index], p, memory_order_release);
        } else {
            fprintf(stderr, "[restore] fork for gpid %d failed: %s\n", cg, strerror(errno));
        }
    }
}

// Restore a re-forked CHILD process (runs in the fresh fork; the engine is already inited, inherited from the
// parent) and resume it. Never returns.
static void ckpt_restore_proc_run(int gpid) {
    char pd[1200];
    ckpt_restore_hold_tty_signals();
    snprintf(pd, sizeof pd, "proc.%d", gpid);
    struct ckpt_meta m;
    if (ckpt_read_meta_dir(pd, &m) != 0) ckpt_restore_commit_failed();
    if (ckpt_restore_filesystem_state(pd) != 0) ckpt_restore_commit_failed();

    // adopt our restored identity BEFORE any pid-reporting syscall or /proc publish
    g_self_gpid = m.self_gpid;
    g_self_gppid = m.ppid_gpid;

    // The cpu image is read from the store, not from guest RAM, so it is available before the memory restore
    // -- which fork_child_hooks needs, and which now has to run FIRST. See below.
    struct cpu c, *images = NULL;
    if (ckpt_restore_cpu_dir(pd, &m, &images) != 0 || ckpt_restore_leader(images, m.n_threads, &c) != 0)
        ckpt_restore_commit_failed();
    // BEFORE the memory restore, not after. jit_after_fork() inside this hook rebuilds the translated-code
    // arena at a fresh VA and UNMAPS the ~64MB pair inherited from the restoring parent -- and a guest
    // mapping's saved VA is an ordinary host mmap result, so the child's MAP_FIXED regions frequently land
    // INSIDE that inherited arena. Run after the restore, the release then punched the restored guest pages
    // back out: x86_64 checkpoint.threads died with a host SIGSEGV on the resumed peer's own stack
    // (si_addr == sp, pc at glibc's __syscall_cancel_arch_end).
    fork_child_hooks(&c); // shared after-fork engine reset (cache re-alias, kqueue rebuild, lock/threg/Mach)

    int trigger_detached = ckpt_trigger_detach_for_restore();
    if (trigger_detached < 0) ckpt_restore_commit_failed();

    // drop the COW-inherited parent guest memory + registries, then load our own
    /* The forked restorer inherited a COW copy of the parent's typed VMA ledger and
     * host mapping ownership. Release those handles before forgetting the generic
     * map registry, otherwise every restored child leaks its parent's mappings. */
    bound_mapping_reset();
    hl_logical_vma_global_reset_quiescent();
    hl_gmap_reset();
    g_nanonmap = 0;
    gna_reset();
    if (ckpt_restore_mem_dir(pd, &m) != 0) ckpt_restore_commit_failed();
    if (ckpt_trigger_reattach_after_restore(trigger_detached) != 0) ckpt_restore_commit_failed();

    ckpt_reinstall_sigacts(&m); // restore guest signal dispositions (AFTER the fork hooks reset host state)

    if (ckpt_restore_fds_dir(pd) != 0 || ckpt_restore_signal_state(pd) != 0) ckpt_restore_commit_failed();
    if (ckpt_restore_pgrp(gpid, m.pgid_gpid, m.sid_gpid) != 0) {
        fprintf(stderr, "[restore] cannot restore process group for gpid %d: %s\n", gpid, strerror(errno));
        ckpt_restore_commit_failed();
    }

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", m.exe_path);
    if (exe[0]) g_exe_path = exe;
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    ckpt_fork_children(gpid, &c); // re-fork our own children before we resume (so a wait finds them)
    if (thread_restore_group(images, (int)m.n_threads, &c) != 0) ckpt_restore_commit_failed();
    free(images);
    ckpt_restore_backings_close();
    ckpt_restore_pipe_seeds_close();
    ckpt_restore_eventfd_seeds_close();
    ckpt_restore_signalfd_seeds_close(); /* was omitted here (it is closed in
                                          * ckpt_restore_tree): every re-forked
                                          * process leaked its signalfd seed
                                          * reader+writer pair for its lifetime */
    ckpt_restore_socket_seeds_close();
    ckpt_restore_commit_wait();
    run_guest(&c);
    _exit(c.exit_code);
}

// Full restore driver: rebuild the whole tree from the store and resume it. The INIT (gpid 1) restores its
// RAM FIRST (before engine init, so MAP_FIXED lands on free VAs), then re-forks the tree.
static int ckpt_restore_tree(const char *rootfs) {
    struct ckpt_manifest man;
    ckpt_restore_hold_tty_signals();
    // Bind the image source before anything is read; every re-forked child inherits the binding.
    if (ckpt_source_bind() == NULL) {
        fprintf(stderr, "[restore] restore requested without a broker descriptor\n");
        return 2;
    }
    if (ckpt_read_manifest(&man) != 0) return 2;
    if (ckpt_scan_procs() != 0) {
        fprintf(stderr, "[restore] the store holds no process images\n");
        return 2;
    }
    if (ckpt_validate_proc_tree(&man) != 0) {
        fprintf(stderr, "[restore] process tree does not match manifest\n");
        return 2;
    }
    int recovery_policy = ckpt_recovery_policy();
    if (ckpt_restore_preflight(recovery_policy) != 0) return 2;
    if (ckpt_prepare_restore_pipes() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint pipe objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_eventfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint eventfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_timerfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint timerfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_signalfds() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint signalfd objects\n");
        return 2;
    }
    if (ckpt_prepare_restore_sockets() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint socketpair objects\n");
        return 2;
    }

    const char *ipd = "proc.1";
    struct ckpt_meta im;
    if (ckpt_read_meta_dir(ipd, &im) != 0) return 2;
    if (ckpt_restore_mem_dir(ipd, &im) != 0) {
        fprintf(stderr, "[restore] init memory restore failed\n");
        return 2;
    } // init RAM before any engine allocation

    container_init(rootfs); // sets g_init_hostpid = getpid() -> this process becomes guest pid 1
    // container_init establishes the rootfs and its default cwd; replay the captured process context after it
    // so neither the rootfs chdir nor HL_CWD can overwrite the checkpointed directory.
    if (ckpt_restore_filesystem_state(ipd) != 0) return 2;
    int irc = engine_global_init();
    if (irc) return irc;
    if (ckpt_prepare_restore_socket_states() != 0) {
        fprintf(stderr, "[restore] cannot rebuild checkpoint standalone sockets\n");
        return 2;
    }
    if (ckpt_restore_eventfds_initialize() != 0) {
        fprintf(stderr, "[restore] init eventfd state initialization failed\n");
        return 70;
    }

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", im.exe_path);
    if (exe[0]) g_exe_path = exe;
    if (ckpt_restore_fds_dir(ipd) != 0) {
        fprintf(stderr, "[restore] init descriptor restore failed\n");
        return 70;
    }
    struct cpu c, *images = NULL;
    if (ckpt_restore_cpu_dir(ipd, &im, &images) != 0 || ckpt_restore_leader(images, im.n_threads, &c) != 0) {
        fprintf(stderr, "[restore] init CPU restore failed\n");
        return 70;
    }
    ckpt_reinstall_sigacts(&im); // restore the init's guest signal dispositions (so ^C reaches bash's handler)
    if (ckpt_restore_signal_state(ipd) != 0) {
        fprintf(stderr, "[restore] init signal-state restore failed\n");
        return 70;
    }
    if (ckpt_restore_commit_create() != 0) {
        fprintf(stderr, "[restore] cannot create process-tree commit barrier\n");
        return 70;
    }
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    // Re-create the init's OWN process group. Restore re-forks the init under the launcher, so it starts in
    // the launcher's group, while at capture it led its own (an interactive shell's job-control setup does
    // setpgid(0,1), which it never repeats after resume). Uncorrected, the guest pgid 1 -> g_init_hostpid
    // translation names a group the shell is not in: its tcsetpgrp handoff after a foreground job puts the
    // terminal's foreground on that empty group, its next read on the ctty is a background read, and with
    // SIGTTIN blocked that read fails EIO -- readline reports EOF and the shell logs out after one line.
    // Only the GROUP is re-created: the session belongs to the launcher, which owns the pty.
    if (im.pgid_gpid == 1 && getpgrp() != getpid() && setpgid(0, 0) < 0 && getpgrp() != getpid()) {
        fprintf(stderr, "[restore] cannot restore init process group: %s\n", strerror(errno));
        return 70;
    }
    if (ckpt_restore_tty_mode(&man) != 0) {
        fprintf(stderr, "[restore] cannot restore terminal mode signal mask: %s\n", strerror(errno));
        return 70;
    }
    // Publish which guest group owned the tty foreground, so whichever re-forked process is that group's leader
    // claims the controlling terminal AFTER it re-creates its group (see ckpt_claim_tty_fg). Set before the fork
    // so every child inherits it. Without this the resumed tree's fg group defaults to the init's, and a tty
    // SIGINT hits the init instead of the foreground job -> the whole tree dies on ^C.
    g_ckpt_fg_gpid = man.fg_pgid_gpid;
    ckpt_fork_children(1, &c); // rebuild the tree BEFORE init runs (empty block map -> no stale translation)
    if (hl_option_get("HL_CKPT_TEST_FAIL_AFTER_FORK") != NULL) {
        fprintf(stderr, "[restore] injected post-fork restore failure\n");
        ckpt_restore_commit_abort();
        ckpt_restore_commit_destroy();
        free(images);
        return 70;
    }
    if (thread_restore_group(images, (int)im.n_threads, &c) != 0) {
        fprintf(stderr, "[restore] init thread-group restore failed\n");
        ckpt_restore_commit_abort();
        ckpt_restore_commit_destroy();
        free(images);
        return 70;
    }
    if (ckpt_restore_commit_publish() != 0) {
        fprintf(stderr, "[restore] restored descendants did not reach the commit barrier\n");
        ckpt_restore_commit_abort();
        ckpt_restore_commit_destroy();
        free(images);
        return 70;
    }
    free(images);
    ckpt_restore_backings_close();
    ckpt_restore_pipe_seeds_close();
    ckpt_restore_eventfd_seeds_close();
    ckpt_restore_signalfd_seeds_close();
    ckpt_restore_socket_seeds_close();
    ckpt_restore_commit_destroy();
    if (ckpt_stream_recovery_complete() != 0) {
        fprintf(stderr, "[restore] cannot close recovery publication scope\n");
        return 70;
    }

    run_guest(&c);
    return c.exit_code;
}
