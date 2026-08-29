static int sentry_worker_proc_leaf(const char *path) {
    static const char *const leaves[] = {"auxv",   "maps",      "smaps",   "stat",        "status",
                                         "statm",  "environ",   "cmdline", "comm",        "exe",
                                         "limits", "mountinfo", "pagemap", "task/1/maps", NULL};
    if (path == NULL || strncmp(path, "/proc/", 6) != 0) return 0;
    const char *rest = path + 6;
    // "/proc/self" itself: glibc realpath("/proc/self/exe") readlinks every component, and the pid this
    // magic link names must be the WORKER's container pid, not the sentry's identity.
    if (strcmp(rest, "self") == 0) return 1;
    if (strncmp(rest, "self/", 5) == 0) {
        rest += 5;
    } else {
        int pid = 0;
        const char *s = rest;
        while (*s >= '0' && *s <= '9')
            pid = pid * 10 + (*s++ - '0');
        if (s == rest || *s != '/' || pid != container_pid()) return 0;
        rest = s + 1;
    }
    for (int i = 0; leaves[i]; i++)
        if (strcmp(rest, leaves[i]) == 0) return 1;
    return 0;
}

// Hand a worker-opened real fd to the sentry for adoption into this process's virtual fd table
// (SENTRY_OP_ADOPT). The fd datagram is queued on the control socketpair BEFORE the turn flips so the
// sentry's recv never blocks. Returns the new virtual fd, or a negated errno.
static int64_t sentry_adopt_fd(int rfd, int cloexec) {
    struct sentry_ring *R = ring_for_thread();
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    int idx = t_ring;
    if (idx < 0 || g_ctl[idx][0] < 0) {
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        return -EIO;
    }
    R->wpid = (uint32_t)g_worker_pid;
    R->wtid = t_token;
    R->inherit_wtid = 0;
    R->rawnr = SENTRY_OP_ADOPT;
    R->a[0] = (uint64_t)(cloexec != 0);
    R->iovn = 0;
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    sentry_send_fd(g_ctl[idx][0], rfd);
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    sentry_request_publish(R);
    sentry_response_wait(R, request);
    int64_t ret = R->ret;
    atomic_store_explicit(&R->busy, 0, memory_order_release);
    return ret;
}

static void sentry_route_rebase(struct cpu *c, uint64_t nr) {
    if (!g_nonpie_lo || !sentry_forwarded(nr)) return;
    uint64_t reb[6] = {G_A0(c), G_A1(c), G_A2(c), G_A3(c), G_A4(c), G_A5(c)};
    nonpie_rebase_args(nr, reb);
    G_A0(c) = reb[0];
    G_A1(c) = reb[1];
    G_A2(c) = reb[2];
    G_A3(c) = reb[3];
    G_A4(c) = reb[4];
    G_A5(c) = reb[5];
}

static int sentry_route_exit(struct cpu *c, uint64_t nr) {
    if (nr != 93 && nr != 94) return 0;
    int process_exit = nr == 94 || atomic_fetch_sub(&g_worker_threads, 1) == 1;
    if (process_exit) {
        translit_sampling_receipt("sentry-process-exit");
        /* The sentry shutdown can terminate the worker before service_local reaches exit_group's
           ordinary cleanup.  Publish this process generation while its translation state and map
           descriptor are still owned by the exiting worker. */
        translit_perf_map_flush();
        if (getpid() != g_sentry_owner_pid) sentry_process_release();
        sentry_shutdown();
    } else if (t_ring >= 0) {
        sentry_ctl_op(SENTRY_OP_THREAD_EXIT, 0, 0);
    }
    ring_release();
    service_local(c);
    return 1;
}

static int sentry_route_exec(struct cpu *c, uint64_t nr) {
    if (nr != 221) return 0;
    int64_t bound = sentry_ctl_op(SENTRY_OP_BIND, 0, 0);
    if (bound < 0) {
        G_RET(c) = (uint64_t)bound;
        return 1;
    }
    service_local(c);
    if (c->redirect) sentry_ctl_op(SENTRY_OP_EXEC, (uint64_t)(uint32_t)g_worker_pid, 0);
    return 1;
}

// The sentry keys a worker process's descriptor table by HOST pid: every request a worker publishes
// stamps R->wpid from its own getpid(). clone(2) hands the PARENT the child's GUEST pid, and under a
// container pid namespace those are different numbers -- a forked shell reads 2 where the host sees
// 238623 -- so a lane that files the child's table under the clone return value files it where no
// request from that child can ever find it. Translate once, here, so the clone, wait and reap lanes all
// name the child by the identity the table is keyed on. An unmapped pid (no active pid namespace) is
// already a host pid and passes through.
static pid_t sentry_host_pid(int64_t guest) {
    int host = 0;
    if (guest > 0 && guest <= INT32_MAX && hl_linux_pidmap_host_checked(&g_pidmap, (int32_t)guest, &host) == 0)
        return (pid_t)host;
    return (pid_t)guest;
}

static int sentry_route_wait(struct cpu *c, uint64_t nr) {
    if (nr != 260) return 0;
    int64_t wpid = (int64_t)(int)G_A0(c);
    if (wpid <= 0 && atomic_load(&g_guest_children) <= 0) {
        G_RET(c) = (uint64_t)(-ECHILD);
        return 1;
    }
    service_local(c);
    int64_t result = (int64_t)G_RET(c);
    if (result <= 0) return 1;
    pid_t host = sentry_host_pid(result);
    if (g_sentry_pid && host == g_sentry_pid) {
        G_RET(c) = (uint64_t)(-ECHILD);
        return 1;
    }
    int terminated = (G_A2(c) & (2u | 8u)) == 0;
    if (!terminated && G_A1(c)) {
        int status;
        if (guest_copy_from(&status, G_A1(c), sizeof status) == sizeof status)
            terminated = (status & 0xff) != 0x7f && status != 0xffff;
    } else if (!terminated && !G_A1(c)) {
        errno = 0;
        terminated = kill(host, 0) < 0 && errno == ESRCH;
    }
    if (terminated) {
        sentry_ctl_op(SENTRY_OP_REAP, (uint64_t)(uint32_t)host, 0);
        atomic_fetch_sub(&g_guest_children, 1);
    }
    return 1;
}

