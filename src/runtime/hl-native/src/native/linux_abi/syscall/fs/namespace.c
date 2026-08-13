static void svc_fs_namespace_33(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 33: {
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            if (g_nlower) {
                char gpm[4200];
                abs_guest((int)a0, (const char *)a1, gpm, sizeof gpm);
                // Merged-view errno the upper-only host mknodat can't produce (lower name -> EEXIST; a
                // lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT). Before whiteout clear.
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                overlay_clear_whiteout(gpm); // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker
            }
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = mknodat(pfd, fin, (mode_t)a2, (dev_t)a3), e = errno;
            char dp[4200];
            if (r >= 0 && hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0) {
                    hl_fdcache_metadata_evict(hp);
                    hl_fdcache_access_evict(hp);
                    if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1);
                }
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = mknodat(ATFD(a0), p, (mode_t)a2, (dev_t)a3);
        if (r >= 0) {
            hl_fdcache_metadata_evict(p);
            hl_fdcache_access_evict(p);
            if (newfile_stamp_wanted()) newfile_stamp_path(p, 1);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // mkdirat(dirfd, path, mode) -- confined
    default: break;
    }
}

static void svc_fs_namespace_34(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    // mknodat(dirfd, path, mode, dev)
    case 34: {
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            // OVERLAY: recreating a name a lower still provides -> drop any stale `.wh.NAME` whiteout first
            // (else the new dir can be hidden by an order-dependent readdir dedup), and if a lower dir of the
            // same name exists, mark the new upper dir OPAQUE so the lower's stale children never re-surface.
            char gpm[4200];
            int had_lower_dir = 0;
            if (g_nlower) {
                abs_guest((int)a0, (const char *)a1, gpm, sizeof gpm);
                // Merged-view errno the upper-only host mkdirat can't produce (a lower still provides the
                // name -> EEXIST; a lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT).
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                int authorization = dac_create_at((int)a0, (const char *)a1);
                if (authorization != 0) {
                    G_RET(c) = (uint64_t)(int64_t)authorization;
                    break;
                }
                overlay_clear_whiteout(gpm);
                had_lower_dir = overlay_lower_has_dir(gpm);
            } else {
                int authorization = dac_create_at((int)a0, (const char *)a1);
                if (authorization != 0) {
                    G_RET(c) = (uint64_t)(int64_t)authorization;
                    break;
                }
            }
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = mkdirat(pfd, fin, (mode_t)a2), e = errno;
            char dp[4200];
            if (r >= 0 && hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0) {
                    hl_fdcache_metadata_evict(hp);
                    hl_fdcache_access_evict(hp);
                    if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1);
                }
            }
            close(pfd);
            if (r >= 0 && had_lower_dir) overlay_set_opaque(gpm);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        {
            int authorization = dac_create_at((int)a0, (const char *)a1);
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = mkdirat(ATFD(a0), p, (mode_t)a2);
        hl_fdcache_metadata_evict(p);
        // namespace change -> evict
        hl_fdcache_access_evict(p);
        if (r >= 0 && newfile_stamp_wanted()) newfile_stamp_path(p, 1);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // unlinkat(dirfd, path, flags) -- confined
    default: break;
    }
}

