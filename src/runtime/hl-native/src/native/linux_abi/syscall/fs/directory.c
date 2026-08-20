static void svc_fs_directory_57(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 57: {
        int cf = (int)a0;
        engine_fd_vacate(cf); // guest close must not clobber an engine-private fd (g_root_fd etc.) on this number
        // Drop every engine-side emulation-table entry for this fd (eventfd peer/timerfd/overlay-dir/socket/epoll/
        // flock/pidfd/memf/getdents caches/path) BEFORE the real close, so a reused number can't be misrouted.
        // SEQPACKET/O_DIRECT-pipe last-close is recorded here while this end is still open, so the shared
        // ownership tracker can wake a blocked peer with EOF. Shared with the execve CLOEXEC sweep.
        fd_reset_emul(cf);
        // A guest that closes its copy right after handing the fd to a peer is the case XNU's unix-rights
        // GC can tear down; retire any receipts that have come in so the engine's holds stay bounded.
        cmsg_inflight_sweep();
        int r = close(cf);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
        // close: -errno on fail
    }
    // getdents64
    default: break;
    }
}

static int overlay_directory_ensure(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    if (!g_ovldir[fd][0] && g_fdpath[fd][0]) {
        char guest_directory[4200];
        uint32_t provider_cursor = 0;
        int mapped = guest_from_host(g_fdpath[fd], guest_directory, sizeof guest_directory);
        if (mapped > 0 &&
            hl_provider_namespace_launch_child(guest_directory, strlen(guest_directory), &provider_cursor) != NULL &&
            path_copy(g_ovldir[fd], sizeof g_ovldir[fd], guest_directory) != 0)
            g_ovldir[fd][0] = 0;
    }
    return g_ovldir[fd][0] != 0;
}