// waitid(95) reaps a guest child worker exactly as wait4(260) does, and until this lane existed the
// sentry never heard about it: waitid is not in the forwarded table and had no route here, so
// service_local collected the host zombie -- freeing the pid the child's virtual descriptor table is
// keyed on -- while the table itself stayed filed under that number. Measured on x86_64 Linux: a guest
// that kills a child and collects it with waitid runs out of sentry process slots after 63 rounds with
// at most one child alive at a time, and a fork landing on a reissued pid is then refused -EEXIST, an
// errno clone(2) cannot return. The guards mirror the wait4 lane: never block on the sentry (a real host
// child of the owner that is not a guest child), never surface its pid, and release on termination.
//
// Linux waitid: a0 idtype (P_ALL 0 / P_PID 1 / P_PGID 2 / P_PIDFD 3), a1 id, a2 siginfo, a3 options,
// a4 rusage. WSTOPPED(2) and WCONTINUED(8) hold the same bit values as wait4's WUNTRACED/WCONTINUED, so
// the "did this reap a corpse or report a stop" test is the same one.
static int sentry_route_waitid(struct cpu *c, uint64_t nr) {
    if (nr != 95) return 0;
    uint64_t idtype = G_A0(c);
    uint64_t infop = G_A2(c);
    uint64_t options = G_A3(c);
    if ((idtype == 0 || idtype == 2) && atomic_load(&g_guest_children) <= 0) {
        G_RET(c) = (uint64_t)(int64_t)(-ECHILD);
        return 1;
    }
    service_local(c);
    if ((int64_t)G_RET(c) < 0) return 1;
    // WNOWAIT reports a child without consuming it, so the pid stays pinned and the table must stay.
    if (options & 0x01000000u) return 1;
    // The reaped child's guest pid comes from the siginfo the call just filled (Linux si_pid is at
    // offset 16, si_code at 8); a caller that asked for no siginfo is answered from its own arguments.
    int64_t reaped = 0;
    int terminated = (options & (2u | 8u)) == 0;
    if (infop) {
        uint8_t siginfo[128];
        if (guest_copy_from(siginfo, infop, sizeof siginfo) != (ssize_t)sizeof siginfo) return 1;
        reaped = *(const int *)(siginfo + 16);
        int code = *(const int *)(siginfo + 8);
        terminated = code == CLD_EXITED || code == CLD_KILLED || code == CLD_DUMPED;
    } else if (idtype == 1) {
        reaped = (int64_t)(int)G_A1(c);
    }
    if (reaped <= 0) return 1;
    pid_t host = sentry_host_pid(reaped);
    if (g_sentry_pid && host == g_sentry_pid) {
        G_RET(c) = (uint64_t)(int64_t)(-ECHILD);
        return 1;
    }
    if (terminated) {
        sentry_ctl_op(SENTRY_OP_REAP, (uint64_t)(uint32_t)host, 0);
        atomic_fetch_sub(&g_guest_children, 1);
    }
    return 1;
}

static int sentry_route_clone(struct cpu *c, uint64_t nr) {
    if (nr != 220 && nr != 435) return 0;
    fork_diagnostic_route previous_route =
        fork_diagnostic_route_enter("sentry-worker", (int)g_worker_pid, (int)g_sentry_pid,
                                    atomic_load_explicit(&g_guest_children, memory_order_relaxed),
                                    atomic_load_explicit(&g_worker_threads, memory_order_relaxed), t_ring);
    uint64_t clone3_flags = 0;
    if (nr == 435 && G_A0(c) && guest_copy_from(&clone3_flags, G_A0(c), sizeof clone3_flags) != sizeof clone3_flags) {
        fork_diagnostic_emit(c, nr, 0, "sentry-clone3-arguments", EFAULT, -1, NULL);
        fork_diagnostic_route_leave(previous_route);
        G_RET(c) = (uint64_t)(int64_t)-EFAULT;
        return 1;
    }
    int is_thread = nr == 220 ? (G_A0(c) & 0x10000) != 0 : (clone3_flags & 0x10000) != 0;
    int is_vfork = nr == 220 ? (G_A0(c) & 0x4000) != 0 : (clone3_flags & 0x4000) != 0;
    uint64_t flags = nr == 220 ? G_A0(c) : clone3_flags;
    int64_t snapshot = is_thread ? 0 : sentry_ctl_op(SENTRY_OP_FORK_PREPARE, 0, 0);
    if (!is_thread && snapshot < 0) {
        fork_diagnostic_emit(c, nr, flags, "sentry-snapshot", (int)-snapshot, -1, NULL);
        fork_diagnostic_route_leave(previous_route);
        G_RET(c) = (uint64_t)snapshot;
        return 1;
    }
    int sync[2] = {-1, -1};
    if (!is_thread && !is_vfork && pipe(sync) != 0) {
        int error = errno;
        sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)snapshot, 0);
        fork_diagnostic_emit(c, nr, flags, "sentry-sync-pipe", error, -1, NULL);
        fork_diagnostic_route_leave(previous_route);
        G_RET(c) = (uint64_t)(int64_t)-error;
        return 1;
    }
    service_local(c);
    if (getpid() != g_worker_pid) {
        if (is_vfork) {
            int64_t installed = sentry_ctl_op(SENTRY_OP_FORK, (uint64_t)snapshot, (uint64_t)(uint32_t)getpid());
            if (installed < 0) _exit(127);
            sentry_fork_child();
            fork_diagnostic_route_leave(previous_route);
            return 1;
        }
        close(sync[1]);
        unsigned char ready;
        ssize_t received;
        do
            received = read(sync[0], &ready, sizeof ready);
        while (received < 0 && errno == EINTR);
        close(sync[0]);
        if (received != sizeof ready || ready != 1) _exit(127);
        sentry_fork_child();
        fork_diagnostic_route_leave(previous_route);
        return 1;
    }
    if (!is_thread && (int64_t)G_RET(c) > 0) {
        if (is_vfork) {
            atomic_fetch_add(&g_guest_children, 1);
            fork_diagnostic_route_leave(previous_route);
            return 1;
        }
        close(sync[0]);
        pid_t child = sentry_host_pid((int64_t)G_RET(c));
        int64_t installed = sentry_ctl_op(SENTRY_OP_FORK, (uint64_t)snapshot, (uint64_t)child);
        if (installed < 0) {
            sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)snapshot, 0);
            close(sync[1]);
            kill(child, SIGKILL);
            int status;
            while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
            fork_diagnostic_emit(c, nr, flags, "sentry-install", (int)-installed, -1, NULL);
            G_RET(c) = (uint64_t)installed;
            fork_diagnostic_route_leave(previous_route);
            return 1;
        }
        unsigned char ready = 1;
        ssize_t written = hl_sentry_pipe_write(sync[1], &ready, sizeof ready);
        close(sync[1]);
        if (written != sizeof ready) {
            kill(child, SIGKILL);
            int status;
            while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
            sentry_ctl_op(SENTRY_OP_REAP, (uint64_t)child, 0);
            fork_diagnostic_emit(c, nr, flags, "sentry-sync-publish", EIO, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)-EIO;
            fork_diagnostic_route_leave(previous_route);
            return 1;
        }
        atomic_fetch_add(&g_guest_children, 1);
    } else if (!is_thread) {
        close(sync[0]);
        close(sync[1]);
        sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)snapshot, 0);
    }
    fork_diagnostic_route_leave(previous_route);
    return 1;
}

static int sentry_route_worker_proc(struct cpu *c, uint64_t nr) {
    if (nr != 56 && nr != 78) return 0;
    char path[SENTRY_PATHCAP];
    int length = G_A1(c) ? guest_copy_string(path, sizeof path, G_A1(c)) : -1;
    if (length < 0 || !sentry_worker_proc_leaf(path)) return 0;
    service_local(c);
    if (nr == 56 && (int64_t)G_RET(c) >= 0) {
        int rfd = (int)G_RET(c);
        int64_t vfd = sentry_adopt_fd(rfd, (G_A2(c) & LX_O_CLOEXEC) != 0);
        sentry_native_close(rfd);
        G_RET(c) = (uint64_t)vfd;
    }
    return 1;
}

static int sentry_route_file_mmap(struct cpu *c, uint64_t nr) {
    if (nr != 222 || (G_A3(c) & 0x20) || (int)G_A4(c) < 0) return 0;
    struct sentry_ring *R = ring_for_thread();
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    int idx = t_ring;
    R->wpid = (uint32_t)g_worker_pid;
    R->wtid = t_token;
    R->inherit_wtid = 0;
    R->rawnr = SENTRY_OP_FDPASS;
    R->a[0] = (uint64_t)(uint32_t)(int)G_A4(c);
    R->iovn = 0;
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    sentry_request_publish(R);
    sentry_response_wait(R, request);
    int lfd = idx >= 0 && g_ctl[idx][0] >= 0 ? sentry_recv_fd(g_ctl[idx][0]) : -1;
    atomic_store_explicit(&R->busy, 0, memory_order_release);
    uint64_t saved = G_A4(c);
    G_A4(c) = (uint64_t)(int64_t)lfd;
    service_local(c);
    G_A4(c) = saved;
    if (lfd >= 0) close(lfd);
    return 1;
}

