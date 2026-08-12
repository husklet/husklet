struct sentry_proc {
    uint32_t refs;                      // process roots + live thread-token bindings
    int real[SENTRY_VFD_MAX];           // virtual fd -> real sentry fd (-1 = unused slot)
    uint8_t borrowed[SENTRY_VFD_MAX];   // 1 = inherited/borrowed real fd (stdio): never close() it on drop
    uint8_t cloexec[SENTRY_VFD_MAX];    // 1 = FD_CLOEXEC set (O_CLOEXEC open / F_SETFD): swept on guest execve
    uint8_t typed[SENTRY_VFD_MAX];      // 1 = real[] names an opaque ABI descriptor shadow, not a native fd
    uint8_t procfd_dir[SENTRY_VFD_MAX]; // 1 = open description is the synthetic /proc/self/fd directory
};
static struct sentry_proc g_table[SENTRY_NTABLE];

struct sentry_process {
    pid_t wpid;
    uint16_t table;
    uint8_t inuse;
};
static struct sentry_process g_proc[SENTRY_NPROC];

static struct hl_sentry_binding g_binding[SENTRY_NBIND];

static struct hl_sentry_snapshot g_snapshot[SENTRY_NSNAP];
static struct hl_sentry_snapshots g_snapshots = {
    .slot = g_snapshot,
    .count = SENTRY_NSNAP,
};

static pthread_mutex_t g_fd_lock = PTHREAD_MUTEX_INITIALIZER; // guards process/thread tables and mappings

// Initialize a freshly claimed table: empty except stdio 0/1/2 mapped 1:1 and marked BORROWED. (All helpers
// below run with g_fd_lock held by the caller.)
static void proc_init_table(struct sentry_proc *p) {
    memset(p, 0, sizeof *p);
    p->refs = 1;
    for (uint32_t i = 0; i < SENTRY_VFD_MAX; i++) {
        p->real[i] = -1;
    }
    for (int i = 0; i < 3; i++) {
        p->real[i] = i;
        p->borrowed[i] = 1;
    }
}

static int table_claim_locked(void) {
    for (uint32_t i = 0; i < SENTRY_NTABLE; i++)
        if (g_table[i].refs == 0) {
            proc_init_table(&g_table[i]);
            return (int)i;
        }
    return -1;
}

static struct sentry_process *process_lookup_locked(pid_t wpid) {
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (g_proc[i].inuse && g_proc[i].wpid == wpid) return &g_proc[i];
    return NULL;
}

static struct sentry_process *process_find_locked(pid_t wpid) {
    struct sentry_process *p = process_lookup_locked(wpid), *free_slot = NULL;
    if (p) return p;
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (!g_proc[i].inuse) {
            free_slot = &g_proc[i];
            break;
        }
    if (!free_slot) return NULL;
    int table = table_claim_locked();
    if (table < 0) return NULL;
    memset(free_slot, 0, sizeof *free_slot);
    free_slot->table = (uint16_t)table;
    free_slot->wpid = wpid;
    free_slot->inuse = 1;
    return free_slot;
}

static struct hl_sentry_binding *binding_lookup_locked(pid_t wpid, uint32_t token) {
    return hl_sentry_binding_find(g_binding, SENTRY_NBIND, wpid, token);
}

static void sentry_native_close(int descriptor) {
    // The sentry table is the ownership ledger for these private host descriptors.  Guest emulation teardown
    // is intentionally not run here: its OFD rehoming can create aliases that the sentry table cannot track.
    int result = close(descriptor);
    int error = errno;
    // A sentry table owns every native descriptor passed here exactly once.  EBADF or another hard failure
    // means the ownership ledger and kernel disagree; fail closed instead of silently creating an EOF leak.
    if (result != 0 && error != EINTR) abort();
}

static void sentry_bound_close(int descriptor) {
    struct cpu tmp;
    memset(&tmp, 0, sizeof tmp);
    if (!sentry_cpu_set_canonical(&tmp, 57)) abort();
    G_A0(&tmp) = (uint64_t)(uint32_t)descriptor;
    service_local(&tmp);
}

static void sentry_owned_close(int descriptor, int typed) {
    if (typed)
        sentry_bound_close(descriptor);
    else
        sentry_native_close(descriptor);
}

static void sentry_created_close(int descriptor) {
    hl_linux_fd_snapshot snapshot;
    sentry_owned_close(descriptor, bound_snapshot((uint64_t)(uint32_t)descriptor, &snapshot));
}

static void table_release_locked(uint16_t index) {
    struct sentry_proc *table = &g_table[index];
    if (table->refs == 0 || --table->refs != 0) return;
    for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++)
        if (table->real[v] >= 0 && !table->borrowed[v]) sentry_owned_close(table->real[v], table->typed[v]);
    memset(table, 0, sizeof *table);
}

static struct sentry_proc *binding_table_locked(pid_t wpid, uint32_t token, uint32_t inherit, int create) {
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (binding) return &g_table[binding->table];
    if (!create || token == 0) return NULL;

    struct sentry_process *process = process_lookup_locked(wpid);
    if (!process) process = process_find_locked(wpid);
    if (!process) return NULL;
    uint16_t table = process->table;
    if (inherit != 0) {
        struct hl_sentry_binding *parent = binding_lookup_locked(wpid, inherit);
        if (parent) table = parent->table;
    }
    if (hl_sentry_binding_reserve(g_binding, SENTRY_NBIND, wpid, token, table) != 0) return NULL;
    g_table[table].refs++;
    return &g_table[table];
}

static void binding_release_locked(pid_t wpid, uint32_t token) {
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (!binding) return;
    uint16_t table = binding->table;
    memset(binding, 0, sizeof *binding);
    table_release_locked(table);
}

static int binding_prepare_locked(pid_t wpid, uint32_t parent_token, uint32_t child_token) {
    if (child_token == 0 || binding_lookup_locked(wpid, child_token)) return -EINVAL;
    struct sentry_proc *source = binding_table_locked(wpid, parent_token, 0, 1);
    if (!source) return -ENOMEM;
    uint16_t table = (uint16_t)(source - g_table);
    int result = hl_sentry_binding_reserve(g_binding, SENTRY_NBIND, wpid, child_token, table);
    if (result == -EEXIST) return -EINVAL;
    if (result == 0) g_table[table].refs++;
    return result;
}

// Allocate the lowest free virtual fd >= minv, map it to (owned, closeable) real fd `rfd`. Returns vfd, or -1
// if the table is full (caller closes `rfd` and returns -EMFILE -- never leaks the real fd to the guest).
static int vfd_alloc(struct sentry_proc *p, int rfd, uint32_t minv) {
    hl_linux_fd_snapshot snapshot;
    for (uint32_t v = minv; v < SENTRY_VFD_MAX; v++)
        if (p->real[v] < 0) {
            p->real[v] = rfd;
            p->borrowed[v] = 0;
            p->typed[v] = bound_snapshot((uint64_t)(uint32_t)rfd, &snapshot) != 0;
            return (int)v;
        }
    return -1;
}

// Translate a guest virtual fd to its real sentry fd, or -1 if it is not mapped in this table (=> -EBADF).
static int vfd_real(struct sentry_proc *p, int vfd) {
    if (vfd < 0 || (uint32_t)vfd >= SENTRY_VFD_MAX) return -1;
    return p->real[vfd];
}