static void svc_fs_directory_61(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 61: {
        int fd = (int)a0;
        (void)overlay_directory_ensure(fd);
        // OVERLAY: merged listing across layers
        if (g_nlower && fd >= 0 && fd < HL_NFD && g_ovldir[fd][0]) {
            ovldents_snapshot *snapshot = ovldents_require(fd);
            if (snapshot == NULL) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
                break;
            }
            // snapshot cache is indexed directly by guest fd (no slot table -> no eviction thrash)
            if (!snapshot->taken) {
                snapshot->taken = 1;
                snapshot->n = overlay_readdir(g_ovldir[fd], &snapshot->nm, &snapshot->ty);
            }
            size_t o = 0;
            int einval = 0;
            while (snapshot->pos < snapshot->n) {
                const char *nm = snapshot->nm[snapshot->pos];
                size_t nl = strlen(nm), lr = (19 + nl + 1 + 7) & ~7ull;
                if (o + lr > (size_t)a2) {
                    // buffer too small for even the first pending entry -> EINVAL (see case 61 below)
                    if (o == 0) einval = 1;
                    break;
                }
                uint8_t record[280] = {0};
                uint8_t *ld = record;
                // The guest result buffer is written directly; a straddling/unmapped destination must
                // EFAULT like Linux copy_to_user, not fault the engine. Validate the exact destination
                // sub-range before every entry: entries that fit in mapped memory are emitted, and the
                // first faulting entry (nothing emitted yet) reports EFAULT.
                if (guest_accessible_prefix(a1 + o, lr, HL_LOGICAL_VMA_WRITE) != lr) {
                    if (o == 0) einval = -1; // sentinel: EFAULT rather than EINVAL
                    break;
                }
                // REAL inode: stat the merged entry (its host backing across upper/lowers), so `ls -i`,
                // `find -inum`, and hardlink detection work on a layered image. The old `pos+1` fabricated a
                // unique per-position number -> every entry looked like a distinct inode (hardlinks/du/rsync
                // dedup broke). Fall back to pos+1 only if the entry can't be stat'd.
                uint64_t d_ino = (uint64_t)snapshot->pos + 1;
                if (nl < 200) {
                    char egp[4300], ehp[4300];
                    int gl = snprintf(egp, sizeof egp, "%s/%s", g_ovldir[fd], nm);
                    if (gl > 0 && (size_t)gl < sizeof egp) {
                        const char *eh = xresolve_overlay(egp, ehp, sizeof ehp);
                        struct stat est;
                        if (eh && lstat(eh, &est) == 0) d_ino = (uint64_t)est.st_ino;
                    }
                }
                *(uint64_t *)(ld + 0) = d_ino;
                // d_off is the cookie for the next directory entry, not an offset within this one
                // result buffer.  The overlay cursor is stored as the shared descriptor offset, so
                // publishing the same position here keeps seekdir and dup aliases in one coordinate
                // system.  A buffer-relative cookie restarts at 24 on every single-entry read.
                *(uint64_t *)(ld + 8) = (uint64_t)snapshot->pos + 1;
                *(uint16_t *)(ld + 16) = (uint16_t)lr;
                *(ld + 18) = snapshot->ty[snapshot->pos];
                memcpy(ld + 19, nm, nl);
                ld[19 + nl] = 0;
                if (guest_copy_to(a1 + o, record, lr) != (ssize_t)lr) {
                    if (o == 0) einval = -1;
                    break;
                }
                o += lr;
                snapshot->pos++;
            }
            // Retaining an exhausted snapshot makes repeated getdents64 calls continue to report EOF.
            G_RET(c) = einval > 0   ? (uint64_t)(int64_t)(-EINVAL)
                       : einval < 0 ? (uint64_t)(int64_t)(-EFAULT)
                                    : (uint64_t)o;
            break;
        }
#if defined(__linux__)
        // Linux already produces the guest's linux_dirent64 wire format.  Read through the original
        // descriptor, not a fdopendir(dup(fd)) stream: DIR buffering advances the shared OFD past entries
        // that only one descriptor-local DIR has consumed, so dup aliases and fork peers incorrectly see
        // EOF.  A bounded staging buffer keeps the host kernel away from guest memory while preserving the
        // kernel-owned shared cursor.  On a guest-memory fault, restore the last published directory cookie
        // so an entry that was not copied remains pending.
        size_t capacity = (size_t)a2;
        if (capacity > (1u << 20)) capacity = 1u << 20;
        if (capacity < 24) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint8_t *records = malloc(capacity);
        if (!records) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        off_t beginning = lseek(fd, 0, SEEK_CUR);
        long count;
        do
            count = syscall(SYS_getdents64, fd, records, capacity);
        while (count < 0 && errno == EINTR);
        if (count <= 0) {
            free(records);
            G_RET(c) = count < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        size_t copied = 0;
        off_t published = beginning;
        while (copied < (size_t)count) {
            if ((size_t)count - copied < 19) {
                copied = 0;
                errno = EIO;
                break;
            }
            uint16_t record_size;
            int64_t next_offset;
            memcpy(&next_offset, records + copied + 8, sizeof next_offset);
            memcpy(&record_size, records + copied + 16, sizeof record_size);
            if (record_size < 24 || record_size > (size_t)count - copied) {
                if (beginning >= 0) (void)lseek(fd, beginning, SEEK_SET);
                copied = 0;
                errno = EIO;
                break;
            }
            if (guest_accessible_prefix(a1 + copied, record_size, HL_LOGICAL_VMA_WRITE) != record_size ||
                guest_copy_to(a1 + copied, records + copied, record_size) != (ssize_t)record_size) {
                if (published >= 0) (void)lseek(fd, published, SEEK_SET);
                if (copied == 0) errno = EFAULT;
                break;
            }
            published = (off_t)next_offset;
            copied += record_size;
        }
        free(records);
        G_RET(c) = copied == 0 && errno == EFAULT ? (uint64_t)(int64_t)(-EFAULT)
                   : copied == 0 && errno == EIO  ? (uint64_t)(int64_t)(-EIO)
                                                  : (uint64_t)copied;
        break;
#endif
        DIR *dir = NULL;
        for (int i = 0; i < g_ndirs; i++)
            if (g_dirs[i].fd == fd) {
                dir = g_dirs[i].d;
                break;
            }
        if (!dir) {
            dir = fdopendir(dup(fd));
            if (!dir) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (g_ndirs < 64) {
                g_dirs[g_ndirs].fd = fd;
                g_dirs[g_ndirs].d = dir;
                g_ndirs++;
            }
        }
        // Cache full (>64 concurrent directory streams): this DIR* was never stored in g_dirs[], so
        // dirs_drop() on the guest's later close(fd) can't release it. Close it after this single call
        // instead of leaking the DIR* + its dup'd host fd on every getdents64 to an untracked dir fd.
        int dir_cached = 0;
        for (int i = 0; i < g_ndirs; i++)
            if (g_dirs[i].fd == fd && g_dirs[i].d == dir) {
                dir_cached = 1;
                break;
            }
        size_t o = 0;
        struct dirent *de;
        long pos = telldir(dir);
        int einval = 0;
        while ((de = readdir(dir))) {
            // Present the guest's own name. On a case-folding host the namespace stores a
            // case-colliding component escaped, and a listing that emitted the stored spelling would
            // hand `ls` a hex blob and make glob-then-open read a name that is not the name. The
            // escape is a total, deterministic function of the guest bytes, so the reverse is a pure
            // decode of this entry -- no lookup, and nothing re-resolved by string.
            char decoded[256];
            const char *name = hl_case_visible(de->d_name, decoded, sizeof decoded);
            size_t nl = strlen(name), lr = (19 + nl + 1 + 7) & ~7ull;
            if (o + lr > (size_t)a2) {
                seekdir(dir, pos);
                // Linux getdents64: a result buffer too small to hold even the first pending entry
                // is EINVAL, not a silent end-of-directory. Only report it when nothing was emitted.
                if (o == 0) einval = 1;
                break;
            }
            uint8_t record[280] = {0};
            uint8_t *ld = record;
            // Guest buffer written directly: a straddling/unmapped destination must EFAULT like Linux
            // copy_to_user, not fault the engine. Validate the exact destination before writing; rewind
            // the stream so the un-emitted entry is not lost, and report EFAULT only when nothing was
            // emitted (matching the kernel's lastdirent behavior).
            if (guest_accessible_prefix(a1 + o, lr, HL_LOGICAL_VMA_WRITE) != lr) {
                seekdir(dir, pos);
                if (o == 0) einval = -1; // sentinel: EFAULT rather than EINVAL
                break;
            }
            *(uint64_t *)(ld + 0) = de->d_ino;
            *(uint64_t *)(ld + 8) = o + lr;
            *(uint16_t *)(ld + 16) = (uint16_t)lr;
            *(ld + 18) = de->d_type;
            memcpy(ld + 19, name, nl);
            ld[19 + nl] = 0;
            if (guest_copy_to(a1 + o, record, lr) != (ssize_t)lr) {
                seekdir(dir, pos);
                if (o == 0) einval = -1;
                break;
            }
            o += lr;
            pos = telldir(dir);
        }
        G_RET(c) = einval > 0 ? (uint64_t)(int64_t)(-EINVAL) : einval < 0 ? (uint64_t)(int64_t)(-EFAULT) : (uint64_t)o;
        if (!dir_cached) closedir(dir); // untracked (cache-full) stream: release it, else DIR* + fd leak
        break;
    }
    // readlinkat(dirfd, path, buf, bufsiz)
    default: break;
    }
}

