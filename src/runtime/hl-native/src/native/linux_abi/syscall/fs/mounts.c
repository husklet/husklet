static void svc_fs_mounts_40(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 40: G_RET(c) = (uint64_t)svc_mount(c, a0, a1, a2, a3); break;
    // umount2(target,flags): detach a runtime bind/tmpfs volume mounted exactly there. A pseudo-mount hl
    // keeps serving (not a registered volume) stays present -> success (unmounting it is a harmless no-op
    // in hl's model; the content is synthetic, not backed by the removed mount).
    case 39: {
        if (!g_rootfs) {
            G_RET(c) = 0;
            break;
        }
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        char utgt[4200];
        guest_abspath_at(-100, (const char *)a0, utgt, sizeof utgt);
        rt_del_vol(utgt); // -EINVAL if not a registered volume; treated as a no-op success below
        G_RET(c) = 0;
        break;
    }
    // pivot_root(new_root,put_old): re-root the guest at new_root, confined within the rootfs jail (modeled
    // as a chroot -- hl has one root fd; put_old is not separately materialized). Validate new_root exists
    // as a directory so a bad target reports ENOENT/ENOTDIR instead of a fake success.
    default: break;
    }
}

static void svc_fs_mounts_41(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    // mount(source,target,fstype,flags,data): implement bind/tmpfs/remount,ro against hl's vfs (svc_mount);
    // a real no-op stub silently gave wrong content + unenforced RO.
    case 41: {
        if (!g_rootfs) {
            G_RET(c) = 0;
            break;
        }
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        char nrabs[4200], nrhost[4200];
        guest_abspath_at(-100, (const char *)a0, nrabs, sizeof nrabs);
        secure_resolve(nrabs, nrhost, sizeof nrhost, 0);
        struct stat nst;
        if (stat(nrhost, &nst) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        if (!S_ISDIR(nst.st_mode)) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOTDIR);
            break;
        }
        char nc[4200];
        chroot_apply(nrabs, nc, sizeof nc);
        snprintf(g_chroot, sizeof g_chroot, "%s", nc[1] ? nc : "");
        hl_fdcache_reset();
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static void svc_fs_mounts_43(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 43:
    case 44: {
        // statfs(path,buf)/fstatfs(fd,buf): wrap the host call, then TRANSLATE the macOS struct statfs
        // into the Linux struct statfs layout (all 8-byte fields on 64-bit; f_fsid is two 32-bit words).
        struct statfs hs;
        int r;
        char gpath[4200];
        gpath[0] = 0; // guest ABSOLUTE path (container mode) -> pseudo-fs classification
        if (nr == 43) {
            // A path pointer outside the address space -> EFAULT (kernel getname copy_from_user), before
            // the buffer is examined (LTP statfs02 "bad path"). svc_fs's import reaches that verdict, and a
            // guest PROT_NONE page still faults there.
            /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
             * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
             * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
             * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
             * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
             * released guest range is re-added by munmap and the host allocator later places the
             * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
            char pb[4200];
            const char *p = atpath(-100, (const char *)a0, pb, sizeof pb, 0);
            guest_abspath_at(-100, (const char *)a0, gpath, sizeof gpath); // guest-absolute path (both modes)
            r = statfs(p, &hs);
            // A SYNTHETIC proc/sys/cgroup leaf (its content is served by the /proc·/sys synth, not the image)
            // has no host file to statfs -> ENOENT. But Linux reports the pseudo-fs magic + geometry for these
            // paths, and tools (UseContainerSupport, magic-based pseudo-fs detection) rely on it. If hl
            // synthesizes the path, adopt the rootfs-root geometry (container mode) or a zeroed pseudo geometry
            // (bare mode), and let the classification below stamp the magic + zero the block/inode counts.
            if (r < 0 && gpath[0] && (!strncmp(gpath, "/proc", 5) || !strncmp(gpath, "/sys", 4))) {
                struct stat stx;
                int is_synth = synth_stat_raw(gpath, &stx) || !strcmp(gpath, "/sys/fs/cgroup") ||
                               !strncmp(gpath, "/sys/fs/cgroup/", 15) || !strcmp(gpath, "/proc") ||
                               !strcmp(gpath, "/sys");
                if (is_synth) {
                    if (g_rootfs) {
                        char rb[4200];
                        const char *rroot = atpath(-100, "/", rb, sizeof rb, 0);
                        r = statfs(rroot, &hs);
                    }
                    if (r < 0) { // bare mode (no rootfs root to borrow): a pseudo-fs geometry
                        memset(&hs, 0, sizeof hs);
                        hs.f_bsize = 4096;
                        r = 0;
                    }
                }
            }
        } else {
            r = fstatfs((int)a0, &hs);
            if (g_rootfs && (int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0] &&
                guest_from_host(g_fdpath[(int)a0], gpath, sizeof gpath) <= 0)
                gpath[0] = 0;
        }
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        uint8_t encoded_statfs[HL_LINUX_STATFS_RECORD_SIZE];
        uint8_t *b = encoded_statfs;
        // The result buffer must be writable -> EFAULT on a bad/unmapped/PROT_NONE pointer (LTP statfs02
        // "bad buf"; the engine fills this buffer itself, so guard before the writes below).
        if (guest_accessible_prefix(a1, sizeof encoded_statfs, HL_LOGICAL_VMA_WRITE) != sizeof encoded_statfs) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // f_type + pseudo-fs geometry: in a container classify by the guest mount (overlay/proc/sysfs/
        // cgroup2/tmpfs/devpts/mqueue); a pseudo-fs (proc/sysfs/cgroup2) reports ZERO blocks/inodes so df
        // hides it and stat -f names it correctly. Bare (no rootfs) keeps the legacy tmpfs magic + host geo.
        int64_t f_type = 0x01021994;
        int pseudo_zero = 0;
        // Classify by the guest mount. In container mode every path is classified (as before); in bare mode
        // only the SYNTHETIC pseudo/dev trees are (a real host file keeps its host-statfs magic -- no regression).
        int classify = gpath[0] && (g_rootfs || !strncmp(gpath, "/proc", 5) || !strncmp(gpath, "/sys", 4) ||
                                    !strncmp(gpath, "/dev", 4));
        if (classify) f_type = guest_statfs_magic(gpath, &pseudo_zero);
        uint64_t blocks = pseudo_zero ? 0 : (uint64_t)hs.f_blocks;
        uint64_t bfree = pseudo_zero ? 0 : (uint64_t)hs.f_bfree;
        uint64_t bavail = pseudo_zero ? 0 : (uint64_t)hs.f_bavail;
        uint64_t files = pseudo_zero ? 0 : (uint64_t)hs.f_files;
        uint64_t ffree = pseudo_zero ? 0 : (uint64_t)hs.f_ffree;
        uint32_t fsid0, fsid1;
#if defined(__linux__)
        fsid0 = (uint32_t)hs.f_fsid.__val[0];
        fsid1 = (uint32_t)hs.f_fsid.__val[1];
#else
        fsid0 = (uint32_t)hs.f_fsid.val[0];
        fsid1 = (uint32_t)hs.f_fsid.val[1];
#endif
        // f_flags: Linux exposes the mount flags (ST_VALID + mount options). hl's mounts are all relatime;
        // the pseudo-fs + tmpfs mounts (/proc /sys /dev /dev/shm) are nosuid,nodev,noexec (per mountinfo).
        // Reporting 0 made ST_NOSUID/NODEV/NOEXEC/RDONLY probes see a false mount view.
        int64_t f_flags = 0;
        if (classify) {
            f_flags = 0x0020 | 0x1000; // ST_VALID | ST_RELATIME
            if (!strncmp(gpath, "/proc", 5) || !strncmp(gpath, "/sys", 4) || !strncmp(gpath, "/dev", 4))
                f_flags |= 0x0002 | 0x0004 | 0x0008; // ST_NOSUID | ST_NODEV | ST_NOEXEC
        }
        const hl_linux_statfs_record record = {
            .type = f_type,
            .block_size = (uint64_t)hs.f_bsize,
            .blocks = blocks,
            .blocks_free = bfree,
            .blocks_available = bavail,
            .files = files,
            .files_free = ffree,
            .filesystem_id = {fsid0, fsid1},
            .name_max = 255,
            .fragment_size = (uint64_t)hs.f_bsize,
            .flags = (uint64_t)f_flags,
        };
        (void)hl_linux_statfs_encode(&record, b, HL_LINUX_STATFS_RECORD_SIZE);
        if (guest_copy_to(a1, encoded_statfs, sizeof encoded_statfs) != sizeof encoded_statfs) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    default: break;
    }
}

static int svc_fs_mounts(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                         uint64_t a5) {
    switch (nr) {
    case 40:
    case 39: svc_fs_mounts_40(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 41: svc_fs_mounts_41(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 43:
    case 44: svc_fs_mounts_43(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