struct sentry_marshal {
    struct cpu *c;
    struct sentry_ring *R;
    uint64_t nr;
    struct iovec worker_iov[SENTRY_IOVMAX];
    uint32_t worker_iovn;
    uint8_t worker_msghdr[SENTRY_MSGHDR_SZ];
    struct iovec worker_msg_iov[SENTRY_IOVMAX];
    uint32_t worker_msg_iovn;
    int worker_msghdr_valid;
    socklen_t worker_socklen;
    int worker_socklen_valid;
};

#define SENTRY_IMPORT_EXACT(dst, src, len)                                                                             \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_copy_from((dst), (uint64_t)(src), _n) != (ssize_t)_n) {                                        \
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);                                                                   \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return -1;                                                                                                 \
        }                                                                                                              \
    } while (0)
#define SENTRY_IMPORT_STRING(dst, cap, src)                                                                            \
    do {                                                                                                               \
        int _r = guest_copy_string((dst), (cap), (uint64_t)(src));                                                     \
        if (_r < 0) {                                                                                                  \
            G_RET(c) = (uint64_t)(int64_t)_r;                                                                          \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return -1;                                                                                                 \
        }                                                                                                              \
        R->inlen = (uint32_t)_r + 1u;                                                                                  \
    } while (0)
#define SENTRY_REQUIRE_WRITE(ptr, len)                                                                                 \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_accessible_prefix((uint64_t)(ptr), _n, HL_LOGICAL_VMA_WRITE) != _n) {                          \
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);                                                                   \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return -1;                                                                                                 \
        }                                                                                                              \
    } while (0)

static int sentry_import_file(struct sentry_marshal *M) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    switch (nr) {
    case 48:  // faccessat
    case 56:  // openat
    case 439: // faccessat2
    {         // dfd, a1=path: in-path
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        R->redir[1] = 0;
        break;
    }
    case 279: { // memfd_create(a0=name, a1=flags): in-name
        if (!G_A0(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A0(c));
        R->redir[0] = 0;
        break;
    }
    case 78: { // readlinkat(dfd, a1=path, a2=buf, a3=size): in-path + bounded out-buffer
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        R->redir[1] = 0;
        R->redir[2] = SENTRY_PATHCAP;
        uint32_t cap = SENTRY_BUFSZ - SENTRY_PATHCAP;
        if (R->a[3] > cap) R->a[3] = cap;
        break;
    }
    case 64:   // write(fd, a1=buf, a2=len)
    case 68: { // pwrite64(fd, a1=buf, a2=len, a3=off): copy the payload into the ring; cap to BUFSZ
        uint32_t n = G_A2(c) > SENTRY_BUFSZ ? SENTRY_BUFSZ : (uint32_t)G_A2(c);
        if (n) {
            ssize_t copied = guest_copy_from(R->buf, G_A1(c), n);
            if (copied <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return -1;
            }
            n = (uint32_t)copied; /* Linux permits a short write up to the first inaccessible byte. */
        }
        R->inlen = n;
        R->redir[1] = 0;
        R->a[2] = n; // ship exactly n bytes; a short (p)write is legal -> guest loops
        break;
    }
    case 71: // sendfile(out, in, offset*, count): optional in/out offset
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A2(c), sizeof(int64_t));
            R->redir[2] = 0;
        }
        break;
    case 76:    // splice(fd_in, off_in*, fd_out, off_out*, len, flags)
    case 285: { // copy_file_range(fd_in, off_in*, fd_out, off_out*, len, flags): both offsets optional in/out
        if (G_A1(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A1(c), sizeof(int64_t));
            R->redir[1] = 0;
        }
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf + sizeof(int64_t), G_A3(c), sizeof(int64_t));
            R->redir[3] = (int32_t)sizeof(int64_t);
        }
        break;
    }
    case 63:   // read(fd, a1=buf, a2=len)
    case 67:   // pread64(fd, a1=buf, a2=len, a3=off)
    case 61: { // getdents64(fd, a1=buf, a2=count): reserve the out window; cap to BUFSZ
        uint32_t n = G_A2(c) > SENTRY_BUFSZ ? SENTRY_BUFSZ : (uint32_t)G_A2(c);
        if (n) {
            size_t prefix = guest_accessible_prefix(G_A1(c), n, HL_LOGICAL_VMA_WRITE);
            if (!prefix) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return -1;
            }
            n = (uint32_t)prefix;
        }
        R->redir[1] = 0;
        R->a[2] = n; // short read / partial getdents is legal -> guest loops
        break;
    }
    case 80: // fstat(fd, a1=statbuf): out-struct only
        SENTRY_REQUIRE_WRITE(G_A1(c), SENTRY_STATSZ);
        R->redir[1] = 0;
        break;
    case 79: { // newfstatat(dfd, a1=path, a2=statbuf, flags): in-path + out-struct (two-buffer)
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        SENTRY_REQUIRE_WRITE(G_A2(c), SENTRY_STATSZ);
        R->redir[1] = 0;              // path     -> buf[0]
        R->redir[2] = SENTRY_PATHCAP; // statbuf -> buf[SENTRY_PATHCAP]; copied back below on success
        break;
    }
    case 291: { // statx(dfd, a1=path, a2=flags, a3=mask, a4=statxbuf): in-path + out-struct
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        SENTRY_REQUIRE_WRITE(G_A4(c), SENTRY_STATXSZ);
        R->redir[1] = 0;              // path      -> buf[0]
        R->redir[4] = SENTRY_PATHCAP; // statxbuf -> buf[SENTRY_PATHCAP]
        break;
    }
    case 65:   // readv(fd, a1=iov, a2=iovcnt)
    case 66: { // writev(fd, a1=iov, a2=iovcnt): flatten the guest iovec into the ring
        // Layout in buf[]: a `struct iovec[n]` header (iov_base = buf-relative OFFSET) followed by the
        // scatter/gather data. For writev we gather the guest segments now; for readv we just reserve
        // the windows and scatter back after the round-trip. iov_base offsets are bounds-checked and
        // rebased to ring pointers by the sentry, so no guest pointer ever crosses.
        uint32_t n = (uint32_t)G_A2(c);
        if (n > SENTRY_IOVMAX) n = SENTRY_IOVMAX; // partial scatter/gather is legal -> guest loops
        const struct iovec *giov = M->worker_iov;
        if (n) {
            if (guest_iov_import(G_A1(c), n, M->worker_iov) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return -1;
            }
            if (g_nonpie_lo)
                for (uint32_t i = 0; i < n; i++)
                    M->worker_iov[i].iov_base =
                        (void *)(uintptr_t)nonpie_p((uint64_t)(uintptr_t)M->worker_iov[i].iov_base);
        }
        M->worker_iovn = n;
        struct iovec *biov = (struct iovec *)R->buf;
        uint32_t cur = n * (uint32_t)sizeof(struct iovec); // data region starts after the iovec header
        uint32_t payload = 0;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t room = SENTRY_BUFSZ - cur;
            uint32_t want = (giov && giov[i].iov_len < room) ? (uint32_t)giov[i].iov_len : room;
            if (nr == 65 && want) {
                size_t prefix =
                    guest_accessible_prefix((uint64_t)(uintptr_t)giov[i].iov_base, want, HL_LOGICAL_VMA_WRITE);
                if (!prefix) {
                    if (!payload) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return -1;
                    }
                    n = i;
                    M->worker_iovn = n;
                    break;
                }
                want = (uint32_t)prefix;
            }
            if (nr == 66 && want && giov) {
                ssize_t copied = guest_copy_from(R->buf + cur, (uint64_t)(uintptr_t)giov[i].iov_base, want);
                if (copied != (ssize_t)want) {
                    if (copied <= 0 && payload == 0) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return -1;
                    }
                    if (copied <= 0) {
                        n = i;
                        M->worker_iovn = n;
                        break;
                    }
                    want = (uint32_t)copied;
                }
            }
            biov[i].iov_base = (void *)(uintptr_t)cur; // buf-relative offset; sentry rebases + checks
            biov[i].iov_len = want;
            cur += want;
            payload += want;
        }
        R->inlen = cur;
        R->redir[1] = 0;
        R->iovn = n;
        R->a[2] = n; // sentry runs the (possibly clamped) segment count
        break;
    }
    default: return 0;
    }
    return 1;
}