static void svc_fs_namespace_35(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 35: {
        // Linux rejects unknown flag bits (only AT_REMOVEDIR=0x200 is valid) with EINVAL BEFORE any
        // path resolution or removal -- otherwise a corrupted/probed flag value would silently fall
        // through and delete the target. This check precedes the EFAULT path check to match the kernel.
        if (a2 & ~0x200u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // The pathname was already imported and validated at the svc_fs boundary.
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        // shm/sem files are flat host files under /tmp (see shm_hostpath); sem_unlink/shm_unlink and glibc's
        // temp-file cleanup must hit that backing, not the jail's <rootfs>/dev/shm. AT_REMOVEDIR never applies.
        char shb[4224];
        const char *shp = shm_hostpath((const char *)a1, shb, sizeof shb);
        if (shp) {
            G_RET(c) = unlink(shp) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        {
            int authorization = dac_sticky_at((int)a0, (const char *)a1);
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
        }
        // RAM-backed scratch adoption: SQLite et al. open a temp file O_CREAT|O_EXCL then unlink it while
        // still open (delete-on-close). After this unlink drops its last link the file is anonymous, so we
        // may adopt it into RAM. Cheap pre-filter (avoid the fd scan on ordinary unlinks): a temp-dir path
        // or the sqlite "etilqs_" prefix, and not a directory removal. dev/ino is captured (per branch,
        // through the same resolution the unlink uses) right before the unlink and matched after.
        int try_adopt = 0;
        if (!memf_disabled() && !(a2 & 0x200)) {
            char gp[4200];
            abs_guest((int)a0, (const char *)a1, gp, sizeof gp);
            const char *base = strrchr(gp, '/');
            base = base ? base + 1 : gp;
            try_adopt = !strncmp(gp, "/tmp/", 5) || !strncmp(gp, "/var/tmp/", 9) || strstr(base, "etilqs_") != 0;
        }
        // OVERLAY: delete. A name a read-only lower still provides must be MASKED with a .wh.NAME whiteout
        // (overlay_whiteout also drops any upper copy) so it stays hidden. An UPPER-ONLY file has no lower to
        // mask, so it is simply removed with NO whiteout -- a spurious .wh.NAME would otherwise linger in the
        // parent and hide a later re-create of that same name (apt's http method deletes partial/X after a
        // failed fetch, then re-creates and renames it -> the stale whiteout ENOENTed the rename source).
        if (g_rootfs && g_nlower) {
            char gp[4200];
            abs_guest((int)a0, (const char *)a1, gp, sizeof gp);
            char host[4300];
            if (!overlay_resolve(gp, host, sizeof host, 1)) {
                G_RET(c) = (uint64_t)(-2);
                break;
                // ENOENT
            }
            // Enforce rmdir/unlink type semantics against the MERGED target BEFORE touching it. The
            // non-overlay branches pass AT_REMOVEDIR straight to unlinkat() so the kernel does this, but
            // the overlay path used remove()/overlay_whiteout() which pick unlink-vs-rmdir by the target's
            // OWN type -- so rmdir() wrongly succeeded on a regular file (and unlink() on a directory). dpkg
            // probes a control file's type with `rmdir(f) == 0`: the wrongly-successful rmdir deleted the
            // file and made dpkg abort "package control info contained directory". Match Linux:
            // rmdir a non-directory -> ENOTDIR; unlink a directory -> EISDIR.
            struct stat lst;
            int isdir = lstat(host, &lst) == 0 && S_ISDIR(lst.st_mode);
            if ((a2 & 0x200) && !isdir) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOTDIR);
                break;
            }
            if (!(a2 & 0x200) && isdir) {
                G_RET(c) = (uint64_t)(int64_t)(-EISDIR);
                break;
            }
            // rmdir must fail ENOTEMPTY on a non-empty MERGED dir. The upper-only branch below lets the
            // kernel enforce this, but a lower-backed dir is whiteout-masked unconditionally -- so it would
            // wrongly "succeed" and hide live lower children. Check the merged listing first (overlay_readdir
            // always includes "." and ".." -> a count > 2 means the directory still has real children).
            if ((a2 & 0x200) && isdir) {
                char (*nm)[256] = NULL;
                uint8_t *ty = NULL;
                int nent = overlay_readdir(gp, &nm, &ty);
                free(nm);
                free(ty);
                if (nent > 2) {
                    G_RET(c) = (uint64_t)(int64_t)(-ENOTEMPTY);
                    break;
                }
            }
            if (overlay_lower_has(gp)) {
                overlay_whiteout(gp);
                G_RET(c) = 0;
            } else {
                // upper-only -> remove with the CORRECT op (rmdir for a dir, unlink for a file) so the
                // kernel still enforces ENOTDIR/EISDIR/ENOTEMPTY exactly as Linux would.
                int r = (a2 & 0x200) ? rmdir(host) : unlink(host);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            }
            // Invalidate the stat/access/readlink caches for the removed path: `host` is the merged-resolve
            // host path, the SAME key case 79/48 memoize under, so a follow-up `test -e`/stat sees it gone
            // (mirrors the non-overlay branch below). Without this a removed upper entry kept reporting as
            // present via a stale mc_ hit even though it no longer appears in a readdir.
            hl_fdcache_metadata_evict(host);
            hl_fdcache_access_evict(host);
            hl_fdcache_readlink_evict(host);
            // hardlink coherence: removing one link drops the sibling links' nlink -- evict their cached
            // stats by inode (lst was captured before the removal, so nlink>=2 means aliases still exist).
            if (S_ISREG(lst.st_mode) && lst.st_nlink >= 2) hl_fdcache_metadata_evict_inode(lst.st_dev, lst.st_ino);
            break;
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            // Capture the pre-unlink identity: (dev,ino) drives the delete-on-close adopt AND the hardlink
            // nlink-coherence eviction below; st_nlink>=2 means other links alias this inode.
            uint64_t adev = 0, aino = 0, nlink = 0;
            struct stat ps;
            if (fstatat(pfd, fin, &ps, AT_SYMLINK_NOFOLLOW) == 0) {
                nlink = (uint64_t)ps.st_nlink;
                if (try_adopt && S_ISREG(ps.st_mode)) {
                    adev = (uint64_t)ps.st_dev;
                    aino = (uint64_t)ps.st_ino;
                }
            }
            // Linux: a trailing slash names a directory, so unlink("file/") is ENOTDIR -- do_unlinkat rejects
            // the slash before removing anything. jail_at strips the trailing slash from `fin`, so re-check the
            // raw guest spelling against the resolved node type (a non-directory under a trailing slash -> ENOTDIR).
            if (!(a2 & 0x200) && nlink != 0 && !S_ISDIR(ps.st_mode)) {
                const char *rawp = (const char *)a1;
                size_t rl = strlen(rawp);
                if (rl > 1 && rawp[rl - 1] == '/') {
                    close(pfd);
                    G_RET(c) = (uint64_t)(int64_t)(-ENOTDIR);
                    break;
                }
            }
            // AT_REMOVEDIR: linux 0x200
            int r = unlinkat(pfd, fin, (a2 & 0x200) ? AT_REMOVEDIR : 0), e = errno;
            HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "unlinkat path=%s flags=%#llx result=%d", (const char *)a1,
                    (unsigned long long)a2, r < 0 ? -e : 0);
            char dp[4200];
            if (r >= 0 && hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0) {
                    hl_fdcache_metadata_evict(hp);
                    hl_fdcache_access_evict(hp);
                    hl_fdcache_readlink_evict(hp);
                }
            }
            close(pfd);
            if (r >= 0 && aino) memf_try_adopt(adev, aino);
            if (r >= 0 && nlink >= 2) hl_fdcache_metadata_evict_inode((dev_t)ps.st_dev, (ino_t)ps.st_ino);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        // unlink: never follow the final symlink (remove the link itself, not its target).
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 1);
        uint64_t adev = 0, aino = 0, nlink = 0;
        struct stat ps;
        if (fstatat(ATFD(a0), p, &ps, AT_SYMLINK_NOFOLLOW) == 0) {
            nlink = (uint64_t)ps.st_nlink;
            if (try_adopt && S_ISREG(ps.st_mode)) {
                adev = (uint64_t)ps.st_dev;
                aino = (uint64_t)ps.st_ino;
            }
        }
        int r = unlinkat(ATFD(a0), p, (a2 & 0x200) ? AT_REMOVEDIR : 0);
        int e = errno;
        hl_fdcache_metadata_evict(p);
        hl_fdcache_access_evict(p);
        hl_fdcache_readlink_evict(p);
        if (r >= 0 && aino) memf_try_adopt(adev, aino);
        if (r >= 0 && nlink >= 2) hl_fdcache_metadata_evict_inode((dev_t)ps.st_dev, (ino_t)ps.st_ino);
        G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
        break;
    }
    // symlinkat(target, newdirfd, linkpath) -- the link is CREATED at (newdirfd, linkpath)
    default: break;
    }
}