static void readlink_copy(struct cpu *c, char *buf, size_t size, const char *target, size_t length) {
    if (length > size) length = size;
    memcpy(buf, target, length);
    G_RET(c) = (uint64_t)length;
}

static void readlink_filesystem(struct cpu *c, int dirfd, const char *path, const char *guest_path, char *buf,
                                size_t size);

static int readlink_empty_path(struct cpu *c, int fd, const char *path, char *buf, size_t size) {
    if (!path || path[0] || fd < 0) return 0;
    hl_linux_fd_snapshot snapshot;
    if (bound_snapshot((uint64_t)(uint32_t)fd, &snapshot)) {
        if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->readlink == NULL) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
            return 1;
        }
        hl_host_result linked = g_host_services->file->readlink(
            g_host_services->context, snapshot.host_handle, (hl_host_bytes){.data = buf, .size = size});
        G_RET(c) = linked.status == HL_STATUS_OK ? linked.value
                                                 : (uint64_t)(int64_t)vfs_host_error((hl_status)linked.status);
        return 1;
    }
    char fd_path[4200];
    const char *named = NULL;
    if (fd < HL_NFD && g_opath[fd] && g_fdpath[fd][0] && g_fdpath_guest[fd]) {
        readlink_filesystem(c, AT_FDCWD, g_fdpath[fd], g_fdpath[fd], buf, size);
        return 1;
    }
    if (fd < HL_NFD && g_opath[fd] && g_fdpath[fd][0])
        named = g_fdpath[fd];
    else if (hl_native_fd_path(fd, fd_path, sizeof fd_path) == 0)
        named = fd_path;
    if (!named) {
        G_RET(c) = (uint64_t)(int64_t)(-EBADF);
        return 1;
    }
    ssize_t result = readlink(named, buf, size);
    G_RET(c) = result < 0 ? (uint64_t)(-errno) : (uint64_t)result;
    return 1;
}