// Translate an exact procfs descriptor link from the guest's virtual descriptor namespace into the
// sentry's real descriptor namespace.  The path is consumed later by service_local(), whose procfs
// implementation necessarily sees sentry-owned descriptors; forwarding the guest number unchanged can
// therefore alias an unrelated internal descriptor.  Return 1 when translated, 0 for a non-procfd path,
// and -1 when the path names an unmapped guest descriptor.
static int vfd_proc_path(struct sentry_proc *p, char *path, size_t cap) {
    static const char dev_prefix[] = "/dev/fd/";
    static const char self_prefix[] = "/proc/self/fd/";
    const char *digits = NULL;
    size_t prefix_len = 0;
    if (strncmp(path, dev_prefix, sizeof dev_prefix - 1) == 0) {
        digits = path + sizeof dev_prefix - 1;
        prefix_len = sizeof dev_prefix - 1;
    } else if (strncmp(path, self_prefix, sizeof self_prefix - 1) == 0) {
        digits = path + sizeof self_prefix - 1;
        prefix_len = sizeof self_prefix - 1;
    } else {
        return 0;
    }
    if (*digits < '0' || *digits > '9') return 0;
    uint32_t vfd = 0;
    for (const char *s = digits; *s; s++) {
        if (*s < '0' || *s > '9') return 0; // only an exact descriptor-link leaf is translated
        if (vfd >= SENTRY_VFD_MAX || vfd > (UINT32_MAX - (uint32_t)(*s - '0')) / 10u) return -1;
        vfd = vfd * 10u + (uint32_t)(*s - '0');
    }
    int real = vfd_real(p, (int)vfd);
    if (real < 0) return -1;
    int n = snprintf(path + prefix_len, cap - prefix_len, "%d", real);
    return n >= 0 && (size_t)n < cap - prefix_len ? (p->typed[vfd] ? 1 : 2) : -1;
}

// Drop a guest virtual fd from the table. Returns the real fd the caller must close(), or -1 if the entry was
// BORROWED (stdio) or unmapped -- in which case the caller must NOT close the real fd.
static int vfd_drop(struct sentry_proc *p, int vfd) {
    if (vfd < 0 || (uint32_t)vfd >= SENTRY_VFD_MAX || p->real[vfd] < 0) return -1;
    int rfd = p->borrowed[vfd] ? -1 : p->real[vfd];
    p->real[vfd] = -1;
    p->borrowed[vfd] = 0;
    p->cloexec[vfd] = 0;
    p->typed[vfd] = 0;
    p->procfd_dir[vfd] = 0;
    return rfd;
}

static int table_clone_locked(const struct sentry_proc *source) {
    int index = table_claim_locked();
    if (index < 0) return -1;
    struct sentry_proc *copy = &g_table[index];
    for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++) {
        if (source->real[v] < 0) continue;
        copy->cloexec[v] = source->cloexec[v];
        copy->typed[v] = source->typed[v];
        copy->procfd_dir[v] = source->procfd_dir[v];
        if (source->borrowed[v]) {
            copy->real[v] = source->real[v];
            copy->borrowed[v] = 1;
            continue;
        }
        int duplicate;
        if (source->typed[v]) {
            hl_linux_fd_snapshot typed;
            if (!bound_snapshot((uint64_t)(uint32_t)source->real[v], &typed)) {
                table_release_locked((uint16_t)index);
                return -1;
            }
            duplicate = (int)bound_dup_at_least(typed.fd, 0, source->cloexec[v] ? HL_LINUX_FD_CLOEXEC : 0);
        } else {
            duplicate = dup(source->real[v]);
        }
        if (duplicate < 0) {
            table_release_locked((uint16_t)index);
            return -1;
        }
        copy->real[v] = duplicate;
        copy->borrowed[v] = 0;
        hl_native_kqueue_duplicate(source->real[v], duplicate);
        if (duplicate < HL_NFD && source->real[v] >= 0 && source->real[v] < HL_NFD) {
            strcpy(g_fdpath[duplicate], g_fdpath[source->real[v]]);
            strcpy(g_proc_text_desc[duplicate], g_proc_text_desc[source->real[v]]);
            g_proc_text_ro[duplicate] = g_proc_text_ro[source->real[v]];
            g_pagemap_fd[duplicate] = g_pagemap_fd[source->real[v]];
            fd_carry_sock(duplicate, source->real[v]);
        }
    }
    return index;
}

static struct sentry_proc *table_unshare_locked(pid_t wpid, uint32_t token, uint32_t inherit) {
    if (token == 0) return NULL;
    struct sentry_proc *current = binding_table_locked(wpid, token, inherit, 1);
    if (!current) return NULL;
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (!binding) return NULL;
    int clone = table_clone_locked(current);
    if (clone < 0) return NULL;
    uint16_t previous = binding->table;
    binding->table = (uint16_t)clone;
    table_release_locked(previous);
    return &g_table[clone];
}

static int64_t sentry_fork_prepare(pid_t parent, uint32_t token, uint32_t inherit) {
    pthread_mutex_lock(&g_fd_lock);
    struct sentry_proc *source = binding_table_locked(parent, token, inherit, 1);
    int table = source ? table_clone_locked(source) : -1;
    if (table < 0) {
        pthread_mutex_unlock(&g_fd_lock);
        return -ENOMEM;
    }
    int64_t handle = hl_sentry_snapshot_reserve(&g_snapshots, parent, token, (uint16_t)table);
    if (handle < 0) {
        table_release_locked((uint16_t)table);
        pthread_mutex_unlock(&g_fd_lock);
        return handle;
    }
    pthread_mutex_unlock(&g_fd_lock);
    return handle;
}

static int sentry_fork_cancel(pid_t owner, uint32_t token, uint64_t handle) {
    pthread_mutex_lock(&g_fd_lock);
    uint16_t table = 0;
    int result = hl_sentry_snapshot_take(&g_snapshots, owner, token, handle, &table);
    if (result == 0) table_release_locked(table);
    pthread_mutex_unlock(&g_fd_lock);
    return result;
}

// Bind the immutable pre-fork snapshot to the child identity before the private fork barrier releases it.
// The child's first token binding then inherits this process root.
static int sentry_proc_fork(pid_t owner, uint32_t token, uint64_t handle, pid_t child) {
    pthread_mutex_lock(&g_fd_lock);
    if (process_lookup_locked(child)) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EEXIST;
    }
    struct hl_sentry_snapshot *snapshot = hl_sentry_snapshot_find(&g_snapshots, owner, token, handle);
    if (!snapshot) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EINVAL;
    }
    struct sentry_process *process = NULL;
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (!g_proc[i].inuse) {
            process = &g_proc[i];
            *process = (struct sentry_process){
                .wpid = child,
                .table = snapshot->payload,
                .inuse = 1,
            };
            break;
        }
    if (!process) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EAGAIN;
    }
    uint16_t table = 0;
    if (hl_sentry_snapshot_take(&g_snapshots, owner, token, handle, &table) != 0 || table != process->table) abort();
    pthread_mutex_unlock(&g_fd_lock);
    return 0;
}

// Release a worker process's table on its exit: close every OWNED real fd it still holds and free the slot.
// (Borrowed stdio is never closed -- it belongs to the sentry.) The init guest's table is reclaimed by the
// sentry process tearing down; only forked children call this.
static void sentry_proc_release(pid_t wpid) {
    pthread_mutex_lock(&g_fd_lock);
    uint16_t snapshot_table = 0;
    while (hl_sentry_snapshot_take_owner(&g_snapshots, wpid, &snapshot_table))
        table_release_locked(snapshot_table);
    struct sentry_process *p = process_lookup_locked(wpid);
    if (p) {
        uint16_t table = p->table;
        memset(p, 0, sizeof *p);
        table_release_locked(table);
    }
    for (uint32_t i = 0; i < SENTRY_NBIND; i++)
        if (g_binding[i].inuse && g_binding[i].owner == wpid) {
            uint16_t table = g_binding[i].table;
            memset(&g_binding[i], 0, sizeof g_binding[i]);
            table_release_locked(table);
        }
    pthread_mutex_unlock(&g_fd_lock);
}

