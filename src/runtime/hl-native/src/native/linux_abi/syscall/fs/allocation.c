static void svc_fs_allocation_46(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 46: {
        // RLIMIT_FSIZE: Linux (do_sys_ftruncate) rejects a truncation whose target length exceeds the soft
        // file-size limit -- it raises SIGXFSZ and returns -EFBIG -- before the filesystem is touched. The
        // generic check runs first (ahead of the memfd seal check below), so mirror that order here. No-op
        // when the limit is infinite (the common case) or the length fits.
        {
            uint64_t fslim = guest_fsize_cur();
            if (fslim != ~UINT64_C(0) && a1 > fslim) {
                raise_guest_signal(c, 25); // SIGXFSZ
                G_RET(c) = (uint64_t)(int64_t)(-EFBIG);
                break;
            }
        }
        // memfd sealing: F_SEAL_SHRINK(0x2) blocks a size-reducing ftruncate, F_SEAL_GROW(0x4) blocks a
        // size-increasing one -> EPERM (matching the write/pwrite F_SEAL_WRITE guards). A sealed shared
        // buffer must not be resized under a receiver (SIGBUS/OOB). Compare against the CURRENT size.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x6)) {
            off_t nlen = (off_t)a1, cur;
            int seals = memfd_seals_fd((int)a0);
            struct memf *sm = memf_get((int)a0);
            struct stat ss;
            if (sm)
                cur = (off_t)sm->size;
            else if (fstat((int)a0, &ss) == 0)
                cur = ss.st_size;
            else {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if ((nlen < cur && (seals & 0x2)) || (nlen > cur && (seals & 0x4))) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
        }
        // ftruncate on a RAM-backed scratch file (spill past the cap)
        if (memf_get((int)a0) && memf_room_or_spill((int)a0, (off_t)a1)) {
            struct memf *m = g_memf[(int)a0];
            off_t len = (off_t)a1;
            if (len < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if ((size_t)len > m->size) {
                if (memf_reserve(m, (size_t)len)) {
                    G_RET(c) = (uint64_t)(-ENOMEM);
                    break;
                }
                atomic_fetch_add(&g_memf_total, (uint64_t)len - m->size);
            } else {
                atomic_fetch_sub(&g_memf_total, m->size - (uint64_t)len);
                if ((size_t)len < m->cap) memset(m->buf + len, 0, m->size - (size_t)len); // re-zero shrunk tail
            }
            m->size = (size_t)len;
            G_RET(c) = 0;
            break;
        }
        struct stat before;
        int have_before = fstat((int)a0, &before) == 0;
        int bus_prepared = 0;
        if (have_before && a1 < (uint64_t)before.st_size) {
            gbus_prepare();
            bus_prepared = 1;
        }
        int r = ftruncate((int)a0, (off_t)a1);
        if (r == 0 && have_before) filemap_resize((int)a0, (uint64_t)before.st_size, a1);
        if (bus_prepared) gbus_prepare_release();
        hl_fdcache_fd_evict((int)a0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
        // ftruncate
    }
    default: break;
    }
}

// fallocate implements every Linux range mode, including portable emulation where the host lacks it.
// KEEP_SIZE=0x01, PUNCH_HOLE=0x02, COLLAPSE_RANGE=0x08, ZERO_RANGE=0x10, INSERT_RANGE=0x20,
// UNSHARE_RANGE=0x40. Seal and range validation order follows Linux's vfs_fallocate path.
static int fallocate_validate(struct cpu *c, int mode, off_t off, off_t len) {
    if (off < 0 || len <= 0) {
        G_RET(c) = (uint64_t)(-EINVAL);
        return 0;
    }
    if (mode & ~0x7b) {
        G_RET(c) = (uint64_t)(-EOPNOTSUPP);
        return 0;
    }
    if (((mode & 0x02) && !(mode & 0x01)) || ((mode & 0x08) && (mode & ~0x08)) || ((mode & 0x20) && (mode & ~0x20)) ||
        ((mode & 0x40) && (mode & ~(0x40 | 0x01)))) {
        G_RET(c) = (uint64_t)(-EINVAL);
        return 0;
    }
    if (off > (off_t)INT64_MAX - len) {
        G_RET(c) = (uint64_t)(-EFBIG);
        return 0;
    }
    if (!(mode & (0x02 | 0x08))) {
        uint64_t limit = guest_fsize_cur();
        if (limit != UINT64_MAX && (uint64_t)(off + len) > limit) {
            raise_guest_signal(c, 25);
            G_RET(c) = (uint64_t)(int64_t)(-EFBIG);
            return 0;
        }
    }
    return 1;
}

static void svc_fs_allocation_47(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 47: {
        int fd = (int)a0, mode = (int)a1;
        off_t off = (off_t)a2, len = (off_t)a3;
        if (!fallocate_validate(c, mode, off, len)) break;
        int seal = (fd >= 0 && fd < HL_NFD) ? memfd_seals_fd(fd) : 0;
        memf_materialize(fd); // flush any RAM cache; every branch below works on the real host fd
        struct stat s;
        if (fstat(fd, &s) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        off_t cur = s.st_size;
#if defined(__linux__)
        // Linux already provides the exact fallocate ABI, including filesystem-specific alignment,
        // sparse-file, seal, and range-mode behavior. Prefer it, but a container rootfs can live on a
        // backing filesystem (notably an OrbStack/macOS share) that rejects ZERO_RANGE even though a
        // Docker overlay presents that operation. The portable zero-fill below has the same observable
        // ZERO_RANGE contract, so use it only for that unsupported native operation.
        int native_result = fallocate(fd, mode, off, len), native_error = errno;
        if (native_result == 0 || !((mode & 0x10) && (native_error == EOPNOTSUPP || native_error == ENOSYS))) {
            hl_fdcache_fd_evict(fd);
            G_RET(c) = native_result < 0 ? (uint64_t)(-(int64_t)native_error) : 0;
            break;
        }
#endif
        char zb[65536];
        // ---- PUNCH_HOLE (keep size, range reads as zeros) ----
        if (mode & 0x02) {
            if (!(mode & 0x01)) { // PUNCH_HOLE requires KEEP_SIZE
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (seal & 0x8) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
#ifdef F_PUNCHHOLE
            struct fpunchhole fph;
            memset(&fph, 0, sizeof fph);
            fph.fp_offset = off;
            fph.fp_length = len;
            int r = fcntl(fd, F_PUNCHHOLE, &fph);
            // F_PUNCHHOLE needs block-aligned offset/length; on EINVAL fall back to a plain zero-fill of the
            // overlap with the file (reads-as-zero is the observable contract) rather than reporting failure.
            if (r < 0 && errno == EINVAL) {
                memset(zb, 0, sizeof zb);
                off_t e = off + len;
                if (e > cur) e = cur; // KEEP_SIZE: never extend
                int ok = 1;
                for (off_t p = off; p < e;) {
                    size_t w = (size_t)((e - p) < (off_t)sizeof zb ? (e - p) : (off_t)sizeof zb);
                    ssize_t k = pwrite(fd, zb, w, p);
                    if (k < 0) {
                        ok = 0;
                        break;
                    }
                    p += k;
                }
                r = ok ? 0 : -1;
            }
            hl_fdcache_fd_evict(fd);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
#else
            G_RET(c) = (uint64_t)(-EOPNOTSUPP);
#endif
            break;
        }
        // ---- ZERO_RANGE (zero the range; extend the file to cover it unless KEEP_SIZE) ----
        if (mode & 0x10) {
            off_t end = off + len;
            if (seal & 0x8) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            if (!(mode & 0x01) && end > cur && (seal & 0x4)) { // would grow, GROW-sealed
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            if (!(mode & 0x01) && end > cur && ftruncate(fd, end) < 0) { // grow first (do NOT swallow ENOSPC)
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            off_t ze = (mode & 0x01) ? (end < cur ? end : cur) : end; // KEEP_SIZE: only zero within old EOF
            memset(zb, 0, sizeof zb);
            for (off_t p = off; p < ze;) {
                size_t w = (size_t)((ze - p) < (off_t)sizeof zb ? (ze - p) : (off_t)sizeof zb);
                ssize_t k = pwrite(fd, zb, w, p);
                if (k < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    goto fallocate_done;
                }
                p += k;
            }
            hl_fdcache_fd_evict(fd);
            G_RET(c) = 0;
            break;
        }
        // ---- COLLAPSE_RANGE (remove [off,off+len) and shift the tail down; file shrinks by len) ----
        if (mode & 0x08) {
            off_t end = off + len;
            if (end >= cur) { // Linux: offset+len must be strictly inside the file
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (seal & (0x2 | 0x8)) { // shrinks + moves data
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            // Copy the tail forward (dst < src, forward scan is safe) then truncate.
            for (off_t rp = end, wp = off; rp < cur;) {
                size_t w = (size_t)((cur - rp) < (off_t)sizeof zb ? (cur - rp) : (off_t)sizeof zb);
                ssize_t k = pread(fd, zb, w, rp);
                if (k <= 0) {
                    G_RET(c) = (uint64_t)(k < 0 ? -errno : -EIO);
                    goto fallocate_done;
                }
                ssize_t wk = pwrite(fd, zb, (size_t)k, wp);
                if (wk < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    goto fallocate_done;
                }
                rp += k;
                wp += wk;
            }
            if (ftruncate(fd, cur - len) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            hl_fdcache_fd_evict(fd);
            G_RET(c) = 0;
            break;
        }
        // ---- INSERT_RANGE (insert `len` zero bytes at off; existing tail shifts up; file grows by len) ----
        if (mode & 0x20) {
            if (off >= cur) { // Linux: offset must be strictly inside the file (else use plain fallocate)
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (seal & (0x4 | 0x8)) { // grows + moves data
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            if (ftruncate(fd, cur + len) < 0) { // grow to the new size (do NOT swallow ENOSPC)
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // Move the tail UP: copy backward (from the end) so the grown region isn't overwritten early.
            off_t remain = cur - off;
            for (off_t done = 0; done < remain;) {
                size_t w = (size_t)((remain - done) < (off_t)sizeof zb ? (remain - done) : (off_t)sizeof zb);
                off_t rp = cur - done - (off_t)w, wp = rp + len;
                ssize_t k = pread(fd, zb, w, rp);
                if (k <= 0) {
                    G_RET(c) = (uint64_t)(k < 0 ? -errno : -EIO);
                    goto fallocate_done;
                }
                ssize_t wk = pwrite(fd, zb, (size_t)k, wp);
                if (wk < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    goto fallocate_done;
                }
                done += k;
            }
            // Zero the freshly inserted gap.
            memset(zb, 0, sizeof zb);
            for (off_t p = off, e = off + len; p < e;) {
                size_t w = (size_t)((e - p) < (off_t)sizeof zb ? (e - p) : (off_t)sizeof zb);
                ssize_t k = pwrite(fd, zb, w, p);
                if (k < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    goto fallocate_done;
                }
                p += k;
            }
            hl_fdcache_fd_evict(fd);
            G_RET(c) = 0;
            break;
        }
        // ---- plain fallocate (mode 0 / KEEP_SIZE / UNSHARE_RANGE): reserve space; extend unless KEEP_SIZE.
        {
            off_t end = off + len;
            if (end > cur) {
                if (seal & 0x4) { // GROW-sealed
                    G_RET(c) = (uint64_t)(-EPERM);
                    break;
                }
                if (!(mode & 0x01) && ftruncate(fd, end) < 0) { // extend; surface ENOSPC (was swallowed)
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
            }
            hl_fdcache_fd_evict(fd);
            G_RET(c) = 0;
        }
        break;
    fallocate_done:
        hl_fdcache_fd_evict(fd);
        break;
    }
    default: break;
    }
}

static int svc_fs_allocation(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 46: svc_fs_allocation_46(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    case 47: svc_fs_allocation_47(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