static int readlink_proc_identity(struct cpu *c, const char *path, char *buf, size_t size) {
    if (!path) return 0;
    if (!strcmp(path, "/proc/self")) {
        char target[16];
        int length = snprintf(target, sizeof target, "%d", container_pid());
        readlink_copy(c, buf, size, target, (size_t)length);
        return 1;
    }
    if (!strcmp(path, "/proc/thread-self")) {
        char target[32];
        int tid = c->tid ? c->tid : container_pid();
        int length = snprintf(target, sizeof target, "%d/task/%d", container_pid(), tid);
        readlink_copy(c, buf, size, target, (size_t)length);
        return 1;
    }
    if (!strcmp(path, "/proc/mounts")) {
        readlink_copy(c, buf, size, "self/mounts", strlen("self/mounts"));
        return 1;
    }
    return 0;
}

static int readlink_procfd_special(struct cpu *c, int fd, char *buf, size_t size) {
    if (eventfd_peer_is_engine_fd(fd)) {
        G_RET(c) = (uint64_t)(-ENOENT);
        return 1;
    }
    if (fd >= 0 && fd < HL_NFD && g_eventfd_peer[fd]) {
        static const char target[] = "anon_inode:[eventfd]";
        readlink_copy(c, buf, size, target, sizeof target - 1u);
        return 1;
    }
    int pts = pts_index_of_fd(fd);
    if (pts >= 0) {
        char target[32];
        int length = pts_fd_is_master(fd) ? snprintf(target, sizeof target, "/dev/ptmx")
                                          : snprintf(target, sizeof target, "/dev/pts/%d", pts);
        readlink_copy(c, buf, size, target, (size_t)length);
        return 1;
    }
    if (fd_is_ctty(fd)) {
        readlink_copy(c, buf, size, "/dev/pts/0", strlen("/dev/pts/0"));
        return 1;
    }
    return 0;
}