// guest execve close-on-exec sweep: a guest execve stays local (service_local reloads the image in this
// worker), so nothing closes the FD_CLOEXEC-marked virtual fds the way a real execve would. Walk the
// worker's table and close+drop every OWNED cloexec fd (stdio/borrowed is never closed). Fds WITHOUT
// FD_CLOEXEC survive, exactly as Linux keeps them open across execve.
static int sentry_proc_exec_sweep(pid_t wpid, uint32_t token) {
    pthread_mutex_lock(&g_fd_lock);
    if (!binding_table_locked(wpid, token, 0, 1)) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EAGAIN;
    }
    struct hl_sentry_binding *caller = binding_lookup_locked(wpid, token);
    struct sentry_process *process = process_lookup_locked(wpid);
    struct sentry_proc *p = caller ? &g_table[caller->table] : NULL;
    if (caller && process && process->table != caller->table) {
        uint16_t previous = process->table;
        process->table = caller->table;
        p->refs++;
        table_release_locked(previous);
    }
    for (uint32_t i = 0; i < SENTRY_NBIND; i++)
        if (g_binding[i].inuse && g_binding[i].owner == wpid && g_binding[i].token != token) {
            uint16_t table = g_binding[i].table;
            memset(&g_binding[i], 0, sizeof g_binding[i]);
            table_release_locked(table);
        }
    if (p)
        for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++)
            if (p->real[v] >= 0 && p->cloexec[v]) {
                int typed = p->typed[v];
                int rfd = vfd_drop(p, (int)v);
                if (rfd >= 0) sentry_owned_close(rfd, typed);
            }
    pthread_mutex_unlock(&g_fd_lock);
    return 0;
}

// SENDMSG SCM_RIGHTS (P2 finding G, virtualized): translate every guest VFD in a (Linux-layout, PRIVATE) cmsg
// buffer to its real sentry fd IN PLACE. Returns 0 if every passed fd was a live guest fd (a correct guest
// only ever passes its own, so this is always 0 for it), -1 if any was not mapped -- in which case the whole
// sendmsg is rejected, so a smuggled sentry-internal fd (g_ctl[]/ring/daemon) can never reach the wire.
// Caller holds g_fd_lock. Strictly bounded by `len` -- never derefs past it.
static int sentry_cmsg_translate_out(struct sentry_proc *p, uint8_t *ctl, size_t len) {
    size_t o = 0;
    while (o + 16u <= len) { // Linux struct cmsghdr: {u64 cmsg_len; int level; int type}
        uint64_t clen = *(const uint64_t *)(ctl + o);
        int level = *(const int *)(ctl + o + 8);
        int type = *(const int *)(ctl + o + 12);
        if (clen < 16u || o + clen > len) break;
        if (level == LX_SOL_SOCKET && type == SCM_RIGHTS) {
            size_t nfd = (size_t)(clen - 16u) / sizeof(int);
            for (size_t i = 0; i < nfd; i++) {
                int *slot = (int *)(ctl + o + 16u + i * sizeof(int));
                int rfd = hl_sentry_native_fd(p->real, p->typed, SENTRY_VFD_MAX, *slot);
                if (rfd < 0) return -1; // not a native fd owned by this guest -> reject the whole sendmsg
                *slot = rfd;
            }
        }
        o += (size_t)((clen + 7u) & ~(uint64_t)7u); // CMSG_ALIGN to 8
    }
    return 0;
}

// RECVMSG SCM_RIGHTS (virtualized): the sentry received real fds; allocate a guest VFD for each and rewrite it
// IN PLACE so the guest only ever sees virtual fds. An exhausted table closes the real fd and writes -1.
// Caller holds g_fd_lock. Strictly bounded by `len` -- never derefs past it.
static void sentry_cmsg_translate_in(struct sentry_proc *p, uint8_t *ctl, size_t len) {
    size_t o = 0;
    while (o + 16u <= len) {
        uint64_t clen = *(uint64_t *)(ctl + o);
        int level = *(int *)(ctl + o + 8);
        int type = *(int *)(ctl + o + 12);
        if (clen < 16u || o + clen > len) break;
        if (level == LX_SOL_SOCKET && type == SCM_RIGHTS) {
            size_t nfd = (size_t)(clen - 16u) / sizeof(int);
            for (size_t i = 0; i < nfd; i++) {
                int *slot = (int *)(ctl + o + 16u + i * sizeof(int));
                int v = vfd_alloc(p, *slot, 0);
                if (v < 0) {
                    sentry_native_close(*slot);
                    *slot = -1;
                } else {
                    *slot = v;
                }
            }
        }
        o += (size_t)((clen + 7u) & ~(uint64_t)7u);
    }
}

// 1 if this canonical syscall carries its OPERATING fd in the a0 register (so the boundary translates a0
// virtual->real). The fd-bearing-but-NOT-a0 cases (openat/stat dirfd, dup3 newfd, epoll_ctl target, ppoll/
// pselect fd containers) are handled explicitly in sentry_service_one.
static int fd_in_a0(uint64_t nr) {
    switch (nr) {
    case 46:
    case 47: // ftruncate/fallocate
    case 61:
    case 62:
    case 63:
    case 64:
    case 65:
    case 66:
    case 67:
    case 68:
    case 71:
    case 80: // fs r/w/seek/stat
    case 200:
    case 201:
    case 202:
    case 203:
    case 204:
    case 205:
    case 206:
    case 207: // socket family
    case 208:
    case 209:
    case 210:
    case 211:
    case 212:
    case 242: // sockopt/shutdown/msg/accept4
    case 23:
    case 25:
    case 29:
    case 22: // dup/fcntl/ioctl/epoll_pwait
        return 1;
    default: return 0;
    }
}

// ------------------------------------------------------------------ sentry process body
// Holds host authority. Services ONE marshaled request on ring R: rebuilds a cpu from the marshaled
// registers, redirects each flagged guest-buffer pointer arg into the shared ring (so service_local()
// never touches worker/guest memory) -- including rebasing the flattened readv/writev iovec offsets to
// ring pointers -- and runs the REAL service_local() -- identical jail/proc/overlay policy, identical
// bytes. NOTE: it MUST call service_local() (the canonical switch), not service() -- service() would
// re-enter syscall_route() in this (g_untrusted) process and recurse onto the ring.
static int sentry_control_operation(uint64_t number) {
    switch (number) {
    case SENTRY_OP_FDPASS:
    case SENTRY_OP_ADOPT:
    case SENTRY_OP_FORK_PREPARE:
    case SENTRY_OP_FORK:
    case SENTRY_OP_FORK_CANCEL:
    case SENTRY_OP_EXEC:
    case SENTRY_OP_EXIT:
    case SENTRY_OP_REAP:
    case SENTRY_OP_THREAD_PREPARE:
    case SENTRY_OP_BIND:
    case SENTRY_OP_THREAD_CANCEL:
    case SENTRY_OP_THREAD_EXIT:
    case 436: return 1;
    default: return 0;
    }
}