static int sentry_import_socket(struct sentry_marshal *M) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    switch (nr) {
    // ---- socket family ---- (sentry owns the real socket fd; only sockaddr/optval/data bytes cross,
    // never a guest pointer; all AF/port-map/jail translation runs inside service_local on the sentry)
    case 200:   // bind(fd, a1=addr, a2=addrlen)
    case 203: { // connect(fd, a1=addr, a2=addrlen): in-sockaddr -> tail window
        const uint8_t *sa = (const uint8_t *)G_A1(c);
        if (sa) {
            uint32_t n = (uint32_t)G_A2(c);
            if (n > SENTRY_SADDRCAP) n = SENTRY_SADDRCAP; // real sockaddrs are <=128; cap defensively
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SADDR_OFF, G_A1(c), n);
            R->redir[1] = SENTRY_SADDR_OFF;
            R->a[2] = n;
            R->inlen = n;
        } // NULL addr: leave register/len as-is (service_local handles it / errors identically)
        break;
    }
    case 202:   // accept(fd, a1=addr_out, a2=addrlen_inout)
    case 242:   // accept4(fd, a1=addr_out, a2=addrlen_inout, a3=flags)
    case 204:   // getsockname(fd, a1=addr_out, a2=addrlen_inout)
    case 205: { // getpeername(fd, a1=addr_out, a2=addrlen_inout): out-sockaddr + in/out socklen
        if (G_A1(c)) R->redir[1] = SENTRY_SADDR_OFF; // out sockaddr -> tail window
        if (G_A2(c)) {                               // in/out socklen: ship the guest cap
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SLEN_OFF, G_A2(c), sizeof(socklen_t));
            memcpy(&M->worker_socklen, R->buf + SENTRY_SLEN_OFF, sizeof M->worker_socklen);
            M->worker_socklen_valid = 1;
            if (G_A1(c)) {
                size_t need = M->worker_socklen < SENTRY_SADDRCAP ? M->worker_socklen : SENTRY_SADDRCAP;
                SENTRY_REQUIRE_WRITE(G_A1(c), need);
            }
            R->redir[2] = SENTRY_SLEN_OFF;
        }
        break;
    }
    case 206: { // sendto(fd, a1=buf, a2=len, a3=flags, a4=destaddr, a5=addrlen): in-data + in-destaddr
        uint32_t n = G_A2(c) > SENTRY_DATACAP ? SENTRY_DATACAP : (uint32_t)G_A2(c);
        if (n) {
            ssize_t copied = guest_copy_from(R->buf, G_A1(c), n);
            if (copied <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return -1;
            }
            n = (uint32_t)copied;
        }
        R->redir[1] = 0;
        R->a[2] = n; // short send is legal -> guest loops
        R->inlen = n;
        if (G_A4(c)) { // optional dest addr (UDP) -> tail window
            uint32_t dl = (uint32_t)G_A5(c);
            if (dl > SENTRY_SADDRCAP) dl = SENTRY_SADDRCAP;
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SADDR_OFF, G_A4(c), dl);
            R->redir[4] = SENTRY_SADDR_OFF;
            R->a[5] = dl;
        }
        break;
    }
    case 207: { // recvfrom(fd, a1=buf, a2=len, a3=flags, a4=srcaddr_out, a5=addrlen_inout)
        uint32_t n = G_A2(c) > SENTRY_DATACAP ? SENTRY_DATACAP : (uint32_t)G_A2(c);
        if (n) {
            size_t prefix = guest_accessible_prefix(G_A1(c), n, HL_LOGICAL_VMA_WRITE);
            if (!prefix) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return -1;
            }
            n = (uint32_t)prefix;
        }
        R->redir[1] = 0;
        R->a[2] = n;                                 // short recv is legal -> guest loops
        if (G_A4(c)) R->redir[4] = SENTRY_SADDR_OFF; // out src sockaddr -> tail window
        if (G_A5(c)) {                               // in/out socklen: ship the guest cap
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SLEN_OFF, G_A5(c), sizeof(socklen_t));
            memcpy(&M->worker_socklen, R->buf + SENTRY_SLEN_OFF, sizeof M->worker_socklen);
            M->worker_socklen_valid = 1;
            if (G_A4(c)) {
                size_t need = M->worker_socklen < SENTRY_SADDRCAP ? M->worker_socklen : SENTRY_SADDRCAP;
                SENTRY_REQUIRE_WRITE(G_A4(c), need);
            }
            R->redir[5] = SENTRY_SLEN_OFF;
        }
        break;
    }
    case 208: { // setsockopt(fd, a1=level, a2=optname, a3=optval, a4=optlen): in-optval -> opt window
        if (G_A3(c)) {
            uint32_t n = (uint32_t)G_A4(c);
            if (n > SENTRY_OPTCAP) n = SENTRY_OPTCAP;
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_OPT_OFF, G_A3(c), n);
            R->redir[3] = SENTRY_OPT_OFF;
            R->a[4] = n;
            R->inlen = n;
        }
        break;
    }
    case 209: {        // getsockopt(fd, a1=level, a2=optname, a3=optval_out, a4=optlen_inout)
        if (G_A4(c)) { // in/out optlen: ship the guest cap (clamped so the kernel can't overrun the window)
            socklen_t cap = 0;
            SENTRY_IMPORT_EXACT(&cap, G_A4(c), sizeof cap);
            M->worker_socklen = cap;
            M->worker_socklen_valid = 1;
            if (G_A3(c)) {
                size_t need = cap < SENTRY_OPTCAP ? cap : SENTRY_OPTCAP;
                SENTRY_REQUIRE_WRITE(G_A3(c), need);
            }
            if (cap > SENTRY_OPTCAP) cap = SENTRY_OPTCAP;
            *(socklen_t *)(R->buf + SENTRY_SLEN_OFF) = cap;
            R->redir[4] = SENTRY_SLEN_OFF;
        }
        if (G_A3(c)) R->redir[3] = SENTRY_OPT_OFF; // out optval -> opt window
        break;
    }
    default: return 0;
    }
    return 1;
}