static int readlink_procfd_typed(struct cpu *c, int fd, char *buf, size_t size) {
    if (fd >= 0 && fd < HL_NFD && !strncmp(g_proc_text_desc[fd], "namespace:", 10)) {
        char target[64];
        int length = ns_link_target(g_proc_text_desc[fd] + 10, target, sizeof target);
        if (length < 0)
            G_RET(c) = (uint64_t)(-ENOENT);
        else
            readlink_copy(c, buf, size, target, (size_t)length);
        return 1;
    }
    if (fd >= 0 && fd < HL_NFD && g_fdpath[fd][0]) {
        char target[4200];
        snprintf(target, sizeof target, "%s", g_fdpath[fd]);
        int mapped = g_fdpath_guest[fd] ? 1 : proc_fd_rebase(target, sizeof target);
        if (mapped < 0 || (g_rootfs && mapped == 0))
            G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
        else
            readlink_copy(c, buf, size, target, strlen(target));
        return 1;
    }
    if (bound_source_is_native() && fcntl(fd, F_GETFD) < 0) {
        G_RET(c) = (uint64_t)(-ENOENT);
        return 1;
    }
    uint32_t kind;
    uint64_t device, object;
    if (proc_fdvis_lookup((int)getpid(), fd, &kind, &device, &object)) {
        if (!bound_source_is_native() && kind == HL_HOST_FD_FILE) {
            hl_linux_fd_snapshot snapshot;
            if (g_linux_box == NULL ||
                hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot) != HL_STATUS_OK) {
                G_RET(c) = (uint64_t)(-ENOENT);
                return 1;
            }
            char target[4200];
            if (bound_handle_host_path(snapshot.host_handle, target, sizeof target) != 0) {
                G_RET(c) = (uint64_t)(-ENOENT);
                return 1;
            }
            int mapped = proc_fd_rebase(target, sizeof target);
            if (mapped < 0 || (g_rootfs && mapped == 0))
                G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
            else
                readlink_copy(c, buf, size, target, strlen(target));
            return 1;
        }
        char target[4200];
        int length = proc_fd_link_pid((int)getpid(), fd, target, sizeof target);
        if (length < 0)
            G_RET(c) = (uint64_t)(-ENOENT);
        else
            readlink_copy(c, buf, size, target, (size_t)length);
        return 1;
    }
    if (!bound_source_is_native()) {
        hl_linux_fd_snapshot typed;
        char target[4200];
        if (g_linux_box != NULL && hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &typed) == HL_STATUS_OK &&
            bound_handle_host_path(typed.host_handle, target, sizeof target) == 0) {
            int mapped = proc_fd_rebase(target, sizeof target);
            if (mapped < 0 || (g_rootfs && mapped == 0))
                G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
            else
                readlink_copy(c, buf, size, target, strlen(target));
            return 1;
        }
        // In bound mode fdvis plus the typed table are the complete guest descriptor authority.
        // Falling through to the worker's native table can resolve an unrelated private descriptor
        // that happens to reuse the closed guest number.
        G_RET(c) = (uint64_t)(-ENOENT);
        return 1;
    }
    return 0;
}

static int readlink_procfd_pipe(struct cpu *c, int fd, char *buf, size_t size) {
    if (fd < 0 || fd >= HL_NFD || g_pipe_identity[fd] == 0) return 0;
    char target[64];
    int length = snprintf(target, sizeof target, "pipe:[%llu]", (unsigned long long)g_pipe_identity[fd]);
    readlink_copy(c, buf, size, target, (size_t)length);
    return 1;
}

static void readlink_procfd_pathless(struct cpu *c, int fd, char *buf, size_t size) {
    if (fcntl(fd, F_GETFD) < 0) {
        G_RET(c) = (uint64_t)(-ENOENT);
        return;
    }
    struct stat status;
    int have_status = fstat(fd, &status) == 0;
    char target[64];
    int length;
    if (have_status && S_ISFIFO(status.st_mode))
        length = snprintf(target, sizeof target, "pipe:[%llu]",
                          (unsigned long long)(g_pipe_identity[fd] ? g_pipe_identity[fd] : (uint64_t)status.st_ino));
    else if (have_status && S_ISSOCK(status.st_mode))
        length = snprintf(target, sizeof target, "socket:[%llu]", (unsigned long long)status.st_ino);
    else if (fd >= 0 && fd < HL_NFD && g_eventfd_peer[fd])
        length = snprintf(target, sizeof target, "anon_inode:[eventfd]");
    else if (fd >= 0 && fd < HL_NFD && g_timerfd[fd])
        length = snprintf(target, sizeof target, "anon_inode:[timerfd]");
    else
        length = snprintf(target, sizeof target, "anon_inode:inode");
    readlink_copy(c, buf, size, target, (size_t)length);
}