static void sentry_service_control(struct sentry_ring *R) {
    // fd-lend (item 3): not a syscall -- lend a sentry-owned fd to the worker over THIS ring's control
    // socketpair (SCM_RIGHTS) for a file-backed mmap; the worker maps it locally then drops it. OWNERSHIP
    // (P1, finding F): the lendable fd MUST be one the sentry opened ON BEHALF OF THE GUEST (tracked at
    // openat/socket/accept/dup/pipe2/socketpair/...). An arbitrary worker-named integer -- the sentry's own
    // g_ctl[] control socket, the daemon stdio, any non-guest host fd -- is rejected -EBADF. Detected before
    // any cpu reconstruction. We ALWAYS send a control datagram (with the fd, or empty on reject) so the
    // worker's matching recv stays in lockstep with the round-trip and never desyncs the next lend.
    if (R->rawnr == SENTRY_OP_FDPASS) {
        int idx = (int)(R - g_shm->ring);
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int vfd = (int)(int64_t)R->a[0];
        int rfd = p ? hl_sentry_native_fd(p->real, p->typed, SENTRY_VFD_MAX, vfd) : -1;
        pthread_mutex_unlock(&g_fd_lock);
        if (rfd >= 0) {
            sentry_send_fd(g_ctl[idx][1], rfd);
            R->ret = 0;
        } else {
            sentry_send_fd(g_ctl[idx][1], -1); // empty datagram: keep the worker recv in lockstep
            R->ret = -EBADF;
        }
        R->nserved++;
        return;
    }
    // Reverse adoption (SENTRY_OP_ADOPT): receive a worker-opened real fd from the ring's control
    // socketpair and install it into the calling worker's virtual fd table. The datagram was queued by
    // the worker BEFORE it handed the turn over, so this recv never blocks on a missing message.
    if (R->rawnr == SENTRY_OP_ADOPT) {
        int idx = (int)(R - g_shm->ring);
        int rfd = (idx >= 0 && g_ctl[idx][1] >= 0) ? sentry_recv_fd(g_ctl[idx][1]) : -1;
        if (rfd < 0) {
            R->ret = -EIO;
            R->nserved++;
            return;
        }
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int v = p ? vfd_alloc(p, rfd, 0) : -1;
        if (v >= 0) p->cloexec[v] = (uint8_t)(R->a[0] != 0);
        pthread_mutex_unlock(&g_fd_lock);
        if (v < 0) {
            sentry_native_close(rfd);
            R->ret = -EMFILE;
        } else {
            R->ret = v;
        }
        R->nserved++;
        return;
    }
    // Per-process fd-table control ops (P1/P2): clone the parent's map into a fresh child table on fork;
    // release a worker's table (close its owned real fds) on exit. Neither reconstructs a cpu.
    if (R->rawnr == SENTRY_OP_FORK_PREPARE) {
        R->ret = sentry_fork_prepare((pid_t)R->wpid, R->wtid, R->inherit_wtid);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_FORK) {
        R->ret = sentry_proc_fork((pid_t)R->wpid, R->wtid, R->a[0], (pid_t)R->a[1]);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_FORK_CANCEL) {
        R->ret = sentry_fork_cancel((pid_t)R->wpid, R->wtid, R->a[0]);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_EXEC) {
        R->ret = sentry_proc_exec_sweep((pid_t)R->wpid, R->wtid);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_EXIT) {
        sentry_proc_release((pid_t)R->wpid);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_REAP) {
        sentry_proc_release((pid_t)R->a[0]);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_PREPARE) {
        pthread_mutex_lock(&g_fd_lock);
        R->ret = binding_prepare_locked((pid_t)R->wpid, R->wtid, (uint32_t)R->a[0]);
        pthread_mutex_unlock(&g_fd_lock);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_BIND) {
        pthread_mutex_lock(&g_fd_lock);
        R->ret = binding_table_locked((pid_t)R->wpid, R->wtid, 0, 1) ? 0 : -EAGAIN;
        pthread_mutex_unlock(&g_fd_lock);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_CANCEL) {
        pthread_mutex_lock(&g_fd_lock);
        binding_release_locked((pid_t)R->wpid, (uint32_t)R->a[0]);
        pthread_mutex_unlock(&g_fd_lock);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_EXIT) {
        pthread_mutex_lock(&g_fd_lock);
        binding_release_locked((pid_t)R->wpid, R->wtid);
        pthread_mutex_unlock(&g_fd_lock);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == 436) { /* close_range over this process's virtual descriptor table */
        uint32_t first = (uint32_t)R->a[0], last = (uint32_t)R->a[1];
        uint32_t flags = (uint32_t)R->a[2];
        if ((flags & ~(uint32_t)(2u | 4u)) != 0 || first > last) {
            R->ret = -EINVAL;
        } else {
            if (last >= SENTRY_VFD_MAX) last = SENTRY_VFD_MAX - 1;
            pthread_mutex_lock(&g_fd_lock);
            struct sentry_proc *p = (flags & 2u) ? table_unshare_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid)
                                                 : binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
            if ((flags & 2u) && p == NULL) {
                pthread_mutex_unlock(&g_fd_lock);
                R->ret = -ENOMEM;
                R->nserved++;
                return;
            }
            if (p != NULL && first < SENTRY_VFD_MAX)
                for (uint32_t v = first; v <= last; ++v) {
                    if (p->real[v] < 0) continue;
                    if ((flags & 4u) != 0) {
                        p->cloexec[v] = 1;
                    } else {
                        int typed = p->typed[v];
                        int real = vfd_drop(p, (int)v);
                        if (real >= 0) sentry_owned_close(real, typed);
                    }
                }
            pthread_mutex_unlock(&g_fd_lock);
            R->ret = 0;
        }
        R->nserved++;
        return;
    }
}

struct sentry_fd_state {
    uint8_t psel_save[3][128];
    uint32_t psel_nfds;
    uint8_t psel_present[3];
    uint8_t poll_nval[SENTRY_DATACAP / 8u / 8u + 1u];
};

static __thread struct sentry_fd_state g_sentry_fd_state;

static int sentry_translate_path(struct sentry_proc *p, struct cpu *tmp, int64_t *local_ret) {
    int path_fd = vfd_proc_path(p, (char *)G_A1(tmp), SENTRY_PATHCAP);
    if (path_fd < 0) {
        *local_ret = -ENOENT;
        return 1;
    }
    if (path_fd == 2) g_bound_source_native = 1;
    int d = (int)(int64_t)G_A0(tmp);
    if (d < 0) return 0;
    int r = vfd_real(p, d);
    if (r < 0) return -1;
    char *relative = (char *)G_A1(tmp);
    int procfd_directory = p->procfd_dir[d];
    if (!procfd_directory && relative[0] != '/') {
        char descriptor_path[64];
        char backing[HL_LINUX_PATH_MAX + 1];
        int descriptor_length = snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", r);
        ssize_t backing_length = descriptor_length > 0 && (size_t)descriptor_length < sizeof descriptor_path
                                     ? readlink(descriptor_path, backing, sizeof backing - 1u)
                                     : -1;
        if (backing_length > 0) {
            backing[backing_length] = 0;
            procfd_directory = strstr(backing, "/.hl-proc-fd") != NULL;
        }
    }
    if (relative[0] != '/' && procfd_directory) {
        char joined[SENTRY_PATHCAP];
        int length = snprintf(joined, sizeof joined, "/proc/self/fd/%s", relative);
        if (length < 0 || (size_t)length >= sizeof joined) {
            *local_ret = -ENAMETOOLONG;
            return 1;
        }
        memcpy(relative, joined, (size_t)length + 1u);
        int translated = vfd_proc_path(p, relative, SENTRY_PATHCAP);
        if (translated < 0) {
            *local_ret = -ENOENT;
            return 1;
        }
        if (translated == 2) g_bound_source_native = 1;
        G_A0(tmp) = (uint64_t)(int64_t)-100;
        return 0;
    }
    const char *directory = r < HL_NFD ? g_fdpath[r] : NULL;
    if (relative[0] != '/' && directory != NULL && directory[0]) {
        if (g_rootfs && !strncmp(directory, g_rootfs_canon, g_rootfs_canon_len)) directory += g_rootfs_canon_len;
        if (!strncmp(directory, "/proc/", 6) || !strcmp(directory, "/proc") || !strncmp(directory, "/dev/fd", 7)) {
            char joined[SENTRY_PATHCAP];
            int length = snprintf(joined, sizeof joined, "%s/%s", directory, relative);
            if (length < 0 || (size_t)length >= sizeof joined) {
                *local_ret = -ENAMETOOLONG;
                return 1;
            }
            memcpy(relative, joined, (size_t)length + 1u);
            G_A0(tmp) = (uint64_t)(int64_t)-100;
            return 0;
        }
    }
    G_A0(tmp) = (uint64_t)(int64_t)r;
    return 0;
}