static int sentry_import_message(struct sentry_marshal *M) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    switch (nr) {
    // ---- sendmsg/recvmsg (item 2): flatten the guest msghdr GRAPH into the ring ----
    case 211:   // sendmsg(fd, a1=msghdr, flags)
    case 212: { // recvmsg(fd, a1=msghdr, flags)
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        SENTRY_IMPORT_EXACT(M->worker_msghdr, G_A1(c), SENTRY_MSGHDR_SZ);
        M->worker_msghdr_valid = 1;
        uint64_t g_name = *(uint64_t *)(M->worker_msghdr + 0);
        uint32_t g_namelen = *(uint32_t *)(M->worker_msghdr + 8);
        uint64_t g_iov = *(uint64_t *)(M->worker_msghdr + 16);
        uint64_t g_iovlen = *(uint64_t *)(M->worker_msghdr + 24);
        uint64_t g_ctl = *(uint64_t *)(M->worker_msghdr + 32);
        uint64_t g_ctllen = *(uint64_t *)(M->worker_msghdr + 40);
        uint32_t g_flags = *(uint32_t *)(M->worker_msghdr + 48);
        if (g_nonpie_lo) {
            g_name = nonpie_p(g_name);
            g_iov = nonpie_p(g_iov);
            g_ctl = nonpie_p(g_ctl);
        }
        uint8_t *h = R->buf; // the 56-byte msghdr COPY at [0,56)
        memset(h, 0, SENTRY_MSGHDR_SZ);
        // msg_name: offset into the sockaddr tail window (send copies the addr; recv just reserves it).
        if (g_name && g_namelen) {
            uint32_t nl = g_namelen > SENTRY_SADDRCAP ? SENTRY_SADDRCAP : g_namelen;
            if (nr == 211) SENTRY_IMPORT_EXACT(R->buf + SENTRY_MSGNAME_OFF, g_name, nl);
            *(uint64_t *)(h + 0) = SENTRY_MSGNAME_OFF; // nonzero offset == present
            *(uint32_t *)(h + 8) = nl;                 // capped to the ring window (real addrs fit)
        }
        // msg_iov: iovec[] header (iov_base = OFFSET) + data, flattened like readv/writev, capped to DATACAP.
        uint32_t n = g_iovlen > SENTRY_IOVMAX ? SENTRY_IOVMAX : (uint32_t)g_iovlen;
        if (n && guest_iov_import(g_iov, n, M->worker_msg_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return -1;
        }
        if (g_nonpie_lo)
            for (uint32_t i = 0; i < n; i++)
                M->worker_msg_iov[i].iov_base =
                    (void *)(uintptr_t)nonpie_p((uint64_t)(uintptr_t)M->worker_msg_iov[i].iov_base);
        M->worker_msg_iovn = n;
        const struct iovec *giov = M->worker_msg_iov;
        struct iovec *biov = (struct iovec *)(R->buf + SENTRY_MSGIOV_OFF);
        uint32_t cur = SENTRY_MSGIOV_OFF + n * (uint32_t)sizeof(struct iovec);
        uint32_t msg_payload = 0;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t room = (cur < SENTRY_DATACAP) ? (SENTRY_DATACAP - cur) : 0; // keep data clear of the tail
            uint32_t want = (giov && giov[i].iov_len < room) ? (uint32_t)giov[i].iov_len : room;
            if (nr == 212 && want) {
                size_t prefix =
                    guest_accessible_prefix((uint64_t)(uintptr_t)giov[i].iov_base, want, HL_LOGICAL_VMA_WRITE);
                if (!prefix) {
                    if (!msg_payload) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return -1;
                    }
                    n = i;
                    M->worker_msg_iovn = n;
                    break;
                }
                want = (uint32_t)prefix;
            }
            if (nr == 211 && want) {
                ssize_t copied = guest_copy_from(R->buf + cur, (uint64_t)(uintptr_t)giov[i].iov_base, want);
                if (copied != (ssize_t)want) {
                    if (copied <= 0 && i == 0) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return -1;
                    }
                    if (copied <= 0) {
                        n = i;
                        M->worker_msg_iovn = n;
                        break;
                    }
                    want = (uint32_t)copied;
                }
            }
            biov[i].iov_base = (void *)(uintptr_t)cur;
            biov[i].iov_len = want;
            cur += want;
            msg_payload += want;
        }
        *(uint64_t *)(h + 16) = SENTRY_MSGIOV_OFF;
        *(uint64_t *)(h + 24) = n;
        // msg_control: offset into the optval tail window (send copies the cmsg; recv reserves it). SCM_RIGHTS
        // fds inside are sentry fds, so the bytes cross verbatim.
        if (g_ctl && g_ctllen) {
            uint32_t cl = g_ctllen > SENTRY_MSGCTLCAP ? SENTRY_MSGCTLCAP : (uint32_t)g_ctllen;
            if (nr == 211) SENTRY_IMPORT_EXACT(R->buf + SENTRY_MSGCTL_OFF, g_ctl, cl);
            *(uint64_t *)(h + 32) = SENTRY_MSGCTL_OFF; // nonzero offset == present
            *(uint64_t *)(h + 40) = cl;                // controllen (send: actual; recv: cap)
        }
        *(uint32_t *)(h + 48) = g_flags;
        R->redir[1] = 0; // a1 -> msghdr copy; the sentry rebases the inner offsets (snr 211/212)
        R->inlen = cur;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int sentry_import_misc(struct sentry_marshal *M) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    switch (nr) {
#define SENTRY_SCALAR_CASE(number) case number:
        HL_LINUX_SENTRY_SCALAR(SENTRY_SCALAR_CASE)
#undef SENTRY_SCALAR_CASE
        break;
    // ---- multiplexing over sentry-owned fds (item 3) ----
    case 73: { // ppoll(fds, nfds, timeout_ts, sigmask, sigsetsz)
        uint32_t nfds = (uint32_t)G_A1(c);
        uint32_t bytes = nfds * 8u; // sizeof(struct pollfd) == 8
        if (bytes > SENTRY_DATACAP) {
            bytes = SENTRY_DATACAP;
            nfds = bytes / 8u;
        }
        if (G_A0(c) && bytes) SENTRY_IMPORT_EXACT(R->buf, G_A0(c), bytes);
        R->redir[0] = 0;
        R->a[1] = nfds;
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_POLL_TMO, G_A2(c), 16);
            R->redir[2] = SENTRY_POLL_TMO;
        } else
            R->a[2] = 0; // NULL timeout == block forever
        R->a[3] = 0;
        R->a[4] = 0; // sigmask ignored by service_local
        break;
    }
    case 72: { // pselect6(nfds, rd, wr, ex, timeout_ts, sigmask)
        uint32_t nfds = (uint32_t)G_A0(c);
        uint32_t fb = (nfds + 7u) / 8u;
        if (fb > 128u) fb = 128u; // fd_set caps at FD_SETSIZE/8 == 128
        if (G_A1(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_RD, G_A1(c), fb);
            R->redir[1] = SENTRY_PSEL_RD;
        } else
            R->a[1] = 0;
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_WR, G_A2(c), fb);
            R->redir[2] = SENTRY_PSEL_WR;
        } else
            R->a[2] = 0;
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_EX, G_A3(c), fb);
            R->redir[3] = SENTRY_PSEL_EX;
        } else
            R->a[3] = 0;
        if (G_A4(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_TMO, G_A4(c), 16);
            R->redir[4] = SENTRY_PSEL_TMO;
        } else
            R->a[4] = 0;
        R->a[5] = 0;
        break;
    }
    case 21: // epoll_ctl(epfd, op, fd, a3=event): in epoll_event (SENTRY_EPEV_SZ, per guest arch)
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A3(c), SENTRY_EPEV_SZ);
            R->redir[3] = 0;
        }
        break;
    case 22: // epoll_pwait(epfd, a1=events_out, maxevents, timeout, sigmask): reserve out window, drop sigmask
        if ((int64_t)G_A2(c) > 0) {
            uint64_t maxevents = G_A2(c);
            if (maxevents > SENTRY_BUFSZ / SENTRY_EPEV_SZ) maxevents = SENTRY_BUFSZ / SENTRY_EPEV_SZ;
            uint64_t bytes = maxevents * SENTRY_EPEV_SZ;
            SENTRY_REQUIRE_WRITE(G_A1(c), bytes);
            R->a[2] = maxevents;
        }
        R->redir[1] = 0;
        R->a[4] = 0;
        break;
    // ---- fd-table ops on sentry-owned fds (item 3) ----
    case 25: // fcntl(fd, cmd, arg): only F_GETLK/SETLK/SETLKW (5/6/7) carry a flock* in a2. Always redir a2
             // to the ring for those (so the sentry's flock deref can never hit a guest/NULL pointer); copy
             // the inbound lock only if the guest pointer is real.
        if ((int)G_A1(c) >= 5 && (int)G_A1(c) <= 7) {
            if (G_A2(c)) SENTRY_IMPORT_EXACT(R->buf, G_A2(c), SENTRY_FLOCKSZ);
            R->redir[2] = 0;
        }
        break;
    case 29: { // ioctl(fd, req, arg): always redir arg to the ring so the sentry never derefs a guest/NULL
               // pointer; copy in exactly the _IOC_SIZE/table byte count (unsized/unknown -> nothing -> ENOTTY)
        if (G_A2(c)) {
            uint32_t isz, osz;
            sentry_ioctl_sizes((unsigned long)G_A1(c), &isz, &osz);
            if (isz > SENTRY_IOCTLCAP) isz = SENTRY_IOCTLCAP;
            if (isz) SENTRY_IMPORT_EXACT(R->buf, G_A2(c), isz);
        }
        R->redir[2] = 0;
        break;
    }
    case 59: // pipe2(a0=int[2], flags): out fd pair
        SENTRY_REQUIRE_WRITE(G_A0(c), 8);
        R->redir[0] = 0;
        break;
    case 199: // socketpair(domain, type, proto, a3=int[2]): out fd pair
        SENTRY_REQUIRE_WRITE(G_A3(c), 8);
        R->redir[3] = 0;
        break;
    default:
        return 0; // 57 close / 62 lseek / 198 socket / 201 listen / 210 shutdown / 20 epoll_create1 /
                  // 23 dup / 24 dup3: no buffer.
    }
    return 1;
}