static int readlink_procfd(struct cpu *c, const char *path, char *buf, size_t size) {
    int fd = procfd_num(path);
    if (fd < 0) return 0;
    if (readlink_procfd_special(c, fd, buf, size) || readlink_procfd_typed(c, fd, buf, size) ||
        readlink_procfd_pipe(c, fd, buf, size))
        return 1;
    char host_path[4200];
    if (hl_native_fd_path(fd, host_path, sizeof host_path) != 0) {
        readlink_procfd_pathless(c, fd, buf, size);
        return 1;
    }
    /* Linux names pathless open descriptions with non-filesystem procfd targets such as
     * "pipe:[inode]", "socket:[inode]", and "anon_inode:[kind]".  readlink(2) on the host
     * returns those names successfully, so reaching this branch does not prove that host_path
     * is an absolute host filesystem path.  Preserve synthetic names verbatim: passing one to
     * proc_fd_rebase() classifies it as outside a rootfs and incorrectly turns a live descriptor
     * link into EACCES. */
    if (hl_proc_fd_pseudo_target(host_path)) {
        readlink_copy(c, buf, size, host_path, strlen(host_path));
        return 1;
    }
    int mapped = proc_fd_rebase(host_path, sizeof host_path);
    if (mapped < 0 || (g_rootfs && mapped == 0))
        G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
    else
        readlink_copy(c, buf, size, host_path[0] ? host_path : "/", strlen(host_path[0] ? host_path : "/"));
    return 1;
}

static int readlink_self_leaf(struct cpu *c, const char *path, char *buf, size_t size) {
    const char *leaf = path ? proc_self_leaf(path) : NULL;
    if (!leaf) return 0;
    if (!strcmp(leaf, "root") || !strcmp(leaf, "cwd")) {
        char host_cwd[4200], guest_cwd[sizeof host_cwd + sizeof g_vols[0].guest];
        const char *target = "/";
        if (!strcmp(leaf, "cwd")) {
            if (!g_rootfs && getcwd(host_cwd, sizeof host_cwd)) {
                int mapped = guest_from_host_volume(host_cwd, guest_cwd, sizeof guest_cwd);
                if (mapped < 0) {
                    G_RET(c) = (uint64_t)(int64_t)mapped;
                    return 1;
                }
                target = mapped > 0 ? guest_cwd : host_cwd;
            } else {
                target = g_cwd[0] ? g_cwd : "/";
            }
        }
        readlink_copy(c, buf, size, target, strlen(target));
        return 1;
    }
    if (!strncmp(leaf, "map_files/", 10) && leaf[10]) {
        char target[4200];
        if (!map_files_target(leaf + 10, target, sizeof target))
            G_RET(c) = (uint64_t)(-ENOENT);
        else
            readlink_copy(c, buf, size, target, strlen(target));
        return 1;
    }
    if (!strncmp(leaf, "ns/", 3) && leaf[3]) {
        char target[64];
        int length = ns_link_target(leaf + 3, target, sizeof target);
        if (length >= 0) {
            readlink_copy(c, buf, size, target, (size_t)length);
            return 1;
        }
    }
    return 0;
}

static int readlink_peer(struct cpu *c, const char *path, char *buf, size_t size) {
    if (!path) return 0;
    int peer = -1, host_pid = 0;
    const char *leaf = proc_any_leaf(path, &peer);
    if (!leaf || !guest_pid_member_checked(peer, &host_pid)) return 0;
    if (!strncmp(leaf, "ns/", 3) && leaf[3]) {
        char target[64];
        int length = ns_link_target(leaf + 3, target, sizeof target);
        if (length >= 0) {
            readlink_copy(c, buf, size, target, (size_t)length);
            return 1;
        }
    }
    if (!strcmp(leaf, "root") || !strcmp(leaf, "cwd")) {
        readlink_copy(c, buf, size, "/", 1);
        return 1;
    }
    if (strncmp(leaf, "fd/", 3) || !leaf[3]) return 0;
    for (const char *digit = leaf + 3; *digit; ++digit)
        if (*digit < '0' || *digit > '9') return 0;
    char target[4200];
    int length = proc_fd_link_pid(host_pid, atoi(leaf + 3), target, sizeof target);
    if (length < 0)
        G_RET(c) = (uint64_t)(-ENOENT);
    else
        readlink_copy(c, buf, size, target, (size_t)length);
    return 1;
}

