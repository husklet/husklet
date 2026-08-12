/* Included by io.c: unity-build access with bounded I/O capability handlers. */

static int svc_lseek(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 62: {
        // lseek -- SEEK_SET/CUR/END(0/1/2) match. SEEK_DATA/SEEK_HOLE use host-native constants because
        // Darwin swaps their numeric values while Linux consumes the guest values directly.
        int whence = (int)a2;
        if ((int64_t)a1 < 0 && (whence == 3 || whence == 4)) {
            G_RET(c) = (uint64_t)(int64_t)(-ENXIO);
            break;
        }
        // Directory streams are read via getdents, backed by a private DIR* (fdopendir(dup(fd))) in the
        // plain path or a merged snapshot in the overlay path -- neither moves when the guest lseeks its
        // own fd. glibc rewinddir()/seekdir() ARE exactly this lseek, so redirect it here or the
        // enumeration never restarts (the readdir-dtype xfail: rewinddir's 2nd pass saw 0 entries).
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_nlower && g_ovldir[(int)a0][0]) {
            if (whence == 0 /*SEEK_SET*/) {
                ovldents_rewind((int)a0, (int)(off_t)a1);
                // The overlay replay cursor is mirrored in the real directory OFD so dup aliases and fork
                // peers share it.  Move that shared offset as well as this descriptor's local snapshot.
                off_t r = lseek((int)a0, (off_t)a1, SEEK_SET);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
                break;
            }
        }
        for (int i = 0; i < g_ndirs; i++)
            if (g_dirs[i].fd == (int)a0) {
                if (whence == 0 /*SEEK_SET*/) {
                    if ((off_t)a1 <= 0)
                        rewinddir(g_dirs[i].d);
                    else
                        seekdir(g_dirs[i].d, (long)(off_t)a1);
                    G_RET(c) = (uint64_t)a1;
                    goto lseek_out; // handled the directory stream
                }
                break; // SEEK_CUR/END on a dir stream: fall through to the raw lseek below
            }
        struct memf *mm = memf_get((int)a0);
        if (mm) {
            off_t mr = memf_lseek(mm, (off_t)a1, whence);
            if (mr != -2) {
                G_RET(c) = mr < 0 ? (uint64_t)(-EINVAL) : (uint64_t)mr;
                break;
            }
            memf_materialize((int)a0); // SEEK_DATA/HOLE: fall through to the now-materialized host fd
        }
        int guest_whence = whence;
        if (whence == 3)
            whence = HL_NATIVE_SEEK_DATA;
        else if (whence == 4)
            whence = HL_NATIVE_SEEK_HOLE;
        off_t r = lseek((int)a0, (off_t)a1, whence);
        int seek_error = errno;
        if (r < 0 && (guest_whence == 3 || guest_whence == 4) && errno != EBADF && errno != ESPIPE) {
            r = sparse_seek_fallback((int)a0, (off_t)a1, guest_whence);
            seek_error = errno;
            /* The fallback's only regular-file miss is "no requested extent before EOF". Preserve the
             * Linux ENXIO contract explicitly across the Darwin errno translation boundary. */
            if (r < 0 && (off_t)a1 >= 0 && seek_error != EBADF) seek_error = ENXIO;
            if (r >= 0 && lseek((int)a0, r, SEEK_SET) < 0) r = -1;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-seek_error) : (uint64_t)r;
    lseek_out:
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_pread64(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 67: {
        // pread64
        if (memf_get((int)a0)) {
            size_t accessible = guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE);
            if (a2 != 0 && accessible == 0) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            void *buffer = malloc(accessible == 0 ? 1 : accessible);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t r = memf_pread(g_memf[(int)a0], buffer, accessible, (off_t)a3);
            if (r > 0) {
                ssize_t copied = guest_copy_to(a1, buffer, (size_t)r);
                if (copied != r) r = copied > 0 ? copied : -EFAULT;
            }
            free(buffer);
            G_RET(c) = (uint64_t)r;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pread in place (see case 63)
        do {
            r = guest_fd_read((int)a0, a1, (size_t)a2, (off_t)a3, 1);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_pwrite64(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 68: {
        // pwrite64
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        // O_APPEND beats the explicit pwrite offset on Linux: generic_write_checks() moves ki_pos to i_size
        // for ANY write to an O_APPEND file, pwrite64 included, so pwrite(fd, b, n, 0) on an O_APPEND fd
        // APPENDS. The host-fd path below inherits that from the real host fd; the RAM-cached (memf) path
        // did not -- it honoured the caller's offset and overwrote the head of the file (silent data
        // corruption: "AAAA" + pwrite("B",0) became "BAAA" instead of "AAAAB"). Redirect the offset to EOF
        // for the memf path only, so nothing changes for plain host-fd pwrites (no extra syscall there).
        {
            struct memf *am = memf_get((int)a0);
            if (am) {
                int aflags = fcntl((int)a0, F_GETFL);
                if (aflags >= 0 && (aflags & O_APPEND)) a3 = (uint64_t)(off_t)am->size;
            }
        }
        if (memf_get((int)a0) && memf_room_or_spill((int)a0, (off_t)a3 + (off_t)a2)) {
            int64_t allowed = memf_fsize_gate(c, (off_t)a3, a2); // RLIMIT_FSIZE at the explicit pwrite offset
            if (allowed < 0) {
                G_RET(c) = (uint64_t)allowed;
                break;
            }
            void *buffer = malloc(allowed == 0 ? 1 : (size_t)allowed);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)allowed);
            if (allowed != 0 && copied <= 0) {
                free(buffer);
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            ssize_t r = memf_pwrite(g_memf[(int)a0], buffer, copied > 0 ? (size_t)copied : 0, (off_t)a3);
            free(buffer);
            G_RET(c) = (uint64_t)r;
            break;
        }
        hl_fdcache_fd_evict((int)a0);
        // RLIMIT_FSIZE: enforce at the explicit pwrite offset (a3), raising SIGXFSZ/EFBIG past the limit.
        int64_t pw_allowed = fsize_gate(c, (int)a0, (off_t)a3, a2);
        if (pw_allowed < 0) {
            G_RET(c) = (uint64_t)pw_allowed;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pwrite in place (see case 63)
        do {
            r = guest_fd_write((int)a0, a1, (size_t)pw_allowed, (off_t)a3, 1);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        if (r > 0) filemap_written((int)a0, a3, (uint64_t)r);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // sendfile(out,in,off*,count)
    default: return 0;
    }
    return svc_done(c);
}

static int svc_sendfile(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 71: {
        int outfd = (int)a0, infd = (int)a1;
        memf_materialize(outfd); // sendfile reads/writes via the real fds -> flush RAM cache first
        memf_materialize(infd);
        off_t offset_value = 0;
        off_t *po = a2 != 0 ? &offset_value : NULL;
        size_t cnt = (size_t)a3;
        // Linux runs the count through rw_verify_area(), which rejects a count that is negative when read
        // as ssize_t with -EINVAL. The engine looped on the raw size_t, so sendfile(out, in, NULL, SIZE_MAX)
        // copied until EOF and returned a 32-bit-truncated byte count instead of failing.
        if ((ssize_t)cnt < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // po (the in/out file offset) is read AND written directly -> validate before the copy loop so a bad
        // pointer returns -EFAULT instead of faulting the engine (and before any bytes move).
        if (po && guest_copy_from(po, a2, sizeof(*po)) != (ssize_t)sizeof(*po)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // With a non-NULL offset the input file position must NOT change: read from *po via pread and
        // report the advanced offset through *po only. A NULL offset reads from (and advances) infd's
        // own file position.
        off_t rpos = po ? *po : 0;
        char bf[65536];
        size_t tot = 0;
        int rerr = 0; // a read/write error hit with NOTHING transferred yet -> report -errno, not a fake 0
        while (tot < cnt) {
            size_t w = cnt - tot < sizeof bf ? cnt - tot : sizeof bf;
            ssize_t n = po ? pread(infd, bf, w, rpos) : read(infd, bf, w);
            if (n < 0) { // a mid-copy read error was previously swallowed as EOF -> silent truncation
                if (tot == 0) rerr = errno;
                break;
            }
            if (n == 0) break; // genuine EOF
            ssize_t wr = write(outfd, bf, n);
            if (wr < 0) {
                if (tot == 0) rerr = errno;
                break;
            }
            tot += wr;
            rpos += wr;
            if (wr < n) break;
        }
        // Linux: once ANY bytes were transferred, sendfile returns that count (a later error surfaces on the
        // next call); an error before the first byte returns -errno.
        if (po && guest_copy_to(a2, &rpos, sizeof(rpos)) != (ssize_t)sizeof(rpos)) {
            G_RET(c) = tot != 0 ? (uint64_t)tot : (uint64_t)(-EFAULT);
            break;
        }
        G_RET(c) = rerr ? (uint64_t)(-rerr) : (uint64_t)tot;
        break;
    }
    // vmsplice(fd, iov, nr_segs, flags): gather user memory INTO a pipe (write end) or scatter a pipe's
    // bytes back into user memory (read end). Direction follows the pipe fd's access mode, matching Linux.
    default: return 0;
    }
    return svc_done(c);
}

static int svc_tee(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 75: {
        int vfd = (int)a0;
        int niov = (int)a2;
        // SPLICE_F_MOVE(1)/NONBLOCK(2)/MORE(4)/GIFT(8) are the only defined bits; Linux rejects any other
        // bit with -EINVAL before touching the iovec. The engine ignored `flags` entirely.
        if (a3 & ~(uint64_t)0xf) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        memf_materialize(vfd);
        int fl = fcntl(vfd, F_GETFL);
        int to_pipe = (fl < 0) || ((fl & O_ACCMODE) != O_RDONLY); // write end -> user pages into the pipe
        hl_fdcache_fd_evict(vfd);
        ssize_t r = guest_fd_vector(vfd, a1, (size_t)niov, 0, 0, !to_pipe);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // splice(fd_in,off_in,fd_out,off_out,len,fl): move bytes between two fds (consumes the source).
    default: return 0;
    }
    return svc_done(c);
}

static int svc_splice(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 76: {
        int fin = (int)a0, fout = (int)a2;
        // SPLICE_F_MOVE(1)/NONBLOCK(2)/MORE(4)/GIFT(8) are the only defined bits -> any other bit is
        // -EINVAL (Linux do_splice() -> SPLICE_F_ALL check). And splice REQUIRES at least one pipe
        // endpoint: file->file is -EINVAL in Linux, but the engine happily read+wrote the bytes.
        if (a5 & ~(uint64_t)0xf) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        {
            struct stat si, so;
            int in_pipe = fstat(fin, &si) == 0 && S_ISFIFO(si.st_mode);
            int out_pipe = fstat(fout, &so) == 0 && S_ISFIFO(so.st_mode);
            if (!in_pipe && !out_pipe) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // An offset may only be supplied for the NON-pipe side; Linux -ESPIPEs a pipe with an offset.
            if ((in_pipe && a1) || (out_pipe && a3)) {
                G_RET(c) = (uint64_t)(int64_t)(-ESPIPE);
                break;
            }
        }
        // splice reads/writes the optional off_in (a1) / off_out (a3) pointers directly; validate them
        // before moving any bytes so a bad pointer returns -EFAULT instead of faulting the engine.
        off_t input_offset = 0, output_offset = 0;
        if (a1 && guest_copy_from(&input_offset, a1, sizeof(input_offset)) != (ssize_t)sizeof(input_offset)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a3 && guest_copy_from(&output_offset, a3, sizeof(output_offset)) != (ssize_t)sizeof(output_offset)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        memf_materialize(fin); // splice moves bytes via the real fds -> flush RAM cache first
        memf_materialize(fout);
        size_t len = (size_t)a4;
        if (len > 65536) len = 65536;
        static __thread char sb[65536];
        ssize_t n;
        hl_fdcache_fd_evict(fout);
        if (a1) {
            n = pread(fin, sb, len, input_offset);
        } else {
            // a pipe source may carry tee()'d pushback -> serve that first (splice consumes it).
            size_t pb = pipe_pushback_take(fin, sb, len);
            n = pb > 0 ? (ssize_t)pb : read(fin, sb, len);
        }
        if (n <= 0) {
            G_RET(c) = n < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        ssize_t w = a3 ? pwrite(fout, sb, n, output_offset) : write(fout, sb, n);
        if (w < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        input_offset += w;
        output_offset += w;
        if ((a1 && guest_copy_to(a1, &input_offset, sizeof(input_offset)) != (ssize_t)sizeof(input_offset)) ||
            (a3 && guest_copy_to(a3, &output_offset, sizeof(output_offset)) != (ssize_t)sizeof(output_offset))) {
            G_RET(c) = (uint64_t)w;
            break;
        }
        G_RET(c) = (uint64_t)w;
        break;
    }
    // tee(fd_in, fd_out, len, flags): duplicate up to `len` bytes between two pipes WITHOUT consuming the
    // source. macOS has no tee, so peek fd_in (drain then re-queue as read-pushback) and copy to fd_out.
    default: return 0;
    }
    return svc_done(c);
}

static int svc_ftruncate(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 77: {
        int fin = (int)a0, fout = (int)a1; // tee(fd_in, fd_out, len, flags) -- NOT the splice arg layout
        // Same SPLICE_F_* mask as splice/vmsplice, and tee needs BOTH ends to be pipes (Linux do_tee()
        // -EINVALs otherwise). The engine validated neither and silently returned 0 for a non-pipe fd.
        if (a3 & ~(uint64_t)0xf) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        {
            struct stat si, so;
            if (!(fstat(fin, &si) == 0 && S_ISFIFO(si.st_mode)) || !(fstat(fout, &so) == 0 && S_ISFIFO(so.st_mode))) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
        }
        memf_materialize(fin);
        memf_materialize(fout);
        size_t len = (size_t)a2;
        if (len > 65536) len = 65536;
        static __thread char sb[65536];
        hl_fdcache_fd_evict(fout);
        // front of the source stream = existing pushback ++ kernel-buffered bytes
        size_t oldlen = (fin >= 0 && fin < HL_NFD) ? g_fd_pb_len[fin] : 0;
        if (oldlen > sizeof sb) oldlen = sizeof sb;
        if (oldlen) memcpy(sb, g_fd_pushback[fin], oldlen);
        size_t pos = oldlen;
        ssize_t kn = 0;
        if (oldlen < len) {
            kn = read(fin, sb + oldlen, len - oldlen);
            if (kn > 0) pos += (size_t)kn;
        }
        if (pos == 0) {
            // Nothing buffered: distinguish EOF (write end closed -> tee returns 0, like Linux)
            // from an empty nonblocking pipe (read failed with EAGAIN -> propagate the errno).
            G_RET(c) = kn == 0 ? 0 : (uint64_t)(int64_t)(-errno);
            break;
        }
        size_t dup = pos < len ? pos : len;
        ssize_t w = write(fout, sb, dup);
        // tee never consumes the source: restore the whole peeked front as pushback.
        pipe_pushback_set(fin, sb, pos);
        G_RET(c) = w < 0 ? (uint64_t)(-errno) : (uint64_t)w;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}