static int sentry_translate_inputs(struct sentry_ring *R, struct cpu *tmp, uint64_t snr, const int have[6],
                                   struct sentry_fd_state *state) {
    // ---- per-process VIRTUAL fd translation (P1): map every guest fd ARGUMENT to its real sentry fd. A guest
    //      fd not in THIS process's table (a sentry-internal fd, another guest's fd, a stale fd) translates to
    //      -EBADF and never reaches the kernel. close + dup3 also mutate the table here (handled fully, then
    //      short-circuit); fds the call CREATES are virtualized on the OUT-path after service_local. ----
    // ppoll: bit k set = pollfd[k] named a POSITIVE virtual fd that is not mapped (stale/closed) -> the
    // OUT-path reports POLLNVAL for it (Linux), rather than the kernel silently ignoring a -1 entry.
    int handled_local = 0;
    int64_t local_ret = 0;
    {
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int eb = (p == NULL);
        g_bound_source_native = 0;
        g_bound_second_native = 0;
        if (p && (fd_in_a0(snr) || snr == 48 || snr == 56 || snr == 78 || snr == 79 || snr == 291 || snr == 439)) {
            int v = (int)(int64_t)G_A0(tmp);
            if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0 && !p->typed[v]) g_bound_source_native = 1;
        }
        if (p) switch (snr) {
            case 71: { // sendfile: a0=output, a1=input; both are virtual descriptors
                int output = (int)(int64_t)G_A0(tmp);
                int input = (int)(int64_t)G_A1(tmp);
                int real_output = vfd_real(p, output);
                int real_input = vfd_real(p, input);
                if (real_output < 0 || real_input < 0) {
                    eb = 1;
                } else {
                    g_bound_source_native = !p->typed[output];
                    g_bound_second_native = !p->typed[input];
                    G_A0(tmp) = (uint64_t)(int64_t)real_output;
                    G_A1(tmp) = (uint64_t)(int64_t)real_input;
                }
                break;
            }
            case 48:
            case 56:
            case 78:
            case 79:
            case 291:
            case 439: { // *at path operations: a0 = dirfd; AT_FDCWD (<0) passes through
                int status = sentry_translate_path(p, tmp, &local_ret);
                if (status < 0) eb = 1;
                if (status > 0) handled_local = 1;
                break;
            }
            case 57: { // close: translate + drop the mapping. A BORROWED (stdio) fd is unmapped but NOT closed.
                int v = (int)(int64_t)G_A0(tmp);
                int r = vfd_real(p, v);
                if (r < 0) {
                    eb = 1;
                    break;
                }
                int typed = p->typed[v];
                if (vfd_drop(p, v) < 0) {
                    handled_local = 1;
                    local_ret = 0;
                    break;
                } // borrowed: success, real fd stays
                sentry_owned_close(r, typed);
                handled_local = 1;
                local_ret = 0;
                break;
            }
            case 24: { // dup3(oldfd, newfd, flags): handled ENTIRELY here -- never let the kernel use the guest's
                       //   virtual newfd as a real target. dup the real oldfd, then bind the guest's chosen virtual
                       //   newfd to the result (closing whatever it named). (fscache flush is skipped -- a pure
                       //   fd-table op.)
                int oldv = (int)(int64_t)G_A0(tmp), newv = (int)(int64_t)G_A1(tmp), flags = (int)G_A2(tmp);
                int rold = vfd_real(p, oldv);
                if (rold < 0) {
                    eb = 1;
                    break;
                }
                handled_local = 1;
                if (oldv == newv) {
                    local_ret = -EINVAL;
                    break;
                } // Linux dup3 EINVAL on equal fds
                if (newv < 0 || (uint32_t)newv >= SENTRY_VFD_MAX) {
                    local_ret = -EBADF;
                    break;
                }
                int rnew;
                if (p->typed[oldv]) {
                    hl_linux_fd_snapshot typed;
                    if (!bound_snapshot((uint64_t)(uint32_t)rold, &typed)) {
                        local_ret = -EBADF;
                        break;
                    }
                    rnew = (int)bound_dup_at_least(typed.fd, 0, (flags & LX_O_CLOEXEC) ? HL_LINUX_FD_CLOEXEC : 0);
                } else {
                    rnew = fcntl(rold, (flags & O_CLOEXEC) ? F_DUPFD_CLOEXEC : F_DUPFD, 0);
                }
                if (rnew < 0) {
                    local_ret = -errno;
                    break;
                }
                int prev_typed = p->typed[newv];
                int prev = vfd_drop(p, newv);
                if (prev >= 0) sentry_owned_close(prev, prev_typed);
                p->real[newv] = rnew;
                p->borrowed[newv] = 0;
                p->typed[newv] = p->typed[oldv];
                p->procfd_dir[newv] = p->procfd_dir[oldv];
                p->cloexec[newv] = (flags & LX_O_CLOEXEC) != 0; // dup3 sets FD_CLOEXEC iff LX_O_CLOEXEC given
                local_ret = newv;
                break;
            }
            case 76:
            case 285: { // splice/copy_file_range(fd_in=a0, fd_out=a2): translate BOTH virtual descriptors
                int r0 = vfd_real(p, (int)(int64_t)G_A0(tmp));
                int r2 = vfd_real(p, (int)(int64_t)G_A2(tmp));
                if (r0 < 0 || r2 < 0)
                    eb = 1;
                else {
                    G_A0(tmp) = (uint64_t)(int64_t)r0;
                    G_A2(tmp) = (uint64_t)(int64_t)r2;
                }
                break;
            }
            case 21: { // epoll_ctl(epfd, op, fd, ev): translate BOTH the epoll fd (a0) and the target fd (a2)
                int r0 = vfd_real(p, (int)(int64_t)G_A0(tmp));
                int r2 = vfd_real(p, (int)(int64_t)G_A2(tmp));
                if (r0 < 0 || r2 < 0)
                    eb = 1;
                else {
                    G_A0(tmp) = (uint64_t)(int64_t)r0;
                    G_A2(tmp) = (uint64_t)(int64_t)r2;
                }
                break;
            }
            case 73: { // ppoll: translate each pollfd.fd (8B/entry, fd at +0) in the ring array to its real fd
                uint32_t nfds = (uint32_t)G_A1(tmp);
                memset(state->poll_nval, 0, sizeof state->poll_nval);
                for (uint32_t k = 0; k < nfds; k++) {
                    int *fdp = (int *)(R->buf + (size_t)k * 8u);
                    int ofd = *fdp;
                    int r = vfd_real(p, ofd);
                    // A POSITIVE fd the sentry never handed this guest is stale/closed -> Linux reports
                    // POLLNVAL for it (remembered here, applied on the OUT-path). A NEGATIVE fd is a
                    // caller-requested ignore and legitimately polls as -1 (revents 0).
                    if (r < 0 && ofd >= 0 && k < sizeof(state->poll_nval) * 8u) state->poll_nval[k >> 3] |= (uint8_t)(1u << (k & 7));
                    *fdp = (r < 0) ? -1 : r; // never forward a wrong fd
                }
                break;
            }
            case 72: { // pselect6: rebuild REAL fd_sets from the virtual ones in place; save the originals so the
                       //   result can be remapped back to virtual fds on the OUT-path
                uint32_t nfds = (uint32_t)G_A0(tmp);
                if (nfds > SENTRY_VFD_MAX) nfds = SENTRY_VFD_MAX;
                state->psel_nfds = nfds;
                uint8_t *win[3] = {R->buf + SENTRY_PSEL_RD, R->buf + SENTRY_PSEL_WR, R->buf + SENTRY_PSEL_EX};
                state->psel_present[0] = (uint8_t)have[1];
                state->psel_present[1] = (uint8_t)have[2];
                state->psel_present[2] = (uint8_t)have[3];
                int maxreal = -1;
                for (int s = 0; s < 3; s++) {
                    if (!state->psel_present[s]) continue;
                    memcpy(state->psel_save[s], win[s], 128); // stash the ORIGINAL virtual set
                    memset(win[s], 0, 128);            // rebuild it as the REAL set
                    for (uint32_t v = 0; v < nfds; v++) {
                        if (!(state->psel_save[s][v >> 3] & (1u << (v & 7)))) continue;
                        int r = vfd_real(p, (int)v);
                        if (r < 0) {
                            eb = 1; // Linux select/pselect: an invalid fd in any set -> EBADF, not a silent skip
                            break;
                        }
                        if ((uint32_t)r >= 1024u) continue; // unrepresentable in the real fd_set -> not selectable
                        win[s][r >> 3] |= (uint8_t)(1u << (r & 7));
                        if (r > maxreal) maxreal = r;
                    }
                    if (eb) break;
                }
                G_A0(tmp) = (uint64_t)(maxreal + 1); // real nfds
                break;
            }
            default:
                if (fd_in_a0(snr)) {
                    int r = vfd_real(p, (int)(int64_t)G_A0(tmp));
                    if (r < 0)
                        eb = 1;
                    else
                        G_A0(tmp) = (uint64_t)(int64_t)r;
                }
                break;
            }
        pthread_mutex_unlock(&g_fd_lock);
        if (eb) {
            R->ret = -EBADF;
            R->nserved++;
            return 1;
        }
        if (handled_local) {
            R->ret = local_ret;
            R->nserved++;
            return 1;
        }
    }

    return 0;
}