#define SENTRY_EXPORT_EXACT(dst, src, len)                                                                             \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_copy_to((uint64_t)(dst), (src), _n) != (ssize_t)_n) ret = -EFAULT;                             \
    } while (0)

static int sentry_export_file(struct sentry_marshal *M, int64_t *result) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    int64_t ret = *result;
    switch (nr) {
    case 78: // readlinkat: sentry landed the non-NUL-terminated link bytes after the path window
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[3]) n = (uint32_t)R->a[3];
            ssize_t copied = guest_copy_to(G_A2(c), R->buf + SENTRY_PATHCAP, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        break;
    case 63: // read
    case 67: // pread64
    case 61: // getdents64: the sentry landed ret bytes at buf[0]
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[2]) n = (uint32_t)R->a[2]; // never exceed the window we shipped
            ssize_t copied = guest_copy_to(G_A1(c), R->buf, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        break;
    case 80: // fstat: struct landed at buf[0]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A1(c), R->buf, SENTRY_STATSZ);
        break;
    case 79: // newfstatat: struct landed at buf[SENTRY_PATHCAP]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A2(c), R->buf + SENTRY_PATHCAP, SENTRY_STATSZ);
        break;
    case 291: // statx: struct landed at buf[SENTRY_PATHCAP]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A4(c), R->buf + SENTRY_PATHCAP, SENTRY_STATXSZ);
        break;
    case 71:
        if (ret >= 0 && G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, sizeof(int64_t));
        break;
    case 76:  // splice: advanced in/out offsets land back in the guest's off_in/off_out
    case 285: // copy_file_range: same offset writeback shape
        if (ret >= 0) {
            if (G_A1(c)) SENTRY_EXPORT_EXACT(G_A1(c), R->buf, sizeof(int64_t));
            if (G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf + sizeof(int64_t), sizeof(int64_t));
        }
        break;
    case 65: // readv: scatter the ret bytes the sentry fetched back into the guest iovecs
        if (ret > 0) {
            const struct iovec *giov = M->worker_iov;
            const struct iovec *biov = (const struct iovec *)R->buf;
            uint32_t n = M->worker_iovn, remaining = (uint32_t)ret;
            uint32_t delivered = 0;
            for (uint32_t i = 0; i < n && remaining; i++) {
                uint32_t seg = (uint32_t)biov[i].iov_len; // window length the sentry scattered into
                if (seg > remaining) seg = remaining;
                // the sentry rebased iov_base to a pointer into buf[] (shared at the same VA -> usable here)
                ssize_t copied = guest_copy_to((uint64_t)(uintptr_t)giov[i].iov_base, biov[i].iov_base, seg);
                if (copied != (ssize_t)seg) {
                    if (copied > 0) delivered += (uint32_t)copied;
                    ret = delivered ? (int64_t)delivered : -EFAULT;
                    break;
                }
                delivered += seg;
                remaining -= seg;
            }
        }
        break;
    default: return 0;
    }
    *result = ret;
    return 1;
}

