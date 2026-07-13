// Extracted from service(): I/O — fd read/write/seek + plain fd ops
// (dup/dup3/fcntl/pipe2/sendfile/splice/tee/copy_file_range/fsync/etc). Returns 1 if nr was handled, 0 otherwise.
// Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).

static int eventfd_peer_owner(int fd) {
    if (fd < 0) return -1;
    for (int i = 0; i < DD_NFD; i++)
        if (g_eventfd_peer[i] == fd + 1) return i;
    return -1;
}

static int eventfd_peer_is_engine_fd(int fd) {
    return eventfd_peer_owner(fd) >= 0;
}

static void eventfd_peer_vacate(int fd) {
    int owner = eventfd_peer_owner(fd);
    if (owner < 0) return;
    int hi = fcntl(fd, F_DUPFD, 1 << 20);
    if (hi < 0) hi = fcntl(fd, F_DUPFD, 64);
    if (hi >= 0 && hi != fd) {
        g_eventfd_peer[owner] = hi + 1;
    }
}

// Carry dd's virtual-fd emulation state from oldfd to newfd on dup/dup2/dup3/F_DUPFD. Linux duplicated
// descriptors refer to the SAME open file description, so a dup'd eventfd/timerfd must share the underlying
// object. The host dup already shares the backing pipe/kqueue; these tables are what route the guest's
// read/write to the virtual handler, so without carrying them the duplicate degraded to a raw pipe/fd.
static void fd_carry_virt(int newfd, int oldfd) {
    if (newfd < 0 || newfd >= DD_NFD || oldfd < 0 || oldfd >= DD_NFD || newfd == oldfd) return;
    // Tag both fds as the same open file description so a later close of one (while the other survives) can
    // find the surviving alias -- e.g. epoll readiness must persist while a dup keeps the watched OFD open.
    ofd_link_dup(newfd, oldfd);
    // eventfd: share the peer write end + counter slot; bump the slot refcount so closing either alias does
    // not tear the shared object down until the last one closes (see fd_reset_emul / g_eventfd_refs).
    if (g_eventfd_peer[oldfd]) {
        g_eventfd_peer[newfd] = g_eventfd_peer[oldfd];
        g_eventfd_cslot[newfd] = g_eventfd_cslot[oldfd];
        g_eventfd_sema[newfd] = g_eventfd_sema[oldfd];
        g_eventfd_gnb[newfd] = g_eventfd_gnb[oldfd]; // carry the guest blocking/non-blocking intent
        g_eventfd_refs[eventfd_counter_slot(oldfd)]++;
    }
    // timerfd: the timer is armed on the (host-shared) kqueue, so the dup drains the same expirations; carry
    // the routing flag plus the deadline/interval bookkeeping timerfd_gettime reports against.
    if (g_timerfd[oldfd]) {
        g_timerfd[newfd] = 1;
        g_tfd_deadline[newfd] = g_tfd_deadline[oldfd];
        g_tfd_interval[newfd] = g_tfd_interval[oldfd];
        g_tfd_first_oneshot[newfd] = g_tfd_first_oneshot[oldfd];
    }
    // inotify: the instance is a (host-shared) kqueue with its watches; carry the routing flag so the dup's
    // read() drains the same event queue. Watches stay owned by the original instance fd -- closing the DUP
    // tears down nothing (no watch is owned by it), and closing the original behaves as before the dup.
    if (oldfd < 1024 && newfd < 1024 && g_inotify[oldfd]) g_inotify[newfd] = 1;
    // signalfd: a duplicate refers to the SAME OFD (shares its self-pipe). Carry the slot mapping and bump the
    // OFD refcount so the pipe is torn down only when the last alias closes (see fd_reset_emul).
    if (g_sigfd_slot[oldfd]) {
        g_sigfd_slot[newfd] = g_sigfd_slot[oldfd];
        g_sfd[g_sigfd_slot[oldfd] - 1].refs++;
    }
    // epoll: a duplicate shares the same (host-shared) kqueue. Mark BOTH aliases dup'd so epoll_ctl/wait use
    // the immediate path (interest goes straight to the shared kqueue, visible to both fds); flush any
    // changelist queued before the dup now so already-registered interest is not stranded on the original.
    if (g_epoll[oldfd]) {
        g_epoll[newfd] = 1;
        g_ep_dupd[oldfd] = 1;
        g_ep_dupd[newfd] = 1;
        if (g_ep_chgn[oldfd] > 0) {
            kevent(oldfd, g_ep_chg[oldfd], g_ep_chgn[oldfd], NULL, 0, NULL);
            g_ep_chgn[oldfd] = 0;
        }
    }
}

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

// ---- F_SETLEASE lease registry + F_NOTIFY (dnotify) directory-change monitor --------------------
// macOS has neither file leases nor dnotify, so a bare success armed nothing. We emulate as far as the host
// allows:
//   * F_SETLEASE/F_GETLEASE: validate arguments exactly like Linux (fcntl setlease) and track the lease
//     type per fd so F_GETLEASE round-trips what F_SETLEASE set. RESIDUAL: the lease-BREAK signal on a
//     conflicting cross-process open is NOT delivered -- macOS gives no rootless hook to intercept another
//     opener of the same file. Documented in syscall-compat.md.
//   * F_NOTIFY: backed by a real kqueue EVFILT_VNODE watch on the directory fd, drained on a lazily-spawned
//     thread that raises the requested signal (F_SETSIG signal, else the SIGIO default) in the guest -- the
//     same async delivery path POSIX timers/timerfd use (g_pending + the signalfd wake). One-shot by
//     default; re-armed each event only when DN_MULTISHOT is set.
#define DN_SIG_DEFAULT 29 // Linux SIGIO
#define DN_MULTISHOT 0x80000000u
#define DN_VALID (1u | 2u | 4u | 8u | 16u | 32u | DN_MULTISHOT) // ACCESS/MODIFY/CREATE/DELETE/RENAME/ATTRIB

static int8_t g_lease[DD_NFD];   // 0 = no lease; else lease type + 1 (F_RDLCK 0->1, F_WRLCK 1->2, F_UNLCK 2->3)
static uint8_t g_fsig[DD_NFD];   // per-fd F_SETSIG signal (0 = default); consulted by O_ASYNC + dnotify
static uint32_t g_dn_mask[DD_NFD]; // per-fd active dnotify mask (0 = no watch)
static uint8_t g_dn_sig[DD_NFD];   // signal captured for this fd's dnotify watch at arm time

