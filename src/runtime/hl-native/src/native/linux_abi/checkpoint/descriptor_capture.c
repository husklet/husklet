static int ckpt_pipe_end_drains(int flags) {
    return (flags & O_ACCMODE) != O_WRONLY;
}

// Capture the bytes in flight in a shared anonymous pipe.
//
// The only way to observe a pipe's buffered bytes is to read them, which removes them. That is safe here
// because it is one half of a closed round trip: every byte read lands in the image object
// "pipe.<identity>", and ckpt_prepare_restore_pipes() writes exactly those bytes back into the freshly
// created pipe (after restoring its capacity with F_SETPIPE_SZ) before any guest process is reforked. The
// capture therefore never loses data on a checkpoint that completes, and a capture that cannot complete
// must fail the whole checkpoint rather than publish a short object -- which is why every error path below
// aborts the stream and returns -1 instead of finishing what it has.
//
// WHAT MAKES THE ROUND TRIP CLOSED, AND WHAT DOES NOT.  "Closed" is a claim about every process that can
// touch this pipe, not just this one, and it is only true under the freeze that ckpt_dump_self establishes:
// a member holds g_ckpt_barrier_active, its whole thread stop-the-world and g_stw_reg_lock from before its
// dump until the coordinator releases it (image.c, HL_CKPT_OP_RELEASE_WAIT), so no released member can run
// guest code -- and therefore cannot write into this pipe -- for the rest of the capture. Before that
// change a member _exit()ed as soon as its own dump finished and its siblings ran on, so a sibling could
// write into the pipe after the drain and those bytes were lost from an image that reported success. That
// is the defect 935dae440 diagnosed correctly.
//
// The residual obligation, stated rather than assumed: the freeze closes the window AFTER a member reaches
// its safepoint, not before. Members reach ckpt_dump_self independently, so a member that has not yet
// converged its stop-the-world can still write into a pipe another member has already drained. Closing that
// needs the drain held behind a broker-side "every sealed member has passed REGISTER_READY" barrier -- F4 in
// the recovery plan, broker work, and not expressible from C on this tree. Every byte written in that window
// is lost, exactly as it was before; what the park removes is the far larger window that used to run from a
// member's own dump to the end of the whole tree's capture.
//
// Two properties the drain must not damage in the live process, since a checkpoint is not required to be
// the process's last act:
//   - O_NONBLOCK lives on the open file description, so even temporarily setting it is observable by every
//     process that inherited this pipe end through fork. Snapshot the buffered byte count and read exactly
//     that many bytes instead; a capture participant must never publish the drain's implementation detail as
//     guest state while another participant is recording the same shared description.
//   - the identity is claimed image-wide, so exactly one participant drains a pipe several processes hold.
//     Every OTHER holder returns 0 immediately and publishes its own record; the records agree by
//     construction, because everything in them is derived from the identity and from the shared open file
//     description, not from who won.
//
// `reason` receives a short static description of the first failing step; the caller reports it, because a
// bare "cannot capture pipe" hides whether the sink, the descriptor, or the read failed.
static int ckpt_capture_pipe_reason(int fd, uint64_t identity, const char **reason, int *cause) {
    const char *unused_reason = NULL;
    int unused_cause = 0;
    if (!reason) reason = &unused_reason;
    if (!cause) cause = &unused_cause;
    *reason = NULL;
    *cause = 0;
    // Admission pass: everything this pipe can be refused for -- drainability, a valid identity, a readable
    // capacity -- is decided by the caller before it gets here, and none of it needs a byte to leave the
    // kernel. Take neither the claim nor the drain until the whole descriptor set has been admitted.
    if (g_ckpt_admission_only) return 0;
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "pipe.%016llx", (unsigned long long)identity);
    errno = 0;
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed > 0) {
        // A co-holder of THIS pipe won the election and is draining, or has already drained, the shared
        // kernel buffer. The bytes leave the pipe for every holder at once, so this process's guest view is
        // damaged exactly as much as the winner's and falls under the SAME abort contract. Marking only the
        // winner let a loser resume out of its park onto a silently emptied pipe whenever the capture was
        // abandoned after the drain -- and an inherited pipe is held by many processes at once, which is the
        // postmaster/backend shape this whole path exists for: one identity, six holders, one winner and
        // five processes that would have gone back to running.
        g_ckpt_capture_destructive = 1;
        return 0;
    }
    if (claimed < 0) {
        // A negative claim is TWO different failures and they were reported as one. The sink may have
        // answered and declined the name (a decision -- errno is whatever an unrelated earlier call left
        // behind, commonly EPIPE from an interrupt_channels teardown, which printed as "(Broken pipe)" and
        // read like a defect in the pipe being captured). Or the channel itself failed, in which case errno
        // is the transport's and worth printing. errno is cleared immediately before the call, so a
        // still-zero errno means the sink decided rather than the transport broke.
        *cause = errno;
        *reason = *cause == 0 ? "the sink declined the image-wide claim for this pipe identity"
                              : "the transport carrying the image-wide claim failed";
        return -1;
    }
    g_ckpt_capture_destructive = 1; // winning the claim makes this process the one that CONSUMES the pipe
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) {
        *reason = "sink refused to open the pipe object";
        *cause = errno;
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    int remaining = -1;
    if (ioctl(fd, FIONREAD, &remaining) != 0 || remaining < 0) {
        *reason = "cannot inspect the pipe's buffered byte count";
        *cause = errno;
        ckpt_sink_abort(sink, &output);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    unsigned char buffer[65536];
    int failed = 0;
    while (remaining > 0) {
        size_t requested = (size_t)remaining < sizeof buffer ? (size_t)remaining : sizeof buffer;
        ssize_t count = read(fd, buffer, requested);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) {
                // The bytes are already out of the pipe and the object is being discarded, so the image can
                // no longer describe this pipe: fail the checkpoint rather than restore a truncated one.
                *reason = "sink rejected buffered pipe bytes";
                *cause = errno;
                failed = 1;
                break;
            }
            remaining -= (int)count;
            continue;
        }
        if (errno == EINTR) continue;
        *reason = "read of the buffered pipe bytes failed";
        *cause = errno;
        failed = 1;
        break;
    }
    if (failed) {
        ckpt_sink_abort(sink, &output);
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    if (ckpt_sink_finish(sink, &output) != 0) {
        *reason = "sink refused to publish the pipe object";
        *cause = errno;
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    return 0;
}

static int ckpt_capture_pipe(int fd, uint64_t identity) {
    return ckpt_capture_pipe_reason(fd, identity, NULL, NULL);
}

// The restore side recreates the pipe with F_SETPIPE_SZ and refills it, and refuses any capacity it cannot
// parse, so the capacity written into the record must be the live kernel capacity of this pipe rather than
// the engine's cached g_pipesz, which is 0 for a pipe the engine never resized.
static int ckpt_pipe_capacity(int fd) {
    int cached = (fd >= 0 && fd < HL_NFD) ? g_pipesz[fd] : 0;
#ifdef F_GETPIPE_SZ
    int live = fcntl(fd, F_GETPIPE_SZ);
    if (live > 0) return live;
#endif
    return cached;
}

static int ckpt_capture_signalfd(int fd, uint64_t identity) {
    // Draining a signalfd removes the queued siginfo records from the task, so it belongs to pass 2. The
    // arm's refusals (slot bounds, a minted identity, the descriptor flags) are all decided by the caller.
    if (g_ckpt_admission_only) return 0;
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "signalfd.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        ckpt_sink_abort(sink, &output);
        return -1;
    }
    unsigned char buffer[4096];
    int failed = 0;
    for (;;) {
        ssize_t count = read(fd, buffer, sizeof buffer);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) failed = 1;
            if (failed) break;
            continue;
        }
        if (count == 0 || HL_HOST_ERRNO_WOULD_BLOCK(errno)) break;
        if (errno == EINTR) continue;
        failed = 1;
        break;
    }
    if (failed) {
        ckpt_sink_abort(sink, &output);
        return -1;
    }
    return ckpt_sink_finish(sink, &output);
}