static int readlink_synth_regular(struct cpu *c, const char *original, const char *path) {
    struct stat status;
    if (!original || path == original) return 0;
    if (strcmp(path, "/proc") && strncmp(path, "/proc/", 6) && strncmp(path, "/sys/fs/cgroup/", 15)) return 0;
    if (strcmp(path, "/proc") && (!synth_stat_raw(path, &status) || S_ISLNK(status.st_mode))) return 0;
    G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
    return 1;
}

static void readlink_filesystem(struct cpu *c, int dirfd, const char *path, const char *guest_path, char *buf,
                                size_t size) {
    char executable[1024];
    if (proc_self_exe(guest_path, executable, sizeof executable)) {
        readlink_copy(c, buf, size, executable, strlen(executable));
        return;
    }
    if (readlink_synth_regular(c, path, guest_path)) return;
    char resolved[4200];
    const char *host_path = atpath(dirfd, path, resolved, sizeof resolved, 1);
    int relative = host_path && host_path[0] != '/';
    int cached_status, cached_length;
    if (!relative && hl_fdcache_readlink_lookup(host_path, &cached_status, buf, size, &cached_length)) {
        G_RET(c) = cached_status < 0 ? (uint64_t)(int64_t)cached_status : (uint64_t)cached_length;
        return;
    }
    ssize_t result = readlinkat(relative ? ATFD((uint64_t)dirfd) : AT_FDCWD, host_path, buf, size);
    if (!relative && (result < 0 || (size_t)result < size))
        hl_fdcache_readlink_store(host_path, result < 0 ? -errno : (int)result, buf, result < 0 ? 0 : (int)result);
    G_RET(c) = result < 0 ? (uint64_t)(-errno) : (uint64_t)result;
}

static void svc_fs_directory_78(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 78: {
        const char *path = (const char *)a1;
        size_t size = (size_t)a3;
        char result[4096];
        if ((int64_t)a3 <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        size_t checked = size > sizeof result ? sizeof result : size;
        if (guest_accessible_prefix(a2, checked, HL_LOGICAL_VMA_WRITE) != checked) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        /* Linux ignores dirfd for absolute operands, but a relative readlinkat must reject a descriptor
         * outside the guest table before lexical projection can fall back to cwd.  Without this check an
         * invalid positive fd was silently converted into a cwd-relative lookup and returned ENOENT. */
        if (path && path[0] && path[0] != '/' && (int)a0 != AT_FDCWD &&
            ((int)a0 < 0 || (int)a0 >= HL_NFD || !g_fdpath[(int)a0][0])) {
            int error = bound_handle_dirfd_error((int)a0);
            if (error != -EACCES) {
                G_RET(c) = (uint64_t)(int64_t)error;
                break;
            }
        }
        if (size > sizeof result) size = sizeof result;
        char absolute[4200];
        const char *guest_path = path;
        if (path) {
            guest_abspath_at((int)a0, path, absolute, sizeof absolute);
            if (!strcmp(absolute, "/proc") || !strncmp(absolute, "/proc/", 6) || !strncmp(absolute, "/dev/fd/", 8) ||
                !strncmp(absolute, "/dev/std", 8))
                guest_path = absolute;
        }
        if (!readlink_empty_path(c, (int)a0, path, result, size) &&
            !readlink_proc_identity(c, guest_path, result, size) && !readlink_procfd(c, guest_path, result, size) &&
            !readlink_self_leaf(c, guest_path, result, size) && !readlink_peer(c, guest_path, result, size))
            readlink_filesystem(c, (int)a0, path, guest_path, result, size);
        if ((int64_t)G_RET(c) > 0 && guest_copy_to(a2, result, (size_t)G_RET(c)) != (ssize_t)G_RET(c))
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
        break;
    }
    default: break;
    }
}

static int svc_fs_directory(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                            uint64_t a5) {
    switch (nr) {
    case 57: svc_fs_directory_57(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 61: svc_fs_directory_61(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 78: svc_fs_directory_78(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
