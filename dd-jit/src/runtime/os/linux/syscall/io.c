// Extracted from service(): I/O — fd read/write/seek + plain fd ops (dup/dup3/fcntl/pipe2/sendfile/splice/tee/copy_file_range/fsync/etc).
// Returns 1 if nr was handled, 0 otherwise. Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).

// Guest O_DIRECT differs per arch (aarch64/asm-generic = 0x10000, x86-64 = 0x4000); derive it from the
// arch's O_DIRECTORY (provided by abi.h) so pipe2(O_DIRECT) is recognised on both targets.
#if G_O_DIRECTORY == 0x10000
#define G_O_DIRECT 0x4000 // x86-64
#else
#define G_O_DIRECT 0x10000 // aarch64 / asm-generic
#endif

// In dd's in-process exec model the guest shares the host descriptor table (fds are 1:1), and the engine
// pins private host fds at LOW numbers (g_root_fd -- every path resolution openat()s off it -- plus the
// timer kqueue, the signalfd pipe, and each bind-mount volume fd). A guest dup2/dup3 onto one of those low
// numbers (e.g. BEAM's erl_child_setup does dup3(controlpipe, 3), landing on g_root_fd) would silently
// clobber the engine's fd. engine_fd_vacate() relocates any engine-private fd sitting on the about-to-be-
// reused target to a fresh high descriptor first, so the guest still gets the exact fd it asked for while
// the runtime keeps a valid one. (Mirrors exec_fd_is_engine()'s skip-list used by the execve CLOEXEC sweep.)
static void engine_fd_reloc(int *slot, int newfd) {
    if (!slot || *slot != newfd || newfd < 0) return;
    // F_DUPFD returns the lowest free fd >= the floor; a very high floor keeps the engine fd clear of the
    // guest's active low fds, and the modest fallback keeps the relocation working under a small RLIMIT_NOFILE.
    int hi = fcntl(newfd, F_DUPFD, 1 << 20);
    if (hi < 0) hi = fcntl(newfd, F_DUPFD, 64);
    if (hi >= 0) *slot = hi;
}
static void engine_fd_vacate(int newfd) {
    if (newfd < 0) return;
    engine_fd_reloc(&g_root_fd, newfd);
    engine_fd_reloc(&g_gtimer_kq, newfd);
    engine_fd_reloc(&g_sigfd_pipe[0], newfd);
    engine_fd_reloc(&g_sigfd_pipe[1], newfd);
    for (int i = 0; i < g_nvols; i++)
        engine_fd_reloc(&g_vols[i].fd, newfd);
}
// Vacate every engine-private fd whose NUMBER falls in [first,last] -- for a guest close_range() that would
// otherwise close the runtime's descriptors (g_root_fd etc.). Visible to fs.c/rare.c (io.c is #included first).
static void engine_fd_vacate_range(unsigned first, unsigned last) {
    int fds[4] = {g_root_fd, g_gtimer_kq, g_sigfd_pipe[0], g_sigfd_pipe[1]};
    for (int i = 0; i < 4; i++)
        if (fds[i] >= 0 && (unsigned)fds[i] >= first && (unsigned)fds[i] <= last) engine_fd_vacate(fds[i]);
    for (int i = 0; i < g_nvols; i++)
        if (g_vols[i].fd >= 0 && (unsigned)g_vols[i].fd >= first && (unsigned)g_vols[i].fd <= last)
            engine_fd_vacate(g_vols[i].fd);
}

