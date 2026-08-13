static void svc_fs_access_49(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 49: {
        char pb[4200];
        char guest_cwd[sizeof g_cwd];
        // Synthetic proc directories are materialized by their directory
        // provider, not by the image overlay. Navigate through that provider's
        // descriptor so stat/open/chdir observe one synthetic namespace.
        if (g_rootfs) {
            char raw[4200], guest[4200];
            if (path_copy(raw, sizeof raw, (const char *)a0) != 0) {
                G_RET(c) = (uint64_t)(int64_t)-ENAMETOOLONG;
                break;
            }
            abs_guest(-100, raw, guest, sizeof guest);
            if (synth_proc_fd_dir_is(guest)) {
                int directory = synth_misc_dir_open(guest);
                if (directory < 0 || fchdir(directory) != 0) {
                    int error = directory < 0 ? ENOENT : errno;
                    if (directory >= 0) close(directory);
                    G_RET(c) = (uint64_t)(int64_t)(-error);
                    break;
                }
                close(directory);
                if (path_copy(g_cwd, sizeof g_cwd, guest) != 0) {
                    G_RET(c) = (uint64_t)(int64_t)-ENAMETOOLONG;
                    break;
                }
                G_RET(c) = 0;
                break;
            }
        }
        // chdir (confined; tracks guest cwd)
        const char *p = atpath(-100, (const char *)a0, pb, sizeof pb, 0);
        if (g_rootfs) {
            char canonical[4200];
            const char *host_cwd = realpath(p, canonical) ? canonical : p;
            int mapped = guest_from_host(host_cwd, guest_cwd, sizeof guest_cwd);
            if (mapped <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                break;
            }
        }
        if (chdir(p) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // Track the guest cwd from the kernel's canonical path, not the lexical path handed to chdir().
        // A successful chdir("/data/.") leaves the process in "/data", and Linux getcwd()/realpath() must
        // not expose the trailing dot component.  Preserving `p` here made MySQL canonicalize its datadir
        // as "/var/lib/mysql/./"; InnoDB then classified the final "." component as hidden and skipped every
        // existing tablespace during recovery.  getcwd() also resolves the actual upper/lower/volume backing,
        // which guest_from_host maps back into the guest namespace.
        if (g_rootfs) (void)path_copy(g_cwd, sizeof g_cwd, guest_cwd);
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_access_50(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 50: {
        int changed;
        int handled = bound_handle_chdir((int)a0, &changed);
        char guest_cwd[sizeof g_cwd];
        if (!handled) {
            if (g_rootfs) {
                char host_cwd[4200];
                const char *path = NULL;
                if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_fdpath[(int)a0][0])
                    path = g_fdpath[(int)a0];
                else if (hl_native_fd_path((int)a0, host_cwd, sizeof host_cwd) == 0)
                    path = host_cwd;
                int mapped = path ? guest_from_host(path, guest_cwd, sizeof guest_cwd) : 0;
                if (mapped <= 0) {
                    G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                    break;
                }
            }
            changed = fchdir((int)a0) == 0 ? 0 : -errno;
        }
        if (changed < 0) {
            G_RET(c) = (uint64_t)(int64_t)changed;
            break;
            // fchdir (tracks guest cwd)
        }
        if (g_rootfs && !handled) (void)path_copy(g_cwd, sizeof g_cwd, guest_cwd);
        G_RET(c) = 0;
        break;
    }
    // fchmod(fd, mode) -- like fchmodat, the new mode must invalidate this file's cached stat, or a
    // subsequent stat() of the same path serves the stale pre-chmod mode from the mc cache (the fd's
    // canonical host path in g_fdpath is the SAME key case 79 memoizes under).
    default: break;
    }
}

static void svc_fs_access_52(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 52: {
        int authorization = dac_chmod_fd((int)a0);
        if (authorization != 0) {
            G_RET(c) = (uint64_t)(int64_t)authorization;
            break;
        }
        struct stat status;
        mode_t host_mode = (mode_t)a1 & 0777;
        if (cred_euid() == 0 && fstat((int)a0, &status) == 0) host_mode |= S_ISDIR(status.st_mode) ? 0700 : 0600;
        /* Preserve enough host authority to publish the virtual mode before a
         * non-root guest removes its own write bit.  Linux permits the owner
         * to chmod such a file again, but setxattr after chmod(0444) is denied
         * and used to leave the previous virtual mode visible through stat. */
        int r = mode_transaction_fd((int)a0, (mode_t)a1, host_mode);
        if (r == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_fdpath[(int)a0][0])
            hl_fdcache_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_access_53(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 53:
    // fchmodat(dirfd,path,mode,flags) / fchmodat2
    case 452: {
        // A pathname pointer outside the accessible address space -> EFAULT (kernel getname
        // copy_from_user), before the dirfd/target is examined (LTP fchmodat02 "invalid address").
        // guest_bad_ptr catches the PROT_NONE tst_get_bad_addr page; the reads below (jail/atpath) would
        // otherwise consume garbage from hl's force-mapped shadow of that page and mis-report the error.
        // fchmodat2 (452) additionally rejects unknown flag bits with EINVAL (AT_SYMLINK_NOFOLLOW|
        // AT_EMPTY_PATH only); glibc screens fchmodat(53)'s flags in userspace so 53's a3 is never trusted.
        if (nr == 452 && (a3 & ~((uint64_t)0x100 | 0x1000))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (!a1 || guest_bad_ptr((uintptr_t)a1, 1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // fchmodat2(fd, "", mode, AT_EMPTY_PATH) changes the inode named by fd.  Coreutils uses this
        // descriptor form while preserving metadata on an atomic replacement, including dpkg's
        // `cp -p` of lower-image configuration files.  Keep it on the same virtual-mode transaction
        // as fchmod so overlay copy-up, guest permission bits, and the host inode stay coherent.
        if (nr == 452 && ((const char *)a1)[0] == '\0' && (a3 & 0x1000)) {
            int authorization = dac_chmod_fd((int)a0);
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
            struct stat status;
            mode_t host_mode = (mode_t)a2 & 0777;
            if (cred_euid() == 0 && fstat((int)a0, &status) == 0) host_mode |= S_ISDIR(status.st_mode) ? 0700 : 0600;
            int r = mode_transaction_fd((int)a0, (mode_t)a2, host_mode);
            if (r == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_fdpath[(int)a0][0])
                hl_fdcache_evict_path(g_fdpath[(int)a0]);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)errno) : 0;
            break;
        }
        // The kernel screens the pathname (getname) BEFORE it examines the dir-fd, so an empty path (no
        // AT_EMPTY_PATH) is ENOENT and an over-long path is ENAMETOOLONG -- even when the dir-fd is a file
        // (which the host fchmodat would otherwise report as ENOTDIR first). LTP fchmodat02 "path is
        // empty" / "pathname too long" pass file_fd (a regular file) as the dir-fd.
        {
            const char *fp = (const char *)a1;
            if (fp[0] == '\0') {
                G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                break;
            }
            if (strnlen(fp, 4096) >= 4096) {
                G_RET(c) = (uint64_t)(int64_t)(-ENAMETOOLONG);
                break;
            }
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        {
            int authorization = dac_chmod_at((int)a0, (const char *)a1, nr == 452 && (a3 & 0x100));
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
            if (nr == 452 && (a3 & 0x100)) {
                int symlink = dac_symlink_at((int)a0, (const char *)a1);
                if (symlink < 0) {
                    G_RET(c) = (uint64_t)(int64_t)symlink;
                    break;
                }
                if (symlink) {
                    G_RET(c) = (uint64_t)(int64_t)-EOPNOTSUPP;
                    break;
                }
            }
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            struct stat status;
            mode_t host_mode = (mode_t)a2 & 0777;
            if (fstatat(pfd, fin, &status, 0) == 0) host_mode |= S_ISDIR(status.st_mode) ? 0700 : 0600;
            int r = -1, e = EINVAL;
            char dp[4200];
            if (hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0) {
                    r = mode_transaction_path(pfd, fin, hp, (mode_t)a2, host_mode);
                    if (r >= 0) hl_fdcache_metadata_evict(hp);
                }
            }
            if (r >= 0 || errno != 0) e = errno;
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        struct stat status;
        mode_t host_mode = (mode_t)a2 & 0777;
        if (fstatat(ATFD(a0), p, &status, 0) == 0) host_mode |= S_ISDIR(status.st_mode) ? 0700 : 0600;
        /* Native chmodat resolves a relative path against directory, but the
         * virtual-mode xattr API is path based.  Give it the same resolved
         * target instead of accidentally consulting the process cwd. */
        char xp[4400];
        const char *xattr_path = p;
        if (p[0] != '/' && ATFD(a0) != AT_FDCWD) {
            char directory[4200];
            if (hl_native_fd_path(ATFD(a0), directory, sizeof directory) == 0 &&
                path_join(xp, sizeof xp, directory, p) == 0)
                xattr_path = xp;
        }
        int r = mode_transaction_path(ATFD(a0), p, xattr_path, (mode_t)a2, host_mode);
        if (r >= 0) hl_fdcache_metadata_evict(xattr_path);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // fchownat(dirfd,path,uid,gid,flags) -- virtualized guest ownership
    default: break;
    }
}

static void svc_fs_access_54(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 54: {
        // Linux validates the flag word before touching the path: only AT_SYMLINK_NOFOLLOW (0x100) and
        // AT_EMPTY_PATH (0x1000) are defined; any other bit is EINVAL. hl emulates a root container, so an
        // ownership is guest metadata and never changes the host inode, but a syntactically invalid call must
        // still fail exactly as Linux does rather than silently mutate the virtual owner record.
        if (a4 & ~0x1100u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        {
            int nofollow = (a4 & 0x100) ? 1 : 0;
            int64_t uid = dac_requested_id(a2);
            int64_t gid = dac_requested_id(a3);
            int authorization = dac_chown_at((int)a0, (const char *)a1, nofollow, uid, gid);
            if (authorization != 0) {
                G_RET(c) = (uint64_t)(int64_t)authorization;
                break;
            }
        }
        if (jail_routed_at((int)a0, (const char *)a1)) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, (a4 & 0x100) ? 1 : 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int nofollow = (a4 & 0x100) ? 1 : 0;
            struct stat target;
            if (fstatat(pfd, fin, &target, nofollow ? AT_SYMLINK_NOFOLLOW : 0) < 0) {
                int error = errno;
                close(pfd);
                G_RET(c) = (uint64_t)(int64_t)(-error);
                break;
            }
            // Ownership belongs to the guest metadata model. Never mutate the host inode: a privileged
            // launcher or build sandbox could make a real chown succeed and leak guest state to the host.
            char dp[4200];
            if (hl_native_fd_path(pfd, dp, sizeof dp) == 0) {
                char hp[4400];
                if (path_join(hp, sizeof hp, dp, fin) == 0)
                    hl_owner_set_path(hp, dac_requested_id(a2), dac_requested_id(a3), nofollow);
            }
            close(pfd);
            G_RET(c) = 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        /* atpath() resolves a confined relative guest path to an absolute host path.  Linux ignores
           dirfd for an absolute pathname; Darwin's fchownat validates it first, so use AT_FDCWD once
           resolution is absolute to preserve the Linux contract on every host. */
        int host_dirfd = p != NULL && p[0] == '/' ? AT_FDCWD : ATFD(a0);
        struct stat target;
        int nofollow = (a4 & 0x100) ? 1 : 0;
        if (fstatat(host_dirfd, p, &target, nofollow ? AT_SYMLINK_NOFOLLOW : 0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        hl_owner_set_path(p, dac_requested_id(a2), dac_requested_id(a3), nofollow);
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_access_55(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 55: {
        int64_t uid = dac_requested_id(a1);
        int64_t gid = dac_requested_id(a2);
        int authorization = dac_chown_fd((int)a0, uid, gid);
        if (authorization != 0) {
            G_RET(c) = (uint64_t)(int64_t)authorization;
            break;
        }
        // A genuinely invalid descriptor must fail like Linux instead of poisoning virtual metadata.
        struct stat target;
        if (fstat((int)a0, &target) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        hl_owner_set_fd((int)a0, dac_requested_id(a1), dac_requested_id(a2));
        // the guest-owner xattr just changed -> drop this path's cached stat so a later stat reports it
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0]) hl_fdcache_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = 0;
        break;
    }
    // openat2(dirfd, path, open_how*, size): unpack open_how { u64 flags; u64 mode; u64 resolve; } into
    // the openat arg positions, then share the full openat path (O_* xlate, overlay, jail). Linux validates
    // the ABI up front, so we do too: NULL how -> EFAULT, size < v0 (24) -> EINVAL, size > PAGE_SIZE or
    // non-zero extension bytes -> E2BIG, unknown resolve bits / mode>07777 / mode set without a create flag
    // -> EINVAL. RESOLVE_NO_SYMLINKS is enforced as O_NOFOLLOW (ELOOP on a symlink final component); the
    // rootfs jail already confines every resolution so the containment RESOLVE_* bits stay advisory.
    default: break;
    }
}

static void svc_fs_access_56(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5);

static void svc_fs_access_437(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                              uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 437: {
        uint64_t how_ptr = a2, usize = a3;
        if (!how_ptr) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (usize < 24) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (usize > 4096) {
            G_RET(c) = (uint64_t)(int64_t)(-E2BIG);
            break;
        }
        uint8_t how_bytes[4096];
        if (guest_copy_from(how_bytes, how_ptr, (size_t)usize) != (ssize_t)usize) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const uint8_t *hb = how_bytes;
        int extbad = 0;
        for (uint64_t i = 24; i < usize; i++)
            if (hb[i]) {
                extbad = 1;
                break;
            }
        if (extbad) {
            G_RET(c) = (uint64_t)(int64_t)(-E2BIG);
            break;
        }
        const uint64_t *how = (const uint64_t *)how_bytes;
        uint64_t oflags = how[0], omode = how[1], resolve = how[2];
        // openat2 (unlike openat, which silently ignores unknown bits) rejects any open-flag bit
        // outside VALID_OPEN_FLAGS with EINVAL (fs/open.c build_open_flags). The mask is identical on
        // both guest arches (0x7fffc3): the arch-varying O_DIRECTORY/O_NOFOLLOW/O_DIRECT/O_LARGEFILE
        // quartet is the same {0x4000,0x8000,0x10000,0x20000} set on x86-64 and aarch64.
        if (oflags & ~0x7fffc3ULL) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // RESOLVE_* valid mask: NO_XDEV|NO_MAGICLINKS|NO_SYMLINKS|BENEATH|IN_ROOT|CACHED = 0x3f
        if ((resolve & ~0x3fULL) || (omode & ~07777ULL) ||
            (omode && !(oflags & (0x40ULL /*O_CREAT*/ | 0x400000ULL /*__O_TMPFILE*/)))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        /* RESOLVE_BENEATH refuses to cross out of the starting directory: an
         * absolute path argument names an escape and is rejected up front. Both
         * BENEATH and IN_ROOT must also refuse symlink escapes; the resolver has
         * no per-link escape check, so the whole walk is confined to no-symlink
         * resolution (fail-closed) when either containment flag is set. */
        if ((resolve & 0x08ULL /*RESOLVE_BENEATH*/) && !guest_bad_ptr(a1, 1) && *(const char *)a1 == '/') {
            G_RET(c) = (uint64_t)(int64_t)(-EXDEV);
            break;
        }
        /* NO_SYMLINKS forbids every symlink; BENEATH/IN_ROOT forbid an escaping
         * one and are enforced fail-closed as no-symlink resolution.  The shared
         * openat handler enforces this through the jail resolver (rootfs setups)
         * and through O_NOFOLLOW plus a no-follow walk on the native bind-volume
         * path. */
        g_openat2_resolve_intent =
            (resolve & (0x04ULL /*NO_SYMLINKS*/ | 0x08ULL /*BENEATH*/ | 0x10ULL /*IN_ROOT*/)) ? HL_OPEN_NO_SYMLINKS : 0;
        if (resolve & (0x04ULL | 0x08ULL | 0x10ULL)) oflags |= (uint64_t)G_O_NOFOLLOW;
        svc_fs_access_56(c, 56, a0, a1, oflags, omode, a4, a5);
        break;
    }
    default: break;
    }
}

static int open_synthetic_path(struct cpu *c, uint64_t a0, uint64_t a1, int lf, int mf, int is_opath) {
    // synthesize /proc/* (macOS has no /proc)
    const char *rp = (const char *)a1;
    // Resolve a RELATIVE target to its guest-absolute path so the /proc checks below fire even when
    // the guest opened e.g. "stat" or "<pid>/stat" relative to a /proc cwd (busybox top xchdir's to
    // /proc, then opens "<pid>/stat"). Absolute paths are untouched -> zero change for those callers;
    // a resolved non-/proc relative path matches none of the synth checks and the real open (which
    // uses the original a1) is unaffected.
    char gpb_syn[4200];
    if (rp && rp[0] != '/') {
        abs_guest((int)a0, rp, gpb_syn, sizeof gpb_syn);
        rp = gpb_syn;
    }
    // abs_guest emits "/<gdir>/<name>", so a gdir tracked as "/proc" (a materialized proc dir fd)
    // yields a leading "//proc/..." -- collapse it so the /proc checks below match. This is what
    // makes htop's relative openat(pid_dirfd, "stat"/"task"/...) re-enter the /proc synthesis.
    while (rp && rp[0] == '/' && rp[1] == '/')
        rp++;
    if (rp && (!strcmp(rp, "/proc/self/fd") || !strcmp(rp, "/proc/self/fd/") ||
               !strcmp(rp, "/proc/thread-self/fd") || !strcmp(rp, "/proc/thread-self/fd/"))) {
        int d = proc_fd_dir_open();
        if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC);
        if (d >= 0 && d < HL_NFD) g_opath[d] = is_opath;
        G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
        return 1;
    }
    // A bare "/proc/self" (or thread-self) opened as a DIRECTORY (`cd /proc/self`, then relative
    // reads) follows the magic symlink to the numeric pid dir -- rewrite it so the /proc/<pid>
    // materialization below (proc_dir_try_open) serves it and tags the fd's guest path.
    char selfdb[40];
    if (rp && (!strncmp(rp, "/proc/self", 10) || !strncmp(rp, "/proc/thread-self", 17))) {
        const char *tail = rp + (rp[6] == 's' ? 10 : 17);
        if (tail[0] == 0 || !strcmp(tail, "/")) {
            snprintf(selfdb, sizeof selfdb, "/proc/%d", container_pid());
            rp = selfdb;
        }
    }
    // runc MaskedPaths / ReadonlyPaths (container isolation). A ReadonlyPath opened for WRITE fails
    // EROFS BEFORE the /proc synth can hand back a (falsely writable) temp fd -- so `sysctl -w` and a
    // write to /proc/sysrq-trigger diverge from Linux exactly like runc's read-only bind. Masked paths
    // are then served as empty file/dir for BOTH read and write intent (an empty, inert stand-in).
    if (rp && g_rootfs) {
        int write_intent = (lf & 3) || (lf & 0x40) || (lf & 0x200) || (lf & 0x400); // RW/CREAT/TRUNC/APPEND
        if (proc_ro_path(rp) && !proc_masked_kind(rp) && write_intent) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            return 1;
        }
        int md = proc_masked_open(rp);
        if (md != -2) {
            if (md >= 0 && (lf & 0x80000)) fcntl(md, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            G_RET(c) = md < 0 ? (uint64_t)(-errno) : (uint64_t)md;
            return 1;
        }
    }
    // Synthetic non-pid directories whose direct leaves already exist but whose DIRECTORY was not
    // enumerable: /proc/net, /proc/[self|pid]/ns, /sys/fs/cgroup, /sys/class/block, /sys/block,
    // cpuN/topology, and /dev/fd (== /proc/self/fd). A directory walk of these now sees their entries.
    if (rp) {
        int md = synth_misc_dir_open(rp);
        if (md != -2) {
            if (md >= 0 && (lf & 0x80000)) fcntl(md, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            if (md >= 0 && md < HL_NFD) g_opath[md] = is_opath;            // O_PATH fd -> I/O EBADF
            G_RET(c) = md < 0 ? (uint64_t)(-errno) : (uint64_t)md;
            return 1;
        }
    }
    // opendir("/proc"): materialize the process table (numeric pid dir per live container process
    // + the synthesized static files) so getdents enumerates the whole container -- `ps`/top/htop
    // read this to find processes. Without it the empty rootfs /proc dir yielded an empty table.
    if (rp && g_rootfs && (!strcmp(rp, "/proc") || !strcmp(rp, "/proc/"))) {
        int d = proc_root_dir_open();
        if (d >= 0) {
            G_RET(c) = (uint64_t)d;
            return 1;
        }
        // else fall through to the real (empty) rootfs /proc
    }
    if (rp && !strncmp(rp, "/proc/", 6)) {
        // /proc/<pid>, /proc/<pid>/task, /proc/<pid>/task/<tid> as DIRECTORIES: materialize a temp
        // dir so opendir/getdents work and htop can descend (it opens each pid as an O_DIRECTORY fd
        // and reads task/<tid>/stat). Per-pid FILES return -2 -> served by proc_open below.
        int pd = proc_dir_try_open(rp);
        if (pd != -2) {
            if (pd >= 0 && (lf & 0x80000)) fcntl(pd, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            G_RET(c) = pd < 0 ? (uint64_t)(-errno) : (uint64_t)pd;
            return 1;
        }
        // /proc/[self|pid]/exe -> open the actual guest executable (the magic symlink target)
        char ep[1024];
        if (proc_self_exe(rp, ep, sizeof ep)) {
            char hb[4200];
            const char *hp = xresolve_overlay(ep, hb, sizeof hb);
            int ef = open(hp, O_RDONLY);
            if (ef >= 0 && (lf & 0x80000)) fcntl(ef, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            G_RET(c) = ef < 0 ? (uint64_t)(-errno) : (uint64_t)ef;
            return 1;
        }
        // /proc/[self|pid]/map_files/<start>-<end> -> the mapped file itself (the kernel opens the
        // VMA's file through this link). Falling through opened the HOST /proc entry, i.e. one of
        // the engine's own mappings.
        {
            const char *mfl = proc_self_leaf(rp);
            if (mfl && !strncmp(mfl, "map_files/", 10) && mfl[10]) {
                char tgt[4200], hb2[4200];
                if (!map_files_target(mfl + 10, tgt, sizeof tgt)) {
                    G_RET(c) = (uint64_t)(-ENOENT);
                    return 1;
                }
                int mf2 = open(xresolve_overlay(tgt, hb2, sizeof hb2), O_RDONLY);
                if (mf2 >= 0 && (lf & 0x80000)) fcntl(mf2, F_SETFD, FD_CLOEXEC);
                G_RET(c) = mf2 < 0 ? (uint64_t)(-errno) : (uint64_t)mf2;
                return 1;
            }
        }
        // /proc/[self|pid]/auxv (rustix/libc read it)
        if (strstr(rp, "/auxv")) {
            char tn[] = "/tmp/.hl-auxvXXXXXX";
            int afd = mkstemp(tn);
            if (afd >= 0) {
                unlink(tn);
                if (write(afd, g_auxv_data, g_auxv_len) < 0) {}
                lseek(afd, 0, SEEK_SET);
            }
            G_RET(c) = afd < 0 ? (uint64_t)(-errno) : (uint64_t)afd;
            return 1;
        }
        // cpuinfo/meminfo/stat/mounts/uptime/loadavg/version
        int pf = proc_open(rp);
        if (pf != -2) {
            G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
            return 1;
        }
        // Any other pid's /proc must not fall through to the host's: a non-member is a host process
        // the guest cannot see (a bare run reached the host's systemd), and a member peer's host
        // /proc describes the engine process running it.
        if (proc_pid_not_self(rp)) {
            G_RET(c) = (uint64_t)(-ENOENT);
            return 1;
        }
    }
    // cgroup v2 limit files (JVM/Go self-size on these). The synthesized cgroup2 mount is advertised
    // read-only (mountinfo "cgroup2 ... ro,nsdelegate") and its values are fixed, so a write-intent
    // open must fail EROFS -- exactly as a non-delegated container's cgroup mount does. Without this
    // proc_open handed back a (falsely writable) temp fd, so `echo max > cpu.max` reported success and
    // a runtime believed it had changed a limit it had not (silent fake-success).
    if (rp && !strncmp(rp, "/sys/fs/cgroup/", 15)) {
        int cg_write = (lf & 3) || (lf & 0x40) || (lf & 0x200) || (lf & 0x400); // RW/CREAT/TRUNC/APPEND
        if (cg_write) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            return 1;
        }
        int pf = proc_open(rp);
        if (pf != -2) {
            G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
            return 1;
        }
    }
    // /sys/class/net: interface introspection. Directory opens (the class dir + per-iface
    // dirs) materialize a temp dir for getdents; attribute files are served by proc_open.
    if (rp && !strncmp(rp, "/sys/class/net", 14)) {
        if (sysnet_hidden(rp)) {
            G_RET(c) = (uint64_t)(int64_t)-ENOENT;
            return 1;
        }
        int d = sysnet_dir_open(rp);
        if (d != -2) {
            if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
            return 1;
        }
        int pf = proc_open(rp);
        if (pf != -2) {
            G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
            return 1;
        }
    }
    // CPU topology sysfs: glibc __get_nprocs and tcmalloc NumPossibleCPUs read these to size
    // their per-CPU structures; an empty/missing file makes mongod abort.
    if (rp && !strncmp(rp, "/sys/devices/system/cpu/", 24)) {
        const char *leaf = rp + 24;
        if (!strcmp(leaf, "online") || !strcmp(leaf, "possible") || !strcmp(leaf, "present")) {
            char rng[32];
            cpu_range_str(rng, sizeof rng);
            int d = synth_str_fd(rng);
            G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
            return 1;
        }
        // cpuN/topology/<core_id|physical_package_id|thread_siblings[_list]|...>: lscpu/util-linux
        // reconstruct sockets/cores/threads from these; an ENOENT makes lscpu mis-count (engine-specific).
        char tb[96];
        int tn = syscpu_topology_content(rp, tb, sizeof tb);
        if (tn >= 0) {
            int d = synth_str_fd(tb);
            G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
            return 1;
        }
    }
    // the CPU-topology sysfs DIRECTORY itself (and each cpuN subdir). htop opendirs
    // /sys/devices/system/cpu and counts cpuN subdirs to size its CPU meters; finding none it keeps
    // its default of 1. macOS has no /sys, so materialize the directory tree for getdents. Matches the
    // base dir "/sys/devices/system/cpu" (no trailing slash) and any "/sys/devices/system/cpu/cpuN".
    if (rp && !strncmp(rp, "/sys/devices/system/cpu", 23)) {
        int d = syscpu_dir_open(rp);
        if (d != -2) {
            if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            if (d >= 0 && d < HL_NFD) g_opath[d] = is_opath;             // O_PATH fd -> I/O EBADF
            G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
            return 1;
        }
    }
    // Other synthesized /sys/kernel attribute files (e.g. /sys/kernel/mm/transparent_hugepage/enabled):
    // served by proc_open's constant table, same as their stat() (synth_stat_raw). proc_open returns
    // -2 for anything it doesn't recognize, so a genuine rootfs /sys path or ENOENT falls through
    // untouched to the normal handler below.
    if (rp && !strncmp(rp, "/sys/kernel/", 12)) {
        int pf = proc_open(rp);
        if (pf != -2) {
            if (pf >= 0 && (lf & 0x80000)) fcntl(pf, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
            G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
            return 1;
        }
    }
    // device nodes -> host devices (rootfs has no real /dev)
    if (rp && !strncmp(rp, "/dev/", 5)) {
        const char *hd = dev_node_hostpath(rp);
        if (hd) {
            // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap, exactly
            // like every other open path (the /dev/* device open used to skip this and let the guest
            // exceed its own soft limit -- opening /dev/null in a loop never hit EMFILE).
            int d = nofile_gate(open(hd, mf));
            // O_CLOEXEC (0x80000) must set FD_CLOEXEC on the new descriptor exactly like every
            // other open branch -- mf here carries only the access mode (the flag translation at
            // the bottom of case 56 runs after this block breaks), so a /dev/null (or any device
            // node) opened O_CLOEXEC used to leak across execve because FD_CLOEXEC was never set.
            if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC);
            // /dev/full is backed by /dev/zero for reads; flag the fd so its writes fail ENOSPC.
            if (d >= 0 && d < HL_NFD) g_devfull[d] = !strcmp(rp, "/dev/full");
            // /dev/tty (and /dev/console, backed by /dev/null): tty read semantics -- a nonblocking
            // empty read is EAGAIN, never EOF (see g_devtty). Flag the fd so svc_io maps 0->EAGAIN.
            if (d >= 0 && d < HL_NFD) g_devtty[d] = (!strcmp(rp, "/dev/tty") || !strcmp(rp, "/dev/console"));
            // This dev open uses only the access mode (mf gains O_NONBLOCK below, after this block),
            // so propagate the guest's O_NONBLOCK onto the tty fd now -- both so its host reads are
            // genuinely nonblocking and so F_GETFL reflects it for the 0->EAGAIN remap in svc_io.
            if (d >= 0 && d < HL_NFD && g_devtty[d] && (lf & 0x800)) {
                int gf = fcntl(d, F_GETFL);
                if (gf >= 0) fcntl(d, F_SETFL, gf | O_NONBLOCK);
            }
            // /dev/urandom + /dev/random accept seed writes on Linux (macOS EPERMs); flag the fd.
            if (d >= 0 && d < HL_NFD)
                g_devseed[d] = (!strcmp(rp, "/dev/urandom") || !strcmp(rp, "/dev/random"));
            G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
            return 1;
        }
    }
    return 0;
}

static int open_jailed_path(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, int lf,
                            int mf, int osymlink, int is_opath, int nf_want, uint32_t openat2_intent,
                            const hl_provider_node *projected, const char *overlay_guest) {
// TOCTOU-free per-component resolve in the jail
if (jail_routed_at((int)a0, (const char *)a1)) {
    // W4D: openat resolution cache. Memoizes the guest-abs-path -> canonical host path that the
    // jail walk below produces, so a REPEATED open of the same path collapses the ~6-syscall
    // per-component walk to a single open(host, O_NOFOLLOW). The real open ALWAYS still runs (no
    // fabricated existence/contents); a stale entry can only ever be the wrong PATH, which the
    // shared g_res_epoch (bumped above on every FS mutation, incl. this case's O_CREAT) prevents.
    // EXCLUDE O_CREAT/O_EXCL/O_TRUNC (mutating/creating) and O_DIRECTORY (deep-host-path reopen
    // regressed; see optimization-research/w4d-openat.md). Kill switch: W4_NOOPENCACHE=1.
    // ALSO exclude O_NOFOLLOW: the cache stores the CANONICAL (symlink-followed) host
    // path from a follow-mode walk, so serving it to an O_NOFOLLOW open of a symlink would
    // succeed on the target where Linux must fail ELOOP -- and an O_NOFOLLOW walk's result
    // stored under the same key would let a later follow-mode open miss the link. Keep both
    // exact by never mixing nofollow opens into the cache.
    int cacheable = !(lf & (0x40 | 0x80 | 0x200 | G_O_DIRECTORY | G_O_NOFOLLOW));
    char gkey[4200], hostc[4200];
    if (cacheable) abs_guest((int)a0, (const char *)a1, gkey, sizeof gkey);
    if (cacheable && hl_fdcache_open_lookup(gkey, hostc, sizeof hostc)) {
        // ONE atomic open replaces the per-component walk; hostc is already canonical+symlink-free.
        int r = open(hostc, mf | O_NOFOLLOW, (mode_t)a3);
        int e = errno;
        r = nofile_gate(r); // fd past the guest's soft RLIMIT_NOFILE -> EMFILE
        if (r < 0 && errno == EMFILE) e = EMFILE;
        if (r >= 0 && r < HL_NFD) g_opath[r] = is_opath;
        if (r >= 0) {
            hl_fdcache_fd_setpath(r, hostc);
            if (lf & 3) { // write-open: keep the metadata caches coherent (same as the walk path)
                hl_fdcache_metadata_evict(hostc);
                hl_fdcache_readlink_evict(hostc);
                hl_fdcache_access_evict(hostc);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
        return 1;
    }
    char fin[512];
    hl_open_plan plan;
    int typed_created = 0;
    bound_handle_slot typed_slot = {0};
    uint32_t intent = (lf & 3) == 0 ? HL_OPEN_READ : HL_OPEN_WRITE;
    if (lf & 0x40) intent |= HL_OPEN_CREATE;
    if (lf & 0x200) intent |= HL_OPEN_TRUNCATE;
    if (lf & 0x400) intent |= HL_OPEN_APPEND;
    if (is_opath) intent |= HL_OPEN_PATH_ONLY;
    if (lf & G_O_NOFOLLOW) intent |= HL_OPEN_NOFOLLOW;
    if (lf & G_O_DIRECTORY) intent |= HL_OPEN_DIRECTORY;
    intent |= openat2_intent;
    if (is_opath)
        intent &=
            ~(uint32_t)(HL_OPEN_READ | HL_OPEN_WRITE | HL_OPEN_CREATE | HL_OPEN_TRUNCATE | HL_OPEN_APPEND);
    // resolve following the final symlink unless the guest asked O_NOFOLLOW (per-arch bit)
    int pfd =
        jail_open_plan((int)a0, (const char *)a1, intent, typed_host_access(a2, is_opath),
                       is_opath ? 0 : typed_host_creation(a2), (uint32_t)a3, !nf_want, bound_handle_reserve,
                       &typed_slot, bound_handle_dirfd_error, &typed_created, fin, sizeof fin, &plan);
    if (pfd < 0) {
        bound_handle_cancel(&typed_slot);
        G_RET(c) = (uint64_t)(int64_t)pfd;
        return 1;
    }
    // fin is resolved -> O_NOFOLLOW safe
    // probe pre-existence (relative to the resolved parent) so we stamp ONLY a fresh create.
    int nf_new = nf_want && faccessat(pfd, fin, F_OK, AT_SYMLINK_NOFOLLOW) != 0;
    char typed_guest_path[4200];
    abs_guest((int)a0, (const char *)a1, typed_guest_path, sizeof typed_guest_path);
    int typed_directory = plan.target_type == HL_HOST_FILE_TYPE_DIRECTORY && (lf & G_O_DIRECTORY) &&
                          (!g_nlower || jail_is_vol(typed_guest_path));
    /* The sentry owns newly opened descriptors and virtualizes their numbers before returning to the
     * worker, so opaque host handles remain typed across the boundary without lending a native fd. */
    if (plan.directory == HL_HOST_HANDLE_INVALID && plan.target != HL_HOST_HANDLE_INVALID &&
        ((plan.target_type == HL_HOST_FILE_TYPE_REGULAR && !(lf & G_O_DIRECTORY)) || typed_directory)) {
        int64_t opened;
        char typed_host_path[HL_LINUX_PATH_MAX + 1];
        int have_typed_host_path =
            bound_handle_host_path(plan.target, typed_host_path, sizeof typed_host_path) == 0;
        close(pfd);
        opened = bound_adopt_handle(&typed_slot, plan.target, typed_open_flags(a2));
        if (opened < 0) (void)g_host_services->file->close(g_host_services->context, plan.target);
        opened = bound_relocate_lowest(opened);
        if (opened >= 0 && (projected != NULL || hl_provider_tree_files_active()) && opened < HL_NFD) {
            if (path_copy(g_fdpath[(int)opened], sizeof g_fdpath[(int)opened], overlay_guest) != 0)
                g_fdpath[(int)opened][0] = 0;
        } else if (opened >= 0 && have_typed_host_path) {
            hl_fdcache_fd_setpath((int)opened, typed_host_path);
            if ((lf & 3) || (lf & 0x40) || (lf & 0x200)) {
                HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "open-cache-evict path=%s typed=1 created=%d",
                        typed_host_path, typed_created);
                hl_fdcache_metadata_evict(typed_host_path);
                hl_fdcache_readlink_evict(typed_host_path);
                hl_fdcache_access_evict(typed_host_path);
            }
            if (typed_created && newfile_stamp_wanted()) newfile_stamp_path(typed_host_path, 1);
        }
        if (opened >= 0 && opened < 1024 && (lf & G_O_DIRECTORY)) {
            uint32_t provider_cursor = 0;
            if (hl_provider_namespace_launch_child(typed_guest_path, strlen(typed_guest_path),
                                                   &provider_cursor) != NULL &&
                path_copy(g_ovldir[(int)opened], sizeof g_ovldir[(int)opened], typed_guest_path) != 0)
                g_ovldir[(int)opened][0] = 0;
        }
        G_RET(c) = (uint64_t)opened;
        return 1;
    }
    bound_handle_cancel(&typed_slot);
    if (plan.target != HL_HOST_HANDLE_INVALID)
        (void)g_host_services->file->close(g_host_services->context, plan.target);
    if (plan.directory != HL_HOST_HANDLE_INVALID)
        (void)g_host_services->file->close(g_host_services->context, plan.directory);
    // O_PATH|O_NOFOLLOW on a symlink -> open the LINK via O_SYMLINK (else O_NOFOLLOW ELOOPs); a
    // regular O_NOFOLLOW open keeps ELOOPing on a symlink as Linux does.
    int r = openat(pfd, fin, mf | (osymlink ? O_SYMLINK : O_NOFOLLOW), (mode_t)a3);
    int e = errno;
    close(pfd);
    r = nofile_gate(r); // fd past the guest's soft RLIMIT_NOFILE -> EMFILE (host table is far larger)
    if (r < 0 && errno == EMFILE) e = EMFILE;
    if (r >= 0 && nf_new) newfile_stamp_fd(r);
    if (r >= 0 && r < HL_NFD) g_opath[r] = is_opath;
    if (r >= 0) {
        char gp[4200];
        // canonical host path for tracking
        if (hl_native_fd_path(r, gp, sizeof gp) == 0) {
            hl_fdcache_fd_setpath(r, gp);
            if ((lf & 3) || (lf & 0x40) || (lf & 0x200)) {
                HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "open-cache-evict path=%s typed=0 created=%d", gp, nf_new);
                hl_fdcache_metadata_evict(gp);
                hl_fdcache_readlink_evict(gp);
                hl_fdcache_access_evict(gp);
            }
            // W4D: memoize this walk's result (gp = F_GETPATH = canonical in-jail host path) so the
            // next open of the same guest path is a single open(). hl_fdcache_open_store re-checks
            // in-jail+epoch.
            if (cacheable) hl_fdcache_open_store(gkey, gp);
        }
    }
    G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
    return 1;
}
    return 0;
}

static uint64_t open_anonymous_tmpfile(uint64_t a0, uint64_t a1, uint64_t a3) {
    char path_buffer[4200];
    const char *directory = atpath((int)a0, (const char *)a1, path_buffer, sizeof path_buffer, 0);
    int directory_fd = open(directory, O_RDONLY | O_DIRECTORY);
    if (directory_fd < 0) return (uint64_t)(-errno);

    int fd = -1;
    int error = ENOENT;
    for (int attempt = 0; attempt < 64; attempt++) {
        char name[40];
        snprintf(name, sizeof name, ".hl-tmpfile-%d-%d", (int)getpid(), rand());
        fd = openat(directory_fd, name, O_CREAT | O_EXCL | O_RDWR, (mode_t)(a3 ? a3 : 0600));
        error = errno;
        if (fd >= 0) {
            unlinkat(directory_fd, name, 0);
            break;
        }
        if (error != EEXIST) break;
    }
    close(directory_fd);
    if (fd >= 0 && fd < HL_NFD) {
        g_fdpath[fd][0] = 0;
        memf_attach(fd, 0, 0);
    }
    return fd < 0 ? (uint64_t)(-(int64_t)error) : (uint64_t)fd;
}

static void svc_fs_access_56(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 56: {
        // openat -- Linux O_* -> macOS O_* (they differ!)
        /* Consume any resolve intent carried over from an openat2 fall-through;
         * a direct openat leaves it cleared. */
        uint32_t openat2_intent = g_openat2_resolve_intent;
        g_openat2_resolve_intent = 0;
        int lf = (int)a2, mf = lf & 0x3;
        // O_PATH (Linux 0x200000, arch-independent): the fd only NAMES the file -- fstat / *at dirfd /
        // fchdir work through it, but read/write are rejected EBADF. macOS has no O_PATH, so we open a
        // normal read fd (O_RDONLY, +O_DIRECTORY for a dir) for the metadata ops and record the flag so the
        // I/O family (svc_io) returns EBADF. Marked on every open-success path below.
        int is_opath = (lf & 0x200000) != 0;
        // Confinement turns a relative guest name into an absolute host path. Validate its directory
        // descriptor first, while Linux still considers it: otherwise host openat() ignores the invalid
        // dirfd and reports an unrelated path error (usually ENOENT).
        int directory_error = at_dirfd_check((int32_t)a0, (const char *)a1);
        if (directory_error < 0) {
            G_RET(c) = (uint64_t)(int64_t)directory_error;
            break;
        }
        // openat/openat2 never give an empty pathname AT_EMPTY_PATH semantics,
        // including for O_PATH. Resolving "" through atpath() instead folded the
        // dirfd itself into the host path and opened it successfully.
        if (a1 && !guest_bad_ptr(a1, 1) && !*(const char *)a1) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
            break;
        }
        // O_PATH|O_NOFOLLOW naming a SYMLINK: Linux opens the LINK ITSELF (so readlinkat(fd,"",..) and
        // fstatat(fd,"",AT_EMPTY_PATH) operate on the symlink --). macOS has no O_PATH, and a plain
        // follow-open would open the TARGET (F_GETPATH then names the target, breaking the empty-path
        // readlink). Use macOS O_SYMLINK for exactly this combination so the fd names the symlink node; a
        // regular file opens normally under O_SYMLINK too, and a plain (non-O_PATH) O_NOFOLLOW open still
        // ELOOPs on a symlink as Linux requires.
        int osymlink = (is_opath && (lf & G_O_NOFOLLOW)) ? O_SYMLINK : 0;
        // Read-only bind mount: any write-intent open (O_WRONLY/O_RDWR/O_CREAT/O_TRUNC/O_APPEND, incl.
        // O_TMPFILE which carries O_RDWR) under an `-v …:ro` volume fails EROFS -- exactly as the kernel
        // rejects a write-open on a read-only mount. A pure O_RDONLY open still succeeds. Checked up front
        // so neither O_TMPFILE nor the memoized open-cache walk below can slip a write through.
        int write_intent = (lf & 3) || (lf & 0x40) || (lf & 0x200) || (lf & 0x400);
        char projected_path[4200];
        const hl_provider_node *projected_open_node = NULL;
        if (write_intent) {
            guest_abspath_at((int)a0, (const char *)a1, projected_path, sizeof projected_path);
            projected_open_node = hl_provider_namespace_launch_resolve(projected_path, strlen(projected_path));
        }
        if (write_intent && projected_open_node == NULL && jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        // O_TMPFILE (the __O_TMPFILE bit 0x400000 is arch-independent): create an unnamed, auto-cleaned
        // regular file inside the named directory by making one + immediately unlinking it (macOS has no
        // O_TMPFILE). The fd is a normal RW file with link count 0.
        if (lf & 0x400000) {
            G_RET(c) = open_anonymous_tmpfile(a0, a1, a3);
            break;
        }
        if (open_synthetic_path(c, a0, a1, lf, mf, is_opath)) break;
        if (lf & 0x40) mf |= O_CREAT;
        if (lf & 0x80) mf |= O_EXCL;
        if (lf & 0x200) mf |= O_TRUNC;
        if (lf & 0x400) mf |= O_APPEND;
        if (lf & 0x800) mf |= O_NONBLOCK;
        if (lf & G_O_DIRECTORY) mf |= O_DIRECTORY;
        if (lf & 0x80000) mf |= O_CLOEXEC;
        // when a runtime-dropped process (gosu postgres) O_CREATs a file, the new inode must be
        // owned by its current fsuid/fsgid, not the cuid/cgid default. Only meaningful when O_CREAT is
        // set AND a cred drop makes the stamp differ from the default; the pre-existence probe (so we
        // never re-own a file merely OPENED with O_CREAT) then runs only in that rare dropped case.
        int nf_want = (lf & 0x40) && newfile_stamp_wanted();
        {
            // /proc/self/fd/N -> reopen what host fd N points at. Linux reopen gives a FRESH file
            // description (offset 0, access narrowed to the requested mode), so prefer reopening by the
            // F_GETPATH path with the guest's flags; for fds with no path (pipe/socket/anon) fall back to
            // dup(N), which at least hands back a working, equivalent fd. /dev/std{in,out,err} map to
            // fd 0/1/2 here (open-only; their readlink stays the on-disk symlink so `ls -l /dev` works).
            char special_path[4200];
            const char *special = guest_symlink_target((const char *)a1, special_path, sizeof special_path);
            // Detect /proc/self/fd/N (and /dev/fd/N) from the GUEST path first: guest_symlink_target follows
            // the host's real /proc/self/fd/N symlink to the backing file, which for an O_TMPFILE/memf-backed
            // fd resolves to the unlinked "(deleted)" scratch inode -- losing the fd number, so the reopen
            // fell through to a plain open of an empty/absent file and the RAM-cached writes vanished (freopen
            // read back nothing). Recovering the fd here lets the reopen alias the descriptor (memf flushed),
            // so the reopened stream sees the written data.
            int pfn = procfd_num_at((int)a0, (const char *)a1);
            if (pfn < 0) pfn = procfd_num(special);
            if (pfn < 0) pfn = dev_std_fd(special);
            // /proc/self/fd/N only EXISTS while N is open: opening the name of a closed descriptor is
            // -ENOENT on Linux (the magic symlink is simply absent). procfd_num() only parses the number,
            // so the engine fell through to dup(N) and surfaced the dup's -EBADF instead.
            if (pfn >= 0 && fcntl(pfn, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                break;
            }
            if (pfn >= 0) {
                hl_linux_fd_snapshot typed;
                if (!bound_source_is_native() && g_linux_box != NULL &&
                    hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)pfn, &typed) == HL_STATUS_OK) {
                    int64_t duplicated = bound_dup_at_least((hl_linux_fd)pfn, 0, lf & 0x80000 ? 1u : 0u);
                    G_RET(c) = (uint64_t)duplicated;
                    break;
                }
                memf_materialize(pfn); // reopen-by-fd would expose the real file -> flush RAM cache first
                char gp[4200];
                int r = -1;
                struct stat descriptor_status;
                int path_reopen = fstat(pfn, &descriptor_status) == 0 &&
                                  (S_ISREG(descriptor_status.st_mode) || S_ISDIR(descriptor_status.st_mode));
                if (path_reopen && hl_native_fd_path(pfn, gp, sizeof gp) == 0 && gp[0])
                    r = open(gp, mf & ~(O_EXCL | O_CREAT), (mode_t)a3);
                if (r < 0) r = dup(pfn); // anonymous/pipe/socket fd -> share the description
                if (r >= 0) {
                    fd_reset_emul(r);
                    char tp[4200];
                    if (hl_native_fd_path(r, tp, sizeof tp) == 0) hl_fdcache_fd_setpath(r, tp);
                }
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
                break;
            }
        }
        {
            // POSIX shm: glibc shm_open opens /dev/shm/<name>; the rootfs has no tmpfs, so back it with a
            // real host file (MAP_SHARED + fork share it). Flatten any subdirs into the single filename.
            char hp[4224];
            const char *sp = shm_hostpath((const char *)a1, hp, sizeof hp);
            if (sp) {
                int d = open(sp, mf, (mode_t)a3);
                G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                break;
            }
        }
        {
            // pty: /dev/ptmx -> posix_openpt; /dev/pts/N -> slave
            const char *rp = (const char *)a1;
            if (rp && (!strcmp(rp, "/dev/ptmx") || !strcmp(rp, "/dev/pts/ptmx"))) {
                int m = posix_openpt(O_RDWR | O_NOCTTY);
                if (m >= 0) {
                    grantpt(m);
                    unlockpt(m);
                    pts_alloc(m); // assign this master a Linux devpts index N (TIOCGPTN / /dev/pts/N use it)
                    if (lf & 0x80000) fcntl(m, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC on the master
                }
                G_RET(c) = m < 0 ? (uint64_t)(-errno) : (uint64_t)m;
                break;
            }
            // /dev/pts/0 is the container's controlling terminal (not a guest-created master's slave): a
            // program that does open(ttyname(0)) must get a fresh fd to the SAME pty. Reopen the anchor's
            // host device (F_GETPATH) or dup it. Guest-opened ptys use /dev/pts/N with N == their master fd
            // (>=3), so this never shadows them.
            if (rp && !strcmp(rp, "/dev/pts/0")) {
                int a = ctty_anchor();
                if (a >= 0) {
                    char hp[4200];
                    int s = (hl_native_fd_path(a, hp, sizeof hp) == 0) ? open(hp, mf, (mode_t)a3) : dup(a);
                    G_RET(c) = s < 0 ? (uint64_t)(-errno) : (uint64_t)s;
                    break;
                }
            }
            // a guest-created pty slave /dev/pts/N. Intercepted HERE, ahead of the overlay resolver, so
            // the freshly-allocated slave (which has no rootfs backing file) is never an ENOENT miss. N is the
            // devpts index hl assigned the master (pts_alloc); ptsname(master) yields the host slave device.
            if (rp && !strncmp(rp, "/dev/pts/", 9) && rp[9] >= '0' && rp[9] <= '9') {
                int n = atoi(rp + 9);
                int mfd = pts_master_fd(n);
                // when this process no longer holds the master (apt's SetupSlavePtyMagic closes it in
                // the forked child), fall back to the host slave path cached at pts_alloc -- the pty is still
                // alive if the PARENT holds the master, so this open succeeds; it fails once the pty is truly
                // gone. Without this the child's open ENOENTed -> apt "Can not write log (Is /dev/pts mounted?)".
                const char *sn = (mfd >= 0) ? ptsname(mfd) : pts_slave_name(n);
                if (!sn) {
                    G_RET(c) = (uint64_t)(int64_t)(-2); // ENOENT: no such pts index (matches Linux)
                    break;
                }
                int s = open(sn, mf, (mode_t)a3);
                if (s >= 0) {
                    if (mfd >= 0) ptm_apply_to_slave(mfd, s); // slave inherits the master's cached termios/winsize
                    pts_note_slave(s, n);                     // stamp /dev/pts/N onto the slave fd + publish the node
                    G_RET(c) = (uint64_t)s;
                } else {
                    // A cached-name open that fails means the pty is gone -> Linux reports ENOENT for the index,
                    // not the host's device errno; a live-master open reports its real errno faithfully.
                    G_RET(c) = (mfd < 0) ? (uint64_t)(int64_t)(-2) : (uint64_t)(-errno);
                }
                break;
            }
        }
        // OVERLAY: resolve across layers (upper shadows lowers). A bind volume is its own jail and must
        // reach the opaque jail planner below; treating it as an overlay path bypasses typed directory I/O.
        char overlay_guest[4200];
        abs_guest((int)a0, (const char *)a1, overlay_guest, sizeof overlay_guest);
        const hl_provider_node *projected = hl_provider_namespace_launch_resolve(overlay_guest, strlen(overlay_guest));
        if (g_rootfs && g_nlower && !jail_is_vol(overlay_guest) && projected == NULL) {
            const char *gp = overlay_guest;
            char host[4300];
            // O_WRONLY/O_RDWR/O_CREAT -> write
            int isw = (lf & 3) || (lf & 0x40);
            if (isw)
                // copy-up the lower file (or upper path to create)
                overlay_copyup(gp, host, sizeof host);
            else
                overlay_resolve(gp, host, sizeof host, (lf & G_O_NOFOLLOW) != 0);
            // after copy-up, `host` (the upper path) exists iff the file was already present in the
            // overlay -> a missing upper means this open will CREATE it fresh; stamp its owner post-open.
            int nf_new = nf_want && access(host, F_OK) != 0;
            // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (host table larger).
            int r = nofile_gate(
                open(host, mf | osymlink | ((lf & G_O_NOFOLLOW) && !osymlink ? O_NOFOLLOW : 0), (mode_t)a3));
            if (r >= 0 && nf_new) newfile_stamp_fd(r);
            if (r >= 0 && r < HL_NFD) g_opath[r] = is_opath;
            if (r >= 0) {
                char gpa[4200];
                int have_canon = hl_native_fd_path(r, gpa, sizeof gpa) == 0;
                if (have_canon) {
                    hl_fdcache_fd_setpath(r, gpa);
                    if (isw) {
                        hl_fdcache_metadata_evict(gpa);
                        hl_fdcache_readlink_evict(gpa);
                        hl_fdcache_access_evict(gpa);
                    }
                }
                // Remember the guest dir for merged getdents. Derive it from the fd's CANONICAL host path
                // (F_GETPATH already resolved `.`/`..`/symlinks per component) rather than the raw guest
                // arg: a `..` out of a nested bind mount (e.g. `/mnt/..`) keeps a mount-point component that
                // lives ONLY in the writable upper, so re-resolving the raw path per layer finds it in no
                // lower and enumerates the upper alone -- the merged root listing then dropped every
                // lower-only entry (bin, lib, usr...). The canonical path folds `/mnt/..` back to the rootfs
                // root, so overlay_readdir enumerates every layer. NOT for a bind-mount volume dir (its own
                // jail, in no layer): it must list via plain readdir of the host fd; tagging it overlay ->
                // overlay_readdir misses it -> an empty `ls` on the mount.
                // ONLY for a DIRECTORY fd: g_ovldir tags a fd for merged-overlay getdents, and the lseek
                // handler (io.c case 62) treats any g_ovldir-tagged fd as a directory stream -- redirecting
                // SEEK_SET to ovldents_rewind and NOT seeking the real host fd. Tagging a regular file here
                // therefore made lseek(fd, off, SEEK_SET) a silent no-op on it (read then served from offset
                // 0): gpg's keyring_get_keyblock seeks to the matched keyblock's found.offset, so the wrong
                // keyblock (the first key) was re-read -> BADSIG on apt-get update over a layered image.
                char gdir[4200] = {0};
                if (have_canon) {
                    if (guest_from_host(gpa, gdir, sizeof gdir) <= 0) gdir[0] = 0;
                } else
                    snprintf(gdir, sizeof gdir, "%s", gp);
                struct stat dst;
                uint32_t provider_cursor = 0;
                int has_provider_children =
                    hl_provider_namespace_launch_child(gp, strlen(gp), &provider_cursor) != NULL;
                if (has_provider_children) snprintf(gdir, sizeof gdir, "%s", gp);
                if (r < HL_NFD && gdir[0] && (!jail_is_vol(gdir) || has_provider_children) && fstat(r, &dst) == 0 &&
                    S_ISDIR(dst.st_mode))
                    if (path_copy(g_ovldir[r], sizeof g_ovldir[r], gdir) != 0) g_ovldir[r][0] = 0;
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
            break;
        }
        if (open_jailed_path(c, a0, a1, a2, a3, lf, mf, osymlink, is_opath, nf_want, openat2_intent, projected,
                             overlay_guest))
            break;
        char pb[4200];
        // no jail
        /* openat2 containment (RESOLVE_NO_SYMLINKS/BENEATH/IN_ROOT, carried as
         * HL_OPEN_NO_SYMLINKS) forbids resolving a symlink: keep the final
         * component unresolved and open it with O_NOFOLLOW so a symlink errors
         * with ELOOP instead of being silently followed. */
        int o2_nofollow = (openat2_intent & HL_OPEN_NO_SYMLINKS) != 0 && !osymlink;
        // A plain (non-O_PATH) guest O_NOFOLLOW must keep the final symlink component unresolved and open it
        // with host O_NOFOLLOW so Linux's ELOOP is produced -- `mf` never carries O_NOFOLLOW, so relying on
        // it silently followed the link and opened the target. The O_PATH|O_NOFOLLOW case is handled by
        // osymlink (O_SYMLINK opens the link itself), so exclude it here.
        int nofollow_final = ((lf & G_O_NOFOLLOW) != 0 || o2_nofollow) && !osymlink;
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, (osymlink || nofollow_final) ? 1 : 0);
        int nf_new = nf_want && faccessat(ATFD(a0), p, F_OK, AT_SYMLINK_NOFOLLOW) != 0; // stamp only fresh
        // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the shared host fd
        // table is far larger; engine-private fds are hoisted above 1<<20, so the guest limit is emulated).
        // O_PATH|O_NOFOLLOW on a symlink -> O_SYMLINK opens the link itself.
        int r = nofile_gate(openat(ATFD(a0), p, mf | osymlink | (nofollow_final ? O_NOFOLLOW : 0), (mode_t)a3));
        if (r >= 0 && nf_new) newfile_stamp_fd(r);
        if (r >= 0 && r < HL_NFD) g_opath[r] = is_opath;
        if (r >= 0) {
            hl_fdcache_fd_setpath(r, p);
            if ((lf & 3) || (lf & 0x40) || (lf & 0x200)) {
                hl_fdcache_metadata_evict(p);
                hl_fdcache_readlink_evict(p);
                hl_fdcache_access_evict(p);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: break;
    }
}

static int svc_fs_access(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                         uint64_t a5) {
    switch (nr) {
    case 49: svc_fs_access_49(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 50: svc_fs_access_50(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 52: svc_fs_access_52(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 53:
    case 452: svc_fs_access_53(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 54: svc_fs_access_54(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 55: svc_fs_access_55(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 437: svc_fs_access_437(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 56: svc_fs_access_56(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