static void sentry_translate_outputs(struct sentry_ring *R, struct cpu *tmp, uint64_t snr, int64_t ret,
                                     uint64_t coff, struct sentry_fd_state *state) {
    // ---- VIRTUALIZE newly-created fds (P1): every real fd service_local just produced is mapped to a fresh
    //      per-process virtual fd, so the worker only ever sees virtual numbers. Also remap pselect's narrowed
    //      result fd_sets back to the guest's virtual fd positions. (close drops its mapping on the IN-path;
    //      dup3 is fully handled there.) ----
    {
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        if (p) switch (snr) {
            case 56:
            case 198:
            case 202:
            case 242:
            case 19:
            case 23:
            case 279: // memfd_create: an anonymous sentry-owned file enters the virtual table like an open
            case 20:  // openat/socket/accept*/dup/eventfd2/epoll_create1
                if (ret >= 0) {
                    int v = vfd_alloc(p, (int)ret, 0);
                    if (v < 0) {
                        sentry_created_close((int)ret);
                        R->ret = -EMFILE;
                    } else {
                        // Track the guest's FD_CLOEXEC intent so a later guest execve sweeps this fd. The
                        // CLOEXEC bit (O_CLOEXEC == SOCK_CLOEXEC == EFD_CLOEXEC == EPOLL_CLOEXEC == 0x80000)
                        // rides a different arg per syscall; dup(23)/accept(202) never set it.
                        int cx = 0;
                        switch (snr) {
                        case 56: cx = (R->a[2] & LX_O_CLOEXEC) != 0; break;  // openat flags
                        case 198: cx = (R->a[1] & LX_O_CLOEXEC) != 0; break; // socket type
                        case 242: cx = (R->a[3] & LX_O_CLOEXEC) != 0; break; // accept4 flags
                        case 19: cx = (R->a[1] & LX_O_CLOEXEC) != 0; break;  // eventfd2 flags
                        case 20: cx = (R->a[0] & LX_O_CLOEXEC) != 0; break;  // epoll_create1 flags
                        case 279: cx = (R->a[1] & 1u) != 0; break;           // memfd_create MFD_CLOEXEC
                        default: cx = 0; break;                              // dup(23) / accept(202)
                        }
                        p->cloexec[v] = (uint8_t)cx;
                        if (snr == 56) {
                            const char *opened = (const char *)G_A1(tmp);
                            p->procfd_dir[v] = opened != NULL && !strcmp(opened, "/proc/self/fd");
                        }
                        R->ret = v;
                    }
                }
                break;
            case 25: // fcntl F_DUPFD(0)/F_DUPFD_CLOEXEC(1030): the result is a new real fd -> virtualize it,
                     //   honoring the guest's minimum-fd hint (a2, a virtual lower bound)
                if ((G_A1(tmp) == 0 || G_A1(tmp) == 1030) && ret >= 0) {
                    uint32_t minv = (uint32_t)R->a[2];
                    int v = vfd_alloc(p, (int)ret, minv < SENTRY_VFD_MAX ? minv : 0);
                    if (v < 0) {
                        sentry_created_close((int)ret);
                        R->ret = -EMFILE;
                    } else {
                        p->cloexec[v] = (G_A1(tmp) == 1030); // F_DUPFD_CLOEXEC sets FD_CLOEXEC on the new fd
                        int source_vfd = (int)(int64_t)R->a[0];
                        if (source_vfd >= 0 && (uint32_t)source_vfd < SENTRY_VFD_MAX) {
                            p->typed[v] = p->typed[source_vfd];
                            p->procfd_dir[v] = p->procfd_dir[source_vfd];
                        }
                        R->ret = v;
                    }
                } else if (G_A1(tmp) == 2 /* F_SETFD */) {
                    // Track FD_CLOEXEC on the guest's virtual fd (the real sentry fd's flag is irrelevant to a
                    // guest execve, which is a local image reload). Serve success without a real-fd flag change.
                    int v = (int)(int64_t)R->a[0];
                    if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0) {
                        p->cloexec[v] = (R->a[2] & 1 /* FD_CLOEXEC */) != 0;
                        R->ret = 0;
                    }
                } else if (G_A1(tmp) == 1 /* F_GETFD */) {
                    // Return the guest's tracked FD_CLOEXEC, not the real sentry fd's.
                    int v = (int)(int64_t)R->a[0];
                    if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0) R->ret = p->cloexec[v] ? 1 : 0;
                }
                break;
            case 59:
            case 199: // pipe2 / socketpair: two real fds at buf[0..8) -> virtualize both in place
                if (ret == 0) {
                    int r0 = *(int *)(R->buf), r1 = *(int *)(R->buf + 4);
                    int v0 = vfd_alloc(p, r0, 0), v1 = (v0 >= 0) ? vfd_alloc(p, r1, 0) : -1;
                    if (v0 < 0 || v1 < 0) {
                        if (v0 >= 0) vfd_drop(p, v0);
                        sentry_native_close(r0);
                        sentry_native_close(r1);
                        R->ret = -EMFILE;
                    } else {
                        // pipe2(fds,flags): flags=a1; socketpair(dom,type,proto,fds): SOCK_CLOEXEC rides type=a1.
                        uint8_t cx = (R->a[1] & LX_O_CLOEXEC) != 0;
                        p->typed[v0] = 0;
                        p->typed[v1] = 0;
                        p->cloexec[v0] = cx;
                        p->cloexec[v1] = cx;
                        *(int *)(R->buf) = v0;
                        *(int *)(R->buf + 4) = v1;
                    }
                }
                break;
            case 212: // recvmsg: virtualize any SCM_RIGHTS real fds the sentry received in the control window
                if (ret >= 0 && coff) sentry_cmsg_translate_in(p, R->buf + coff, (size_t)*(uint64_t *)(R->buf + 40));
                break;
            case 73: // ppoll: stamp POLLNVAL(0x20) into revents(+6,2B) for each entry that named a stale/closed
                     //   positive virtual fd (marked on the IN-path). The kernel returned revents 0 for the -1
                     //   we substituted; Linux reports POLLNVAL so an event loop notices the invalidation. A
                     //   POLLNVAL entry also counts toward the ready-fd return value.
            {
                uint32_t nf = (uint32_t)R->a[1];
                for (uint32_t k = 0; k < nf; k++) {
                    if (!(state->poll_nval[k >> 3] & (1u << (k & 7)))) continue;
                    uint16_t *rev = (uint16_t *)(R->buf + (size_t)k * 8u + 6u);
                    if (!(*rev & 0x20u)) {
                        *rev |= 0x20u; // POLLNVAL
                        if (R->ret >= 0) R->ret++;
                    }
                }
            } break;
            case 72: // pselect6: remap the kernel-narrowed REAL fd_sets back to the guest's VIRTUAL fd positions
                if (ret >= 0) {
                    uint8_t *win[3] = {R->buf + SENTRY_PSEL_RD, R->buf + SENTRY_PSEL_WR, R->buf + SENTRY_PSEL_EX};
                    for (int s = 0; s < 3; s++) {
                        if (!state->psel_present[s]) continue;
                        uint8_t out[128];
                        memset(out, 0, sizeof out);
                        for (uint32_t v = 0; v < state->psel_nfds; v++) {
                            if (!(state->psel_save[s][v >> 3] & (1u << (v & 7)))) continue; // only originally-requested fds
                            int r = vfd_real(p, (int)v);
                            if (r < 0 || (uint32_t)r >= 1024u) continue;
                            if (win[s][r >> 3] & (1u << (r & 7))) out[v >> 3] |= (uint8_t)(1u << (v & 7));
                        }
                        memcpy(win[s], out, 128); // worker copies the window -> guest fd_set
                    }
                }
                break;
            default: break;
            }
        pthread_mutex_unlock(&g_fd_lock);
    }
}