static int svc_io(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    // An O_PATH fd names a file but is not open for I/O -- Linux rejects the read/write family through it
    // with EBADF (fs/read_write.c). It stays valid as a dirfd for *at() and for fstat/fchdir (served by
    // svc_fs), so only the I/O syscalls are gated here.
    if ((int)a0 >= 0 && (int)a0 < 1024 && g_opath[(int)a0]) {
        switch (nr) {
        case 63: case 64: case 65: case 66: case 67: case 68: case 69: case 70:
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            return svc_done(c);
        default: break;
        }
    }
    // /dev/full: any write fails ENOSPC (reads are served from the /dev/zero backing). Installers and
    // test suites probe this to check out-of-space handling.
    if ((int)a0 >= 0 && (int)a0 < 1024 && g_devfull[(int)a0]) {
        switch (nr) {
        case 64: case 66: case 68: case 70:
            G_RET(c) = (uint64_t)(int64_t)(-ENOSPC);
            return svc_done(c);
        default: break;
        }
    }
    // Guest PROT_NONE buffer in the fd-I/O family (fd, BUF=a1, count=a2): dd force-maps guest anon pages
    // host-writable (mem.c case 222) so the host read/write does NOT fault on a guest PROT_NONE page the way
    // Linux's copy_{to,from}_user would. Reject it here with -EFAULT, exactly as Linux. Near-free when no
    // PROT_NONE region exists (g_npn==0). read/pread WRITE the buffer, write/pwrite READ it; both fault. (read02)
    if (g_npn) {
        switch (nr) {
        case 63: case 64: case 67: case 68: // read / write / pread64 / pwrite64
            if (pn_hit(a1, a2)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); return svc_done(c); }
            break;
        default: break;
        }
    }
    switch (nr) {
    // ===================== I/O — read/write/seek (+ eventfd/timerfd/signalfd fd redirection) =====================
    case 62: {
        // lseek -- SEEK_SET/CUR/END(0/1/2) match, but SEEK_DATA/SEEK_HOLE are SWAPPED between the ABIs:
        // Linux SEEK_DATA=3,SEEK_HOLE=4 ; macOS SEEK_HOLE=3,SEEK_DATA=4. Translate so sparse-file
        // probing finds holes/data correctly.
        int whence = (int)a2;
        // Directory streams are read via getdents, backed by a private DIR* (fdopendir(dup(fd))) in the
        // plain path or a merged snapshot in the overlay path -- neither moves when the guest lseeks its
        // own fd. glibc rewinddir()/seekdir() ARE exactly this lseek, so redirect it here or the
        // enumeration never restarts (the readdir-dtype xfail: rewinddir's 2nd pass saw 0 entries).
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_nlower && g_ovldir[(int)a0][0]) {
            if (whence == 0 /*SEEK_SET*/) {
                ovldents_rewind((int)a0, (int)(off_t)a1);
                G_RET(c) = (uint64_t)a1;
                break;
            }
        }
        for (int i = 0; i < g_ndirs; i++)
            if (g_dirs[i].fd == (int)a0) {
                if (whence == 0 /*SEEK_SET*/) {
                    if ((off_t)a1 <= 0) rewinddir(g_dirs[i].d);
                    else seekdir(g_dirs[i].d, (long)(off_t)a1);
                    G_RET(c) = (uint64_t)a1;
                    goto lseek_out; // handled the directory stream
                }
                break; // SEEK_CUR/END on a dir stream: fall through to the raw lseek below
            }
        struct memf *mm = memf_get((int)a0);
        if (mm) {
            off_t mr = memf_lseek(mm, (off_t)a1, whence);
            if (mr != -2) { G_RET(c) = mr < 0 ? (uint64_t)(-EINVAL) : (uint64_t)mr; break; }
            memf_materialize((int)a0); // SEEK_DATA/HOLE: fall through to the now-materialized host fd
        }
        if (whence == 3) whence = 4;      // Linux SEEK_DATA -> macOS SEEK_DATA
        else if (whence == 4) whence = 3; // Linux SEEK_HOLE -> macOS SEEK_HOLE
        off_t r = lseek((int)a0, (off_t)a1, whence);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
    lseek_out:
        break;
    }
    case 63: {
        int rfd = (int)a0;
        // tee(2) pushback: bytes a prior tee() peeked out of this pipe are re-served here first, in order.
        if (rfd >= 0 && rfd < 1024 && g_fd_pb_len[rfd]) {
            G_RET(c) = (uint64_t)pipe_pushback_take(rfd, (void *)a1, (size_t)a2);
            break;
        }
        // AF_NETLINK socket read (#293): busybox `ip` receives its RTNETLINK dump with read(2)/recvmsg;
        // drain our queued reply with the Linux MSG_PEEK/MSG_TRUNC semantics (see netns.c nl_recv).
        if (nl_is(rfd)) {
            struct iovec iov = {(void *)a1, (size_t)a2};
            G_RET(c) = (uint64_t)nl_recv(rfd, &iov, 1, 0, NULL);
            break;
        }
        // RAM-backed scratch file: serve the read from memory. Unlike a host-fd read (whose kernel copyout
        // faults a bad buffer to EFAULT), this copies straight into the guest buffer, so a bad/unmapped
        // pointer must be validated here or the engine memcpy faults (#395, access_ok).
        if (memf_get(rfd)) {
            if (a2 && !host_range_mapped((uintptr_t)a1, (size_t)a2)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            ssize_t r = memf_read_pos(g_memf[rfd], (void *)a1, (size_t)a2);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        // signalfd read -> struct signalfd_siginfo
        if (rfd >= 0 && rfd == g_sigfd_read) {
            char b;
            // drain one wake byte
            ssize_t pr = read(rfd, &b, 1);
            if (pr <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(pr < 0 ? -errno : -EAGAIN);
                break;
            }
            // Each wake byte IS the queued signal number (host_sigh / raise_guest_signal write (char)signo),
            // so realtime signals delivered N times read back as N siginfo records each carrying the right
            // ssi_signo -- unlike the single-bit g_pending, which cannot represent a queue of the same signo.
            int sig = (unsigned char)b;
            if (sig > 0 && sig < 64) __atomic_and_fetch(&g_pending, ~(1ull << (unsigned)sig), __ATOMIC_SEQ_CST);
            if (a1 && a2 >= 128) {
                // a1 is a raw guest buffer we write directly -> EFAULT a bad pointer instead of faulting the engine
                if (!host_range_mapped((uintptr_t)a1, 128)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
                memset((void *)a1, 0, 128);
                *(uint32_t *)a1 = (uint32_t)sig;
            // ssi_signo
            }
            G_RET(c) = 128;
            break;
        }
        // inotify read -> struct inotify_event[]
        if (rfd >= 0 && rfd < 1024 && g_inotify[rfd]) {
            // The whole [a1, a1+a2) buffer is written directly by the engine below; validate it up front so a
            // bad/unmapped pointer returns -EFAULT (without consuming events) instead of faulting the engine.
            if (!host_range_mapped((uintptr_t)a1, (size_t)a2)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            uint8_t *out = (uint8_t *)a1;
            size_t off = 0;
            // First drain any queued rename events (IN_MOVED_FROM/IN_MOVED_TO) for this instance; the
            // snapshot diff below can only synthesize IN_CREATE/IN_DELETE, not paired moves.
            off += inomv_drain(rfd, out, (size_t)a2);
            struct kevent kv[32];
            struct timespec zero = {0, 0};
            int nb = fcntl(rfd, F_GETFL) & O_NONBLOCK;
            // If we already produced move events, poll the kqueue non-blocking so we never wait behind them.
            int n = kevent(rfd, NULL, 0, kv, 32, (nb || off > 0) ? &zero : NULL);
            if (n <= 0) {
                if (off > 0) { G_RET(c) = (uint64_t)off; break; } // return the moves we already have
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                break;
            }
            for (int i = 0; i < n; i++) {
                int wd = (int)kv[i].ident;
                if (wd >= 0 && wd < 1024 && g_inotify_wpath[wd][0]) {
                    // directory watch: diff current entries against the snapshot -> IN_CREATE/IN_DELETE+name
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) {            // pass 0 = created, pass 1 = deleted
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u;  // IN_CREATE / IN_DELETE
                        for (const char *p = src ? src : ""; *p;) {
                            const char *e = strchr(p, '\n');
                            size_t l = e ? (size_t)(e - p) : strlen(p);
                            if (l && !snap_has(other, p, l)) {
                                size_t nlen = (l + 1 + 15) & ~(size_t)15; // padded name field
                                if (off + 16 + nlen > a2) break;
                                *(int32_t *)(out + off) = wd;
                                *(uint32_t *)(out + off + 4) = mask;
                                *(uint32_t *)(out + off + 8) = 0;               // cookie
                                *(uint32_t *)(out + off + 12) = (uint32_t)nlen; // len
                                memcpy(out + off + 16, p, l);
                                memset(out + off + 16 + l, 0, nlen - l);
                                off += 16 + nlen;
                            }
                            p = e ? e + 1 : p + l;
                        }
                    }
                    free(old);
                    g_inotify_snap[wd] = cur;
                } else {
                    if (off + 16 > a2) break;
                    uint32_t f = kv[i].fflags, m = 0;
                    if (f & (NOTE_WRITE | NOTE_EXTEND)) m |= 0x2;  // IN_MODIFY
                    if (f & NOTE_ATTRIB) m |= 0x4;                 // IN_ATTRIB
                    if (f & NOTE_DELETE) m |= 0x400;               // IN_DELETE_SELF
                    if (f & NOTE_RENAME) m |= 0x800;               // IN_MOVE_SELF
                    *(int32_t *)(out + off) = wd;
                    *(uint32_t *)(out + off + 4) = m;
                    *(uint32_t *)(out + off + 8) = 0;
                    *(uint32_t *)(out + off + 12) = 0;
                    off += 16;
                }
            }
            G_RET(c) = (uint64_t)off;
            break;
        }
        // timerfd read -> drain timer, return count
        if (rfd >= 0 && rfd < 1024 && g_timerfd[rfd]) {
            struct kevent kv;
            struct timespec zero = {0, 0};
            int nb = fcntl(rfd, F_GETFL) & O_NONBLOCK;
            int n = kevent(rfd, NULL, 0, &kv, 1, nb ? &zero : NULL);
            if (n <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                break;
            // EAGAIN
            }
            if (a1 && a2 >= 8) {
                if (!host_range_mapped((uintptr_t)a1, 8)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
                *(uint64_t *)a1 = (uint64_t)kv.data;
            }
            G_RET(c) = 8;
            break;
        }
        // eventfd read: return the accumulated counter, reset it, drain the readiness pipe
        if (rfd >= 0 && rfd < 1024 && g_eventfd_peer[rfd]) {
            if (a2 < 8) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            // a1 (the result counter) is written directly below; reject a bad pointer here, before any side
            // effect (counter reset / pipe drain), so it returns -EFAULT rather than faulting the engine.
            if (a1 && !host_range_mapped((uintptr_t)a1, 8)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            if (g_eventfd_count[rfd] == 0) {
                if (fcntl(rfd, F_GETFL) & O_NONBLOCK) { G_RET(c) = (uint64_t)(-EAGAIN); break; }
                char b; if (read(rfd, &b, 1) < 0) {} // block until a writer signals 0->positive
            }
            uint64_t v;
            if (g_eventfd_sema[rfd]) { v = 1; g_eventfd_count[rfd] -= 1; } // EFD_SEMAPHORE: one at a time
            else { v = g_eventfd_count[rfd]; g_eventfd_count[rfd] = 0; }
            // re-sync the pipe to "counter > 0": drain it, then re-signal one byte if still positive
            int fl = fcntl(rfd, F_GETFL);
            fcntl(rfd, F_SETFL, fl | O_NONBLOCK);
            char buf[64]; while (read(rfd, buf, sizeof buf) > 0) {}
            fcntl(rfd, F_SETFL, fl);
            if (g_eventfd_count[rfd] > 0) { char b = 1; if (write(g_eventfd_peer[rfd] - 1, &b, 1) < 0) {} }
            if (a1) *(uint64_t *)a1 = v;
            G_RET(c) = 8;
            break;
        }
        // /proc/<pid>/pagemap (vfs.c backs it with an empty seekable fd; g_pagemap_fd marks it): synthesize
        // one 64-bit entry per page with the PRESENT bit (63) set. The guest lseek'd to vaddr/pagesize*8 and
        // reads sequentially; advance the real fd offset so its position tracks what we "read" (LTP mmap12).
        if (rfd >= 0 && rfd < 1024 && g_pagemap_fd[rfd]) {
            size_t want = (size_t)a2 & ~(size_t)7; // whole 8-byte pagemap entries only
            if (want == 0) { G_RET(c) = 0; break; }
            if (a1 && !host_range_mapped((uintptr_t)a1, want)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            uint64_t *out = (uint64_t *)a1;
            for (size_t i = 0; i < want / 8; i++) out[i] = (1ULL << 63); // PM_PRESENT
            lseek(rfd, (off_t)want, SEEK_CUR);
            G_RET(c) = (uint64_t)want;
            break;
        }
        // SA_RESTART: a blocking read interrupted by a signal whose guest handler asked for restart is
        // resumed in place (the dispatcher runs the handler after the read finally returns); a handler
        // WITHOUT SA_RESTART lets EINTR through. (Well-behaved programs block in poll/select/epoll -- which
        // always return EINTR -- and only read when ready, so this never defers a needed handler.)
        ssize_t r;
        ts_wait_enter(); // #404: 'S' while a read may block (pipe/socket/tty; a ready/regular fd returns at once)
        do { r = read(rfd, (void *)a1, (size_t)a2); } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        // SEQPACKET/O_DIRECT-pipe EOF over a DGRAM backing: a peer-closed read reports ECONNRESET, but the
        // emulated endpoint must return 0 (EOF) like the Linux original. (See netns.c / case 199 / pipe2.)
        if (r < 0 && errno == ECONNRESET && seq_is(rfd)) r = 0;
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 64: {
        int wfd = (int)a0;
        // memfd F_SEAL_WRITE: a write to a write-sealed memfd fails EPERM (emulated seal state).
        if (wfd >= 0 && wfd < 1024 && (g_memfd_seal[wfd] & 0x8)) { G_RET(c) = (uint64_t)(-EPERM); break; }
        // AF_NETLINK socket write (#293): busybox `ip` (libbb) sends its RTM_GET* dump request via
        // write(2), NOT sendto/sendmsg -- so the netlink responder (which only hooked send*) never saw
        // it, no dump was queued, and the follow-up recvmsg blocked forever ("container stuck Up").
        // Route it to nl_send so the dump is synthesized exactly as for the send* path.
        if (nl_is(wfd)) {
            G_RET(c) = (uint64_t)nl_send(wfd, (const uint8_t *)a1, (size_t)a2);
            break;
        }
        // Container DNS: a query write(2)'d on a DNS socket (TCP DNS via write, or a connected-UDP write) is
        // parsed + answered by the host resolver (net.c/netns.c dns_send); nothing reaches the wire.
        if (wfd >= 0 && wfd < 1024 && g_dns_sock[wfd]) {
            G_RET(c) = (uint64_t)dns_send(wfd, (const uint8_t *)a1, (size_t)a2, g_sock_stream[wfd]);
            break;
        }
        // RAM-backed scratch file: serve the write from memory (spill to the host file past the cap).
        // Copies straight from the guest buffer, so validate it (a host-fd write's kernel copyin would fault
        // a bad pointer to EFAULT; this engine memcpy would instead crash) -- #395, access_ok.
        if (memf_get(wfd) && memf_room_or_spill(wfd, (off_t)g_memf[wfd]->pos + (off_t)a2)) {
            if (a2 && !host_range_mapped((uintptr_t)a1, (size_t)a2)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            ssize_t r = memf_write_pos(g_memf[wfd], (void *)a1, (size_t)a2);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        // eventfd write: ADD to the counter (not a raw pipe write); regenerate the readable edge.
        if (wfd >= 0 && wfd < 1024 && g_eventfd_peer[wfd]) {
            if (a2 < 8) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            // a1 is a raw guest pointer we read directly -> validate before the deref (covers NULL too)
            if (!host_range_mapped((uintptr_t)a1, 8)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            uint64_t add = *(uint64_t *)a1;
            if (add == 0xffffffffffffffffULL) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            g_eventfd_count[wfd] += add;
            // Linux wakes epoll edge-triggered waiters on EVERY write, not just the 0->positive transition.
            // A waker eventfd that is never drained (mio/tokio's cross-thread wakeup) would otherwise lose
            // its 2nd and later wakeups: the backing pipe already holds a byte, so an EV_CLEAR kqueue filter
            // never re-fires and a blocked epoll_wait hangs forever. Drain the pipe to exactly one fresh byte
            // so each write produces a new readable edge, bounded even when the reader never keeps up.
            if (add > 0) {
                int fl = fcntl(wfd, F_GETFL);
                if (fl >= 0 && !(fl & O_NONBLOCK)) fcntl(wfd, F_SETFL, fl | O_NONBLOCK);
                char buf[64];
                while (read(wfd, buf, sizeof buf) > 0) {}
                if (fl >= 0 && !(fl & O_NONBLOCK)) fcntl(wfd, F_SETFL, fl);
                char b = 1;
                if (write(g_eventfd_peer[wfd] - 1, &b, 1) < 0) {}
            }
            G_RET(c) = 8;
            break;
        }
        fd_evict(wfd);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking write in place (see case 63)
        do { r = write(wfd, (void *)a1, (size_t)a2); } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 65: {
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_fd_pb_len[(int)a0]) { // tee(2) pushback served first
            const struct iovec *iv = (const struct iovec *)a1;
            size_t tot = 0;
            for (int i = 0; i < (int)a2 && (int)a0 < 1024 && g_fd_pb_len[(int)a0]; i++) {
                size_t k = pipe_pushback_take((int)a0, iv[i].iov_base, iv[i].iov_len);
                tot += k;
                if (k < iv[i].iov_len) break;
            }
            G_RET(c) = (uint64_t)tot;
            break;
        }
        if (nl_is((int)a0)) { // netlink readv (#293): drain the queued dump into the guest iov
            G_RET(c) = (uint64_t)nl_recv((int)a0, (struct iovec *)a1, (int)a2, 0, NULL);
            break;
        }
        if (memf_get((int)a0)) {
            ssize_t r = memf_preadv(g_memf[(int)a0], (const struct iovec *)a1, (int)a2, -1, 1);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking readv in place (see case 63)
        ts_wait_enter(); // #404: 'S' while readv may block
        do { r = readv((int)a0, (void *)a1, (int)a2); } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    // readv
    }
    case 66: {
        if ((int)a0 >= 0 && (int)a0 < 1024 && (g_memfd_seal[(int)a0] & 0x8)) { G_RET(c) = (uint64_t)(-EPERM); break; } // F_SEAL_WRITE
        if (nl_is((int)a0)) { // netlink writev (#293): gather the request iov + queue the dump
            const struct iovec *iv = (const struct iovec *)a1;
            uint8_t tmp[4096];
            size_t tl = 0;
            for (int i = 0; iv && i < (int)a2 && tl < sizeof tmp; i++) {
                size_t n = iv[i].iov_len;
                if (tl + n > sizeof tmp) n = sizeof tmp - tl;
                memcpy(tmp + tl, iv[i].iov_base, n);
                tl += n;
            }
            nl_send((int)a0, tmp, tl);
            G_RET(c) = (uint64_t)tl;
            break;
        }
        // Container DNS: TCP DNS is commonly writev(len-prefix, query) (glibc send_vc). Gather + answer it.
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_dns_sock[(int)a0]) {
            uint8_t tmp[2048];
            size_t tl = dns_gather((const struct iovec *)a1, (int)a2, tmp, sizeof tmp);
            G_RET(c) = (uint64_t)dns_send((int)a0, tmp, tl, g_sock_stream[(int)a0]);
            break;
        }
        if (memf_get((int)a0)) {
            // The iovec array a1 is read directly by the engine (the regular writev path lets the host
            // syscall validate it, but the memf path dereferences it here) -> guard it before the loop.
            int niov = (int)a2;
            if (niov > 0 && !host_range_mapped((uintptr_t)a1, (size_t)niov * sizeof(struct iovec))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            const struct iovec *iv = (const struct iovec *)a1;
            off_t end = g_memf[(int)a0]->pos;
            for (int i = 0; i < (int)a2; i++) end += iv[i].iov_len;
            if (memf_room_or_spill((int)a0, end)) {
                ssize_t r = memf_pwritev(g_memf[(int)a0], iv, (int)a2, -1, 1);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
                break;
            }
        }
        fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking writev in place (see case 63)
        do { r = writev((int)a0, (void *)a1, (int)a2); } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    // writev
    }
    case 67: {
        // pread64
        if (memf_get((int)a0)) {
            ssize_t r = memf_pread(g_memf[(int)a0], (void *)a1, (size_t)a2, (off_t)a3);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pread in place (see case 63)
        do { r = pread((int)a0, (void *)a1, (size_t)a2, (off_t)a3); } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 68: {
        // pwrite64
        if ((int)a0 >= 0 && (int)a0 < 1024 && (g_memfd_seal[(int)a0] & 0x8)) { G_RET(c) = (uint64_t)(-EPERM); break; } // F_SEAL_WRITE
        if (memf_get((int)a0) && memf_room_or_spill((int)a0, (off_t)a3 + (off_t)a2)) {
            ssize_t r = memf_pwrite(g_memf[(int)a0], (void *)a1, (size_t)a2, (off_t)a3);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pwrite in place (see case 63)
        do { r = pwrite((int)a0, (void *)a1, (size_t)a2, (off_t)a3); } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // sendfile(out,in,off*,count)
    case 71: {
        int outfd = (int)a0, infd = (int)a1;
        memf_materialize(outfd); // sendfile reads/writes via the real fds -> flush RAM cache first
        memf_materialize(infd);
        off_t *po = (off_t *)a2;
        size_t cnt = (size_t)a3;
        // po (the in/out file offset) is read AND written directly -> validate before the copy loop so a bad
        // pointer returns -EFAULT instead of faulting the engine (and before any bytes move).
        if (po && !host_range_mapped((uintptr_t)a2, sizeof(off_t))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        if (po) lseek(infd, *po, SEEK_SET);
        char bf[65536];
        size_t tot = 0;
        while (tot < cnt) {
            size_t w = cnt - tot < sizeof bf ? cnt - tot : sizeof bf;
            ssize_t n = read(infd, bf, w);
            if (n <= 0) break;
            ssize_t wr = write(outfd, bf, n);
            if (wr < 0) break;
            tot += wr;
            if (wr < n) break;
        }
        if (po) *po += tot;
        G_RET(c) = tot;
        break;
    }
    // vmsplice(fd, iov, nr_segs, flags): gather user memory INTO a pipe (write end) or scatter a pipe's
    // bytes back into user memory (read end). Direction follows the pipe fd's access mode, matching Linux.
    case 75: {
        int vfd = (int)a0;
        const struct iovec *iv = (const struct iovec *)a1;
        int niov = (int)a2;
        if (niov > 0 && !host_range_mapped((uintptr_t)a1, (size_t)niov * sizeof(struct iovec))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        memf_materialize(vfd);
        int fl = fcntl(vfd, F_GETFL);
        int to_pipe = (fl < 0) || ((fl & O_ACCMODE) != O_RDONLY); // write end -> user pages into the pipe
        fd_evict(vfd);
        ssize_t r = to_pipe ? writev(vfd, iv, niov) : readv(vfd, iv, niov);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // splice(fd_in,off_in,fd_out,off_out,len,fl): move bytes between two fds (consumes the source).
    case 76: {
        int fin = (int)a0, fout = (int)a2;
        // splice reads/writes the optional off_in (a1) / off_out (a3) pointers directly; validate them
        // before moving any bytes so a bad pointer returns -EFAULT instead of faulting the engine.
        if (a1 && !host_range_mapped((uintptr_t)a1, sizeof(off_t))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        if (a3 && !host_range_mapped((uintptr_t)a3, sizeof(off_t))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        memf_materialize(fin); // splice moves bytes via the real fds -> flush RAM cache first
        memf_materialize(fout);
        size_t len = (size_t)a4;
        if (len > 65536) len = 65536;
        static __thread char sb[65536];
        ssize_t n;
        fd_evict(fout);
        if (a1) {
            n = pread(fin, sb, len, *(off_t *)a1);
        } else {
            // a pipe source may carry tee()'d pushback -> serve that first (splice consumes it).
            size_t pb = pipe_pushback_take(fin, sb, len);
            n = pb > 0 ? (ssize_t)pb : read(fin, sb, len);
        }
        if (n <= 0) {
            G_RET(c) = n < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        ssize_t w = a3 ? pwrite(fout, sb, n, *(off_t *)a3) : write(fout, sb, n);
        if (w < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (a1) *(off_t *)a1 += w;
        if (a3) *(off_t *)a3 += w;
        G_RET(c) = (uint64_t)w;
        break;
    }
    // tee(fd_in, fd_out, len, flags): duplicate up to `len` bytes between two pipes WITHOUT consuming the
    // source. macOS has no tee, so peek fd_in (drain then re-queue as read-pushback) and copy to fd_out.
    case 77: {
        int fin = (int)a0, fout = (int)a1; // tee(fd_in, fd_out, len, flags) -- NOT the splice arg layout
        memf_materialize(fin);
        memf_materialize(fout);
        size_t len = (size_t)a2;
        if (len > 65536) len = 65536;
        static __thread char sb[65536];
        fd_evict(fout);
        // front of the source stream = existing pushback ++ kernel-buffered bytes
        size_t oldlen = (fin >= 0 && fin < 1024) ? g_fd_pb_len[fin] : 0;
        if (oldlen > sizeof sb) oldlen = sizeof sb;
        if (oldlen) memcpy(sb, g_fd_pushback[fin], oldlen);
        size_t pos = oldlen;
        if (oldlen < len) {
            ssize_t kn = read(fin, sb + oldlen, len - oldlen);
            if (kn > 0) pos += (size_t)kn;
        }
        if (pos == 0) { G_RET(c) = (uint64_t)(int64_t)(-EAGAIN); break; } // nothing available (nonblocking)
        size_t dup = pos < len ? pos : len;
        ssize_t w = write(fout, sb, dup);
        // tee never consumes the source: restore the whole peeked front as pushback.
        pipe_pushback_set(fin, sb, pos);
        G_RET(c) = w < 0 ? (uint64_t)(-errno) : (uint64_t)w;
        break;
    }
    case 23: {
        // dup -- a 2nd fd would share the description; flush the RAM cache so both see the real file
        memf_materialize((int)a0);
        int r = nofile_gate(dup((int)a0)); // EMFILE if the new fd would be >= the guest's soft RLIMIT_NOFILE
        // carry path + socket-emulation metadata to the new fd
        if (r >= 0 && r < 1024 && (int)a0 >= 0 && (int)a0 < 1024) { strcpy(g_fdpath[r], g_fdpath[(int)a0]); fd_carry_sock(r, (int)a0); }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 24: {
        // dup3(old,new,flags). x86's legacy dup2 arrives here rewritten to the dup3 form + a private
        // DUP2_COMPAT marker (bit 30) in the flags (see translate/x86_64/legacy.c) because the two calls
        // DIVERGE on oldfd==newfd: dup3 -> EINVAL, but dup2 -> returns newfd unchanged (EBADF if oldfd is
        // invalid), with no close and no CLOEXEC change. (LTP dup201)
        unsigned d3flags = (unsigned)a2;
        int is_dup2 = (d3flags & 0x40000000u) != 0;
        d3flags &= ~0x40000000u;
        int oldfd = (int)a0, newfd = (int)a1, nofile = guest_nofile_cur();
        if (oldfd == newfd) {
            if (is_dup2) {
                // dup2(fd,fd): a no-op returning fd iff it is a valid open fd, else EBADF.
                G_RET(c) = (oldfd < 0 || fcntl(oldfd, F_GETFD) < 0) ? (uint64_t)(-EBADF) : (uint64_t)(unsigned)newfd;
                break;
            }
            G_RET(c) = (uint64_t)(-EINVAL); // genuine dup3(old,old,*): EINVAL (before any fd/flag validation)
            break;
        }
        // dup3 flag validation: only O_CLOEXEC is a valid flag (dup2 carries none -> the marker was stripped).
        if (!is_dup2 && (d3flags & ~0x80000u)) { G_RET(c) = (uint64_t)(-EINVAL); break; }
        // newfd must lie within the guest's descriptor range (the emulated soft RLIMIT_NOFILE); the host fd
        // table is far larger, so a raw dup2/dup3(.., newfd>=cap) would wrongly succeed -> EBADF. (LTP dup201)
        if (newfd < 0 || newfd >= nofile) { G_RET(c) = (uint64_t)(-EBADF); break; }
        // oldfd must be an open descriptor -> EBADF, and (per Linux) checked WITHOUT closing newfd first.
        if (oldfd < 0 || fcntl(oldfd, F_GETFD) < 0) { G_RET(c) = (uint64_t)(-EBADF); break; }
        memf_materialize((int)a0); // source: a 2nd fd shares the description -> flush RAM cache
        memf_close((int)a1);       // target fd is about to be reused; drop any cache it held
        engine_fd_vacate((int)a1); // move any engine-private fd off the target before dup2 overwrites it
        int r = dup2((int)a0, (int)a1);
        if (r >= 0) {
            if (d3flags & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC); // O_CLOEXEC
            if ((int)a1 >= 0 && (int)a1 < 1024 && (int)a0 >= 0 && (int)a0 < 1024) {
                strcpy(g_fdpath[(int)a1], g_fdpath[(int)a0]);
                fd_carry_sock((int)a1, (int)a0);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 25: {
        // fcntl -- Linux cmd# -> macOS (they diverge!)
        int lcmd = (int)a1;
        // F_DUPFD(_CLOEXEC): the floor arg must be a valid descriptor index -- Linux rejects a negative or
        // >= RLIMIT_NOFILE floor with EINVAL (before allocating). (LTP: fcntl bad-arg matrix.)
        if (lcmd == 0 || lcmd == 1030) {
            int floor = (int)a2;
            if (floor < 0 || floor >= guest_nofile_cur()) { G_RET(c) = (uint64_t)(-EINVAL); break; }
        }
        // F_DUPFD(_CLOEXEC) makes a 2nd fd sharing the description; F_SETFL O_APPEND changes write-offset
        // semantics. Either way, flush a RAM-backed fd so the real host fd takes over with correct bytes.
        if (lcmd == 0 || lcmd == 1030 || (lcmd == 4 && ((int)a2 & 0x400))) memf_materialize((int)a0);
        // F_GETFL: macOS O_* -> Linux O_*
        if (lcmd == 3) {
            int r = fcntl((int)a0, F_GETFL, 0);
            if (r < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // access mode identical
            int lf = r & 0x3;
            if (r & 0x8) lf |= 0x400;
            if (r & 0x4) lf |= 0x800;
            // APPEND/NONBLOCK/ASYNC
            if (r & 0x40) lf |= 0x2000;
            G_RET(c) = (uint64_t)(unsigned)lf;
            break;
        }
        // F_SETFL: Linux O_* -> macOS O_*
        if (lcmd == 4) {
            int la = (int)a2, mf = 0;
            if (la & 0x400) mf |= 0x8;
            if (la & 0x800) mf |= 0x4;
            // APPEND/NONBLOCK/ASYNC
            if (la & 0x2000) mf |= 0x40;
            int r = fcntl((int)a0, F_SETFL, mf);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // F_GETLK/SETLK/SETLKW: xlate struct flock + cmd
        if (lcmd == 5 || lcmd == 6 || lcmd == 7) {
            // macOS F_GETLK=7,SETLK=8,SETLKW=9
            int mc = lcmd == 5 ? F_GETLK : lcmd == 6 ? F_SETLK : F_SETLKW;
            uint8_t *lf = (uint8_t *)a2;
            // Linux order (SYSCALL_DEFINE3(fcntl) -> fcntl_getlk/setlk): the fd is validated (EBADF) BEFORE the
            // flock is copied in, so a bad fd wins over a bad pointer / bad l_whence. (LTP fcntl13: fcntl(-1,...).)
            if ((int)a0 < 0 || fcntl((int)a0, F_GETFD) < 0) { G_RET(c) = (uint64_t)(-EBADF); break; }
            // The Linux struct flock at a2 (fields up to lf+24) is read directly and written back for F_GETLK;
            // validate the 32-byte struct before any deref so a bad pointer returns -EFAULT, not a crash. A guest
            // PROT_NONE flock buffer (LTP fcntl13 uses one) is force-mapped host-writable by dd, so the
            // host_range_mapped probe passes -- pn_hit catches the guest's intent so it still EFAULTs like Linux.
            if (!host_range_mapped((uintptr_t)a2, 32) || pn_hit((uintptr_t)a2, 32)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            // l_whence must be SEEK_SET/SEEK_CUR/SEEK_END; Linux rejects anything else with EINVAL in
            // flock_to_posix_lock -- BEFORE the fd type is consulted, so it applies to a pipe fd too. (LTP fcntl13)
            {
                short whence = *(short *)(lf + 2);
                if (whence != 0 && whence != 1 && whence != 2) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            }
            // #340: service advisory byte-range locks on regular files from the in-engine cross-process
            // table (no host round-trip). F_SETLKW blocks by poll-retry, interruptible by a deliverable
            // pending signal (g_pending/tpending, honouring the per-thread block mask) -> EINTR, exactly
            // as a real F_SETLKW returns. poslk_op returns 0 only for non-regular fds -> host path below.
            {
                int pout = 0, claimed;
                for (;;) {
                    claimed = poslk_op((int)a0, lcmd, (uint8_t *)a2, &pout);
                    if (!claimed) break; // not a regular file -> fall through to the host fcntl path
                    if (lcmd == 7 && pout == -EAGAIN) {
                        uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) |
                                     __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
                        int intr = 0;
                        for (int s = 1; s < 64; s++)
                            if ((p & (1ull << s)) && !(c->sigmask & (1ull << (s - 1)))) { intr = 1; break; }
                        if (intr) { G_RET(c) = (uint64_t)(-EINTR); break; }
                        struct timespec ts = {0, 1000000}; // 1 ms poll
                        nanosleep(&ts, NULL);
                        continue;
                    }
                    G_RET(c) = (uint64_t)(int64_t)pout;
                    break;
                }
                if (claimed) break; // handled in-engine (or interrupted); done
            }
            struct flock fl;
            // Linux flock: type/whence/pad/start@8/len@16/pid@24
            memset(&fl, 0, sizeof fl);
            short lt = *(short *)(lf + 0);
            // Linux RDLCK=0,WRLCK=1,UNLCK=2 -> macOS
            fl.l_type = lt == 0 ? F_RDLCK : lt == 1 ? F_WRLCK : F_UNLCK;
            fl.l_whence = *(short *)(lf + 2);
            fl.l_start = *(int64_t *)(lf + 8);
            fl.l_len = *(int64_t *)(lf + 16);
            fl.l_pid = *(int32_t *)(lf + 24);
            int r = fcntl((int)a0, mc, &fl), e = errno;
            // F_GETLK writes the conflicting lock back
            if (r >= 0 && lcmd == 5) {
                *(short *)(lf + 0) = fl.l_type == F_RDLCK ? 0 : fl.l_type == F_WRLCK ? 1 : 2;
                *(short *)(lf + 2) = fl.l_whence;
                *(int64_t *)(lf + 8) = fl.l_start;
                *(int64_t *)(lf + 16) = fl.l_len;
                *(int32_t *)(lf + 24) = (int32_t)fl.l_pid;
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
            break;
        }
        // F_SETPIPE_SZ(1031)/F_GETPIPE_SZ(1032): macOS can't resize a pipe, so emulate -- record the
        // requested size (rounded up to a page, >= requested) and report it back on GET.
        if (lcmd == 1031) {
            int want = (int)a2;
            long pg = sysconf(_SC_PAGESIZE);
            if (pg <= 0) pg = 4096;
            int rounded = (int)(((want + pg - 1) / pg) * pg);
            if (rounded < (int)pg) rounded = (int)pg;
            if ((int)a0 >= 0 && (int)a0 < 1024) g_pipesz[(int)a0] = rounded;
            G_RET(c) = (uint64_t)(unsigned)rounded;
            break;
        }
        if (lcmd == 1032) {
            int sz = ((int)a0 >= 0 && (int)a0 < 1024 && g_pipesz[(int)a0]) ? g_pipesz[(int)a0] : 65536;
            G_RET(c) = (uint64_t)(unsigned)sz;
            break;
        }
        int mcmd = lcmd;
        if (lcmd == 8)
            mcmd = F_SETOWN;
        else if (lcmd == 9)
            // owner cmds also swapped on macOS
            mcmd = F_GETOWN;
        else if (lcmd == 1030)
            mcmd = F_DUPFD_CLOEXEC;
        // memfd sealing: F_ADD_SEALS(1033) / F_GET_SEALS(1034) are honoured on an anonymous memfd (macOS has
        // no native seals, so the state + the F_SEAL_WRITE write-guard are emulated). On a non-memfd both
        // return EINVAL, as on Linux.
        else if (lcmd == 1033) { // F_ADD_SEALS(fd, seals)
            int fd = (int)a0;
            if (fd < 0 || fd >= 1024 || !g_memfd_is[fd]) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            if (g_memfd_seal[fd] & 0x1) { G_RET(c) = (uint64_t)(-EPERM); break; } // already F_SEAL_SEAL'd
            g_memfd_seal[fd] |= (int)a2 & 0x1f; // SEAL|SHRINK|GROW|WRITE|FUTURE_WRITE
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1034) { // F_GET_SEALS(fd)
            int fd = (int)a0;
            if (fd < 0 || fd >= 1024 || !g_memfd_is[fd]) { G_RET(c) = (uint64_t)(-EINVAL); break; }
            G_RET(c) = (uint64_t)(unsigned)g_memfd_seal[fd];
            break;
        } else if (lcmd == 1024 || lcmd == 1025 || lcmd == 1026) {
            G_RET(c) = 0;
            break;
        // lease/notify: no-op
        }
        // A command this kernel does not recognize is EINVAL (Linux do_fcntl default), NOT forwarded to
        // macOS -- whose fcntl cmd numbering DIVERGES, so a stray Linux cmd# would mean a different op there.
        // Everything valid was handled above or is one of these benign pass-throughs; reject the rest. (LTP
        // fcntl13 F_BADCMD=999.) 10/11=SETSIG/GETSIG, 15/16/17=SET/GETOWN_EX+GETOWNER_UIDS, 36/37/38=OFD locks.
        switch (lcmd) {
        case 0: case 1: case 2: case 8: case 9: case 10: case 11:
        case 15: case 16: case 17: case 36: case 37: case 38: case 1030:
            break; // recognized Linux command -> proceed to the host fcntl
        default:
            G_RET(c) = (uint64_t)(-EINVAL);
            goto fcntl_done;
        }
        int r = fcntl((int)a0, mcmd, a2);
        if (lcmd == 0 || lcmd == 1030) r = nofile_gate(r); // F_DUPFD(_CLOEXEC): EMFILE past the guest fd cap
        if (r >= 0 && (lcmd == 0 || lcmd == 1030) && r < 1024 && (int)a0 >= 0 && (int)a0 < 1024) {
            // F_DUPFD(_CLOEXEC)
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            fd_carry_sock(r, (int)a0);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
    fcntl_done:
        break;
    }
    case 29: {
        // ioctl(fd, req, arg). Almost every request (termios/winsize/job-control) is owned by svc_fs below;
        // we only claim FIOASYNC here. On a SOCKET/PIPE Linux's FIOASYNC toggles signal-driven I/O and
        // returns 0, but svc_fs's terminal-centric handler answers ENOTTY for it -- and nginx's master arms
        // ioctl(listenfd, FIOASYNC, &on) on its listen socket, so an ENOTTY aborts worker startup and every
        // connection then hangs. Translate it to the O_ASYNC file-status flag (fcntl), exactly like Linux,
        // and defer every other request to svc_fs by returning "not handled".
        if (a1 != 0x5452) return 0; // not FIOASYNC -> let svc_fs handle it
        if (a2 && !host_range_mapped((uintptr_t)a2, sizeof(int))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        int on = a2 ? *(int *)a2 : 0, fl = fcntl((int)a0, F_GETFL);
        if (fl < 0) { G_RET(c) = (uint64_t)(-errno); break; }
        fl = on ? (fl | O_ASYNC) : (fl & ~O_ASYNC);
        G_RET(c) = fcntl((int)a0, F_SETFL, fl) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 59: {
        // pipe2(fds, flags). O_DIRECT requests "packet mode": each write is a distinct record that reads
        // back whole, never coalesced. macOS pipes can't do this, but an AF_UNIX SOCK_DGRAM socketpair
        // preserves message boundaries exactly, so back an O_DIRECT pipe with one (SOCK_SEQPACKET would be
        // closer but macOS PF_LOCAL doesn't support it). A plain pipe is fine for the non-O_DIRECT case.
        int fds[2], fl = (int)a1;
        // a0 receives the two result fds (8 bytes). Validate it BEFORE creating the pipe so a bad pointer
        // returns -EFAULT without leaking the freshly-opened fds (and without faulting the engine).
        if (!host_range_mapped((uintptr_t)a0, 2 * sizeof(int))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        int mk = (fl & G_O_DIRECT) ? socketpair(AF_UNIX, SOCK_DGRAM, 0, fds) : pipe(fds);
        if (mk < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // Either new fd past the guest's soft RLIMIT_NOFILE -> EMFILE (the host table is far larger). Close
        // both so no descriptor leaks, exactly as Linux fails a pipe2 that would exceed the limit.
        {
            int cap = guest_nofile_cur();
            if (fds[0] >= cap || fds[1] >= cap) { close(fds[0]); close(fds[1]); G_RET(c) = (uint64_t)(-EMFILE); break; }
        }
        if (fl & 0x80000) {
            fcntl(fds[0], F_SETFD, FD_CLOEXEC);
            fcntl(fds[1], F_SETFD, FD_CLOEXEC);
        }
        if (fl & 0x800) {
            fcntl(fds[0], F_SETFL, O_NONBLOCK);
            fcntl(fds[1], F_SETFL, O_NONBLOCK);
        }
        ((int *)a0)[0] = fds[0];
        ((int *)a0)[1] = fds[1];
        // An O_DIRECT pipe is backed by a DGRAM socketpair (above). Like a real pipe it must report EOF
        // when the write end closes, but macOS DGRAM sockets don't -- mark both ends so close() sends a
        // zero-length EOF datagram and read() coerces the peer-closed ECONNRESET to 0. (See netns.c.)
        if ((fl & G_O_DIRECT)) {
            if (fds[0] >= 0 && fds[0] < 1024) g_sock_seqpacket[fds[0]] = 1;
            if (fds[1] >= 0 && fds[1] < 1024) g_sock_seqpacket[fds[1]] = 1;
        }
        G_RET(c) = 0;
        break;
    }
    // fsync -- durability policy (S3DB_DURABILITY): default/fast == plain fsync() (legacy path)
    // A RAM-backed scratch file is anonymous/private: fsync has no observable effect -> 0.
    case 82: G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0); break;
    // fdatasync -> fsync (no macOS fdatasync); same durability policy
    case 83: G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0); break;
    // copy_file_range(fdin,offin*,fdout,offout*,len,flags)
    case 285: {
        int fdin = (int)a0, fdout = (int)a2;
        memf_materialize(fdin); // copy_file_range moves bytes via the real fds -> flush RAM caches first
        memf_materialize(fdout);
        size_t len = (size_t)a4, done = 0;
        int err = 0;
        off_t *poi = (off_t *)a1, *poo = (off_t *)a3;
        // off_in (a1) / off_out (a3) are read here and written back below -> validate before any deref so a
        // bad pointer returns -EFAULT instead of faulting the engine (and before any bytes are copied).
        if (poi && !host_range_mapped((uintptr_t)a1, sizeof(off_t))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        if (poo && !host_range_mapped((uintptr_t)a3, sizeof(off_t))) { G_RET(c) = (uint64_t)(-EFAULT); break; }
        off_t oi = poi ? *poi : -1, oo = poo ? *poo : -1;
        char cb[8192];
        while (done < len) {
            size_t chunk = (len - done > sizeof cb) ? sizeof cb : len - done;
            ssize_t r = (oi >= 0) ? pread(fdin, cb, chunk, oi) : read(fdin, cb, chunk);
            if (r < 0) {
                err = errno;
                break;
            }
            if (r == 0) break;
            ssize_t w = (oo >= 0) ? pwrite(fdout, cb, (size_t)r, oo) : write(fdout, cb, (size_t)r);
            if (w < 0) {
                err = errno;
                break;
            }
            done += (size_t)w;
            if (oi >= 0) oi += w;
            if (oo >= 0) oo += w;
            if (w < r) break;
        }
        if (poi) *poi = oi;
        if (poo) *poo = oo;
        fd_evict(fdout);
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
        break;
    }
    // preadv/pwritev: struct iovec layout is identical Linux<->macOS
    case 69: {
        if (memf_get((int)a0)) {
            ssize_t r = memf_preadv(g_memf[(int)a0], (const struct iovec *)a1, (int)a2, (off_t)a3, 0);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r = preadv((int)a0, (const struct iovec *)a1, (int)a2, (off_t)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 70: {
        if (memf_get((int)a0)) {
            // memf path dereferences the iovec array a1 directly (the host pwritev path validates its own) ->
            // guard it before the loop so a bad array pointer returns -EFAULT instead of faulting the engine.
            int niov = (int)a2;
            if (niov > 0 && !host_range_mapped((uintptr_t)a1, (size_t)niov * sizeof(struct iovec))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            const struct iovec *iv = (const struct iovec *)a1;
            off_t end = (off_t)a3;
            for (int i = 0; i < (int)a2; i++) end += iv[i].iov_len;
            if (memf_room_or_spill((int)a0, end)) {
                ssize_t r = memf_pwritev(g_memf[(int)a0], iv, (int)a2, (off_t)a3, 0);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
                break;
            }
        }
        ssize_t r = pwritev((int)a0, (const struct iovec *)a1, (int)a2, (off_t)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 84: G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0); break; // sync_file_range -> fsync (no-op for RAM scratch)
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