static int ckpt_capture_socket_queue(int fd, uint64_t identity, uint32_t type) {
    // recvmsg(MSG_DONTWAIT) below empties the receive queue, and MSG_PEEK is not an alternative: it installs
    // a fresh descriptor for every in-flight SCM_RIGHTS. Pass 2 only.
    if (g_ckpt_admission_only) return 0;
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "socket.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    g_ckpt_capture_destructive = 1; // this process drains the receive queue; the bytes leave the kernel
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    struct ckpt_socket_queue_header header = {CKPT_SOCKET_QUEUE_MAGIC, type, 0};
    if (ckpt_sink_write(sink, output, &header, sizeof header) != 0) goto fail;
    size_t capacity = 1u << 20;
    unsigned char *payload = malloc(capacity);
    if (payload == NULL) goto fail;
    for (;;) {
        unsigned char control[4096];
        struct iovec iov = {payload, capacity};
        struct msghdr message;
        memset(&message, 0, sizeof message);
        message.msg_iov = &iov;
        message.msg_iovlen = 1;
        message.msg_control = control;
        message.msg_controllen = sizeof control;
        ssize_t received = recvmsg(fd, &message, MSG_DONTWAIT);
        if (received < 0 && errno == EINTR) continue;
        if (received < 0 && HL_HOST_ERRNO_WOULD_BLOCK(errno)) break;
        if (received < 0 && errno == ECONNRESET && type != SOCK_STREAM) {
            header.peer_closed = 1;
            break;
        }
        if (received < 0 || (message.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) != 0) {
            fprintf(stderr, "[ckpt] socket queue %016llx recv failed: n=%lld errno=%d flags=%x control=%zu\n",
                    (unsigned long long)identity, (long long)received, errno, message.msg_flags,
                    (size_t)message.msg_controllen);
            free(payload);
            goto fail;
        }
        if (received == 0 && type == SOCK_STREAM) {
            /* End of the readable stream, and nothing more: NOT a statement that the peer closed.  On Linux
             * this same 0 is returned when the peer did shutdown(SHUT_WR) and is still open, and when this
             * end did shutdown(SHUT_RD) itself.  Whether the far end still exists is decided on restore by
             * whether any restored process holds it, and the half-close directions are carried explicitly
             * in each endpoint's own socket-state record. */
            break;
        }
        struct ckpt_fd rights[253];
        uint32_t nrights = 0;
        for (struct cmsghdr *control_message = CMSG_FIRSTHDR(&message); control_message != NULL;
             control_message = CMSG_NXTHDR(&message, control_message)) {
            if (control_message->cmsg_level != SOL_SOCKET || control_message->cmsg_type != SCM_RIGHTS ||
                control_message->cmsg_len < CMSG_LEN(0)) {
                fprintf(stderr, "[ckpt] socket queue %016llx has unsupported ancillary type\n",
                        (unsigned long long)identity);
                free(payload);
                goto fail;
            }
            size_t bytes = (size_t)control_message->cmsg_len - CMSG_LEN(0);
            int *fds = (int *)CMSG_DATA(control_message);
            int count = (int)(bytes / sizeof(int));
            int visible = cmsg_import_ofd_trailer(fds, count);
            visible = cmsg_import_signalfd_trailer(fds, visible);
            visible = cmsg_import_kqueue_trailer(fds, visible);
            visible = cmsg_import_pipe_trailer(fds, visible);
            visible = cmsg_import_memfd_trailer(fds, visible);
            visible = cmsg_import_timerfd_trailer(fds, visible);
            visible = cmsg_import_eventfd_trailer(fds, visible);
            visible = cmsg_import_seq_trailer(fds, visible);
            if (nrights + (uint32_t)visible > 253) {
                for (int index = 0; index < visible; ++index)
                    close(fds[index]);
                free(payload);
                goto fail;
            }
            for (int index = 0; index < visible; ++index) {
                cmsg_note_recv_sock_fd(fds[index]);
                if (ckpt_capture_right_resource(fds[index], &rights[nrights]) != 0) {
                    fprintf(stderr, "[ckpt] socket queue %016llx has unsupported SCM_RIGHTS fd\n",
                            (unsigned long long)identity);
                    for (int rest = index; rest < visible; ++rest)
                        close(fds[rest]);
                    free(payload);
                    goto fail;
                }
                ckpt_release_captured_right(fds[index]);
                close(fds[index]);
                nrights++;
            }
        }
        struct ckpt_socket_queue_frame frame = {(uint32_t)received, nrights};
        if ((uint64_t)received > UINT32_MAX || ckpt_sink_write(sink, output, &frame, sizeof frame) != 0 ||
            ckpt_sink_write(sink, output, payload, (size_t)received) != 0 ||
            (nrights && ckpt_sink_write(sink, output, rights, (size_t)nrights * sizeof rights[0]) != 0)) {
            free(payload);
            goto fail;
        }
    }
    free(payload);
    // peer_closed is only known after the drain loop: patch the header that was emitted first.
    if (ckpt_sink_write_at(sink, output, 0, &header, sizeof header) != 0) goto fail;
    if (ckpt_sink_finish(sink, &output) != 0) {
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    return 0;
fail:
    ckpt_sink_abort(sink, &output);
    ckpt_sink_unclaim(sink, name);
    return -1;
}

static int ckpt_socket_option_int(int fd, int option, int *value) {
    socklen_t size = sizeof(*value);
    *value = 0;
    return getsockopt(fd, SOL_SOCKET, option, value, &size);
}

static int ckpt_recovery_permissive_requested(void);

// Every way an unpaired socket can be refused, evaluated without claiming a name, publishing a byte, or
// touching the socket's queues. Pass 1 calls it alone; pass 2 calls it inside the claim so the same
// decision is re-derived rather than inherited across the two passes.
static int ckpt_socket_state_admit(int fd, int require_quiescent, int degraded_connection) {
    if (!require_quiescent || degraded_connection) return 0;
    if (fd >= 0 && fd < HL_NFD && (g_sock_conn[fd] || g_sock_connecting[fd])) {
        fprintf(stderr, "[ckpt] refuse: connected/in-progress socket fd %d requires connection-state transfer\n", fd);
        return -1;
    }
    struct sockaddr_storage peer;
    socklen_t peer_size = sizeof peer;
    if (getpeername(fd, (struct sockaddr *)&peer, &peer_size) == 0) {
        fprintf(stderr, "[ckpt] refuse: connected socket fd %d requires connection-state transfer\n", fd);
        return -1;
    }
    struct pollfd readiness = {fd, POLLIN, 0};
    if (poll(&readiness, 1, 0) < 0 || (readiness.revents & (POLLIN | POLLERR | POLLHUP)) != 0) {
        fprintf(stderr, "[ckpt] refuse: socket fd %d has pending input/accept/error state\n", fd);
        return -1;
    }
    return 0;
}

static int ckpt_socket_state_degraded(int fd, int require_quiescent) {
    return require_quiescent && fd >= 0 && fd < HL_NFD && (g_sock_conn[fd] || g_sock_connecting[fd]) &&
           ckpt_recovery_permissive_requested(); // capture stays strict unless asked
}

static int ckpt_capture_socket_state(int fd, uint64_t identity, int require_quiescent) {
    int degraded_connection = ckpt_socket_state_degraded(fd, require_quiescent);
    // Admission pass: prove the quiescence gates, take no claim. This arm publishes rather than consumes,
    // but the claim it takes is an image-wide election that must not be won by a capture pass 1 may still
    // refuse -- a claimed-then-abandoned name is invisible to every other holder of the same object.
    if (g_ckpt_admission_only) return ckpt_socket_state_admit(fd, require_quiescent, degraded_connection);
    struct ckpt_sink *sink = ckpt_sink_current();
    char name[128];
    snprintf(name, sizeof name, "socket-state.%016llx", (unsigned long long)identity);
    int claimed = ckpt_sink_claim(sink, name);
    if (claimed != 0) return claimed > 0 ? 0 : -1;
    if (ckpt_socket_state_admit(fd, require_quiescent, degraded_connection) != 0) {
        ckpt_sink_unclaim(sink, name);
        return -1;
    }
    struct ckpt_socket_state state;
    memset(&state, 0, sizeof state);
    state.magic = CKPT_SOCKET_STATE_MAGIC;
    state.guest_family = g_sock_fam[fd];
    socklen_t type_size = sizeof state.type;
    socklen_t local_size = sizeof state.local;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &state.type, &type_size) != 0 ||
        getsockname(fd, (struct sockaddr *)&state.local, &local_size) != 0 || local_size > sizeof state.local)
        goto fail;
    state.host_family = state.local.ss_family;
    state.local_size = local_size;
    if (state.guest_family == AF_UNIX && g_unix_bind[fd][0] == '/') {
        struct sockaddr_un *local = (void *)&state.local;
        size_t path_length = strlen(g_unix_bind[fd]);
        if (path_length >= sizeof local->sun_path) goto fail;
        memset(local, 0, sizeof *local);
        local->sun_family = AF_UNIX;
        memcpy(local->sun_path, g_unix_bind[fd], path_length + 1);
        state.host_family = AF_UNIX;
        state.local_size = (uint32_t)(offsetof(struct sockaddr_un, sun_path) + path_length + 1);
    }
    if (state.guest_family == AF_UNIX && state.host_family == 0) {
        state.host_family = AF_UNIX;
#if defined(__APPLE__)
        ((struct sockaddr *)&state.local)->sa_len = (uint8_t)local_size;
#endif
        ((struct sockaddr *)&state.local)->sa_family = AF_UNIX;
    }
    state.protocol = state.type == SOCK_STREAM ? IPPROTO_TCP : state.type == SOCK_DGRAM ? IPPROTO_UDP : 0;
    if (state.host_family == AF_UNIX) state.protocol = 0;
    state.shutdown_mask = (uint8_t)sock_state_shutdown(fd);
    state.listening = g_tcp_listen[fd] != 0;
    state.backlog = g_sock_backlog[fd];
    state.lo_port = g_lo_port[fd];
    state.lo_v6 = g_lo_v6[fd];
    state.lo_v6only = g_lo_v6only[fd];
    state.br_port = g_br_port[fd];
    state.br_ip = g_br_ip[fd];
    state.br_interface = g_br_interface[fd];
    state.tcp_local_port = g_tcp_lport[fd];
    state.udp_local_port = g_udp_local_port[fd];
    state.udp_peer_port = g_udp_peer_port[fd];
    state.udp_local_ip = g_udp_local_ip[fd];
    state.udp_peer_ip = g_udp_peer_ip[fd];
    state.udp_local_v6 = g_udp_local_v6[fd];
    state.udp_peer_v6 = g_udp_peer_v6[fd];
    state.udp_local_interface = g_udp_local_interface[fd];
    state.udp_peer_interface = g_udp_peer_interface[fd];
    state.pending_error = degraded_connection ? ECONNRESET : g_so_error[fd];
    state.shadow_reuse_port = g_so_reuseport[fd];
    state.tcp_local_address = g_tcp_laddr[fd];
    state.tcp_local_v6 = g_tcp_l6[fd];
    memcpy(state.tcp_local_address_v6, g_tcp_laddr6[fd], sizeof state.tcp_local_address_v6);
    memcpy(state.tcp_option_value, g_tcp_optval[fd], sizeof state.tcp_option_value);
    memcpy(state.tcp_option_set, g_tcp_optset[fd], sizeof state.tcp_option_set);
    memcpy(state.ip_option_value, g_ipopt_val[fd], sizeof state.ip_option_value);
    memcpy(state.ip_option_set, g_ipopt_set[fd], sizeof state.ip_option_set);
    socklen_t linger_size = sizeof state.linger;
    if (ckpt_socket_option_int(fd, SO_RCVBUF, &state.receive_buffer) != 0 ||
        ckpt_socket_option_int(fd, SO_SNDBUF, &state.send_buffer) != 0 ||
        ckpt_socket_option_int(fd, SO_REUSEADDR, &state.reuse_address) != 0 ||
        ckpt_socket_option_int(fd, SO_REUSEPORT, &state.reuse_port) != 0 ||
        ckpt_socket_option_int(fd, SO_KEEPALIVE, &state.keepalive) != 0 ||
        ckpt_socket_option_int(fd, SO_BROADCAST, &state.broadcast) != 0 ||
        getsockopt(fd, SOL_SOCKET, SO_LINGER, &state.linger, &linger_size) != 0)
        goto fail;
    if (ckpt_sink_put(sink, NULL, name, CKPT_SINK_PUBLISH_ATOMIC, &state, sizeof state) != 0) goto fail;
    return 0;