static int sentry_prepare_call(struct sentry_ring *R, struct cpu *tmp, uint32_t off[6], int have[6],
                               uint32_t *iovn) {
    memset(tmp, 0, sizeof *tmp);
    G_RAWNR(tmp) = R->rawnr;
    G_A0(tmp) = R->a[0];
    G_A1(tmp) = R->a[1];
    G_A2(tmp) = R->a[2];
    G_A3(tmp) = R->a[3];
    G_A4(tmp) = R->a[4];
    G_A5(tmp) = R->a[5];
    int32_t redir[6];
    for (int i = 0; i < 6; i++) redir[i] = R->redir[i];
    *iovn = R->iovn;
    uint64_t *args[6] = {&G_A0(tmp), &G_A1(tmp), &G_A2(tmp), &G_A3(tmp), &G_A4(tmp), &G_A5(tmp)};
    for (int i = 0; i < 6; i++) {
        if (redir[i] < 0) continue;
        uint32_t offset = (uint32_t)redir[i];
        if (offset >= SENTRY_BUFSZ) return -1;
        off[i] = offset;
        have[i] = 1;
        *args[i] = (uint64_t)(R->buf + offset);
    }
    return 0;
}

static void sentry_copy_private_outputs(struct sentry_ring *R, socklen_t pslen, int slen_back, int msg_built,
                                        uint64_t snr, const uint8_t ph[64]) {
    if (slen_back) *(socklen_t *)(R->buf + SENTRY_SLEN_OFF) = pslen;
    if (msg_built && snr == 212) {
        *(uint32_t *)(R->buf + 8) = *(const uint32_t *)(ph + 8);
        *(uint64_t *)(R->buf + 40) = *(const uint64_t *)(ph + 40);
        *(uint32_t *)(R->buf + 48) = *(const uint32_t *)(ph + 48);
    }
}