static int sentry_export_socket(struct sentry_marshal *M, int64_t *result) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    int64_t ret = *result;
    switch (nr) {
    // ---- socket family: scatter the out-sockaddr / its length / out-optval / recv data back ----
    case 202: // accept
    case 242: // accept4
    case 204: // getsockname
    case 205: // getpeername: sentry wrote the translated sockaddr to the tail window + the length to SLEN
        // accept/accept4 succeed with ret>=0 (the new fd); getsockname/getpeername with ret==0.
        if (ret >= 0 && G_A2(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF); // length service_local reported
            socklen_t gcap = M->worker_socklen_valid ? M->worker_socklen : 0;
            if (G_A1(c)) {
                socklen_t cpy = outlen < gcap ? outlen : gcap; // truncate to the guest buffer
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(G_A1(c), R->buf + SENTRY_SADDR_OFF, cpy);
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A2(c), &outlen, sizeof outlen);
        }
        break;
    case 207: // recvfrom: recv data landed at buf[0]; src sockaddr + its length in the tail windows
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[2]) n = (uint32_t)R->a[2]; // never exceed the window we shipped
            ssize_t copied = guest_copy_to(G_A1(c), R->buf, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        if (ret >= 0 && G_A5(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
            socklen_t gcap = M->worker_socklen_valid ? M->worker_socklen : 0;
            if (G_A4(c)) {
                socklen_t cpy = outlen < gcap ? outlen : gcap;
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(G_A4(c), R->buf + SENTRY_SADDR_OFF, cpy);
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A5(c), &outlen, sizeof outlen);
        }
        break;
    case 209: // getsockopt: optval landed at the opt window; its length at SLEN
        if (ret == 0 && G_A4(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
            socklen_t gcap = M->worker_socklen_valid ? M->worker_socklen : 0;
            socklen_t eff = gcap < SENTRY_OPTCAP ? gcap : SENTRY_OPTCAP; // we shipped at most OPTCAP
            if (G_A3(c)) {
                socklen_t cpy = outlen < eff ? outlen : eff;
                SENTRY_EXPORT_EXACT(G_A3(c), R->buf + SENTRY_OPT_OFF, cpy);
            }
            if (ret == 0) SENTRY_EXPORT_EXACT(G_A4(c), &outlen, sizeof outlen);
        }
        break;
    // ---- recvmsg (item 2): scatter received data + write back name/control/flags into the guest msghdr ----
    case 212:
        if (ret >= 0 && M->worker_msghdr_valid) {
            uint8_t *h = R->buf; // the ring msghdr copy service_local filled
            uint64_t g_name = *(uint64_t *)(M->worker_msghdr + 0);
            if (g_nonpie_lo) g_name = nonpie_p(g_name);
            uint32_t g_namecap = *(uint32_t *)(M->worker_msghdr + 8);
            uint32_t outnl = *(uint32_t *)(h + 8); // length the sentry reported
            if (g_name && g_namecap) {
                uint32_t cpy = outnl < g_namecap ? outnl : g_namecap;
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(g_name, R->buf + SENTRY_MSGNAME_OFF, cpy);
            }
            *(uint32_t *)(M->worker_msghdr + 8) = outnl;
            uint64_t g_ctl = *(uint64_t *)(M->worker_msghdr + 32);
            if (g_nonpie_lo) g_ctl = nonpie_p(g_ctl);
            uint64_t g_ctlcap = *(uint64_t *)(M->worker_msghdr + 40);
            uint64_t outcl = *(uint64_t *)(h + 40); // control length the sentry wrote
            if (g_ctl && g_ctlcap) {
                uint64_t cpy = outcl < g_ctlcap ? outcl : g_ctlcap;
                if (cpy > SENTRY_MSGCTLCAP) cpy = SENTRY_MSGCTLCAP;
                SENTRY_EXPORT_EXACT(g_ctl, R->buf + SENTRY_MSGCTL_OFF, cpy);
            }
            *(uint64_t *)(M->worker_msghdr + 40) = outcl;
            *(uint32_t *)(M->worker_msghdr + 48) = *(uint32_t *)(h + 48);
            // scatter the ret payload bytes back into the guest's iovec segments
            if (ret > 0) {
                const struct iovec *biov = (const struct iovec *)(R->buf + SENTRY_MSGIOV_OFF);
                uint32_t n = M->worker_msg_iovn;
                uint32_t remaining = (uint32_t)ret, delivered = 0;
                for (uint32_t i = 0; i < n && remaining; i++) {
                    uint32_t seg = (uint32_t)biov[i].iov_len;
                    if (seg > remaining) seg = remaining;
                    ssize_t copied =
                        guest_copy_to((uint64_t)(uintptr_t)M->worker_msg_iov[i].iov_base, biov[i].iov_base, seg);
                    if (copied != (ssize_t)seg) {
                        if (copied > 0) delivered += (uint32_t)copied;
                        ret = delivered ? (int64_t)delivered : -EFAULT;
                        break;
                    }
                    delivered += seg;
                    remaining -= seg;
                }
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A1(c), M->worker_msghdr, SENTRY_MSGHDR_SZ);
        }
        break;
    default: return 0;
    }
    *result = ret;
    return 1;
}

static int sentry_export_misc(struct sentry_marshal *M, int64_t *result) {
    struct cpu *c = M->c;
    struct sentry_ring *R = M->R;
    uint64_t nr = M->nr;
    int64_t ret = *result;
    switch (nr) {
    // ---- multiplexing copy-back (item 3) ----
    case 73: // ppoll: copy back ONLY each entry's revents (+6, 2B). The sentry rewrote the ring pollfd.fd
             //   fields to REAL fds for the kernel, so the guest's own pollfd.fd/events must be left untouched.
        if (ret >= 0 && G_A0(c)) {
            uint32_t nf = (uint32_t)R->a[1];
            for (uint32_t k = 0; k < nf; k++)
                if (guest_copy_to(G_A0(c) + (size_t)k * 8u + 6u, R->buf + (size_t)k * 8u + 6u, 2u) != 2) {
                    ret = -EFAULT;
                    break;
                }
        }
        break;
    case 72: // pselect6: the three fd_sets were narrowed in place
        if (ret >= 0) {
            uint32_t fb = ((uint32_t)G_A0(c) + 7u) / 8u;
            if (fb > 128u) fb = 128u;
            if (G_A1(c)) SENTRY_EXPORT_EXACT(G_A1(c), R->buf + SENTRY_PSEL_RD, fb);
            if (G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf + SENTRY_PSEL_WR, fb);
            if (G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf + SENTRY_PSEL_EX, fb);
        }
        break;
    case 22: // epoll_pwait: ret ready events (SENTRY_EPEV_SZ each, per guest arch) landed at buf[0]
        if (ret > 0 && G_A1(c)) {
            uint32_t mx = (uint32_t)G_A2(c);
            uint32_t got = (uint32_t)ret < mx ? (uint32_t)ret : mx;
            SENTRY_EXPORT_EXACT(G_A1(c), R->buf, got * SENTRY_EPEV_SZ);
        }
        break;
    case 25: // fcntl F_GETLK: the conflicting lock was written back into the ring flock
        if ((int)G_A1(c) == 5 && ret >= 0 && G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, SENTRY_FLOCKSZ);
        break;
    case 29: // ioctl: write back exactly the out bytes the request defines (never clobber past them)
        if (ret >= 0 && G_A2(c)) {
            uint32_t isz, osz;
            sentry_ioctl_sizes((unsigned long)G_A1(c), &isz, &osz);
            if (osz > SENTRY_IOCTLCAP) osz = SENTRY_IOCTLCAP;
            if (osz) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, osz);
        }
        break;
    case 59: // pipe2: out fd pair (both ends sentry fds, virtual to the guest)
        if (ret == 0 && G_A0(c)) SENTRY_EXPORT_EXACT(G_A0(c), R->buf, 8);
        break;
    case 199: // socketpair: out fd pair
        if (ret == 0 && G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf, 8);
        break;
    default:
        return 0; // 56 openat / 57 close / 62 lseek / 64 write / 66 writev / 68 pwrite / 198 socket /
                  // 200 bind / 203 connect / 206 sendto / 208 setsockopt / 210 shutdown / 211 sendmsg /
                  // 20 epoll_create1 / 21 epoll_ctl / 23 dup / 24 dup3: no out bytes
    }
    *result = ret;
    return 1;
}

static int64_t sentry_export(struct sentry_marshal *M) {
    int64_t ret = M->R->ret;
    if (M->nr == 56 && ret >= 0 && ret < HL_NFD) {
        const char *opened_path = (const char *)M->R->buf;
        if (!strcmp(opened_path, "/proc") || !strncmp(opened_path, "/proc/", 6) || !strcmp(opened_path, "/dev/fd"))
            snprintf(g_fdpath[(int)ret], sizeof g_fdpath[(int)ret], "%.*s", (int)sizeof(g_fdpath[(int)ret]) - 1,
                     opened_path);
    }
    if (!sentry_export_file(M, &ret) && !sentry_export_socket(M, &ret)) (void)sentry_export_misc(M, &ret);
    return ret;
}

