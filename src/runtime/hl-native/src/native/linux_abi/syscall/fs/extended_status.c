static int statx_provider_status(const hl_provider_node *provider, struct stat *status) {
    hl_host_result opened = hl_provider_files_open_service(provider->service, HL_HOST_FILE_READ);
    if (opened.status != HL_STATUS_OK) return -EIO;

    hl_host_file_metadata metadata;
    hl_status metadata_status =
        g_host_services->file->metadata(g_host_services->context, opened.value, &metadata).status;
    (void)g_host_services->file->close(g_host_services->context, opened.value);
    if (metadata_status != HL_STATUS_OK) return -EIO;

    memset(status, 0, sizeof *status);
    status->st_mode = (provider->kind == HL_PROVIDER_NODE_CHARACTER ? S_IFCHR
                       : provider->kind == HL_PROVIDER_NODE_BLOCK   ? S_IFBLK
                                                                    : S_IFREG) |
                      (mode_t)provider->mode;
    status->st_uid = (uid_t)provider->uid;
    status->st_gid = (gid_t)provider->gid;
    if (provider->kind == HL_PROVIDER_NODE_CHARACTER || provider->kind == HL_PROVIDER_NODE_BLOCK)
        status->st_rdev = (dev_t)hl_linux_device_make(provider->major, provider->minor);
    status->st_size = (off_t)metadata.size;
    status->st_nlink = 1;
    return 0;
}

static int statx_resolve_status(const hl_provider_node *provider, const char *guest_path, const char *raw_path,
                                const char *native_path, uint64_t dirfd, int nofollow, int empty,
                                struct stat *status, const char **ownership_path, int *ownership_fd,
                                char *backing_path, size_t backing_path_size) {
    if (provider != NULL && provider->kind != HL_PROVIDER_NODE_DIRECTORY &&
        provider->kind != HL_PROVIDER_NODE_SYMLINK)
        return statx_provider_status(provider, status);

    char executable[1024];
    if (proc_self_exe(guest_path, executable, sizeof executable)) {
        if (nofollow) {
            memset(status, 0, sizeof *status);
            status->st_mode = S_IFLNK | 0777;
            status->st_nlink = 1;
            return 0;
        }
        char host_path[4200];
        const char *resolved = xresolve_overlay(executable, host_path, sizeof host_path);
        int rc = stat(resolved, status) == 0 ? 0 : -errno;
        if (rc == 0 && path_copy(backing_path, backing_path_size, resolved) == 0) *ownership_path = backing_path;
        return rc;
    }
    if (sysnet_hidden(guest_path)) return -ENOENT;
    if (!nofollow && procfd_num(guest_path) >= 0)
        return procfd_follow_stat(guest_path, status) > 0 ? 0 : -ENOENT;
    if (synth_stat_raw(guest_path, status)) return 0;
    if (raw_path && raw_path[0] && !empty && !nofollow) {
        int rc;
        if (!hl_fdcache_metadata_lookup(native_path, &rc, status)) {
            int result = fstatat(ATFD(dirfd), native_path, status, 0);
            rc = result < 0 ? -errno : 0;
            hl_fdcache_metadata_store(native_path, rc, status);
        }
        if (rc == 0) *ownership_path = native_path;
        return rc;
    }

    int memory_file = empty && memf_get((int)dirfd);
    int result = memory_file ? memf_fstat((int)dirfd, status)
                 : empty    ? fstat((int)dirfd, status)
                            : fstatat(ATFD(dirfd), native_path, status, nofollow ? AT_SYMLINK_NOFOLLOW : 0);
    int rc = result < 0 ? -errno : 0;
    if (rc == 0 && empty) *ownership_fd = (int)dirfd;
    if (rc == 0 && !empty) *ownership_path = native_path;
    return rc;
}