static void sentry_service_one(struct sentry_ring *R) {
    if (sentry_control_operation(R->rawnr)) {
        sentry_service_control(R);
        return;
    }
    struct cpu tmp;
    // Snapshot scalars and pointer redirects before validation to avoid shared-ring TOCTOU races.
    uint32_t iovn;
    uint32_t off[6] = {0, 0, 0, 0, 0, 0};
    int have[6] = {0, 0, 0, 0, 0, 0};
    int bad = sentry_prepare_call(R, &tmp, off, have, &iovn) < 0;
    uint64_t snr = bad ? 0 : G_NR(&tmp);
    if (snr == 220 || snr == 435) {
        // Process creation is worker-local memory authority. A stale or corrupted mailbox request must
        // never make a sentry servicer fork into a second consumer of the shared rings.
        R->ret = -EPERM;
        R->nserved++;
        return;
    }
    // Per-servicer-thread PRIVATE iovec[] -- the kernel scatters/gathers through THIS, not the shared ring,
    // so a racing worker thread cannot move a segment after we validated it (finding E). 16B/seg * IOVMAX.
    static __thread struct iovec piov[SENTRY_IOVMAX];
    socklen_t pslen = 0;            // PRIVATE in/out socklen: the kernel never sources the length from shared memory
    int slen_back = 0;              // after the call, mirror pslen back into the SLEN window for the worker copy-back
    uint8_t ph[64];                 // PRIVATE Linux-layout 56-byte msghdr copy (sendmsg/recvmsg graph)
    uint8_t pctl[SENTRY_MSGCTLCAP]; // PRIVATE sendmsg cmsg copy (validated SCM_RIGHTS fds, race-free; finding G)
    int msg_built = 0;
    uint64_t coff = 0; // recvmsg control-window offset (for the SCM_RIGHTS fd-track after the call)

    // ---- P0 finding A/D: clamp EVERY length the kernel will use to read/write buf[] down to the bytes
    //      actually remaining in that ring window (BUFSZ - offset). Correct traffic is already inside its
    //      window, so the min() is a no-op for it; only a hostile over-large length is cut. The worker-side
    //      caps are NOT a security control -- this is the sentry re-deriving the bound from the redir window.
    //      In/out socklen/optlen values are routed through PRIVATE storage (pslen) so the kernel reads the
    //      clamped capacity from sentry memory, race-free, and the output is mirrored back afterwards. ----
    if (!bad) {
        switch (snr) {
        case 61:
        case 63:
        case 67: // getdents64 / read / pread64: a2 = byte count through buf+off[1]
        case 64:
        case 68: // write / pwrite64
        case 200:
        case 203: // bind / connect: a2 = addrlen through buf+off[1]
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            break;
        case 206: // sendto: a2 = data len (off[1]); a5 = destaddr len (off[4])
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            if (have[4] && G_A5(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[4])) G_A5(&tmp) = SENTRY_BUFSZ - off[4];
            break;
        case 207: // recvfrom: a2 = data len; a5 = in/out socklen -> PRIVATE (clamped to window)
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            if (have[5]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_SADDRCAP) pslen = SENTRY_SADDRCAP;
                G_A5(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 202:
        case 242: // accept / accept4
        case 204:
        case 205: // getsockname / getpeername: a2 = in/out socklen -> PRIVATE (clamped)
            if (have[2]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_SADDRCAP) pslen = SENTRY_SADDRCAP;
                G_A2(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 208: // setsockopt: a4 = optlen through buf+off[3]
            if (have[3] && G_A4(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[3])) G_A4(&tmp) = SENTRY_BUFSZ - off[3];
            break;
        case 209: // getsockopt: a4 = in/out optlen -> PRIVATE (clamped to the optval window)
            if (have[4]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_OPTCAP) pslen = SENTRY_OPTCAP;
                G_A4(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 73: // ppoll: a1 = nfds (8B/entry) into the pollfd window [0,DATACAP)
            if (G_A1(&tmp) > (uint64_t)(SENTRY_DATACAP / 8u)) G_A1(&tmp) = SENTRY_DATACAP / 8u;
            break;
        case 72: // pselect6: a0 = nfds -> (nfds+7)/8 <= 128B fits each fd_set window
            if (G_A0(&tmp) > 1024u) G_A0(&tmp) = 1024u;
            break;
        case 22: // epoll_pwait: a2 = maxevents (SENTRY_EPEV_SZ/entry) into the out window [0,BUFSZ)
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ / SENTRY_EPEV_SZ))
                G_A2(&tmp) = SENTRY_BUFSZ / SENTRY_EPEV_SZ;
            break;
        case 48:
        case 56:
        case 78:
        case 79:
        case 291: // openat / newfstatat / statx: force the in-path NUL-terminated within
        case 439:
            R->buf[SENTRY_PATHCAP - 1] = 0; // its window so service_local()'s C-string walk can't run off buf
            break;
        default: break;
        }
    }

    // ---- P0 finding B/E: readv/writev -- bound the segment count, reject a wild base, then COPY the iovec[]
    //      descriptor array OUT of the shared ring into private memory, validate the copy, and point the
    //      kernel at it. (We also mirror the validated descriptors back into buf[] for the worker's own
    //      scatter copy-back; that read is worker-side / intra-principal, not a sentry crossing.) ----
    if (!bad && iovn) {
        if (!have[1]) {
            bad = 1; // iovn>0 with no valid a1 redir window would be a wild deref off buf[] -- reject (finding B.2)
        } else {
            uint32_t maxn = (uint32_t)((SENTRY_BUFSZ - off[1]) / sizeof(struct iovec));
            if (iovn > SENTRY_IOVMAX) iovn = SENTRY_IOVMAX;
            if (iovn > maxn) iovn = maxn;
            struct iovec *iv = (struct iovec *)(R->buf + off[1]); // shared (attacker-writable)
            for (uint32_t k = 0; k < iovn; k++) {
                uint64_t boff = (uint64_t)(uintptr_t)iv[k].iov_base, len = iv[k].iov_len; // read ONCE
                if (boff > SENTRY_BUFSZ || len > SENTRY_BUFSZ || boff + len > SENTRY_BUFSZ) {
                    piov[k].iov_base = R->buf;
                    piov[k].iov_len = 0; // bad seg -> empty (don't escape the ring)
                    iv[k].iov_base = R->buf;
                    iv[k].iov_len = 0;
                } else {
                    piov[k].iov_base = R->buf + boff;
                    piov[k].iov_len = (size_t)len;
                    iv[k].iov_base = R->buf + boff;
                    iv[k].iov_len = (size_t)len;
                }
            }
            G_A1(&tmp) = (uint64_t)piov; // kernel reads the PRIVATE iovec[]
            G_A2(&tmp) = iovn;
        }
    }

    // ---- P0 finding C/E: sendmsg/recvmsg -- build the WHOLE msghdr graph in private memory: a Linux-layout
    //      56-byte header pointing at the private iovec[], with msg_namelen/msg_controllen clamped to their
    //      windows. service_local() reads/writes this private header; nothing it touches is re-read by the
    //      kernel from attacker-writable shared memory. (R->iovn stays 0 for these so the block above is skipped.)
    if (!bad && (snr == 211 || snr == 212)) {
        uint8_t *h = R->buf;
        uint64_t noff = *(uint64_t *)(h + 0);
        uint32_t nlen = *(uint32_t *)(h + 8);
        uint64_t ioff = *(uint64_t *)(h + 16);
        uint64_t in = *(uint64_t *)(h + 24);
        uint64_t clen = *(uint64_t *)(h + 40);
        uint32_t mflags = *(uint32_t *)(h + 48);
        coff = *(uint64_t *)(h + 32);
        if (noff >= SENTRY_BUFSZ || ioff >= SENTRY_BUFSZ || coff >= SENTRY_BUFSZ) {
            bad = 1;
        } else {
            memset(ph, 0, sizeof ph);
            if (noff) {
                if (nlen > (uint32_t)(SENTRY_BUFSZ - noff)) nlen = (uint32_t)(SENTRY_BUFSZ - noff);
                *(uint64_t *)(ph + 0) = (uint64_t)(R->buf + noff); // msg_name -> ring ptr
                *(uint32_t *)(ph + 8) = nlen;                      // msg_namelen, clamped to window
            }
            uint32_t n = 0;
            if (ioff) {
                uint32_t maxn = (uint32_t)((SENTRY_BUFSZ - ioff) / sizeof(struct iovec));
                n = (in > SENTRY_IOVMAX) ? SENTRY_IOVMAX : (uint32_t)in; // bound msg_iovlen (finding C)
                if (n > maxn) n = maxn;
                struct iovec *iv = (struct iovec *)(R->buf + ioff);
                for (uint32_t k = 0; k < n; k++) {
                    uint64_t boff = (uint64_t)(uintptr_t)iv[k].iov_base, len = iv[k].iov_len;
                    if (boff > SENTRY_BUFSZ || len > SENTRY_BUFSZ || boff + len > SENTRY_BUFSZ) {
                        piov[k].iov_base = R->buf;
                        piov[k].iov_len = 0;
                        iv[k].iov_base = R->buf;
                        iv[k].iov_len = 0;
                    } else {
                        piov[k].iov_base = R->buf + boff;
                        piov[k].iov_len = (size_t)len;
                        iv[k].iov_base = R->buf + boff;
                        iv[k].iov_len = (size_t)len;
                    }
                }
                *(uint64_t *)(ph + 16) = (uint64_t)piov; // msg_iov -> PRIVATE iovec[]
            }
            *(uint64_t *)(ph + 24) = n;
            if (coff) {
                if (clen > (uint64_t)(SENTRY_BUFSZ - coff)) clen = SENTRY_BUFSZ - coff;
                if (snr == 211) {
                    // ---- P2 finding G: OUTBOUND SCM_RIGHTS fd validation. A guest sendmsg may only emit fds
                    //      the sentry handed it. Copy the cmsg into PRIVATE memory FIRST (so the validation is
                    //      race-free vs a concurrent worker thread rewriting the ring -- finding E), then verify
                    //      every SCM_RIGHTS fd is guest-owned. If any is not (a smuggled g_ctl[]/ring/daemon fd),
                    //      fail the WHOLE call -EPERM -- simplest and clearly correct; a correct guest only ever
                    //      passes its own fds so all pass and this never fires for it. service_local then sends
                    //      from the validated PRIVATE copy, not attacker-writable shared memory. ----
                    uint64_t ccap = clen > SENTRY_MSGCTLCAP ? SENTRY_MSGCTLCAP : clen; // legit cmsg already <= cap
                    memcpy(pctl, R->buf + coff, (size_t)ccap);
                    // VIRTUALIZE the SCM_RIGHTS fds in the PRIVATE copy: translate each guest VFD -> its real
                    // sentry fd. A non-guest fd (smuggled g_ctl[]/ring/daemon fd) is not in the table -> reject
                    // the whole sendmsg -EPERM, so it can never reach the wire.
                    pthread_mutex_lock(&g_fd_lock);
                    struct sentry_proc *cp = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
                    int ctl_ok = cp && sentry_cmsg_translate_out(cp, pctl, (size_t)ccap) == 0;
                    pthread_mutex_unlock(&g_fd_lock);
                    if (!ctl_ok) {
                        R->ret = -EPERM;
                        R->nserved++;
                        return;
                    }
                    *(uint64_t *)(ph + 32) = (uint64_t)pctl; // msg_control -> validated PRIVATE copy
                    *(uint64_t *)(ph + 40) = ccap;           // msg_controllen, clamped to the control window
                } else {
                    *(uint64_t *)(ph + 32) = (uint64_t)(R->buf + coff); // recvmsg: ring ptr (sentry writes fds here)
                    *(uint64_t *)(ph + 40) = clen;                      // msg_controllen, clamped to window
                }
            }
            *(uint32_t *)(ph + 48) = mflags;
            G_A1(&tmp) = (uint64_t)ph; // service_local reads/writes the PRIVATE msghdr
            msg_built = 1;
        }
    }

    if (bad) {
        R->ret = -EFAULT;
        R->nserved++;
        return;
    }

    if (sentry_translate_inputs(R, &tmp, snr, have, &g_sentry_fd_state)) return;

    service_local(&tmp); // real host authority + container policy (touches only ring + private memory now)
    int64_t ret = (int64_t)G_RET(&tmp);
    R->ret = ret;

    sentry_copy_private_outputs(R, pslen, slen_back, msg_built, snr, ph);

    sentry_translate_outputs(R, &tmp, snr, ret, coff, &g_sentry_fd_state);

    R->nserved++;
}

// One servicer thread per ring: spin for a request, service it, hand the ring back. The orphan-guard
// and the shared quit flag both _exit() the WHOLE sentry process (killing every servicer thread).
static void sentry_ring_loop(struct sentry_ring *R) {
    for (;;) {
        uint32_t spins = 0;
        uint32_t idle_rounds = 0; // yield rounds since the last serviced request (resets per request)
        while (atomic_load_explicit(&R->turn, memory_order_acquire) != 1 ||
               atomic_load_explicit(&R->request, memory_order_acquire) ==
                   atomic_load_explicit(&R->response, memory_order_acquire)) {
            if (atomic_load_explicit(&g_shm->quit, memory_order_acquire)) _exit(0);
            if (++spins > 256) {
                if (getppid() == 1) _exit(0); // orphan-guard: worker died/crashed -> don't spin forever
                // A quiet lane must not burn a core forever: with a 64-lane pool most lanes are idle most
                // of the time, so after ~1k yield rounds fall back to a real sleep. A newly armed turn is
                // still observed within ~100us -- negligible against a forwarded syscall's round-trip --
                // and a BUSY lane (request in flight or back-to-back traffic) never reaches the sleep.
                if (++idle_rounds > 1024) {
                    struct timespec nap = {0, 100000}; // 100us
                    nanosleep(&nap, NULL);
                } else {
                    sched_yield();
                }
                spins = 0;
            }
        }
        uint64_t request = atomic_load_explicit(&R->request, memory_order_acquire);
        sentry_service_one(R);
        atomic_store_explicit(&R->turn, 0, memory_order_release); // hand back to the worker
        atomic_store_explicit(&R->response, request, memory_order_release);
    }
}

static void *sentry_ring_thread(void *p) {
    sentry_ring_loop((struct sentry_ring *)p);
    return NULL; // unreachable (loop _exit()s)
}

// The sentry process body: ONE process (so all servicers share the host fd table) running N servicer
// threads -- one per ring. Spawns N-1 threads for ring[1..N-1] and services ring[0] on the main thread.