#undef SENTRY_EXPORT_EXACT

static int sentry_import(struct sentry_marshal *M) {
    int handled = sentry_import_file(M);
    if (!handled) handled = sentry_import_socket(M);
    if (!handled) handled = sentry_import_message(M);
    if (!handled) handled = sentry_import_misc(M);
    return handled;
}

// ------------------------------------------------------------------ the routed trust boundary
// Replaces the direct service_local(c) call for untrusted guests. When g_untrusted is off this is a
// transparent pass-through (trusted path byte-identical to baseline -- and service() already gated us
// out before getting here). When on, fs/net/proc syscalls are marshaled to the sentry over the ring;
// everything else stays local in the worker.
static void syscall_route(struct cpu *c) {
    if (!g_untrusted) {
        service_local(c);
        return;
    }
    // Normalize legacy x86 forms (open->openat, ...) in the worker so we classify by canonical number;
    // a return of 1 means it was fully handled locally (arch_prctl/TLS) -- it must stay here.
    if (G_NORMALIZE(c)) return;
    uint64_t nr = G_NR(c);
    // SA_NOCLDWAIT's handler leaves terminated children pinned as zombies. Publish their sentry releases
    // and collect them before the guest can observe the next syscall (especially wait, kill, or clone).
    sentry_reap_drain();

    /* service_local rebases pointer arguments from a biased ET_EXEC's Linux link range to the host mapping.
     * A FORWARDED call is marshaled before service_local runs, so apply the same table here -- the same
     * table, from nonpie_args.h, restricted to what we forward, because anything else reaches service_local
     * and gets it there. (Two independently maintained copies of this list is what let static x86 strings
     * and buffers be copied from their unmapped low link addresses: empty paths, zero-filled pipe writes.)
     * The fold is idempotent, so a forwarded call that also falls through to service_local is unharmed. */
    sentry_route_rebase(c, nr);

    // exit(93)/exit_group(94): service_local() never returns.  exit(93) also ends the PROCESS when this is
    // its last thread, so release the process table in that case before entering the host syscall.  Missing
    // that distinction leaves the last child's duplicated pipe writers alive in the sentry forever.
    if (sentry_route_exit(c, nr)) return;

    // --- fork/exec/wait lane (item 1) -------------------------------------------------------------------
    // clone(220)/clone3(435): a guest THREAD is a host pthread (stays this process; gets its own lane
    // lazily). A guest FORK is a real worker fork() (the guest address space is worker-side COW memory the
    // sentry cannot duplicate) done LOCALLY by service_local. The freshly forked CHILD inherited the
    // parent's lane + sentry-ownership, so re-init its bookkeeping; the PARENT counts the new child so a
    // later wait4 with no real children doesn't deadlock on the hidden sentry child.
    if (sentry_route_clone(c, nr)) return;
    // execve(221) stays LOCAL: service_local reloads the guest image IN THIS PROCESS (it is not a host
    // execve), so the worker keeps its pid, ring lane, control sockets, sentry, and confinement across it.
    // But because it is NOT a real execve, the kernel never applies FD_CLOEXEC: ask the sentry to close+drop
    // this worker's cloexec virtual fds first, so a guest that set FD_CLOEXEC before exec sees them gone
    // (pipe EOF for the peer, no leaked resources) exactly as on Linux. Only on a SUCCESSFUL exec — if the
    // image load fails (service_local returns with a negated errno in the return reg) the fds must survive.
    if (sentry_route_exec(c, nr)) return;
    // openat(56)/readlinkat(78) of a per-process guest-state /proc file: serve it LOCALLY -- only this
    // worker holds the current image's identity (the sentry's copy is the pre-fork/pre-exec one; see
    // sentry_worker_proc_leaf). readlinkat is pure state (bytes into a guest buffer). openat yields a real
    // worker-local fd, which must not leak into the guest's (fully virtual) descriptor space -- hand it to
    // the sentry for adoption so read/lseek/close forward exactly like any sentry-opened descriptor.
    if (sentry_route_worker_proc(c, nr)) return;
    // wait4(260): reap the guest's child WORKER processes locally. The sentry is ALSO a child of the owner,
    // so a blocking wait-any with no GUEST children would hang on it -> short-circuit to -ECHILD; and never
    // surface the sentry's own pid to the guest. A specific-pid wait passes straight through.
    if (sentry_route_wait(c, nr)) return;
    // waitid(95): the same reap, through the other syscall. See sentry_route_waitid -- without this lane
    // a guest that collects its children with waitid leaves their descriptor tables behind.
    if (sentry_route_waitid(c, nr)) return;
    // file-backed mmap(222): the mapping must live in the WORKER (memory authority) but the fd is
    // sentry-owned and invalid here. Borrow the real fd over this lane's control socket (SCM_RIGHTS), map
    // it locally with the borrowed number, then drop it -- so the worker holds the real fd only for the
    // single mmap. Anonymous mmap (MAP_ANON 0x20) needs no fd and stays fully local below.
    if (sentry_route_file_mmap(c, nr)) return;

    if (!sentry_forwarded(nr)) {
        service_local(c); // LOCAL authority (its G_NORMALIZE re-runs as a no-op on already-*at registers)
        return;
    }

    struct sentry_ring *R = ring_for_thread(); // this worker thread's private ring (pool, keyed lazily)
    // Producer lock: at <=N concurrent worker threads each owns a distinct ring and this is an
    // uncontended single TAS; overflow threads (sharing a lane) serialize here, preserving the SPSC
    // ping-pong on the shared ring. Held across the whole round-trip + the output copy-back.
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    R->wpid = (uint32_t)g_worker_pid; // stamp the worker PROCESS: selects this guest's virtual fd table (P1/P2)
    R->wtid = t_token;
    R->inherit_wtid = 0;
    {
        uint32_t groups[HL_NGROUPS_MAX];
        hl_dac_credentials credentials = dac_credentials_current(groups);
        R->credentials.fsuid = credentials.fsuid;
        R->credentials.fsgid = credentials.fsgid;
        R->credentials.group_count = (uint32_t)credentials.group_count;
        R->credentials.capabilities = credentials.capabilities;
        for (size_t index = 0; index < credentials.group_count; ++index)
            R->credentials.groups[index] = credentials.groups[index];
    }
#ifdef G_PROF_EXTRA
    R->rawnr = hl_linux_syscall_guest_number(HL_LINUX_GUEST_X86_64, nr);
    if (R->rawnr == UINT64_MAX) {
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        G_RET(c) = (uint64_t)(int64_t)-ENOSYS;
        return;
    }
#else
    R->rawnr = nr;
#endif
    R->a[0] = G_A0(c);
    R->a[1] = G_A1(c);
    R->a[2] = G_A2(c);
    R->a[3] = G_A3(c);
    R->a[4] = G_A4(c);
    R->a[5] = G_A5(c);
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    R->iovn = 0;
    R->inlen = 0;

    struct sentry_marshal M = {.c = c, .R = R, .nr = nr};
    int imported = sentry_import(&M);
    if (imported <= 0) {
        if (!imported) G_RET(c) = (uint64_t)(int64_t)-ENOSYS;
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        return;
    }

    // ---- ring round-trip ----
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    sentry_request_publish(R); // publish request -> sentry
    sentry_response_wait(R, request);

    int64_t ret = sentry_export(&M);
    G_RET(c) = (uint64_t)ret;
    atomic_store_explicit(&R->busy, 0, memory_order_release); // release the producer lock (round-trip done)
}