static void svc_fs_namespace_36(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 36: {
        // a relative linkpath under a bad/non-dir newdirfd -> EBADF/ENOTDIR (hl's g_fdpath fold used to
        // leak macOS EOPNOTSUPP for a non-dir dirfd). (LTP symlinkat01.)
        {
            int adc = at_dirfd_check((int)a1, (const char *)a2);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        if (jail_ro_at((int)a1, (const char *)a2)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        const char *target =
            // target is the link CONTENT (unresolved); follow-time confinement guards it
            (const char *)a0;
        if (jail_routed_at((int)a1, (const char *)a2)) {
            if (g_nlower) {
                char gpm[4200];
                abs_guest((int)a1, (const char *)a2, gpm, sizeof gpm);
                // Merged-view errno the upper-only host symlinkat can't produce (lower name -> EEXIST; a
                // lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT). Before whiteout clear.
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                overlay_clear_whiteout(gpm); // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker
            }
            char fin[512];
            int pfd = jail_at((int)a1, (const char *)a2, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = symlinkat(target, pfd, fin), e = errno;
            if (r == 0 && newfile_stamp_wanted()) {
                char parent[4200], created[4300];
                if (hl_native_fd_path(pfd, parent, sizeof parent) == 0 &&
                    path_join(created, sizeof created, parent, fin) == 0)
                    newfile_stamp_path(created, 1);
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a1, (const char *)a2, pb, sizeof pb, 0);
        int linked = symlinkat(target, ATFD(a1), p);
        if (linked == 0 && newfile_stamp_wanted()) newfile_stamp_path(p, 1);
        G_RET(c) = linked < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // linkat(odir,opath,ndir,npath,flags) -- writes both ends (new link + source link count)
    default: break;
    }
}

static int linkat_publish_procfd(uint64_t old_dirfd, uint64_t old_path, uint64_t new_dirfd, uint64_t new_path,
                                 int64_t *result) {
    char source_guest[4200];
    abs_guest((int)old_dirfd, (const char *)old_path, source_guest, sizeof source_guest);
    int source_fd = procfd_num(source_guest);
    if (source_fd < 0) return 0;
    if (jail_ro_at((int)new_dirfd, (const char *)new_path)) {
        *result = -EROFS;
        return 1;
    }

    memf_materialize(source_fd);
    char final_name[512], native_path[4200];
    int parent_fd;
    const char *name;
    int close_parent = jail_routed_at((int)new_dirfd, (const char *)new_path);
    if (close_parent) {
        parent_fd = jail_at((int)new_dirfd, (const char *)new_path, final_name, sizeof final_name, 1);
        if (parent_fd < 0) {
            *result = parent_fd;
            return 1;
        }
        name = final_name;
    } else {
        name = atpath((int)new_dirfd, (const char *)new_path, native_path, sizeof native_path, 0);
        parent_fd = ATFD(new_dirfd);
    }

    int rc, error;
#if defined(AT_EMPTY_PATH)
    rc = linkat(source_fd, "", parent_fd, name, AT_EMPTY_PATH);
    error = errno;
#else
    rc = -1;
    error = ENOTSUP;
#endif
    if (rc < 0) {
        int output = openat(parent_fd, name, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (output < 0) {
            error = errno;
        } else {
            char copy[65536];
            off_t offset = 0;
            ssize_t count;
            rc = 0;
            while ((count = pread(source_fd, copy, sizeof copy, offset)) > 0) {
                ssize_t written = 0;
                while (written < count) {
                    ssize_t amount = pwrite(output, copy + written, (size_t)(count - written), offset + written);
                    if (amount <= 0) {
                        rc = -1;
                        error = errno;
                        break;
                    }
                    written += amount;
                }
                if (rc < 0) break;
                offset += count;
            }
            if (count < 0) {
                rc = -1;
                error = errno;
            }
            close(output);
            if (rc < 0) (void)unlinkat(parent_fd, name, 0);
        }
    }
    if (close_parent) close(parent_fd);
    *result = rc < 0 ? -(int64_t)error : 0;
    return 1;
}

static int linkat_in_jail(uint64_t old_dirfd, uint64_t old_path, uint64_t new_dirfd, uint64_t new_path, int flags,
                          int64_t *result) {
    if (!jail_routed_at((int)old_dirfd, (const char *)old_path) &&
        !jail_routed_at((int)new_dirfd, (const char *)new_path))
        return 0;

    overlay_copyup_at((int)old_dirfd, (const char *)old_path);
    char old_final[512], new_final[512];
    int old_parent = jail_at((int)old_dirfd, (const char *)old_path, old_final, sizeof old_final, 1);
    if (old_parent < 0) {
        *result = old_parent;
        return 1;
    }
    int new_parent = jail_at((int)new_dirfd, (const char *)new_path, new_final, sizeof new_final, 1);
    if (new_parent < 0) {
        close(old_parent);
        *result = new_parent;
        return 1;
    }
    int rc = linkat(old_parent, old_final, new_parent, new_final, flags), error = errno;
    if (rc == 0) {
        struct stat status;
        if (fstatat(new_parent, new_final, &status, AT_SYMLINK_NOFOLLOW) == 0)
            hl_fdcache_metadata_evict_inode(status.st_dev, status.st_ino);
    }
    close(old_parent);
    close(new_parent);
    *result = rc < 0 ? -(int64_t)error : 0;
    return 1;
}

static void svc_fs_namespace_37(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 37: {
        // reject unknown linkat flag bits with EINVAL (valid: AT_SYMLINK_FOLLOW 0x400 | AT_EMPTY_PATH
        // 0x1000). hl otherwise ignored the flags and the link wrongly succeeded. (LTP linkat01 case 22.)
        if (a4 & ~(uint64_t)0x1400) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // a relative old/new path under a bad/non-dir dirfd -> EBADF/ENOTDIR, before any resolution
        // (hl's g_fdpath fold leaked macOS EOPNOTSUPP for a non-dir dirfd). (LTP linkat01 cases 8/9.)
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (!adc) adc = at_dirfd_check((int)a2, (const char *)a3);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        // O_TMPFILE materialization: linkat(AT_SYMLINK_FOLLOW) of /proc/self/fd/N gives a name to the
        // anonymous inode named by descriptor N. Recover the descriptor, flush any RAM write-back cache to
        // its backing host file, then re-link it through the host's own /proc/self/fd magic symlink (guest
        // fd numbers match the engine's native numbers). This must precede the /proc-source EXDEV rejection.
        if (a4 & 0x400 /*AT_SYMLINK_FOLLOW*/) {
            int64_t result;
            if (linkat_publish_procfd(a0, a1, a2, a3, &result)) {
                G_RET(c) = (uint64_t)result;
                break;
            }
        }
        // A hardlink whose SOURCE lives on a hl-synthetic pseudo-filesystem (/proc, /sys, /dev) crosses a
        // device boundary -> EXDEV, exactly as on Linux where those are separate mounts. (LTP linkat01 case 20.)
        {
            char sgp[4200];
            abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
            if (!strncmp(sgp, "/proc/", 6) || !strncmp(sgp, "/sys/", 5) || !strncmp(sgp, "/dev/", 5)) {
                char dgp[4200];
                abs_guest((int)a2, (const char *)a3, dgp, sizeof dgp);
                // only when the destination is NOT on the same pseudo-fs (a shm/sem /dev link is handled below)
                if (strncmp(dgp, "/dev/shm/", 9)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EXDEV);
                    break;
                }
            }
        }
        // glibc's sem_open/shm_open creation links a temp /dev/shm/sem.<rnd> to the final /dev/shm/<name>;
        // both ends are shm-backed host files under /tmp, so link them directly (the jail branch below would
        // resolve them into the empty <rootfs>/dev/shm and ENOENT).
        char lob[4224], lnb[4224];
        const char *loh = shm_hostpath((const char *)a1, lob, sizeof lob);
        const char *lnh = shm_hostpath((const char *)a3, lnb, sizeof lnb);
        if (loh && lnh) {
            G_RET(c) = link(loh, lnh) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1) || jail_ro_at((int)a2, (const char *)a3)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        int fl = (a4 & 0x400) ? AT_SYMLINK_FOLLOW : 0;
        int64_t jail_result;
        if (linkat_in_jail(a0, a1, a2, a3, fl, &jail_result)) {
            G_RET(c) = (uint64_t)jail_result;
            break;
        }
        char ob[4200], nb[4200];
        const char *op = atpath((int)a0, (const char *)a1, ob, sizeof ob, 0);
        const char *np = atpath((int)a2, (const char *)a3, nb, sizeof nb, 0);
        int r = linkat(ATFD(a0), op, ATFD(a2), np, fl);
        if (r == 0) {
            struct stat ls;
            if (fstatat(ATFD(a2), np, &ls, AT_SYMLINK_NOFOLLOW) == 0)
                hl_fdcache_metadata_evict_inode(ls.st_dev, ls.st_ino);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_namespace_38(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 38:
    // renameat(38) / renameat2(276): translate Linux flags to the native host operation.
    case 276: {
        // renameat2 flag validation (LTP renameat201). Valid flags are RENAME_NOREPLACE(1) |
        // RENAME_EXCHANGE(2) | RENAME_WHITEOUT(4); any unknown bit -> EINVAL, and RENAME_EXCHANGE is exclusive
        // of NOREPLACE and WHITEOUT. Checked before touching the fs (Linux orders this ahead of the path walk).
        if (nr == 276) {
            int lf = (int)a4;
            if ((lf & ~0x7) || ((lf & 2) && (lf & (1 | 4)))) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
        }
        // A relative old/new path under a bad/non-dir dirfd -> EBADF/ENOTDIR.
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (!adc) adc = at_dirfd_check((int)a2, (const char *)a3);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        if (jail_ro_at((int)a0, (const char *)a1) || jail_ro_at((int)a2, (const char *)a3)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        {
            int authorization = dac_sticky_at((int)a0, (const char *)a1);
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
            hl_dac_snapshot destination;
            if (dac_snapshot_at((int)a2, (const char *)a3, 1, &destination) == 0) {
                authorization = dac_sticky_at((int)a2, (const char *)a3);
                if (authorization != 0) {
                    G_RET(c) = (uint64_t)(int64_t)authorization;
                    break;
                }
            }
        }
        // inotify: a rename generates IN_MOVED_FROM(src)/IN_MOVED_TO(dst) with a shared cookie on any watch
        // covering the source / destination directory. Queue them now (before the move) so a watch's read()
        // can pair them -- the snapshot diff cannot. No-op when nothing watches either directory.
        inotify_notify_move((int)a0, (const char *)a1, (int)a2, (const char *)a3);
        bound_inotify_notify_move((int)a0, (const char *)a1, (int)a2, (const char *)a3);
        unsigned int rxflags = 0;
        if (nr == 276) {
            int lf = (int)a4;
            if (lf & 1) rxflags |= HL_NATIVE_RENAME_NOREPLACE;
            if (lf & 2) rxflags |= HL_NATIVE_RENAME_EXCHANGE;
        }
        // shm/sem create that renames (rather than links) a temp /dev/shm file to the final name: both ends
        // are shm-backed host files under /tmp, so rename them directly (the jail branch would ENOENT them).
        char rob[4224], rnb[4224];
        const char *roh = shm_hostpath((const char *)a1, rob, sizeof rob);
        const char *rnh = shm_hostpath((const char *)a3, rnb, sizeof rnb);
        if (roh && rnh) {
            G_RET(c) = renameatx_np(AT_FDCWD, roh, AT_FDCWD, rnh, rxflags) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (jail_routed_at((int)a0, (const char *)a1) || jail_routed_at((int)a2, (const char *)a3)) {
            // both ends confined (TOCTOU-free). Copy a lower-only SOURCE up first so renameatx_np finds it in
            // the writable upper (jail_at already materializes the dest's upper parent via overlay_mkparents).
            // RECURSIVE for a lower-only directory: the whole subtree must be in the upper before the move,
            // else the rename moves an EMPTY dir and loses the contents. For an EXCHANGE, the DEST must also
            // be copied up (both ends land in the upper before the atomic swap).
            overlay_copyup_at_tree((int)a0, (const char *)a1);
            if (rxflags & HL_NATIVE_RENAME_EXCHANGE) overlay_copyup_at_tree((int)a2, (const char *)a3);
            char ofin[512], nfin[512];
            int opfd = jail_at((int)a0, (const char *)a1, ofin, sizeof ofin, 1);
            if (opfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)opfd;
                break;
            }
            int npfd = jail_at((int)a2, (const char *)a3, nfin, sizeof nfin, 1);
            if (npfd < 0) {
                close(opfd);
                G_RET(c) = (uint64_t)(int64_t)npfd;
                break;
            }
            char dp[4200];
            if (hl_native_fd_path(opfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, ofin) == 0) {
                    hl_fdcache_metadata_evict(hp);
                    hl_fdcache_access_evict(hp);
                }
            }
            int r = renameatx_np(opfd, ofin, npfd, nfin, rxflags), e = errno;
            close(opfd);
            close(npfd);
            // Overlay: a plain move (not RENAME_EXCHANGE) of a file the image lower still provides leaves the
            // copied-up upper source moved away but the lower copy exposed -> the source would re-appear. Drop
            // a whiteout at the source so it stays gone (real overlayfs rename semantics). No-op outside overlay.
            if (r == 0 && !(rxflags & HL_NATIVE_RENAME_EXCHANGE)) {
                char sgp[4200];
                abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
                if (overlay_lower_has(sgp)) overlay_whiteout(sgp);
                // RENAME_WHITEOUT: Linux additionally leaves a whiteout char device (0,0) at the source.
                // Record it so lstat(src) reports that char device (synth_stat_raw); overlay_whiteout above
                // already dropped the union `.wh.` marker when a lower entry needed masking.
                if (nr == 276 && ((int)a4 & 4)) whiteout_note(sgp);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char ob[4200], nb[4200];
        const char *op = atpath((int)a0, (const char *)a1, ob, sizeof ob, 0);
        const char *np = atpath((int)a2, (const char *)a3, nb, sizeof nb, 0);
        int rr = renameatx_np(ATFD(a0), op, ATFD(a2), np, rxflags);
        // RENAME_WHITEOUT (non-overlay): record the source as a whiteout char device (0,0) so lstat(src)
        // reports it, matching Linux -- macOS cannot mknod a real device node rootless.
        if (rr == 0 && nr == 276 && ((int)a4 & 4)) {
            char sgp[4200];
            abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
            whiteout_note(sgp);
        }
        G_RET(c) = rr < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: break;
    }
}

static int svc_fs_namespace(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                            uint64_t a5) {
    switch (nr) {
    case 33: svc_fs_namespace_33(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 34: svc_fs_namespace_34(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 35: svc_fs_namespace_35(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 36: svc_fs_namespace_36(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 37: svc_fs_namespace_37(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 38:
    case 276: svc_fs_namespace_38(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