static void svc_fs_extended_status_291(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                       uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 291: {
        struct stat s;
        // statx(dfd, path, flags, mask, buf)
        char pb[4200];
        int nofollow = (a2 & 0x100); // AT_SYMLINK_NOFOLLOW: stat the link itself, don't dereference
        const char *raw = (const char *)a1;
        // statx error-path fidelity, in the kernel's pre-walk order (LTP statx03). EINVAL on any unknown
        // flag bit or both AT_STATX_SYNC_TYPE bits set; EINVAL on a reserved mask bit (STATX__RESERVED);
        // EBADF/ENOTDIR on a bad/non-dir dirfd for a relative path -- all BEFORE resolving the path.
        if ((a2 & ~(uint64_t)0x7900) || (a2 & 0x6000) == 0x6000) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (a3 & 0x80000000u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        {
            int adc = at_dirfd_check((int)a0, raw);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        // The pathname was already imported and validated at the svc_fs boundary. Validate only the
        // still-guest-owned output buffer here.
        if (guest_accessible_prefix(a4, 256, HL_LOGICAL_VMA_WRITE) != 256) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        int procfd = procfd_num_at((int)a0, raw) >= 0 || procfd_directory_path(raw);
        const char *p = procfd ? raw : atpath((int)a0, raw, pb, sizeof pb, nofollow);
        if (resolve_loop_detected()) { // followed symlink chain past the traversal limit -> ELOOP
            G_RET(c) = (uint64_t)(int64_t)(-ELOOP);
            break;
        }
        int rc, empty = (raw && !raw[0] && (a2 & 0x1000));
        const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
        char gsyn291[4200], procfd_path291[64];
        if (raw && raw[0] && raw[0] != '/') {
            guest_abspath_at((int)a0, raw, gsyn291, sizeof gsyn291);
            if (!strncmp(gsyn291, "/proc/", 6) || !strncmp(gsyn291, "/dev/fd/", 8)) gp = gsyn291;
        } else if (raw && (!strncmp(raw, "/proc/", 6) || !strncmp(raw, "/dev/fd/", 8))) {
            gp = raw;
        }
        gp = procfd_namespace_path(gp, procfd_path291, sizeof procfd_path291);
        // Track the host backing file so ownership virtualization reads the SAME guest-chown xattr that
        // fstat/newfstatat do: xpath = the host path we stat'd, or xfd = the fd for AT_EMPTY_PATH;
        // both stay NULL/-1 for synthetic entries (no backing file -> cuid/cgid default applies).
        const char *xpath = NULL;
        int xfd = -1;
        char provider_path[4200];
        char backing_path[4200];
        const hl_provider_node *provider;
        guest_abspath_at((int)a0, raw, provider_path, sizeof provider_path);
        provider = hl_provider_namespace_launch_resolve(provider_path, strlen(provider_path));
        rc = statx_resolve_status(provider, gp, raw, p, a0, nofollow, empty, &s, &xpath, &xfd, backing_path,
                                  sizeof backing_path);
        if (rc < 0) {
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        // Route ownership through the SHARED virtualization (cuid/cgid default + guest-chown xattr via
        // the cache) so statx's uid/gid are byte-identical to fstat/newfstatat for the same file.
        uint32_t vuid, vgid;
        stat_virt_ids(&s, xpath, xfd, &vuid, &vgid);
        uint8_t encoded_statx[256];
        uint8_t *d = encoded_statx;
        // Birth time is mirrored from the host filesystem so the mask bit is honest per-fs (see
        // hl_statx_host_btime). Synthetic entries have no backing file (xpath==NULL && xfd<0) and so
        // never advertise btime -- matching native procfs/sysfs, which do not report it.
        int64_t btime_sec;
        uint32_t btime_nsec;
        int have_btime = hl_statx_host_btime(xpath, xfd, nofollow, &btime_sec, &btime_nsec);
        // struct statx (Linux uapi offsets). We fill STATX_BASIC_STATS (+ STATX_BTIME when supported).
        memset(d, 0, 256);
        // stx_mask @0 = basic(0x7ff) | btime(0x800 only when the host filesystem reports it); stx_blksize @4
        *(uint32_t *)(d + 0) = 0x7ff | (have_btime ? 0x800u : 0u);
        *(uint32_t *)(d + 4) = 4096;
        // stx_nlink @16 (raw, matching fill_linux_stat)
        *(uint32_t *)(d + 16) = (uint32_t)s.st_nlink;
        // stx_uid @20  stx_gid @24 (virtualized)
        *(uint32_t *)(d + 20) = vuid;
        *(uint32_t *)(d + 24) = vgid;
        // stx_mode @28
        *(uint16_t *)(d + 28) = (uint16_t)stat_virt_mode(&s, xpath, xfd);
        // stx_ino @32
        *(uint64_t *)(d + 32) = s.st_ino;
        // stx_size @40
        *(uint64_t *)(d + 40) = (uint64_t)s.st_size;
        // stx_blocks @48
        *(uint64_t *)(d + 48) = HL_HOST_STAT_BLOCKS(&s);
        // stx_{atime,btime,ctime,mtime} @64/80/96/112: {s64 tv_sec; u32 tv_nsec} each 16 bytes
        *(int64_t *)(d + 64) = HL_HOST_STAT_ATIME_SEC(&s);
        *(uint32_t *)(d + 72) = (uint32_t)HL_HOST_STAT_ATIME_NSEC(&s);
        *(int64_t *)(d + 80) = have_btime ? btime_sec : 0;
        *(uint32_t *)(d + 88) = have_btime ? btime_nsec : 0;
        *(int64_t *)(d + 96) = HL_HOST_STAT_CTIME_SEC(&s);
        *(uint32_t *)(d + 104) = (uint32_t)HL_HOST_STAT_CTIME_NSEC(&s);
        *(int64_t *)(d + 112) = HL_HOST_STAT_MTIME_SEC(&s);
        *(uint32_t *)(d + 120) = (uint32_t)HL_HOST_STAT_MTIME_NSEC(&s);
        // stx_rdev_major @128 / minor @132, stx_dev_major @136 / minor @140 -- decoded from the SAME raw
        // dev values fill_linux_stat packs into st_rdev/st_dev, so a caller sees identical major:minor.
        *(uint32_t *)(d + 128) = hl_linux_device_major((uint64_t)s.st_rdev);
        *(uint32_t *)(d + 132) = hl_linux_device_minor((uint64_t)s.st_rdev);
        *(uint32_t *)(d + 136) = hl_linux_device_major((uint64_t)s.st_dev);
        *(uint32_t *)(d + 140) = hl_linux_device_minor((uint64_t)s.st_dev);
        // stx_mnt_id @144 -- modern kernels fill it opportunistically regardless of the requested mask,
        // so mirror the host: set the value and the STATX_MNT_ID bit whenever the host reports it.
        {
            uint64_t mnt_id;
            if (hl_statx_host_mnt_id(xpath, xfd, nofollow, &mnt_id)) {
                *(uint64_t *)(d + 144) = mnt_id;
                *(uint32_t *)(d + 0) |= 0x1000u;
            }
        }
        if (guest_copy_to(a4, encoded_statx, sizeof encoded_statx) != sizeof encoded_statx) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    // name_to_handle_at(dfd, path, file_handle*, mount_id*, flags): macOS has no FS file handles, so
    // synthesize a stable 16-byte handle from st_dev+st_ino (round-trips file identity). file_handle is
    // { u32 handle_bytes; i32 handle_type; u8 f_handle[]; }; handle_bytes is the buffer size on input
    // and is rewritten to the produced size (-EOVERFLOW if the caller's buffer is too small).
    default: break;
    }
}

static void svc_fs_extended_status_264(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                       uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 264: {
        uint32_t handle_capacity;
        if (!a2 || guest_copy_from(&handle_capacity, a2, sizeof handle_capacity) != sizeof handle_capacity) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        int empty = (a4 & 0x1000);    // AT_EMPTY_PATH
        int nofollow = !(a4 & 0x400); // default: don't dereference the final symlink (AT_SYMLINK_FOLLOW=0x400)
        struct stat s;
        char pb[4200];
        int rr;
        if (empty && memf_get((int)a0))
            rr = memf_fstat((int)a0, &s);
        else if (empty)
            rr = fstat((int)a0, &s);
        else {
            const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, nofollow);
            rr = fstatat(ATFD(a0), p, &s, nofollow ? AT_SYMLINK_NOFOLLOW : 0);
        }
        if (rr < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        const uint32_t need = 16; // dev(8) + ino(8)
        if (handle_capacity < need) {
            if (guest_copy_to(a2, &need, sizeof need) != sizeof need) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            G_RET(c) = (uint64_t)(int64_t)(-EOVERFLOW);
            break;
        }
        if (guest_accessible_prefix(a2, need + 8, HL_LOGICAL_VMA_WRITE) != need + 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t dev = (uint64_t)s.st_dev, ino = (uint64_t)s.st_ino;
        uint8_t handle[24] = {0};
        memcpy(handle, &need, 4);
        int32_t handle_type = 1;
        memcpy(handle + 4, &handle_type, 4);
        memcpy(handle + 8, &dev, 8);
        memcpy(handle + 16, &ino, 8);
        if (guest_copy_to(a2, handle, sizeof handle) != sizeof handle) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3) {
            int mount_id = (int)s.st_dev;
            if (guest_copy_to(a3, &mount_id, sizeof mount_id) != sizeof mount_id) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // open_by_handle_at(mount_fd, file_handle*, flags): reopening a file from an opaque handle requires
    // CAP_DAC_READ_SEARCH, which an unprivileged task never holds, so the kernel rejects it with EPERM
    // before it ever looks at the handle. hl cannot reconstruct a file from the synthetic dev+ino handle
    // minted by name_to_handle_at anyway, so report the same EPERM the host kernel returns (an unhandled
    // fall-through would answer ENOSYS, which wrongly tells NFS/backup tools the syscall is unavailable).
    default: break;
    }
}

static void svc_fs_extended_status_265(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                       uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 265: G_RET(c) = (uint64_t)(int64_t)(-EPERM); break;
    // faccessat2(dirfd,path,mode,flags) -- glibc access() uses it; same path/confinement, flags ignored
    case 439:
    case 48: {
        char pb[4200];
        // Linux validates the mode up front: only F_OK(0) | R_OK(4) | W_OK(2) | X_OK(1) are defined, so any
        // other bit (e.g. 0x8) is -EINVAL (do_faccessat rejects it before touching the path).
        if ((int)a2 & ~(R_OK | W_OK | X_OK)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (nr == 439 && (a3 & ~(uint64_t)(AT_EACCESS | AT_SYMLINK_NOFOLLOW | 0x1000))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // Linux: an empty pathname is ENOENT for faccessat(48), and for faccessat2(439) unless
        // AT_EMPTY_PATH(0x1000) is set. hl used to resolve "" to the rootfs root (a searchable dir) and
        // report it executable, so `[ -x "$(command -v missing)" ]` (dash's `command -v` yields "" for a
        // missing command) wrongly passed and ran a nonexistent `update-menus` -> exit 127. That is the
        // dh_installmenu postinst guard in fish/lynx/many packages, so it broke `dpkg --configure`.
        if (!a1 || !((const char *)a1)[0]) {
            if (!(nr == 439 && (a3 & 0x1000))) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                break;
            }
            G_RET(c) = (uint64_t)(int64_t)dac_access_fd((int)a0, (int)a2, (a3 & AT_EACCESS) != 0);
            break;
        }
        // /proc/[self|pid]/exe magic symlink -> access the actual executable (matched on the
        // guest-absolute path so dirfd-relative and cwd-relative spellings work too)
        char ep[1024], gsyn48[4200];
        const char *gp48 = (const char *)a1;
        if (gp48 && gp48[0] && gp48[0] != '/') {
            guest_abspath_at((int)a0, gp48, gsyn48, sizeof gsyn48);
            if (!strncmp(gsyn48, "/proc/", 6) || !strncmp(gsyn48, "/dev/fd/", 8)) gp48 = gsyn48;
        }
        if (!g_rootfs && proc_self_exe(gp48, ep, sizeof ep)) {
            G_RET(c) = (uint64_t)(int64_t)dac_access_executable((int)a2, nr == 439 && (a3 & AT_EACCESS));
            break;
        }
        // pseudo /dev char devices (open() backs them with a host node) must also test as present: e.g.
        // libgcrypt probes access("/dev/urandom",R_OK) to pick its RNG module -- an ENOENT there aborts
        // gpgv and breaks `apt-get update`. Test the host device with the requested mode.
        if (!g_rootfs) {
            const char *hd = dev_node_hostpath((const char *)a1);
            if (hd) {
                int r = access(hd, (int)a2);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // faccessat2(439) accepts AT_SYMLINK_NOFOLLOW(0x100): check the LINK itself, don't dereference it.
        // hl ignored the flag and always followed, so faccessat(dangling-symlink, F_OK, AT_SYMLINK_NOFOLLOW)
        // wrongly ENOENT'd (the link exists) instead of succeeding. Resolve the final component unfollowed
        // and evaluate the link node directly. (faccessat(48) has no flags word; a3 is unused there.)
        int access_nofollow = (nr == 439) && (a3 & 0x100);
        char canonical_access[4200];
        int final_requires_directory = 0;
        int search_result = dac_search_at((int)a0, (const char *)a1, access_nofollow,
                                          nr == 439 && (a3 & AT_EACCESS),
                                          canonical_access, sizeof canonical_access, &final_requires_directory);
        const char *synthetic_device = search_result == 0 ? dev_node_hostpath(canonical_access) : NULL;
        if (g_rootfs && synthetic_device != NULL) {
            int result = final_requires_directory ? -ENOTDIR : search_result;
            if (result == 0 && access(synthetic_device, F_OK) != 0) result = -errno;
            if (result == 0)
                result = dac_access_synthetic(canonical_access, (int)a2, nr == 439 && (a3 & AT_EACCESS));
            G_RET(c) = (uint64_t)(int64_t)result;
            break;
        }
        if (g_rootfs && a1 && ((const char *)a1)[0]) {
            int cursor_access = dac_access_at((int)a0, (const char *)a1, access_nofollow, (int)a2,
                                              nr == 439 && (a3 & AT_EACCESS));
            // Synthetic /proc, /sys, and device entries may not have an on-disk cursor node. Let their
            // established providers below answer absence; every other cursor verdict is authoritative.
            if (cursor_access != -ENOENT && cursor_access != -ENOSYS) {
                G_RET(c) = (uint64_t)(int64_t)cursor_access;
                break;
            }
        }
        const char *proc_candidate = search_result == 0 ? canonical_access : gp48;
        if (proc_self_exe(proc_candidate, ep, sizeof ep)) {
            G_RET(c) = final_requires_directory
                           ? (uint64_t)(int64_t)(-ENOTDIR)
                           : (uint64_t)(int64_t)dac_access_executable((int)a2,
                                                                      nr == 439 && (a3 & AT_EACCESS));
            break;
        }
        {
            const char *host_device = synthetic_device;
            if (host_device) {
                int result = final_requires_directory ? -ENOTDIR : access(host_device, F_OK);
                if (result < 0 && !final_requires_directory) result = -errno;
                G_RET(c) = result < 0 ? (uint64_t)(int64_t)result
                                      : (uint64_t)(int64_t)dac_access_synthetic(
                                            canonical_access, (int)a2, nr == 439 && (a3 & AT_EACCESS));
                break;
            }
        }
        // faccessat
        const char *p = procfd_directory_path(gp48)
                            ? gp48
                            : atpath((int)a0, (const char *)a1, pb, sizeof pb, access_nofollow ? 1 : 0);
        if (access_nofollow && p) {
            struct stat ls;
            if (fstatat(ATFD(a0), p, &ls, AT_SYMLINK_NOFOLLOW) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // The link node itself: existence (F_OK) always holds; a symlink's own mode bits are 0777, so a
            // permission probe reduces to its mode -- match the synth mode-check for consistency.
            int mode = (int)a2 & 7, ok = 1;
            if ((mode & 4) && !(ls.st_mode & 0444)) ok = 0;
            if ((mode & 2) && !(ls.st_mode & 0222)) ok = 0;
            if ((mode & 1) && !(S_ISDIR(ls.st_mode) || (ls.st_mode & 0111))) ok = 0;
            G_RET(c) = ok ? 0 : (uint64_t)(int64_t)(-EACCES);
            break;
        }
        {
            const char *gp =
                (g_rootfs && p && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
            if (gp48 && (!strncmp(gp48, "/proc/", 6) || !strncmp(gp48, "/dev/fd/", 8))) gp = gp48;
            char procfd_path[64];
            gp = procfd_namespace_path(gp, procfd_path, sizeof procfd_path);
            struct stat ss;
            if (synth_stat_raw(gp, &ss)) {
                int mode = (int)a2 & 7;
                int ok = 1;
                if ((mode & 4) && !(ss.st_mode & 0444)) ok = 0;
                if ((mode & 2) && !(ss.st_mode & 0222)) ok = 0;
                if ((mode & 1) && !(S_ISDIR(ss.st_mode) || (ss.st_mode & 0111))) ok = 0;
                G_RET(c) = ok ? 0 : (uint64_t)(int64_t)(-EACCES);
                break;
            }
        }
        // F_OK existence check: cacheable
        if (a2 == 0 && p) {
            int rc;
            if (!hl_fdcache_access_lookup(p, &rc)) {
                int r = faccessat(ATFD(a0), p, 0, 0);
                rc = r < 0 ? -errno : 0;
                hl_fdcache_access_store(p, rc);
            }
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        int r = faccessat(ATFD(a0), p, (int)a2, 0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: break;
    }
}

static int svc_fs_extended_status(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                  uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 291: svc_fs_extended_status_291(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 264: svc_fs_extended_status_264(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 265:
    case 439:
    case 48: svc_fs_extended_status_265(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