fail:
    ckpt_sink_unclaim(sink, name);
    return -1;
}

static int ckpt_capture_file_blob(int fd, char *record_path, size_t record_capacity) {
    static _Atomic uint64_t blob_sequence;
    char destination[1280], temporary[1320];
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) || status.st_size < 0) return -1;
    // The blob read is non-destructive (pread, no offset change), but it publishes an object under a
    // pid-and-sequence-unique name, so running it in both passes would emit the file twice. The refusal it
    // owns -- "this deleted descriptor cannot be persisted" -- is the fstat above, which pass 1 does keep.
    if (g_ckpt_admission_only) {
        record_path[0] = '\0';
        return 0;
    }
    uint64_t sequence = atomic_fetch_add_explicit(&blob_sequence, 1, memory_order_relaxed) + 1;
    if (snprintf(record_path, record_capacity, "file.%d.%d.%llu", (int)getpid(), fd, (unsigned long long)sequence) >=
        (int)record_capacity)
        return -1;
    struct ckpt_sink *sink = ckpt_sink_current();
    struct ckpt_sink_stream *output = NULL;
    if (ckpt_sink_begin(sink, NULL, record_path, CKPT_SINK_PUBLISH_ATOMIC, &output) != 0) return -1;
    int input = fd;
#if defined(__linux__)
    int reader = -1;
    int access_mode = fcntl(fd, F_GETFL);
    if (access_mode >= 0 && (access_mode & O_ACCMODE) == O_WRONLY) {
        char descriptor_path[64];
        if (snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", fd) < (int)sizeof descriptor_path)
            reader = open(descriptor_path, O_RDONLY | O_CLOEXEC);
        if (reader >= 0) input = reader;
    }
#endif
    unsigned char buffer[65536];
    off_t offset = 0;
    int failed = 0;
    while (offset < status.st_size) {
        size_t wanted =
            (uint64_t)(status.st_size - offset) < sizeof buffer ? (size_t)(status.st_size - offset) : sizeof buffer;
        ssize_t count = pread(input, buffer, wanted, offset);
        if (count > 0) {
            if (ckpt_sink_write(sink, output, buffer, (size_t)count) != 0) {
                failed = 1;
                break;
            }
            offset += count;
            continue;
        }
        if (count < 0 && errno == EINTR) continue;
        failed = 1;
        break;
    }
    if (failed) {
        ckpt_sink_abort(sink, &output);
#if defined(__linux__)
        if (reader >= 0) close(reader);
#endif
        return -1;
    }
    int result = ckpt_sink_finish(sink, &output);
#if defined(__linux__)
    if (reader >= 0) close(reader);
#endif
    return result;
}

