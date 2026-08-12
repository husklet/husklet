// newfstatat preserves Linux's flag, path-resolution, synthetic-node, and copyout ordering.
static int newfstatat_validate(struct cpu *c, int dirfd, const char *raw, uint64_t flags) {
    if (flags & ~(uint64_t)0x1900) {
        G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        return 0;
    }
    int status = at_dirfd_check(dirfd, raw);
    if (status) {
        G_RET(c) = (uint64_t)(int64_t)status;
        return 0;
    }
    if (raw && !raw[0] && !(flags & 0x1000)) {
        G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
        return 0;
    }
    return 1;
}

static void svc_fs_metadata_79(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 79: {
        struct stat s;
        // newfstatat(dfd, path, buf, flags)
        char pb[4200];
        // AT_SYMLINK_NOFOLLOW (0x100): lstat -- resolve the final component WITHOUT following it.
        const char *raw = (const char *)a1;
        if (!newfstatat_validate(c, (int)a0, raw, a3)) break;
        {
            char service_path[4200];
            const hl_provider_node *service;
            guest_abspath_at((int)a0, raw, service_path, sizeof service_path);
            service = hl_provider_namespace_launch_resolve(service_path, strlen(service_path));
            if (service != NULL) {
                hl_host_result opened;
                hl_host_file_metadata metadata;
                struct stat provider_stat;
                if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) !=
                    GUEST_LINUX_STAT_BYTES) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                opened = hl_provider_files_open_service(service->service, HL_HOST_FILE_READ);
                if (opened.status != HL_STATUS_OK ||
                    g_host_services->file->metadata(g_host_services->context, opened.value, &metadata).status !=
                        HL_STATUS_OK) {
                    if (opened.status == HL_STATUS_OK)
                        (void)g_host_services->file->close(g_host_services->context, opened.value);
                    G_RET(c) = (uint64_t)(int64_t)(-EIO);
                    break;
                }
                (void)g_host_services->file->close(g_host_services->context, opened.value);
                memset(&provider_stat, 0, sizeof provider_stat);
                provider_stat.st_mode = (service->kind == HL_PROVIDER_NODE_CHARACTER ? S_IFCHR
                                         : service->kind == HL_PROVIDER_NODE_BLOCK   ? S_IFBLK
                                                                                     : S_IFREG) |
                                        (mode_t)service->mode;
                provider_stat.st_uid = (uid_t)service->uid;
                provider_stat.st_gid = (gid_t)service->gid;
                if (service->kind == HL_PROVIDER_NODE_CHARACTER || service->kind == HL_PROVIDER_NODE_BLOCK)
                    provider_stat.st_rdev = (dev_t)hl_linux_device_make(service->major, service->minor);
                provider_stat.st_size = (off_t)metadata.size;
                provider_stat.st_nlink = 1;
                (void)guest_fill_linux_stat(a2, &provider_stat, NULL, -1);
                G_RET(c) = 0;
                break;
            }
        }
        int procfd = procfd_num_at((int)a0, raw) >= 0 || procfd_directory_path(raw);
        const char *p = procfd ? raw : atpath((int)a0, raw, pb, sizeof pb, (a3 & 0x100) ? 1 : 0);
        if (resolve_loop_detected()) { // a followed self/cyclic symlink chain past the traversal limit -> ELOOP
            G_RET(c) = (uint64_t)(int64_t)(-ELOOP);
            break;
        }
        {
            const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
            // A dirfd-RELATIVE name (fstatat(pid_dirfd, "exe")) that lands in /proc must hit the same
            // magic-link synthesis as its absolute spelling (consistency; bare mode included, where
            // atpath leaves the raw relative path untouched).
            char gsyn[4200];
            if (raw && raw[0] && raw[0] != '/') {
                guest_abspath_at((int)a0, raw, gsyn, sizeof gsyn);
                if (!strncmp(gsyn, "/proc/", 6) || !strncmp(gsyn, "/dev/fd/", 8)) gp = gsyn;
            } else if (raw && (!strncmp(raw, "/proc/", 6) || !strncmp(raw, "/dev/fd/", 8))) {
                /* Synthetic absolute paths must be classified from the guest spelling. atpath() resolves
                 * ordinary host-backed paths and may already have followed the rootfs's /dev/fd symlink
                 * into a host pathname that cannot represent the synthetic proc namespace. statx already
                 * preserves /dev/fd this way; newfstatat must observe the same coherent namespace. */
                gp = raw;
            }
            char procfd_path[64];
            gp = procfd_namespace_path(gp, procfd_path, sizeof procfd_path);
            char ep[1024];
            if (proc_self_exe(gp, ep, sizeof ep)) {
                struct stat es;
                // The magic /proc/self/exe always "exists", so validate the guest stat buffer now (before
                // the engine fills it directly) -> a bad pointer is -EFAULT, matching Linux's copyout.
                if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) !=
                    GUEST_LINUX_STAT_BYTES) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                if (a3 & 0x100) { // lstat: report the magic symlink itself (Linux: st_size == 0)
                    memset(&es, 0, sizeof es);
                    es.st_mode = S_IFLNK | 0777;
                    es.st_size = 0;
                    es.st_nlink = 1;
                    (void)guest_fill_linux_stat(a2, &es, NULL, -1); // synth /proc/self/exe symlink
                    G_RET(c) = 0;
                    break;
                }
                // stat (follow): stat the actual executable file through the jail
                char hb[4200];
                const char *hp = xresolve_overlay(ep, hb, sizeof hb);
                if (stat(hp, &es) == 0) {
                    (void)guest_fill_linux_stat(a2, &es, hp, -1);
                    G_RET(c) = 0;
                    break;
                }
                // file unexpectedly missing -> fall through to the generic ENOENT path
            }
            // /proc/[self|pid]/{root,cwd} magic symlinks: lstat reports the link, stat follows to the dir.
            {
                const char *sleaf = proc_self_leaf(gp);
                if (sleaf && (!strcmp(sleaf, "root") || !strcmp(sleaf, "cwd"))) {
                    // Magic /proc/self/{root,cwd} always resolves; validate the guest stat buffer before the
                    // engine fills it -> a bad pointer is -EFAULT, matching Linux's copyout ordering.
                    if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) !=
                        GUEST_LINUX_STAT_BYTES) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                    char cwb[4200], cwg[sizeof cwb + sizeof g_vols[0].guest];
                    const char *tgt = "/";
                    // Bare mode: the live host cwd IS the guest cwd, except inside a mapped volume. tgt is a
                    // GUEST path here -- it is handed to xresolve_overlay below, which maps guest -> host.
                    if (!strcmp(sleaf, "cwd")) {
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
                    struct stat es;
                    if (a3 & 0x100) { // lstat: the symlink itself (Linux: st_size == 0)
                        memset(&es, 0, sizeof es);
                        es.st_mode = S_IFLNK | 0777;
                        es.st_size = 0;
                        es.st_nlink = 1;
                        (void)guest_fill_linux_stat(a2, &es, NULL, -1);
                        G_RET(c) = 0;
                        break;
                    }
                    char hb[4200];
                    const char *hp = xresolve_overlay(tgt, hb, sizeof hb);
                    if (stat(hp, &es) == 0) {
                        (void)guest_fill_linux_stat(a2, &es, hp, -1);
                        G_RET(c) = 0;
                        break;
                    }
                }
            }
            // synthesized /proc or /sys file: split synth_stat so we only validate the guest buffer once we
            // KNOW it is a synth path (which "exists") -> a bad pointer is -EFAULT on copyout, and a
            // non-synth path falls through to the generic handler below with Linux's normal ordering.
            {
                struct stat synth_s;
                if (sysnet_hidden(gp)) {
                    G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                    break;
                }
                int procfd_status = (a3 & 0x100) ? 0 : procfd_follow_stat(gp, &synth_s);
                if (procfd_status < 0) {
                    G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                    break;
                }
                if (procfd_status > 0 || synth_stat_raw(gp, &synth_s)) {
                    if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) !=
                        GUEST_LINUX_STAT_BYTES) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                    (void)guest_fill_linux_stat(a2, &synth_s, NULL, -1);
                    G_RET(c) = 0;
                    break;
                }
            }
        }
        // cacheable: named path, follow
        if (raw && raw[0] && !(a3 & 0x100)) {
            int rc;
            int cache_hit = hl_fdcache_metadata_lookup(p, &rc, &s);
            if (!cache_hit) {
                int r = fstatat(ATFD(a0), p, &s, 0);
                rc = r < 0 ? -errno : 0;
                hl_fdcache_metadata_store(p, rc, &s);
            }
            HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "stat-cache path=%s hit=%d result=%d", p, cache_hit, rc);
            // Validate the guest buffer only after a successful stat (copyout-last: a bad path still
            // reports its own errno first, matching Linux) -> a bad pointer is -EFAULT, not an engine fault.
            if (rc == 0) {
                if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) !=
                    GUEST_LINUX_STAT_BYTES) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                (void)guest_fill_linux_stat(a2, &s, p, -1);
            }
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        // AT_EMPTY_PATH -> fstat(dfd)
        int empty_self = (raw && !raw[0] && (a3 & 0x1000));
        int r = (empty_self && memf_get((int)a0)) ? memf_fstat((int)a0, &s)
                : empty_self                      ? fstat((int)a0, &s)
                                                  : fstatat(ATFD(a0), p, &s, AT_SYMLINK_NOFOLLOW);
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // guest-chown xattr lives on the host backing file: read via fd for AT_EMPTY_PATH, else by path.
        // The stat succeeded above, so validate the guest buffer here (copyout-last) -> bad ptr = -EFAULT.
        if (guest_accessible_prefix(a2, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        (void)guest_fill_linux_stat(a2, &s, empty_self ? NULL : p, empty_self ? (int)a0 : -1);
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_metadata_80(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 80: {
        // fstat(fd, buf)
        struct stat s;
        int sr = memf_get((int)a0) ? memf_fstat((int)a0, &s) : fstat((int)a0, &s);
        if (sr < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // The guest stat buffer is filled DIRECTLY by the engine; validate it (after the fd/stat succeeds,
        // so a bad fd still reports EBADF first, matching Linux's copyout-last ordering) so a bad pointer
        // returns -EFAULT instead of faulting the engine (access_ok).
        if (guest_accessible_prefix(a1, GUEST_LINUX_STAT_BYTES, HL_LOGICAL_VMA_WRITE) != GUEST_LINUX_STAT_BYTES) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        (void)guest_fill_linux_stat(a1, &s, NULL, (int)a0);
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_metadata_81(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 81:
        sync();
        G_RET(c) = 0;
        // sync
        break;
    // syncfs(fd): no macOS syncfs -> flush this fd then sync the system. RAM-backed scratch is a no-op.
    default: break;
    }
}

static void svc_fs_metadata_267(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 267:
        if (!memf_get((int)a0)) {
            fsync((int)a0);
            sync();
        }
        G_RET(c) = 0;
        break;
    // utimensat(dirfd, path, times, flags)
    default: break;
    }
}

static void svc_fs_metadata_88(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 88: {
        // Linux rejects unknown flag bits (only AT_SYMLINK_NOFOLLOW=0x100 is valid) with EINVAL before
        // touching the file -- otherwise a bad flag value would still update the timestamps.
        if (a3 & ~0x100u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        struct timespec *ts = (struct timespec *)a2;
        struct timespec lts[2];
        // Linux and macOS disagree on the tv_nsec "special" sentinels: Linux UTIME_NOW = 0x3fffffff /
        // UTIME_OMIT = 0x3ffffffe, but the host (macOS) wants UTIME_NOW = -1 / UTIME_OMIT = -2. The host
        // utimensat/futimens only honor the macOS values, so a guest passing the Linux sentinels (glibc's
        // futimens/utimensat, and hl's own utime/utimes/futimesat -> utimensat rewrites whenever a field is
        // "set to now") would otherwise write the raw 0x3ffffffe nanoseconds instead of omitting/now-ing the
        // field. Copy out to a local (never mutate guest memory) and translate both slots. a2==NULL stays
        // NULL (= set both to now). EFAULT a bad non-NULL times pointer (we now dereference it in-engine).
        if (a2) {
            if (guest_copy_from(lts, a2, sizeof lts) != sizeof lts) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            for (int i = 0; i < 2; i++) {
                if (lts[i].tv_nsec == 0x3fffffff)
                    lts[i].tv_nsec = UTIME_NOW; // Linux UTIME_NOW  -> macOS
                else if (lts[i].tv_nsec == 0x3ffffffe)
                    lts[i].tv_nsec = UTIME_OMIT; // Linux UTIME_OMIT -> macOS
            }
            ts = lts;
        }
        if (!a1) {
            G_RET(c) = futimens((int)a0, ts) < 0 ? (uint64_t)(-errno) : 0;
            break;
            // path NULL -> futimens(fd)
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, (a3 & 0x100) ? 1 : 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = utimensat(pfd, fin, ts, (a3 & 0x100) ? AT_SYMLINK_NOFOLLOW : 0), e = errno;
            char dp[4200];
            if (r >= 0 && hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0) hl_fdcache_metadata_evict(hp);
                // mtime changed
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = utimensat(ATFD(a0), p, ts, (a3 & 0x100) ? AT_SYMLINK_NOFOLLOW : 0);
        if (r >= 0) hl_fdcache_metadata_evict(p);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // umask -> old mask. Forward to the host so real inode creation honours the guest's mask, and mirror it
    // into g_umask so /proc/self/status `Umask:` reflects the current value (returns the tracked previous mask,
    // which the host call keeps in lockstep).
    default: break;
    }
}

static void svc_fs_metadata_166(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 166: {
        int old = g_umask;
        g_umask = (int)a0 & 0777;
        (void)umask((mode_t)a0);
        G_RET(c) = (uint64_t)(unsigned)old;
        break;
    }
    // fadvise64(fd, offset, len, advice) -- advisory no-op, but the ADVICE is a fixed-ABI enum:
    // POSIX_FADV_NORMAL..NOREUSE are 0..5, and Linux (mm/fadvise.c) rejects any other value with
    // EINVAL before doing anything advisory. The engine treated every advice as a silent success.
    default: break;
    }
}

static void svc_fs_metadata_223(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 223:
        if (a3 > 5) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        G_RET(c) = 0;
        break;
    default: break;
    }
}

static int svc_fs_metadata(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                           uint64_t a5) {
    switch (nr) {
    case 79: svc_fs_metadata_79(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 80: svc_fs_metadata_80(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 81: svc_fs_metadata_81(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 267: svc_fs_metadata_267(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 88: svc_fs_metadata_88(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 166: svc_fs_metadata_166(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 223: svc_fs_metadata_223(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