static int g_dn_kq = -1;
static pthread_t g_dn_thr;
static int g_dn_thr_up;
static pthread_mutex_t g_dn_lk = PTHREAD_MUTEX_INITIALIZER;

// dnotify drain thread: block on the watch kqueue and, per vnode event, raise the armed signal in the guest.
static void *dn_loop(void *arg) {
    (void)arg;
    for (;;) {
        struct kevent ev;
        int n = kevent(g_dn_kq, NULL, 0, &ev, 1, NULL);
        if (n < 0) {
            if (errno == EINTR) continue;
            break; // kq closed -> thread exits
        }
        if (n == 0) continue;
        int fd = (int)ev.ident;
        if (fd < 0 || fd >= DD_NFD) continue;
        pthread_mutex_lock(&g_dn_lk);
        uint32_t mask = g_dn_mask[fd];
        int sig = g_dn_sig[fd] ? g_dn_sig[fd] : DN_SIG_DEFAULT;
        if (mask && !(mask & DN_MULTISHOT)) { // one-shot: consume the watch (Linux re-arm is explicit)
            struct kevent del;
            EV_SET(&del, fd, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
            kevent(g_dn_kq, &del, 1, NULL, 0, NULL);
            g_dn_mask[fd] = 0;
        }
        pthread_mutex_unlock(&g_dn_lk);
        if (!mask) continue; // raced a removal
        if (sig >= 1 && sig <= 64) {
            g_sigcode[sig] = 0x80; // SI_KERNEL (generic async source; dnotify carries no user siginfo)
            __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
            sfd_deliver(sig); // wake every signalfd whose per-OFD mask matches (ofd pool)
        }
    }
    return NULL;
}

// per-process timers/dnotify are NOT inherited across fork(): a forked child's inherited kqueue + drain
// thread are dead, so reset the dnotify table so the child re-arms cleanly on its own first F_NOTIFY.
static void dn_atfork_child(void) {
    g_dn_kq = -1;
    g_dn_thr_up = 0;
    memset(g_dn_mask, 0, sizeof g_dn_mask);
    pthread_mutex_init(&g_dn_lk, NULL);
}

// Lazily bring up the shared watch kqueue + drain thread. Caller holds g_dn_lk. Returns 0 / -errno.
static int dn_init(void) {
    static int reg = 0;
    if (!reg) {
        pthread_atfork(NULL, NULL, dn_atfork_child);
        reg = 1;
    }
    if (g_dn_kq < 0) {
        g_dn_kq = kqueue();
        if (g_dn_kq < 0) return -errno;
        fcntl(g_dn_kq, F_SETFD, FD_CLOEXEC);
    }
    if (!g_dn_thr_up) {
        if (pthread_create(&g_dn_thr, NULL, dn_loop, NULL) != 0) return -EAGAIN;
        g_dn_thr_up = 1;
    }
    return 0;
}

// fcntl(fd, F_NOTIFY, mask): arm/replace/remove a dnotify watch on the (directory) fd. mask 0 removes it.
static int dnotify_apply(int fd, uint32_t mask, int sig) {
    if (fd < 0 || fd >= DD_NFD) return -EBADF;
    if (mask & ~DN_VALID) return -EINVAL;
    pthread_mutex_lock(&g_dn_lk);
    int rc = 0;
    if (mask == 0) { // remove the watch
        if (g_dn_mask[fd]) {
            struct kevent del;
            EV_SET(&del, fd, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
            kevent(g_dn_kq, &del, 1, NULL, 0, NULL);
            g_dn_mask[fd] = 0;
        }
        pthread_mutex_unlock(&g_dn_lk);
        return 0;
    }
    rc = dn_init();
    if (rc < 0) {
        pthread_mutex_unlock(&g_dn_lk);
        return rc;
    }
    // Translate the requested DN_* bits into the kqueue vnode fflags (the closest macOS equivalents).
    unsigned fflags = 0;
    if (mask & (2u | 32u)) fflags |= NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB; // DN_MODIFY/DN_ATTRIB
    if (mask & (4u | 8u)) fflags |= NOTE_WRITE | NOTE_LINK;                  // DN_CREATE/DN_DELETE (dir entry)
    if (mask & 16u) fflags |= NOTE_RENAME | NOTE_WRITE;                      // DN_RENAME
    if (mask & 1u) fflags |= NOTE_ATTRIB;                                    // DN_ACCESS (atime) -- best effort
    if (!fflags) fflags = NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND | NOTE_LINK;
    struct kevent kv;
    EV_SET(&kv, fd, EVFILT_VNODE, EV_ADD | EV_CLEAR, fflags, 0, (void *)(intptr_t)fd);
    if (kevent(g_dn_kq, &kv, 1, NULL, 0, NULL) < 0) {
        rc = -errno;
        pthread_mutex_unlock(&g_dn_lk);
        return rc;
    }
    g_dn_mask[fd] = mask;
    g_dn_sig[fd] = (uint8_t)(sig > 0 ? sig : 0);
    pthread_mutex_unlock(&g_dn_lk);
    return 0;
}

static void engine_fd_vacate(int newfd) {
    if (newfd < 0) return;
    eventfd_peer_vacate(newfd);
    engine_fd_reloc(&g_root_fd, newfd);
    engine_fd_reloc(&g_gtimer_kq, newfd);
    // signalfd write ends are engine-private; a guest dup2/dup3 onto one must relocate it (the read ends are
    // guest fds and are NOT relocated -- a dup2 onto a signalfd read end legitimately replaces that signalfd).
    for (int i = 0; i < DD_SFD_MAX; i++)
        if (g_sfd[i].refs > 0) engine_fd_reloc(&g_sfd[i].wr, newfd);
    engine_fd_reloc(&g_dn_kq, newfd);
    for (int i = 0; i < g_nvols; i++)
        engine_fd_reloc(&g_vols[i].fd, newfd);
}

// Vacate every engine-private fd whose NUMBER falls in [first,last] -- for a guest close_range() that would
// otherwise close the runtime's descriptors (g_root_fd etc.). Visible to fs.c/rare.c (io.c is #included first).
static void engine_fd_vacate_range(unsigned first, unsigned last) {
    int fds[3] = {g_root_fd, g_gtimer_kq, g_dn_kq};
    for (int i = 0; i < 3; i++)
        if (fds[i] >= 0 && (unsigned)fds[i] >= first && (unsigned)fds[i] <= last) engine_fd_vacate(fds[i]);
    for (int i = 0; i < DD_SFD_MAX; i++) // signalfd write ends (engine-private)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr >= 0 && (unsigned)g_sfd[i].wr >= first && (unsigned)g_sfd[i].wr <= last)
            engine_fd_vacate(g_sfd[i].wr);
    for (int i = 0; i < DD_NFD; i++) {
        int p = g_eventfd_peer[i] - 1;
        if (p >= 0 && (unsigned)p >= first && (unsigned)p <= last) eventfd_peer_vacate(p);
    }
    for (int i = 0; i < g_nvols; i++)
        if (g_vols[i].fd >= 0 && (unsigned)g_vols[i].fd >= first && (unsigned)g_vols[i].fd <= last)
            engine_fd_vacate(g_vols[i].fd);
}

static int svc_io(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                  uint64_t a5) {
    // An O_PATH fd names a file but is not open for I/O -- Linux rejects the read/write family through it
    // with EBADF (fs/read_write.c). It stays valid as a dirfd for *at() and for fstat/fchdir (served by
    // svc_fs), so only the I/O syscalls are gated here.
    if ((int)a0 >= 0 && (int)a0 < 1024 && g_opath[(int)a0]) {
        switch (nr) {
        case 63:
        case 64:
        case 65:
        case 66:
        case 67:
        case 68:
        case 69:
        case 70: G_RET(c) = (uint64_t)(int64_t)(-EBADF); return svc_done(c);
        default: break;
        }
    }
    // /dev/full: any write fails ENOSPC (reads are served from the /dev/zero backing). Installers and
    // test suites probe this to check out-of-space handling.
    if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_devfull[(int)a0]) {
        switch (nr) {
        case 64:
        case 66:
        case 68:
        case 70: G_RET(c) = (uint64_t)(int64_t)(-ENOSPC); return svc_done(c);
        default: break;
        }
    }
    // /dev/urandom + /dev/random: Linux accepts writes as entropy seeding and returns the byte count;
    // macOS EPERMs them. Swallow the write (count for write/pwrite; summed iov length for writev/pwritev).
    if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_devseed[(int)a0]) {
        switch (nr) {
        case 64:
        case 68: G_RET(c) = a2; return svc_done(c); // write / pwrite64: count = a2
        case 66:
        case 70: { // writev / pwritev: sum the iovec lengths
            uint64_t tot = 0;
            const struct iovec *iov = (const struct iovec *)a1;
            if (iov)
                for (int i = 0; i < (int)a2; i++)
                    tot += iov[i].iov_len;
            G_RET(c) = tot;
            return svc_done(c);
        }
        default: break;
        }
    }
    // Guest PROT_NONE buffer in the fd-I/O family (fd, BUF=a1, count=a2): dd force-maps guest anon pages
    // host-writable (mem.c case 222) so the host read/write does NOT fault on a guest PROT_NONE page the way
    // Linux's copy_{to,from}_user would. Reject it here with -EFAULT, exactly as Linux. Near-free when no
    // PROT_NONE region exists (g_ngna==0). read/pread WRITE the buffer, write/pwrite READ it; both fault. (read02)
    if (g_ngna) {
        switch (nr) {
        case 63:
        case 64:
        case 67:
        case 68: // read / write / pread64 / pwrite64
            if (gna_hit(a1, a2)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
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
        if (whence == 3)
            whence = 4; // Linux SEEK_DATA -> macOS SEEK_DATA
        else if (whence == 4)
            whence = 3; // Linux SEEK_HOLE -> macOS SEEK_HOLE
        off_t r = lseek((int)a0, (off_t)a1, whence);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
    lseek_out:
        break;
    }
    case 63: {
        int rfd = (int)a0;
        // tee(2) pushback: bytes a prior tee() peeked out of this pipe are re-served here first, in order.
        if (rfd >= 0 && rfd < DD_NFD && g_fd_pb_len[rfd]) {
            G_RET(c) = (uint64_t)pipe_pushback_take(rfd, (void *)a1, (size_t)a2);
            break;
        }
        // AF_NETLINK socket read: busybox `ip` receives its RTNETLINK dump with read(2)/recvmsg;
        // drain our queued reply with the Linux MSG_PEEK/MSG_TRUNC semantics (see netns.c nl_recv).
        if (nl_is(rfd)) {
            struct iovec iov = {(void *)a1, (size_t)a2};
            G_RET(c) = (uint64_t)nl_recv(rfd, &iov, 1, 0, NULL);
            break;
        }
        // RAM-backed scratch file: serve the read from memory. Unlike a host-fd read (whose kernel copyout
        // faults a bad buffer to EFAULT), this copies straight into the guest buffer, so a bad/unmapped
        // pointer must be validated here or the engine memcpy faults (access_ok).
        if (memf_get(rfd)) {
            if (a2 && !host_range_mapped((uintptr_t)a1, (size_t)a2)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            ssize_t r = memf_read_pos(g_memf[rfd], (void *)a1, (size_t)a2);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        // signalfd read -> struct signalfd_siginfo. Each signalfd OFD has its own self-pipe; the fd number
        // (original OR a dup, both mapped by g_sigfd_slot) is the read end, so read straight from rfd.
        if (rfd >= 0 && rfd < DD_NFD && g_sigfd_slot[rfd]) {
            // Linux needs room for at least one struct signalfd_siginfo (128 bytes); a shorter buffer is
            // EINVAL and must NOT consume a pending signal (checked before draining the wake byte).
            if (a2 < 128) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
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
                if (!host_range_mapped((uintptr_t)a1, 128)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                memset((void *)a1, 0, 128);
                *(uint32_t *)a1 = (uint32_t)sig;
                // ssi_signo
            }
            G_RET(c) = 128;
            break;
        }
        // inotify read -> struct inotify_event[]
        if (rfd >= 0 && rfd < 1024 && g_inotify[rfd]) {
            // Linux needs room for at least one struct inotify_event (16-byte header); a shorter buffer is
            // EINVAL and must NOT consume the queued event (checked before any kqueue drain / snapshot diff).
            if (a2 < 16) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // The whole [a1, a1+a2) buffer is written directly by the engine below; validate it up front so a
            // bad/unmapped pointer returns -EFAULT (without consuming events) instead of faulting the engine.
            if (!host_range_mapped((uintptr_t)a1, (size_t)a2)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
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
                if (off > 0) {
                    G_RET(c) = (uint64_t)off;
                    break;
                } // return the moves we already have
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                break;
            }
            for (int i = 0; i < n; i++) {
                int wd = (int)kv[i].ident;
                if (wd >= 0 && wd < 1024 && g_inotify_wpath[wd][0]) {
                    // directory watch: diff current entries against the snapshot -> IN_CREATE/IN_DELETE+name
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) { // pass 0 = created, pass 1 = deleted
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u; // IN_CREATE / IN_DELETE
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
                    if (f & (NOTE_WRITE | NOTE_EXTEND)) m |= 0x2; // IN_MODIFY
                    if (f & NOTE_ATTRIB) m |= 0x4;                // IN_ATTRIB
                    if (f & NOTE_DELETE) m |= 0x400;              // IN_DELETE_SELF
                    if (f & NOTE_RENAME) m |= 0x800;              // IN_MOVE_SELF
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
        if (rfd >= 0 && rfd < DD_NFD && g_timerfd[rfd]) {
            // Linux needs an 8-byte buffer; a shorter read is EINVAL and must NOT drain the expiration
            // (checked before the kqueue drain so the pending tick survives an invalid short read).
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
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
                if (!host_range_mapped((uintptr_t)a1, 8)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                *(uint64_t *)a1 = (uint64_t)kv.data;
            }
            // A periodic timerfd whose first expiry (it_value) differed from its interval was armed as a
            // one-shot for that first tick (event.c case 86). Now that the first tick has been consumed,
            // re-arm the recurring periodic at the interval so subsequent expiries fire every it_interval.
            if (g_tfd_first_oneshot[rfd] && g_tfd_interval[rfd] > 0) {
                struct kevent rkv;
                EV_SET(&rkv, 1, EVFILT_TIMER, EV_ADD, NOTE_NSECONDS, g_tfd_interval[rfd], NULL);
                kevent(rfd, &rkv, 1, NULL, 0, NULL);
                g_tfd_first_oneshot[rfd] = 0;
                struct timespec tnow;
                clock_gettime(CLOCK_MONOTONIC, &tnow);
                g_tfd_deadline[rfd] = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec + g_tfd_interval[rfd];
            }
            G_RET(c) = 8;
            break;
        }
        // eventfd read: return the accumulated counter, reset it, drain the readiness pipe
        if (rfd >= 0 && rfd < DD_NFD && g_eventfd_peer[rfd]) {
            int eslot = eventfd_counter_slot(rfd);
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 (the result counter) is written directly below; reject a bad/NULL pointer here, before any
            // side effect (counter reset / pipe drain). Linux read(eventfd, NULL, 8) is EFAULT, not a
            // silent 8-byte success, so a null pointer must fault too (not just an out-of-range one).
            if (!a1 || !host_range_mapped((uintptr_t)a1, 8)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            // The counter read/reset + pipe drain/re-signal is done atomically under g_eventfd_lock so it
            // never races a concurrent write() (which mutates the same counter+pipe pair) -- see the
            // _eventfd-atomicity_ note. The BLOCKING branch (count==0, not O_NONBLOCK) must wait for a
            // writer's byte OUTSIDE the lock (or it would deadlock the very writer that unblocks it), then
            // re-take the lock and re-check.
            pthread_mutex_lock(&g_eventfd_lock);
            while (g_eventfd_count[eslot] == 0) {
                if (eventfd_guest_nb(rfd)) {
                    // Guest asked for non-blocking. The read end is ALWAYS host-O_NONBLOCK now, so no flag
                    // toggle is needed (the toggle used to race a cross-process reader — the spurious-EAGAIN
                    // bug). Drain any stale readiness byte so a level-triggered epoll won't report the fd
                    // ready-forever, then return EAGAIN.
                    char stale[64];
                    while (read(rfd, stale, sizeof stale) > 0) {}
                    pthread_mutex_unlock(&g_eventfd_lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    goto eventfd_read_done;
                }
                // Guest wants to block. The read end is O_NONBLOCK (a raw read would EAGAIN, not wait), so
                // wait for a writer's 0->positive edge with poll(), then consume one readiness byte.
                pthread_mutex_unlock(&g_eventfd_lock);
                struct pollfd pf = {.fd = rfd, .events = POLLIN, .revents = 0};
                poll(&pf, 1, -1); // block until a writer signals 0->positive
                char b;
                if (read(rfd, &b, 1) < 0) {} // consume one readiness byte (non-blocking; EAGAIN is fine)
                pthread_mutex_lock(&g_eventfd_lock);
            }
            uint64_t v;
            if (g_eventfd_sema[rfd]) {
                v = 1;
                g_eventfd_count[eslot] -= 1;
            } // EFD_SEMAPHORE: one at a time
            else {
                v = g_eventfd_count[eslot];
                g_eventfd_count[eslot] = 0;
            }
            // re-sync the pipe to "counter > 0": drain it, then re-signal one byte if still positive. The
            // read end is permanently O_NONBLOCK, so drain directly with no flag toggle (no cross-process race).
            char buf[64];
            while (read(rfd, buf, sizeof buf) > 0) {}
            if (g_eventfd_count[eslot] > 0) {
                char b = 1;
                if (write(g_eventfd_peer[rfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            if (a1) *(uint64_t *)a1 = v;
            G_RET(c) = 8;
        eventfd_read_done:
            break;
        }
        // /proc/<pid>/pagemap (vfs.c backs it with an empty seekable fd; g_pagemap_fd marks it): synthesize
        // one 64-bit entry per page with the PRESENT bit (63) set. The guest lseek'd to vaddr/pagesize*8 and
        // reads sequentially; advance the real fd offset so its position tracks what we "read" (LTP mmap12).
        if (rfd >= 0 && rfd < DD_NFD && g_pagemap_fd[rfd]) {
            size_t want = (size_t)a2 & ~(size_t)7; // whole 8-byte pagemap entries only
            if (want == 0) {
                G_RET(c) = 0;
                break;
            }
            if (a1 && !host_range_mapped((uintptr_t)a1, want)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint64_t *out = (uint64_t *)a1;
            for (size_t i = 0; i < want / 8; i++)
                out[i] = (1ULL << 63); // PM_PRESENT
            lseek(rfd, (off_t)want, SEEK_CUR);
            G_RET(c) = (uint64_t)want;
            break;
        }
        // SA_RESTART: a blocking read interrupted by a signal whose guest handler asked for restart is
        // resumed in place (the dispatcher runs the handler after the read finally returns); a handler
        // WITHOUT SA_RESTART lets EINTR through. (Well-behaved programs block in poll/select/epoll -- which
        // always return EINTR -- and only read when ready, so this never defers a needed handler.)
        ssize_t r;
        ts_wait_enter(); // 'S' while a read may block (pipe/socket/tty; a ready/regular fd returns at once)
        do {
            r = read(rfd, (void *)a1, (size_t)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        // /dev/tty (or /dev/console) tty semantics: a controlling terminal has no EOF-from-emptiness, so a
        // NONBLOCKING read that came back with 0 bytes ("no input") must be EAGAIN, never EOF -- otherwise
        // readline/TUI/event-loop code reads the 0 as terminal closure and tears the terminal down. dd may
        // back /dev/tty with a host device (or /dev/null for console) that returns 0 when empty; remap it.
        if (r == 0 && a2 > 0 && rfd >= 0 && rfd < DD_NFD && g_devtty[rfd]) {
            int fl = fcntl(rfd, F_GETFL);
            if (fl >= 0 && (fl & O_NONBLOCK)) {
                r = -1;
                errno = EAGAIN;
            }
        }
        // SEQPACKET/O_DIRECT-pipe EOF over a DGRAM backing: a peer-closed read reports ECONNRESET, but the
        // emulated endpoint must return 0 (EOF) like the Linux original. (See netns.c / case 199 / pipe2.)
        if (r < 0 && errno == ECONNRESET && seq_is(rfd)) r = 0;
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 64: {
        int wfd = (int)a0;
        // memfd F_SEAL_WRITE: a write to a write-sealed memfd fails EPERM (emulated seal state).
        if (wfd >= 0 && wfd < DD_NFD && (memfd_seals_fd(wfd) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        }
        // AF_NETLINK socket write: busybox `ip` (libbb) sends its RTM_GET* dump request via
        // write(2), NOT sendto/sendmsg -- so the netlink responder (which only hooked send*) never saw
        // it, no dump was queued, and the follow-up recvmsg blocked forever ("container stuck Up").
        // Route it to nl_send so the dump is synthesized exactly as for the send* path.
        if (nl_is(wfd)) {
            G_RET(c) = (uint64_t)nl_send(wfd, (const uint8_t *)a1, (size_t)a2);
            break;
        }
        // Container DNS: a query write(2)'d on a DNS socket (TCP DNS via write, or a connected-UDP write) is
        // parsed + answered by the host resolver (net.c/netns.c dns_send); nothing reaches the wire.
        if (wfd >= 0 && wfd < DD_NFD && g_dns_sock[wfd]) {
            G_RET(c) = (uint64_t)dns_send(wfd, (const uint8_t *)a1, (size_t)a2, g_sock_stream[wfd]);
            break;
        }
        // RAM-backed scratch file: serve the write from memory (spill to the host file past the cap).
        // Copies straight from the guest buffer, so validate it (a host-fd write's kernel copyin would fault
        // a bad pointer to EFAULT; this engine memcpy would instead crash) --, access_ok.
        if (memf_get(wfd) && memf_room_or_spill(wfd, (off_t)g_memf[wfd]->pos + (off_t)a2)) {
            if (a2 && !host_range_mapped((uintptr_t)a1, (size_t)a2)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            ssize_t r = memf_write_pos(g_memf[wfd], (void *)a1, (size_t)a2);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        // eventfd write: ADD to the counter (not a raw pipe write); regenerate the readable edge.
        if (wfd >= 0 && wfd < DD_NFD && g_eventfd_peer[wfd]) {
            int eslot = eventfd_counter_slot(wfd);
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 is a raw guest pointer we read directly -> validate before the deref (covers NULL too)
            if (!host_range_mapped((uintptr_t)a1, 8)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint64_t add = *(uint64_t *)a1;
            if (add == 0xffffffffffffffffULL) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Counter bump + pipe re-signal held together under g_eventfd_lock so a concurrent read()'s
            // drain (or a peer write) can never strand the pipe readable-with-count-0 / empty-with-count>0
            // (the Chrome pump spin / lost-wakeup root cause -- see the _eventfd-atomicity_ note in vfs.c).
            pthread_mutex_lock(&g_eventfd_lock);
            // Linux caps the counter at ULLONG_MAX-1 (0xfffffffffffffffe). A write that would overflow that
            // maximum does NOT wrap: a nonblocking eventfd returns EAGAIN and leaves the counter unchanged
            // (a blocking one sleeps until a reader makes room -- an extreme edge dd does not model, so it
            // also returns EAGAIN rather than silently wrapping the counter to zero and losing wake state).
            if (add > 0xfffffffffffffffeULL - g_eventfd_count[eslot]) {
                pthread_mutex_unlock(&g_eventfd_lock);
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            g_eventfd_count[eslot] += add;
            // Linux wakes epoll edge-triggered waiters on EVERY write, not just the 0->positive transition.
            // A waker eventfd that is never drained (mio/tokio's cross-thread wakeup) would otherwise lose
            // its 2nd and later wakeups: the backing pipe already holds a byte, so an EV_CLEAR kqueue filter
            // never re-fires and a blocked epoll_wait hangs forever. Drain the pipe to exactly one fresh byte
            // so each write produces a new readable edge, bounded even when the reader never keeps up.
            if (add > 0) {
                // The read end is permanently O_NONBLOCK, so drain to exactly one fresh byte with no flag
                // toggle. The old toggle mutated the cross-process-shared fd flags and a concurrent reader in
                // another process observed the transient O_NONBLOCK -> spurious EAGAIN (the Chrome wake bug).
                char buf[64];
                while (read(wfd, buf, sizeof buf) > 0) {}
                char b = 1;
                if (write(g_eventfd_peer[wfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            G_RET(c) = 8;
            break;
        }
        fd_evict(wfd);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking write in place (see case 63)
        do {
            r = write(wfd, (void *)a1, (size_t)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        if (r >= 0) seq_mark_wrote(wfd); // genuine writer on a SEQPACKET/O_DIRECT-pipe end: may EOF its peer on close
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 65: {
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_fd_pb_len[(int)a0]) { // tee(2) pushback served first
            const struct iovec *iv = (const struct iovec *)a1;
            size_t tot = 0;
            for (int i = 0; i < (int)a2 && (int)a0 < DD_NFD && g_fd_pb_len[(int)a0]; i++) {
                size_t k = pipe_pushback_take((int)a0, iv[i].iov_base, iv[i].iov_len);
                tot += k;
                if (k < iv[i].iov_len) break;
            }
            G_RET(c) = (uint64_t)tot;
            break;
        }
        if (nl_is((int)a0)) { // netlink readv: drain the queued dump into the guest iov
            G_RET(c) = (uint64_t)nl_recv((int)a0, (struct iovec *)a1, (int)a2, 0, NULL);
            break;
        }
        if (memf_get((int)a0)) {
            ssize_t r = memf_preadv(g_memf[(int)a0], (const struct iovec *)a1, (int)a2, -1, 1);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r;       // SA_RESTART: restart a signal-interrupted blocking readv in place (see case 63)
        ts_wait_enter(); // 'S' while readv may block
        do {
            r = readv((int)a0, (void *)a1, (int)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
        // readv
    }
    case 66: {
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (nl_is((int)a0)) { // netlink writev: gather the request iov + queue the dump
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
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_dns_sock[(int)a0]) {
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
            for (int i = 0; i < (int)a2; i++)
                end += iv[i].iov_len;
            if (memf_room_or_spill((int)a0, end)) {
                ssize_t r = memf_pwritev(g_memf[(int)a0], iv, (int)a2, -1, 1);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
                break;
            }
        }
        fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking writev in place (see case 63)
        do {
            r = writev((int)a0, (void *)a1, (int)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        if (r >= 0) seq_mark_wrote((int)a0); // genuine writer on a SEQPACKET/O_DIRECT-pipe end (see netns.c)
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
        do {
            r = pread((int)a0, (void *)a1, (size_t)a2, (off_t)a3);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 68: {
        // pwrite64
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (memf_get((int)a0) && memf_room_or_spill((int)a0, (off_t)a3 + (off_t)a2)) {
            ssize_t r = memf_pwrite(g_memf[(int)a0], (void *)a1, (size_t)a2, (off_t)a3);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pwrite in place (see case 63)
        do {
            r = pwrite((int)a0, (void *)a1, (size_t)a2, (off_t)a3);
        } while (r < 0 && SVC_EINTR_RESTART(c));
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
        if (po && !host_range_mapped((uintptr_t)a2, sizeof(off_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (po) lseek(infd, *po, SEEK_SET);
        char bf[65536];
        size_t tot = 0;
        int rerr = 0; // a read/write error hit with NOTHING transferred yet -> report -errno, not a fake 0
        while (tot < cnt) {
            size_t w = cnt - tot < sizeof bf ? cnt - tot : sizeof bf;
            ssize_t n = read(infd, bf, w);
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
            if (wr < n) break;
        }
        // Linux: once ANY bytes were transferred, sendfile returns that count (a later error surfaces on the
        // next call); an error before the first byte returns -errno.
        if (po) *po += tot;
        G_RET(c) = rerr ? (uint64_t)(-rerr) : (uint64_t)tot;
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
        if (a1 && !host_range_mapped((uintptr_t)a1, sizeof(off_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a3 && !host_range_mapped((uintptr_t)a3, sizeof(off_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
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
        size_t oldlen = (fin >= 0 && fin < DD_NFD) ? g_fd_pb_len[fin] : 0;
        if (oldlen > sizeof sb) oldlen = sizeof sb;
        if (oldlen) memcpy(sb, g_fd_pushback[fin], oldlen);
        size_t pos = oldlen;
        if (oldlen < len) {
            ssize_t kn = read(fin, sb + oldlen, len - oldlen);
            if (kn > 0) pos += (size_t)kn;
        }
        if (pos == 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        } // nothing available (nonblocking)
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
        if (r >= 0 && r < DD_NFD && (int)a0 >= 0 && (int)a0 < DD_NFD) {
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            strcpy(g_proc_text_desc[r], g_proc_text_desc[(int)a0]);
            g_proc_text_ro[r] = g_proc_text_ro[(int)a0]; // dup shares the open file description (dup3/F_DUPFD do too)
            g_pagemap_fd[r] = g_pagemap_fd[(int)a0];
            if (memfd_ensure_fd((int)a0)) {
                g_memfd_is[r] = 1;
                g_memfd_seal[r] = g_memfd_seal[(int)a0];
                memfd_reg_set_fd(r, g_memfd_seal[r]);
            }
            fd_carry_sock(r, (int)a0);
            fd_carry_virt(r, (int)a0); // eventfd/timerfd share the same object across a dup
        }
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
        if (!is_dup2 && (d3flags & ~0x80000u)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // newfd must lie within the guest's descriptor range (the emulated soft RLIMIT_NOFILE); the host fd
        // table is far larger, so a raw dup2/dup3(.., newfd>=cap) would wrongly succeed -> EBADF. (LTP dup201)
        if (newfd < 0 || newfd >= nofile) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // oldfd must be an open descriptor -> EBADF, and (per Linux) checked WITHOUT closing newfd first.
        if (oldfd < 0 || fcntl(oldfd, F_GETFD) < 0) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        memf_materialize((int)a0); // source: a 2nd fd shares the description -> flush RAM cache
        memf_close((int)a1);       // target fd is about to be reused; drop any cache it held
        engine_fd_vacate((int)a1); // move any engine-private fd off the target before dup2 overwrites it
        fd_reset_emul((int)a1);    // dup2 atomically closes newfd -> shed ALL its emulation tables (timerfd/
                                   // eventfd/inotify/epoll/sock/...) so the reused number isn't left misrouted; the
                                   // real close is dup2's, and fd_carry_sock below repopulates from oldfd
        int r = dup2((int)a0, (int)a1);
        if (r >= 0) {
            if (d3flags & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC); // O_CLOEXEC
            if ((int)a1 >= 0 && (int)a1 < DD_NFD && (int)a0 >= 0 && (int)a0 < DD_NFD) {
                strcpy(g_fdpath[(int)a1], g_fdpath[(int)a0]);
                strcpy(g_proc_text_desc[(int)a1], g_proc_text_desc[(int)a0]);
                g_proc_text_ro[(int)a1] = g_proc_text_ro[(int)a0];
                g_pagemap_fd[(int)a1] = g_pagemap_fd[(int)a0];
                if (memfd_ensure_fd((int)a0)) {
                    g_memfd_is[(int)a1] = 1;
                    g_memfd_seal[(int)a1] = g_memfd_seal[(int)a0];
                    memfd_reg_set_fd((int)a1, g_memfd_seal[(int)a1]);
                }
                fd_carry_sock((int)a1, (int)a0);
                fd_carry_virt((int)a1, (int)a0); // eventfd/timerfd share the same object across a dup
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
            if (floor < 0 || floor >= guest_nofile_cur()) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
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
            if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_proc_text_ro[(int)a0]) lf = 0;
            char fgetpath_buf[4096] = {0};
            int have_fgetpath = 0;
#ifdef F_GETPATH
            if ((lf & 0x3) && fcntl((int)a0, F_GETPATH, fgetpath_buf) == 0) {
                have_fgetpath = 1;
                if (proc_text_host_path(fgetpath_buf)) lf &= ~0x3;
            }
#endif
            if (r & 0x8) lf |= 0x400;
            if (r & 0x4) lf |= 0x800;
            // APPEND/NONBLOCK/ASYNC
            if (r & 0x40) lf |= 0x2000;
            // eventfd: the host read end is kept permanently O_NONBLOCK internally, so report the guest's
            // OWN blocking/non-blocking intent (g_eventfd_gnb), not the host flag. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_eventfd_peer[(int)a0]) {
                lf = eventfd_guest_nb((int)a0) ? (lf | 0x800) : (lf & ~0x800);
            }
            int proc_text_for_log = ((int)a0 >= 0 && (int)a0 < DD_NFD && g_proc_text_ro[(int)a0]) ||
                                    (have_fgetpath && proc_text_host_path(fgetpath_buf));
            if (getenv("DD_FATALSIG_LOG") && proc_text_for_log) {
                char p[4096] = {0};
                if (have_fgetpath) {
                    snprintf(p, sizeof p, "%s", fgetpath_buf);
                } else {
                    (void)fcntl((int)a0, F_GETPATH, p);
                }
                fprintf(stderr, "[DDFCNTL] pid=%d cpid=%d fd=%d mflags=0x%x lflags=0x%x path=%s\n",
                        getpid(), container_pid(), (int)a0, r, lf, p);
            }
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
            // eventfd: record the guest's blocking/non-blocking intent in the shadow and NEVER clear the
            // host read end's O_NONBLOCK (the internal drains rely on it; clearing it would let a drain
            // block). Other flag changes still apply to the host fd. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < DD_NFD && g_eventfd_peer[(int)a0]) {
                g_eventfd_gnb[(int)a0] = (la & 0x800) != 0;
                mf |= 0x4; // keep host O_NONBLOCK on regardless of the guest's request
            }
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
            if ((int)a0 < 0 || fcntl((int)a0, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            // The Linux struct flock at a2 (fields up to lf+24) is read directly and written back for F_GETLK;
            // validate the 32-byte struct before any deref so a bad pointer returns -EFAULT, not a crash. A guest
            // PROT_NONE flock buffer (LTP fcntl13 uses one) is force-mapped host-writable by dd, but
            // host_range_mapped rejects it via its internal gna_hit check so it still EFAULTs like Linux.
            if (!host_range_mapped((uintptr_t)a2, 32)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            // l_whence must be SEEK_SET/SEEK_CUR/SEEK_END; Linux rejects anything else with EINVAL in
            // flock_to_posix_lock -- BEFORE the fd type is consulted, so it applies to a pipe fd too. (LTP fcntl13)
            {
                short whence = *(short *)(lf + 2);
                if (whence != 0 && whence != 1 && whence != 2) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
            }
            // service advisory byte-range locks on regular files from the in-engine cross-process
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
                            if ((p & (1ull << s)) && !(c->sigmask & (1ull << (s - 1)))) {
                                intr = 1;
                                break;
                            }
                        if (intr) {
                            G_RET(c) = (uint64_t)(-EINTR);
                            break;
                        }
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
        // requested size (rounded up to a page, >= requested) and report it back on GET. Linux's pipe_fcntl
        // first rejects a non-pipe object with EBADF (and an invalid fd faults out even earlier), so validate
        // the fd is a real FIFO before fabricating a size -- otherwise a regular file/socket or bad fd was
        // reported as a pipe with a plausible size.
        if (lcmd == 1031 || lcmd == 1032) {
            struct stat pst;
            if (fstat((int)a0, &pst) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (!S_ISFIFO(pst.st_mode)) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
        }
        if (lcmd == 1031) {
            int want = (int)a2;
            long pg = sysconf(_SC_PAGESIZE);
            if (pg <= 0) pg = 4096;
            int rounded = (int)(((want + pg - 1) / pg) * pg);
            if (rounded < (int)pg) rounded = (int)pg;
            if ((int)a0 >= 0 && (int)a0 < DD_NFD) g_pipesz[(int)a0] = rounded;
            G_RET(c) = (uint64_t)(unsigned)rounded;
            break;
        }
        if (lcmd == 1032) {
            int sz = ((int)a0 >= 0 && (int)a0 < DD_NFD && g_pipesz[(int)a0]) ? g_pipesz[(int)a0] : 65536;
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
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= DD_NFD || !g_memfd_is[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (g_memfd_seal[fd] & 0x1) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            } // already F_SEAL_SEAL'd
            g_memfd_seal[fd] |= (int)a2 & 0x1f; // SEAL|SHRINK|GROW|WRITE|FUTURE_WRITE
            memfd_reg_set_fd(fd, g_memfd_seal[fd]);
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1034) { // F_GET_SEALS(fd)
            int fd = (int)a0;
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= DD_NFD || !g_memfd_is[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_memfd_seal[fd];
            break;
        } else if (lcmd == 1025) { // F_GETLEASE: report the tracked lease for this fd (F_UNLCK if none).
            // Returning a fixed value fabricated/erased lease state; consult g_lease so F_GETLEASE round-trips
            // whatever F_SETLEASE last set on this fd. Encoding: g_lease[fd] = type+1, 0 = no lease -> F_UNLCK(2).
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            int held = (fd < DD_NFD && g_lease[fd]) ? g_lease[fd] - 1 : 2; // stored type, else F_UNLCK
            G_RET(c) = (uint64_t)(unsigned)held;
            break;
        } else if (lcmd == 1024) { // F_SETLEASE(fd, F_RDLCK|F_WRLCK|F_UNLCK)
            int fd = (int)a0, arg = (int)a2;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (arg != 0 && arg != 1 && arg != 2) { // not F_RDLCK/F_WRLCK/F_UNLCK -> EINVAL (Linux)
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            struct stat lst;
            if (fstat(fd, &lst) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (!S_ISREG(lst.st_mode)) { // leases are only for regular files (Linux: EINVAL)
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // A read lease (F_RDLCK) may only be taken on a descriptor NOT open for writing -- Linux
            // generic_add_lease returns EAGAIN when the inode has a writer, and the requesting fd itself
            // counts (an O_RDWR/O_WRONLY fd -> EAGAIN). This single-fd check matches the kernel exactly for
            // the common case. A write lease (F_WRLCK) requires the fd be the SOLE opener; dd cannot
            // enumerate other openers across guest processes, so it is tracked but its BREAK on a conflicting
            // open is never delivered (see syscall-compat.md). Both states round-trip through F_GETLEASE.
            if (arg == 0) { // F_RDLCK
                int fl = fcntl(fd, F_GETFL);
                if (fl >= 0 && (fl & O_ACCMODE) != O_RDONLY) {
                    G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
                    break;
                }
            }
            if (fd < DD_NFD) g_lease[fd] = (arg == 2) ? 0 : (int8_t)(arg + 1); // F_UNLCK clears
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1026) { // F_NOTIFY(fd, DN_* mask): arm a real kqueue directory-change watch.
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            int sig = (fd < DD_NFD && g_fsig[fd]) ? g_fsig[fd] : 0; // F_SETSIG override, else default SIGIO
            G_RET(c) = (uint64_t)(int64_t)dnotify_apply(fd, (uint32_t)a2, sig);
            break;
        } else if (lcmd == 10) { // F_SETSIG(fd, signo): record the signal for O_ASYNC/dnotify on this fd.
            int fd = (int)a0, sig = (int)a2;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (sig < 0 || sig > 64) { // 0 restores the SIGIO default; anything above the signal range is EINVAL
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (fd < DD_NFD) g_fsig[fd] = (uint8_t)sig;
            G_RET(c) = 0;
            break;
        } else if (lcmd == 11) { // F_GETSIG(fd): the signal set by F_SETSIG (0 = default SIGIO).
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)((fd < DD_NFD) ? g_fsig[fd] : 0);
            break;
        }
        // A command this kernel does not recognize is EINVAL (Linux do_fcntl default), NOT forwarded to
        // macOS -- whose fcntl cmd numbering DIVERGES, so a stray Linux cmd# would mean a different op there.
        // Everything valid was handled above or is one of these benign pass-throughs; reject the rest. (LTP
        // fcntl13 F_BADCMD=999.) 10/11=SETSIG/GETSIG, 15/16/17=SET/GETOWN_EX+GETOWNER_UIDS, 36/37/38=OFD locks.
        switch (lcmd) {
        case 0:
        case 1:
        case 2:
        case 8:
        case 9:
        case 10:
        case 11:
        case 15:
        case 16:
        case 17:
        case 36:
        case 37:
        case 38:
        case 1030: break; // recognized Linux command -> proceed to the host fcntl
        default: G_RET(c) = (uint64_t)(-EINVAL); goto fcntl_done;
        }
        int r = fcntl((int)a0, mcmd, a2);
        if (lcmd == 0 || lcmd == 1030) r = nofile_gate(r); // F_DUPFD(_CLOEXEC): EMFILE past the guest fd cap
        if (r >= 0 && (lcmd == 0 || lcmd == 1030) && r < DD_NFD && (int)a0 >= 0 && (int)a0 < DD_NFD) {
            // F_DUPFD(_CLOEXEC)
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            strcpy(g_proc_text_desc[r], g_proc_text_desc[(int)a0]);
            g_proc_text_ro[r] = g_proc_text_ro[(int)a0];
            g_pagemap_fd[r] = g_pagemap_fd[(int)a0];
            fd_carry_sock(r, (int)a0);
            fd_carry_virt(r, (int)a0); // eventfd/timerfd share the same object across a dup
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
        if (a2 && !host_range_mapped((uintptr_t)a2, sizeof(int))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int on = a2 ? *(int *)a2 : 0, fl = fcntl((int)a0, F_GETFL);
        if (fl < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
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
        if (!host_range_mapped((uintptr_t)a0, 2 * sizeof(int))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int mk = (fl & G_O_DIRECT) ? socketpair(AF_UNIX, SOCK_DGRAM, 0, fds) : pipe(fds);
        if (mk < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // Either new fd past the guest's soft RLIMIT_NOFILE -> EMFILE (the host table is far larger). Close
        // both so no descriptor leaks, exactly as Linux fails a pipe2 that would exceed the limit.
        {
            int cap = guest_nofile_cur();
            if (fds[0] >= cap || fds[1] >= cap) {
                close(fds[0]);
                close(fds[1]);
                G_RET(c) = (uint64_t)(-EMFILE);
                break;
            }
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
            if (fds[0] >= 0 && fds[0] < DD_NFD) {
                g_sock_seqpacket[fds[0]] = 1;
                g_sock_pair_peer[fds[0]] = fds[1] + 1; // partner end (see seq_send_eof)
            }
            if (fds[1] >= 0 && fds[1] < DD_NFD) {
                g_sock_seqpacket[fds[1]] = 1;
                g_sock_pair_peer[fds[1]] = fds[0] + 1;
            }
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
        if (poi && !host_range_mapped((uintptr_t)a1, sizeof(off_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (poo && !host_range_mapped((uintptr_t)a3, sizeof(off_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
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
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
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
            for (int i = 0; i < (int)a2; i++)
                end += iv[i].iov_len;
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
    case 84:
        G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0);
        break; // sync_file_range -> fsync (no-op for RAM scratch)
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