int hl_ckpt_interrupt_executors(void);

// Called at the top of the dispatcher loop (a clean safepoint: all guest arch state is spilled into `c`).
// Referenced by engine/dispatch.c via the G_CKPT_POLL seam (aarch64-only). Cheap: a NULL test + one shared
// memory load on the hot path. When the trigger generation advances, the container INIT coordinates the
// whole tree; a peer dumps only itself and then PARKS inside its own freeze until the coordinator releases
// it, so it returns here only when the capture was abandoned without consuming anything.
static int ckpt_peer_gpid(int64_t host_pid);

// WHO COORDINATES. Exactly one process in a domain freeze may run ckpt_coordinate_and_exit, because there
// is exactly one broker and exactly one manifest. `container_pid() == 1` is NOT that predicate: every ENGINE
// LAUNCH's top process sets g_init_hostpid to its own pid (target/{aarch64,x86_64}.c), so a container exec
// session's top process reports guest pid 1 exactly as the container init does. Once one trigger word and
// one broker served the whole process domain, all four launch tops elected themselves, and a coordinator
// does not commit a proc.N group of its own -- so the manifest was refused for four missing members that
// were each busy coordinating.
//
// The authority is the EMBEDDER'S REQUEST, not the shape of the process tree. Only the machine holding a
// CheckpointControl can be sent REQUEST_CHECKPOINT (runtime/execution.rs: a member launch carries a channel
// and no Server, and capture_checkpoint_until refuses with Unsupported), and that machine -- and only it --
// projects HL_CHECKPOINT_COORDINATOR onto its launch. Reading the option rather than the request byte keeps
// the answer established BEFORE the guest starts: the trigger word is bumped before the control byte is
// written, so a process that decided its role when the request arrived could observe the generation first
// and take the wrong path.
