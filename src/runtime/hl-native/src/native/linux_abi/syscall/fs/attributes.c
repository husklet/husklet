static void svc_fs_attributes_5(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 5:
    case 6:
    case 7: {
        char host[4300];
        int e;
        // EFAULT before any deref: the path (a0, non-fd forms) is walked by abs_guest and the name (a1) is
        // copied by snprintf in guest_xattr_set -- a wild guest pointer to either would fault the engine
        // (SIGSEGV) instead of returning the kernel's EFAULT. The value (a2) is validated by the host set.
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        if (nr == 7)
            e = hl_native_fd_path((int)a0, host, sizeof host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 6, 1, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) =
            (uint64_t)(int64_t)guest_xattr_set(host, (const char *)a1, (const void *)a2, (size_t)a3, a4, nr == 6);
        break;
    }
    // getxattr(8)/lgetxattr(9)/fgetxattr(10): a0=path|fd, a1=name, a2=val, a3=size
    default: break;
    }
}

static void svc_fs_attributes_8(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    switch (nr) {
    // ===================== Filesystem — open/stat/dir/link/perm/xattr/cwd, all path-confined to the rootfs jail
    // =====================
    // setxattr(5)/lsetxattr(6)/fsetxattr(7): a0=path|fd, a1=name, a2=val, a3=size, a4=flags
    case 8:
    case 9:
    case 10: {
        char host[4300];
        int e;
        // EFAULT before deref: path (a0, non-fd) walked by abs_guest, name (a1) copied by snprintf. The
        // value out-buffer (a2) is validated by the host get.
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        if (nr == 10)
            e = hl_native_fd_path((int)a0, host, sizeof host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 9, 0, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        {
            // Reading a guest xattr needs host read permission on the backing inode. The virtual DAC
            // decides first (guest root's CAP_DAC_OVERRIDE grants); only then are the host owner bits
            // lent for the duration of the read. Without this `ls -l` on a guest `chmod 000` file warns
            // "Permission denied" where a native root sees none.
            hl_dac_host_grant grant;
            int authorized = (nr == 10 ? dac_access_fd((int)a0, R_OK, 1)
                                       : dac_access_at(-100, (const char *)a0, nr == 9, R_OK, 1)) == 0;
            dac_host_grant_begin_path(&grant, host, HL_DAC_READ, authorized);
            G_RET(c) = (uint64_t)(int64_t)guest_xattr_get(host, (const char *)a1, (void *)a2, (size_t)a3,
                                                          nr == 9 ? XATTR_NOFOLLOW : 0);
            dac_host_grant_end(&grant);
        }
        break;
    }
    // listxattr(11)/llistxattr(12)/flistxattr(13): a0=path|fd, a1=list, a2=size
    default: break;
    }
}

static void svc_fs_attributes_11(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 11:
    case 12:
    case 13: {
        char host[4300];
        int e;
        // EFAULT before deref: path (a0, non-fd) walked by abs_guest. The list out-buffer (a1) is validated
        // inside guest_xattr_list, which mirrors the kernel's order (ERANGE/length-query before any copy) and
        // only faults when bytes are actually written -- so an empty list with a bad buffer succeeds, as on Linux.
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        if (nr == 13)
            e = hl_native_fd_path((int)a0, host, sizeof host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 12, 0, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        {
            hl_dac_host_grant grant;
            int authorized = (nr == 13 ? dac_access_fd((int)a0, R_OK, 1)
                                       : dac_access_at(-100, (const char *)a0, nr == 12, R_OK, 1)) == 0;
            dac_host_grant_begin_path(&grant, host, HL_DAC_READ, authorized);
            G_RET(c) =
                (uint64_t)(int64_t)guest_xattr_list(host, (char *)a1, (size_t)a2, nr == 12 ? XATTR_NOFOLLOW : 0);
            dac_host_grant_end(&grant);
        }
        break;
    }
    // removexattr(14)/lremovexattr(15)/fremovexattr(16): a0=path|fd, a1=name
    default: break;
    }
}

static void svc_fs_attributes_14(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 14:
    case 15:
    case 16: {
        char host[4300];
        int e;
        // EFAULT before deref: path (a0, non-fd) walked by abs_guest, name (a1) copied by snprintf in
        // guest_xattr_remove.
        /* No guest-pointer guard here: svc_fs (fs.c) imported this pathname operand into engine
         * storage with guest_copy_string BEFORE dispatch, and returned the guest's own -EFAULT
         * (NULL or inaccessible source) / -ENAMETOOLONG there, against the GUEST address and the
         * same PROT_NONE ledger.  What arrives here is an engine C-stack buffer, so re-probing it
         * asks the guest ledger about ENGINE memory -- and g_gna does cover engine storage (a
         * released guest range is re-added by munmap and the host allocator later places the
         * engine's own thread stacks there), which turned valid calls into -EFAULT.  */
        if (nr == 16)
            e = hl_native_fd_path((int)a0, host, sizeof host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 15, 1, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)guest_xattr_remove(host, (const char *)a1, nr == 15 ? XATTR_NOFOLLOW : 0);
        break;
    }
    default: break;
    }
}

static void svc_fs_attributes_17(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 17: {
        // getcwd(BUF, size). Resolve the guest cwd into an ENGINE-local buffer first, then apply the exact
        // kernel order (fs/dcache.c SYSCALL_DEFINE2(getcwd)): the path length is compared to `size` BEFORE any
        // copy_to_user, so a too-small buffer is -ERANGE regardless of BUF's validity, and only when the path
        // FITS does the copy run -> -EFAULT on a NULL/bad BUF. The old code passed the guest BUF straight to
        // the host getcwd(BUF,size): a NULL/huge-size probe (LTP getcwd01 case 2: buf=NULL,size=(size_t)-1)
        // made libc getcwd write through NULL -> SIGSEGV in the engine instead of returning EFAULT.
        char cwbuf[4200], cwguest[sizeof cwbuf + sizeof g_vols[0].guest];
        const char *cw;
        if (g_rootfs) {
            cw = g_cwd[0] ? g_cwd : "/"; // the GUEST cwd (not the host path)
        } else {
            // Bare mode: the engine chdir()s for real, so the live host cwd IS the guest cwd -- EXCEPT
            // inside a mapped volume, where the host cwd is the volume's backing directory. Translate
            // through the volume table there; outside every volume bare mode is identity and cwbuf stands.
            if (!getcwd(cwbuf, sizeof cwbuf)) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int mapped = guest_from_host_volume(cwbuf, cwguest, sizeof cwguest);
            if (mapped < 0) {
                G_RET(c) = (uint64_t)(int64_t)mapped;
                break;
            }
            cw = mapped > 0 ? cwguest : cwbuf;
        }
        size_t len = strlen(cw) + 1; // path length INCLUDING the terminating NUL, exactly like the kernel
        if (len > (size_t)a1) {
            G_RET(c) = (uint64_t)(-ERANGE);
            break;
        }
        if (!a0 || guest_copy_to(a0, cw, len) != (ssize_t)len) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = len;
        break;
    }
    // ioctl(fd, req, arg) -- Linux req# -> macOS
    default: break;
    }
}

static int svc_fs_attributes(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 5:
    case 6:
    case 7: svc_fs_attributes_5(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 8:
    case 9:
    case 10: svc_fs_attributes_8(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 11:
    case 12:
    case 13: svc_fs_attributes_11(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 14:
    case 15:
    case 16: svc_fs_attributes_14(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 17: svc_fs_attributes_17(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
