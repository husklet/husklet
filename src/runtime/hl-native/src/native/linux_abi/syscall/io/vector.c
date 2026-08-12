/* Included by io.c: unity-build access with bounded I/O capability handlers. */

static int svc_readv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 65: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_fd_pb_len[(int)a0]) { // tee(2) pushback served first
            size_t available = g_fd_pb_len[(int)a0];
            void *buffer = malloc(available == 0 ? 1 : available);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            size_t taken = pipe_pushback_take((int)a0, buffer, available);
            ssize_t copied = io_guest_vector_scatter(a1, (size_t)a2, buffer, taken);
            free(buffer);
            G_RET(c) = copied >= 0 ? (uint64_t)copied : (uint64_t)(int64_t)copied;
            break;
        }
        if (nl_is((int)a0)) { // netlink readv: drain the queued dump into the guest iov
            uint8_t buffer[65536];
            struct iovec host = {buffer, sizeof(buffer)};
            ssize_t received = nl_recv((int)a0, &host, 1, 0, NULL);
            ssize_t copied =
                received > 0 ? io_guest_vector_scatter(a1, (size_t)a2, buffer, (size_t)received) : received;
            G_RET(c) = (uint64_t)(int64_t)copied;
            break;
        }
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r;       // SA_RESTART: restart a signal-interrupted blocking readv in place (see case 63)
        ts_wait_enter(); // 'S' while readv may block
        do {
            r = guest_fd_vector((int)a0, a1, (size_t)a2, 0, 0, 1);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
        // readv
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_writev(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 66: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (nl_is((int)a0)) { // netlink writev: gather the request iov + queue the dump
            uint8_t tmp[4096];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t tl = (size_t)gathered;
            nl_send((int)a0, tmp, tl);
            G_RET(c) = (uint64_t)tl;
            break;
        }
        // Container DNS: TCP DNS is commonly writev(len-prefix, query) (glibc send_vc). Gather + answer it.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
            uint8_t tmp[2048];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t tl = (size_t)gathered;
            G_RET(c) = (uint64_t)dns_send((int)a0, tmp, tl, g_sock_stream[(int)a0]);
            break;
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0]) {
            uint8_t tmp[2048];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t size = (size_t)gathered;
            int64_t result;
            if (icmp_try_send((int)a0, tmp, size, NULL, 0, &result)) {
                G_RET(c) = (uint64_t)result;
                break;
            }
        }
        {
            int64_t result;
            uint8_t tmp[65536];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            struct iovec host = {tmp, gathered > 0 ? (size_t)gathered : 0};
            if (gathered < 0) {
                // Same ordering as case 64: this probe gathers before the fd is consulted, and do_writev
                // resolves the descriptor ahead of import_iovec.
                G_RET(c) = (uint64_t)(int64_t)(gathered == -EFAULT && guest_fd_rejects((int)a0, 0) ? -EBADF : gathered);
                break;
            }
            if (udp_switch_write((int)a0, &host, 1, &result)) {
                G_RET(c) = (uint64_t)result;
                break;
            }
        }
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        hl_fdcache_fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking writev in place (see case 63)
        do {
            r = guest_fd_vector((int)a0, a1, (size_t)a2, 0, 0, 0);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        svc_sigpipe_on_epipe(c, (int64_t)G_RET(c)); // writev(2) to a broken pipe/socket -> guest SIGPIPE
        break;
        // writev
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_preadv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 69: {
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r = guest_fd_vector((int)a0, a1, (size_t)a2, (off_t)a3, 1, 1);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_pwritev(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 70: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r = guest_fd_vector((int)a0, a1, (size_t)a2, (off_t)a3, 1, 0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}
