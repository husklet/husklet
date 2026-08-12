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

static void svc_fs_directory_61(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 61: {
        int fd = (int)a0;
        if (fd >= 0 && fd < HL_NFD && !g_ovldir[fd][0] && g_fdpath[fd][0]) {
            char guest_directory[4200];
            uint32_t provider_cursor = 0;
            int mapped = guest_from_host(g_fdpath[fd], guest_directory, sizeof guest_directory);
            if (mapped > 0 &&
                hl_provider_namespace_launch_child(guest_directory, strlen(guest_directory), &provider_cursor) !=
                    NULL &&
                path_copy(g_ovldir[fd], sizeof g_ovldir[fd], guest_directory) != 0)
                g_ovldir[fd][0] = 0;
        }
        // OVERLAY: merged listing across layers
        if (g_nlower && fd >= 0 && fd < HL_NFD && g_ovldir[fd][0]) {
            // snapshot cache is indexed directly by guest fd (no slot table -> no eviction thrash)
            if (!g_ovldents[fd].taken) {
                g_ovldents[fd].taken = 1;
                g_ovldents[fd].n = overlay_readdir(g_ovldir[fd], &g_ovldents[fd].nm, &g_ovldents[fd].ty);
            }
            // The host directory descriptor is the open-file-description state shared by dup() and fork().
            // Keep the synthetic overlay cursor in its offset instead of treating this descriptor-indexed
            // replay cache as authoritative.  Otherwise every alias starts its own snapshot at zero, and a
            // fork copies the parent's cursor rather than observing the child's reads.  It also makes EOF a
            // durable offset: freeing the snapshot at EOF used to make the next call silently restart at zero.
            off_t shared_pos = lseek(fd, 0, SEEK_CUR);
            g_ovldents[fd].pos = shared_pos >= 0 && shared_pos <= g_ovldents[fd].n ? (int)shared_pos : 0;
            size_t o = 0;
            int einval = 0;
            while (g_ovldents[fd].pos < g_ovldents[fd].n) {
                const char *nm = g_ovldents[fd].nm[g_ovldents[fd].pos];
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
                uint64_t d_ino = (uint64_t)g_ovldents[fd].pos + 1;
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
                *(uint64_t *)(ld + 8) = o + lr;
                *(uint16_t *)(ld + 16) = (uint16_t)lr;
                *(ld + 18) = g_ovldents[fd].ty[g_ovldents[fd].pos];
                memcpy(ld + 19, nm, nl);
                ld[19 + nl] = 0;
                if (guest_copy_to(a1 + o, record, lr) != (ssize_t)lr) {
                    if (o == 0) einval = -1;
                    break;
                }
                o += lr;
                g_ovldents[fd].pos++;
            }
            // Publish the cursor through the real descriptor so every alias and fork peer observes it.
            // The snapshot remains owned until the last descriptor number using it is closed; retaining an
            // exhausted snapshot is what makes repeated getdents64 calls continue to report EOF.
            if (!einval) (void)lseek(fd, (off_t)g_ovldents[fd].pos, SEEK_SET);
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
            size_t nl = strlen(de->d_name), lr = (19 + nl + 1 + 7) & ~7ull;
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
            memcpy(ld + 19, de->d_name, nl);
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

static int readlink_empty_path(struct cpu *c, int fd, const char *path, char *buf, size_t size) {
    if (!path || path[0] || fd < 0) return 0;
    char fd_path[4200];
    const char *named = NULL;
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
    }
    uint32_t kind;
    uint64_t device, object;
    if (!proc_fdvis_lookup((int)getpid(), fd, &kind, &device, &object)) return 0;
    char target[4200];
    int length = proc_fd_link_pid((int)getpid(), fd, target, sizeof target);
    if (length < 0)
        G_RET(c) = (uint64_t)(-ENOENT);
    else
        readlink_copy(c, buf, size, target, (size_t)length);
    return 1;
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
    if (!leaf || !proc_pid_member(peer, &host_pid)) return 0;
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
    if (hl_provider_tree_files_active()) {
        char projected[4200];
        guest_abspath_at(dirfd, path, projected, sizeof projected);
        hl_host_result opened = hl_provider_tree_open_root(projected, strlen(projected),
            HL_HOST_FILE_READ | HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0, HL_PROVIDER_TREE_LINK);
        if (opened.status != HL_STATUS_OK) {
            G_RET(c) = (uint64_t)(int64_t)vfs_host_error((hl_status)opened.status);
            return;
        }
        hl_host_result linked = g_host_services->file->readlink(g_host_services->context, opened.value,
                                                                 (hl_host_bytes){.data = buf, .size = size});
        (void)g_host_services->file->close(g_host_services->context, opened.value);
        G_RET(c) = linked.status == HL_STATUS_OK ? linked.value
                                                 : (uint64_t)(int64_t)vfs_host_error((hl_status)linked.status);
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
        const char *p = (const char *)a1;
        size_t bs = (size_t)a3;
        char local_result[4096];
        char *buf = local_result;
        // Linux validates the buffer size FIRST: bufsiz <= 0 is EINVAL even for a nonexistent path.
        if ((int64_t)a3 <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // The link target is written straight into the guest buffer by the branches below (memcpy / host
        // readlink), so a bad/unmapped destination must EFAULT like Linux, not fault the engine. A symlink
        // is at most PATH_MAX bytes, so validating the first min(bufsiz, PATH_MAX) bytes bounds every write.
        {
            size_t chk = bs > 4096 ? 4096 : bs;
            if (guest_accessible_prefix(a2, chk, HL_LOGICAL_VMA_WRITE) != chk) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (bs > sizeof local_result) bs = sizeof local_result;
        do {
            // AT_EMPTY_PATH form: readlinkat(dirfd, "", buf, sz) with an EMPTY pathname operates on the file the
            // DIRFD itself names -- an O_PATH|O_NOFOLLOW fd opened directly on a symlink. macOS has no
            // AT_EMPTY_PATH (and passing "" to host readlinkat yields ENOTDIR/ENOENT), so recover the fd's own
            // host path via F_GETPATH and readlink THAT link. (-- LTP readlinkat01 dir_fd2/emptypath;
            // AT_FDCWD is excluded: an empty path there is a genuine ENOENT, handled by the normal path below.)
            if (p && !p[0] && (int)a0 >= 0) {
                char fp[4200];
                const char *named = NULL;
                /* F_GETPATH may report an O_SYMLINK descriptor's resolved target
                 * rather than the link node.  The open path is the authoritative
                 * identity retained for O_PATH, and preserves the final symlink. */
                if ((int)a0 < HL_NFD && g_opath[(int)a0] && g_fdpath[(int)a0][0])
                    named = g_fdpath[(int)a0];
                else if (hl_native_fd_path((int)a0, fp, sizeof fp) == 0)
                    named = fp;
                if (named) {
                    ssize_t r = readlink(named, buf, bs);
                    G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
                } else {
                    G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                }
                break;
            }
            // Match every /proc magic link on the GUEST-ABSOLUTE path, so readlink("/proc/self/exe"),
            // readlinkat(AT_FDCWD, "proc/self/exe") from "/", and readlinkat(pid_dirfd, "exe") agree
            // byte-exactly. Paths that don't land in /proc (or /dev/fd) keep the raw pointer,
            // so the real-resolution fallback below is byte-identical for ordinary symlinks.
            char gpb[4200];
            const char *gp = p;
            if (p) {
                guest_abspath_at((int)a0, p, gpb, sizeof gpb);
                if (!strcmp(gpb, "/proc") || !strncmp(gpb, "/proc/", 6) || !strncmp(gpb, "/dev/fd/", 8) ||
                    !strncmp(gpb, "/dev/std", 8))
                    gp = gpb;
            }
            // /proc/self is a magic symlink to the caller's own pid; /proc/thread-self resolves to the calling
            // thread's per-task dir "<pid>/task/<tid>" (glibc/tcmalloc/profilers readlink it to reach the current
            // thread's files without a gettid syscall). On the main thread tid==pid. `ls -l /proc` readlinks
            // "self" now that /proc lists it.
            if (p && !strcmp(gp, "/proc/self")) {
                char num[16];
                int l = snprintf(num, sizeof num, "%d", container_pid());
                if ((size_t)l > bs) l = (int)bs;
                memcpy(buf, num, (size_t)l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            if (p && !strcmp(gp, "/proc/thread-self")) {
                char num[32];
                int tid = c->tid ? c->tid : container_pid();
                int l = snprintf(num, sizeof num, "%d/task/%d", container_pid(), tid);
                if ((size_t)l > bs) l = (int)bs;
                memcpy(buf, num, (size_t)l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            // /proc/mounts is itself a symlink to self/mounts (glibc/util-linux realpath it before parsing).
            if (p && !strcmp(gp, "/proc/mounts")) {
                static const char *const mt = "self/mounts";
                size_t l = strlen(mt);
                if (l > bs) l = bs;
                memcpy(buf, mt, l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            // /proc/self/fd/N -> the path host fd N currently points at (recovered via F_GETPATH on macOS).
            int pfn = procfd_num(gp);
            if (pfn >= 0) {
                if (eventfd_peer_is_engine_fd(pfn)) {
                    G_RET(c) = (uint64_t)(-ENOENT);
                    break;
                }
                // a guest-created pty. Its slave must readlink to /dev/pts/N (never the host /dev/ttysNNN)
                // so ttyname(3)/`ls -l /proc/self/fd` resolve the Linux path; its master to the /dev/ptmx
                // multiplexer. Checked ahead of F_GETPATH, which would otherwise leak the host device name.
                {
                    int pn = pts_index_of_fd(pfn);
                    if (pn >= 0) {
                        char nm[32];
                        int l = pts_fd_is_master(pfn) ? snprintf(nm, sizeof nm, "/dev/ptmx")
                                                      : snprintf(nm, sizeof nm, "/dev/pts/%d", pn);
                        if ((size_t)l > bs) l = (int)bs;
                        memcpy(buf, nm, (size_t)l);
                        G_RET(c) = (uint64_t)l;
                        break;
                    }
                }
                // The controlling terminal (stdio pty from `docker run -t`) is named /dev/pts/0 in the
                // container -- return that instead of leaking the host pty device (mac /dev/ttysNNN), so
                // ttyname(3)/`tty`/`ps` resolve a device that actually exists in the guest.
                if (fd_is_ctty(pfn)) {
                    static const char *const cn = "/dev/pts/0";
                    size_t l = strlen(cn);
                    if (l > bs) l = bs;
                    memcpy(buf, cn, l);
                    G_RET(c) = l;
                    break;
                }
                // Typed descriptors live in the Rust host-service table, not at the same native descriptor
                // number in this isolated C worker. Ask that authority for the path before inspecting the
                // worker's unrelated native fd table.
                if (!bound_source_is_native()) {
                    hl_linux_fd_snapshot typed;
                    char target[4200];
                    if (g_linux_box != NULL &&
                        hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)pfn, &typed) == HL_STATUS_OK &&
                        bound_handle_host_path(typed.host_handle, target, sizeof target) == 0) {
                        int mapped = proc_fd_rebase(target, sizeof target);
                        if (mapped < 0 || (g_rootfs && mapped == 0)) {
                            G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                        } else {
                            size_t copied = strlen(target);
                            if (copied > bs) copied = bs;
                            memcpy(buf, target, copied);
                            G_RET(c) = (uint64_t)copied;
                        }
                        break;
                    }
                }
                /* A descriptor supplied through the engine API may deliberately
                 * have no native descriptor at the same number (typed stdio is
                 * the important case).  Resolve its published logical identity
                 * before inspecting the engine process's unrelated native fd. */
                {
                    uint32_t kind;
                    uint64_t device, object;
                    if (proc_fdvis_lookup((int)getpid(), pfn, &kind, &device, &object)) {
                        char target[4200];
                        int length = proc_fd_link_pid((int)getpid(), pfn, target, sizeof target);
                        if (length < 0) {
                            G_RET(c) = (uint64_t)(-ENOENT);
                        } else {
                            size_t copied = (size_t)length > bs ? bs : (size_t)length;
                            memcpy(buf, target, copied);
                            G_RET(c) = (uint64_t)copied;
                        }
                        break;
                    }
                }
                /* Linux exposes anonymous pipes through native /proc, while macOS has no native fd path for
                   them.  Prefer the engine's OFD identity on both hosts so self and peer procfs views report
                   the same object.  A named FIFO has no pipe identity and still follows its filesystem path. */
                if (pfn >= 0 && pfn < HL_NFD && g_pipe_identity[pfn] != 0) {
                    char syn[64];
                    int sl = snprintf(syn, sizeof syn, "pipe:[%llu]", (unsigned long long)g_pipe_identity[pfn]);
                    size_t l = (size_t)sl > bs ? bs : (size_t)sl;
                    memcpy(buf, syn, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
                char gp[4200];
                if (hl_native_fd_path(pfn, gp, sizeof gp) != 0) {
                    // A pathless fd (pipe/socket/eventfd/timerfd/anon inode): Linux still resolves
                    // /proc/self/fd/N to a synthetic "pipe:[ino]" / "socket:[ino]" / "anon_inode:[...]" name --
                    // never EBADF for an OPEN fd. Reproduce that so `ls -l /proc/self/fd`, lsof, and Go's
                    // os.Readlink on a pipe fd work instead of erroring.
                    if (fcntl(pfn, F_GETFD) < 0) {
                        // Linux: the /proc/self/fd entry for a CLOSED fd simply doesn't exist -> ENOENT
                        // (EBADF is only for a bad dirfd argument, never for the named link).
                        G_RET(c) = (uint64_t)(-ENOENT);
                        break;
                    }
                    struct stat ss;
                    int have = fstat(pfn, &ss) == 0;
                    char syn[64];
                    int sl;
                    if (have && S_ISFIFO(ss.st_mode))
                        sl = snprintf(
                            syn, sizeof syn, "pipe:[%llu]",
                            (unsigned long long)(g_pipe_identity[pfn] ? g_pipe_identity[pfn] : (uint64_t)ss.st_ino));
                    else if (have && S_ISSOCK(ss.st_mode))
                        sl = snprintf(syn, sizeof syn, "socket:[%llu]", (unsigned long long)ss.st_ino);
                    else if (pfn >= 0 && pfn < HL_NFD && g_eventfd_peer[pfn])
                        sl = snprintf(syn, sizeof syn, "anon_inode:[eventfd]");
                    else if (pfn >= 0 && pfn < HL_NFD && g_timerfd[pfn])
                        sl = snprintf(syn, sizeof syn, "anon_inode:[timerfd]");
                    else
                        sl = snprintf(syn, sizeof syn, "anon_inode:inode");
                    size_t l = (size_t)sl > bs ? bs : (size_t)sl;
                    memcpy(buf, syn, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
                // Map the host path back into the guest's view: strip the rootfs prefix if jailed AND rebase a
                // bound volume (e.g. /tmp -> a host scratch dir) through the volume table, so the guest never
                // sees the raw host path -- the old rootfs-only strip leaked the macOS /private/tmp path for a
                // fd on a mapped volume. proc_fd_rebase is a no-op for a host path under no known mount.
                int mapped = proc_fd_rebase(gp, sizeof gp);
                if (mapped < 0 || (g_rootfs && mapped == 0)) {
                    G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                    break;
                }
                const char *gpath = gp;
                if (!gpath[0]) gpath = "/";
                size_t l = strlen(gpath);
                if (l > bs) l = bs;
                memcpy(buf, gpath, l);
                G_RET(c) = l;
                break;
            }
            // /proc/[self|pid]/root and /proc/[self|pid]/cwd are magic symlinks: root -> the container's "/",
            // cwd -> the process's current working dir (Go/Rust path code and some init resolve these).
            if (p) {
                const char *leaf = proc_self_leaf(gp);
                if (leaf && (!strcmp(leaf, "root") || !strcmp(leaf, "cwd"))) {
                    char cwb[4200], cwg[sizeof cwb + sizeof g_vols[0].guest];
                    const char *tgt = "/";
                    // Bare mode (no rootfs): the live host cwd IS the guest cwd, except inside a mapped volume
                    // -- readlink() must never hand the guest a host path (see guest_from_host_volume).
                    if (!strcmp(leaf, "cwd")) {
                        if (!g_rootfs && getcwd(cwb, sizeof cwb)) {
                            int mapped = guest_from_host_volume(cwb, cwg, sizeof cwg);
                            if (mapped < 0) {
                                G_RET(c) = (uint64_t)(int64_t)mapped;
                                break;
                            }
                            tgt = mapped > 0 ? cwg : cwb;
                        } else {
                            tgt = g_cwd[0] ? g_cwd : "/";
                        }
                    }
                    size_t l = strlen(tgt);
                    if (l > bs) l = bs;
                    memcpy(buf, tgt, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
                // /proc/[self|pid]/map_files/<start>-<end> -> the path of the file-backed VMA with exactly those
                // bounds. Unintercepted this resolved against the HOST directory and named the engine's own
                // binary and libraries by absolute host path. A name with no matching VMA is ENOENT, as in Linux.
                if (leaf && !strncmp(leaf, "map_files/", 10) && leaf[10]) {
                    char tgt[4200];
                    if (!map_files_target(leaf + 10, tgt, sizeof tgt)) {
                        G_RET(c) = (uint64_t)(-ENOENT);
                        break;
                    }
                    size_t l = strlen(tgt);
                    if (l > bs) l = bs;
                    memcpy(buf, tgt, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
                // /proc/[self|pid]/ns/<name> -> "<name>:[<inode>]" namespace links (nsenter/iproute2 read these;
                // the inode constants are the kernel's initial-namespace values -- stable and plausible).
                if (leaf && !strncmp(leaf, "ns/", 3) && leaf[3]) {
                    char nsb[64];
                    int nl = ns_link_target(leaf + 3, nsb, sizeof nsb);
                    if (nl >= 0) {
                        size_t l = (size_t)nl > bs ? bs : (size_t)nl;
                        memcpy(buf, nsb, l);
                        G_RET(c) = (uint64_t)l;
                        break;
                    }
                }
            }
            // Peer /proc/<pid>/ns/<name>: a container is a single namespace set, so a LIVE peer process's
            // namespace links readlink to the SAME "<name>:[<inode>]" values as self (lsns/nsenter inspect
            // live children by peer pid). proc_self_leaf matches only our own pid, so cover foreign pids here.
            if (p) {
                int peer = -1, hp = 0;
                const char *aleaf = proc_any_leaf(gp, &peer);
                if (aleaf && !strncmp(aleaf, "ns/", 3) && aleaf[3] && proc_pid_member(peer, &hp)) {
                    char nsb[64];
                    int nl = ns_link_target(aleaf + 3, nsb, sizeof nsb);
                    if (nl >= 0) {
                        size_t l = (size_t)nl > bs ? bs : (size_t)nl;
                        memcpy(buf, nsb, l);
                        G_RET(c) = (uint64_t)l;
                        break;
                    }
                }
                if (aleaf && (!strcmp(aleaf, "root") || !strcmp(aleaf, "cwd")) && proc_pid_member(peer, &hp)) {
                    // The process registry does not expose a peer's host cwd capability. Forked peers inherit
                    // the container root/cwd, and returning the confined root is both useful and non-leaking.
                    const char *target = "/";
                    size_t copied = strlen(target);
                    if (copied > bs) copied = bs;
                    memcpy(buf, target, copied);
                    G_RET(c) = (uint64_t)copied;
                    break;
                }
                // Peer /proc/<pid>/fd/<N> -> the fd's target (symlink-target view), read from the peer's libproc
                // fd table (its fds live in another hl worker process; procfd_num rejected the foreign pid above).
                // A closed/absent peer fd -> ENOENT. Opening the link stays deferred (needs cross-process fd
                // passing). proc_self_leaf matched only our own pid, so cover foreign pids here.
                if (aleaf && !strncmp(aleaf, "fd/", 3) && aleaf[3] && proc_pid_member(peer, &hp)) {
                    int isnum = 1;
                    for (const char *t = aleaf + 3; *t; t++)
                        if (*t < '0' || *t > '9') isnum = 0;
                    if (isnum) {
                        char tgt[4200];
                        int tl = proc_fd_link_pid(hp, atoi(aleaf + 3), tgt, sizeof tgt);
                        if (tl < 0) {
                            G_RET(c) = (uint64_t)(-ENOENT);
                            break;
                        }
                        size_t l = (size_t)tl > bs ? bs : (size_t)tl;
                        memcpy(buf, tgt, l);
                        G_RET(c) = (uint64_t)l;
                        break;
                    }
                }
            }
            char ep[1024];
            if (proc_self_exe(gp, ep, sizeof ep)) {
                size_t l = strlen(ep);
                if (l > bs) l = bs;
                memcpy(buf, ep, l);
                G_RET(c) = l;
            } else if (hl_provider_tree_files_active()) {
                char projected_path[4200];
                guest_abspath_at((int)a0, p, projected_path, sizeof projected_path);
                hl_host_result opened = hl_provider_tree_open_root(
                    projected_path, strlen(projected_path),
                    HL_HOST_FILE_READ | HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0, HL_PROVIDER_TREE_LINK);
                if (opened.status != HL_STATUS_OK) {
                    G_RET(c) = (uint64_t)(int64_t)vfs_host_error((hl_status)opened.status);
                } else {
                    hl_host_result linked = g_host_services->file->readlink(g_host_services->context, opened.value,
                                                                            (hl_host_bytes){.data = buf, .size = bs});
                    (void)g_host_services->file->close(g_host_services->context, opened.value);
                    G_RET(c) = linked.status == HL_STATUS_OK
                                   ? linked.value
                                   : (uint64_t)(int64_t)vfs_host_error((hl_status)linked.status);
                }
            } else {
                // A path that EXISTS in the synthesized /proc (or cgroup /sys) view but is not one of the
                // magic links above is a regular file/dir there -> EINVAL, exactly like Linux. It must NOT
                // fall through to ENOENT: glibc/musl realpath() readlink every component and treat ENOENT as
                // "no such path" but EINVAL as "ordinary component" (completeness).
                struct stat ss;
                if (p && gp != p &&
                    (!strcmp(gp, "/proc") || !strncmp(gp, "/proc/", 6) || !strncmp(gp, "/sys/fs/cgroup/", 15)) &&
                    (!strcmp(gp, "/proc") || (synth_stat_raw(gp, &ss) && !S_ISLNK(ss.st_mode)))) {
                    G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                    break;
                }
                char pb[4200];
                // Resolve through atpath (overlay-aware, nofollow=read the link itself, dirfd-relative confined):
                // a bare xlate() only consults the writable upper, so readlink of a lower-only path (e.g. a
                // PATH-launched binary in a read-only image layer) hit a non-existent upper path and returned
                // ENOENT instead of EINVAL -- breaking musl/glibc realpath(), which readlinks each path prefix
                // and treats ENOENT as "no such path" (PostgreSQL find_my_exec: "could not resolve path ...").
                const char *rp = atpath((int)a0, p, pb, sizeof pb, 1);
                // a result atpath left RELATIVE (bare mode, no rootfs) must resolve against the CALLER's
                // dirfd, not the engine cwd -- readlink(2) on it silently used the host cwd, so a dirfd-relative
                // link came back ENOENT/garbage. An absolute result ignores the dirfd, as before.
                int rel = rp && rp[0] != '/';
                int rc, len;
                if (!rel && hl_fdcache_readlink_lookup(rp, &rc, buf, bs, &len)) {
                    G_RET(c) = rc < 0 ? (uint64_t)(int64_t)rc : (uint64_t)len;
                    break;
                }
                ssize_t r = readlinkat(rel ? ATFD(a0) : AT_FDCWD, rp, buf, bs);
                // Cache only absolute keys, and only UNTRUNCATED reads: r == bs may be a clipped read whose
                // stored text would poison a later full-buffer readlink of the same path with the short length.
                if (!rel && (r < 0 || (size_t)r < bs))
                    hl_fdcache_readlink_store(rp, r < 0 ? -errno : (int)r, buf, r < 0 ? 0 : (int)r);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
            }
        } while (0);
        if ((int64_t)G_RET(c) > 0 && guest_copy_to(a2, local_result, (size_t)G_RET(c)) != (ssize_t)G_RET(c))
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
