#ifndef G_IS_DUP2_COMPAT
#define G_IS_DUP2_COMPAT() 0 /* aarch64 guests have no legacy dup2; every case 24 is a real dup3 */
#endif
// Extracted from service(): I/O — fd read/write/seek + plain fd ops
// (dup/dup3/fcntl/pipe2/sendfile/splice/tee/copy_file_range/fsync/etc). Returns 1 if nr was handled, 0 otherwise.
// Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).

// Whether this host could actually put an eventfd's readiness-pipe READ END into O_NONBLOCK. It is a
// property of the host, not of a descriptor, so one scalar answers for every eventfd: the creation path
// (eventfd2) issues one F_SETFL and records whether it was honoured. Hosts whose descriptors carry a
// status-flag channel record 1 here and keep the byte-identical drain/wait they always had.
//
// Where it is 0 the difference is not a nicety. The emulation's drains are written as "read until the read
// returns <= 0", which terminates only because the read end is non-blocking; on a host that cannot set that
// flag the very first drain of an EMPTY pipe blocks the calling thread forever, and every eventfd read()
// performs a drain -- so a guest that merely writes 1 and reads it back never returns. Optimistic default:
// a host that never creates an eventfd never touches this, and the first eventfd2 corrects it.
static int g_eventfd_readend_nb = 1;

// Remove the eventfd readiness byte, if one is there.  `signalled` is the caller's knowledge of whether the
// pipe holds a byte right now, and it is exact rather than a guess: every mutation of the {counter, pipe}
// pair is made under g_eventfd_lock and every one of them leaves the pipe holding AT MOST ONE byte, present
// exactly when the counter is positive (creation writes one iff initval > 0; the write path drains and
// re-writes one; the read path drains and re-writes one iff the counter is still positive). So on a host
// with no non-blocking read end, "counter was positive on entry" is precisely "there is one byte to take",
// and taking exactly that many bytes cannot block.
//
// The two hosts differ only in HOW the same bytes are removed, never in how many end up removed, so a
// descriptor whose read end really is non-blocking follows the loop it always followed.
//
// ONE NAMED RESIDUAL, and it is bounded to a host that has neither half of the pair. g_eventfd_lock is
// process-private, so the "at most one byte" invariant is only best-effort across a fork -- two processes
// sharing the counter and the inherited pipe can both read a positive counter and both try to take the one
// byte, and on a blocking read end the loser parks instead of seeing EAGAIN. That cannot happen where the
// read end is non-blocking, which is every host that shares an eventfd across a fork today; the counter's
// cross-process sharing was already documented as best-effort for the same reason.
static void eventfd_drain_readiness(int rfd, int signalled) {
    char buffer[64];
    if (g_eventfd_readend_nb) {
        while (read(rfd, buffer, sizeof buffer) > 0) {}
        return;
    }
    if (signalled && read(rfd, buffer, 1) < 0) {}
}

static int eventfd_peer_owner(int fd) {
    if (fd < 0) return -1;
    for (int i = 0; i < HL_NFD; i++)
        if (g_eventfd_peer[i] == fd + 1) return i;
    return -1;
}

static int eventfd_peer_is_engine_fd(int fd) {
    return eventfd_peer_owner(fd) >= 0;
}

static int proc_text_replace(int descriptor, const char *text, size_t size) {
    size_t written = 0;
    while (written < size) {
        ssize_t result = pwrite(descriptor, text + written, size - written, (off_t)written);
        if (result < 0) {
            if (errno == EINTR) continue;
            return -errno;
        }
        if (result == 0) return -EIO;
        written += (size_t)result;
    }
    if (ftruncate(descriptor, (off_t)size) != 0) return -errno;
    if (lseek(descriptor, (off_t)size, SEEK_SET) < 0) return -errno;
    return 0;
}

static void eventfd_peer_vacate(int fd) {
    int owner = eventfd_peer_owner(fd);
    if (owner < 0) return;
    int hi = fcntl(fd, F_DUPFD, 1 << 20);
    if (hi < 0) hi = fcntl(fd, F_DUPFD, 64);
    if (hi >= 0 && hi != fd) {
        g_eventfd_peer[owner] = hi + 1;
        close(fd);
    }
}

// Carry hl's virtual-fd emulation state from oldfd to newfd on dup/dup2/dup3/F_DUPFD. Linux duplicated
// descriptors refer to the SAME open file description, so a dup'd eventfd/timerfd must share the underlying
// object. The host dup already shares the backing pipe/kqueue; these tables are what route the guest's
// read/write to the virtual handler, so without carrying them the duplicate degraded to a raw pipe/fd.
static int fd_virt_reserve(int oldfd, struct fdvis_reservation *reservation) {
    memset(reservation, 0, sizeof *reservation);
    if (oldfd < 0 || oldfd >= HL_NFD || g_pipe_identity[oldfd] == 0) return 0;
    return proc_fdvis_reserve(reservation);
}

static int fd_virt_reserve_at(int oldfd, int newfd, struct fdvis_reservation *reservation) {
    memset(reservation, 0, sizeof *reservation);
    if (oldfd < 0 || oldfd >= HL_NFD || g_pipe_identity[oldfd] == 0) return 0;
    return proc_fdvis_reserve_at(newfd, reservation);
}

static void fd_carry_virt(int newfd, int oldfd, struct fdvis_reservation *reservation) {
    if (newfd < 0 || newfd >= HL_NFD || oldfd < 0 || oldfd >= HL_NFD || newfd == oldfd) return;
    // Tag both fds as the same open file description so a later close of one (while the other survives) can
    // find the surviving alias -- e.g. epoll readiness must persist while a dup keeps the watched OFD open.
    ofd_link_dup(newfd, oldfd);
    hl_native_kqueue_duplicate(oldfd, newfd);
    // A host handle is per open file description, so the duplicate needs its own
    // reference to the same description -- which is what the file group's
    // clone_for_fork produces. Failure is not an error here: the new descriptor
    // simply has no binding, which is the state every descriptor is in on a host
    // where nothing publishes, and every consumer already treats as "ambient".
    (void)hl_fdhandle_clone(oldfd, newfd);
    // Synthetic character devices keep their Linux behavior across descriptor duplication. Shell
    // redirections open the target and dup2 it onto stdout before writing; dropping these tags made
    // `echo x > /dev/full` write successfully to the /dev/zero backing instead of failing ENOSPC.
    g_devfull[newfd] = g_devfull[oldfd];
    g_devseed[newfd] = g_devseed[oldfd];
    g_devtty[newfd] = g_devtty[oldfd];
    mq_fd_duplicate(newfd, oldfd);
    if (g_pipe_identity[oldfd] != 0) {
        g_pipe_identity[newfd] = g_pipe_identity[oldfd];
        proc_fdvis_reservation_publish(reservation, newfd, HL_HOST_FD_PIPE, 1, g_pipe_identity[newfd]);
    }
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
        int slot = timerfd_slot(oldfd);
        g_timerfd[newfd] = 1;
        g_tfd_deadline[newfd] = g_tfd_deadline[oldfd];
        g_tfd_interval[newfd] = g_tfd_interval[oldfd];
        g_tfd_first_oneshot[newfd] = g_tfd_first_oneshot[oldfd];
        g_tfd_clock[newfd] = g_tfd_clock[oldfd];
        g_tfd_object[newfd] = g_tfd_object[oldfd];
        g_tfd_nb[newfd] = g_tfd_nb[oldfd];
        g_tfd_shared[newfd] = g_tfd_shared[oldfd];
        g_tfd_cslot[newfd] = slot + 1;
        g_tfd_refs[slot]++;
    }
    // inotify: the instance is a (host-shared) kqueue with its watches; carry the routing flag so the dup's
    // read() drains the same event queue. Watches stay owned by the original instance fd -- closing the DUP
    // tears down nothing (no watch is owned by it), and closing the original behaves as before the dup.
    if (oldfd < 1024 && newfd < 1024 && g_inotify[oldfd]) {
        g_inotify[newfd] = 1;
        g_inotify_nb[newfd] = g_inotify_nb[oldfd];
        g_inotify_object[newfd] = g_inotify_object[oldfd];
    }
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
        g_ep_cslot[newfd] = (uint16_t)(epoll_slot(oldfd) + 1);
        if (!g_ep_cslot[oldfd]) g_ep_cslot[oldfd] = (uint16_t)(oldfd + 1);
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

// x86-64 exposes its historical kernel O_LARGEFILE bit through F_GETFL. Native AArch64 does not: its
// 64-bit ABI defines libc O_LARGEFILE as zero and F_GETFL on files and sockets omits the asm-generic
// 0x20000 value. Inventing that bit makes a valid F_GETFL/F_SETFL round trip fail Chromium's seccomp mask.
#if G_O_DIRECTORY == 0x10000
#define G_O_LARGEFILE 0x8000 // x86-64
#else
#define G_O_LARGEFILE 0 // aarch64
#endif

/* FUSE/shared host mounts may expose regular I/O but reject sparse seeking. Keep Linux guest semantics
 * available there by finding logical zero/data runs; native filesystem extents remain preferred. */
static off_t sparse_seek_fallback(int fd, off_t offset, int guest_whence) {
    unsigned char bytes[16384];
    struct stat metadata;
    off_t cursor = offset;
    int want_data = guest_whence == 3;
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (fstat(fd, &metadata) != 0) return -1;
    if (!S_ISREG(metadata.st_mode)) {
        errno = EINVAL;
        return -1;
    }
    if (offset >= metadata.st_size) {
        errno = ENXIO;
        return -1;
    }
    while (cursor < metadata.st_size) {
        size_t amount =
            (uint64_t)(metadata.st_size - cursor) < sizeof(bytes) ? (size_t)(metadata.st_size - cursor) : sizeof(bytes);
        ssize_t count = pread(fd, bytes, amount, cursor);
        if (count <= 0) {
            if (count == 0) errno = ENXIO;
            return -1;
        }
        for (ssize_t index = 0; index < count; ++index)
            if ((bytes[index] != 0) == want_data) return cursor + index;
        cursor += count;
    }
    if (!want_data) return metadata.st_size;
    errno = ENXIO;
    return -1;
}

// In hl's in-process exec model the guest shares the host descriptor table (fds are 1:1), and the engine
// pins private host fds at LOW numbers (g_root_fd -- every path resolution openat()s off it -- plus the
// signalfd pipe and each bind-mount volume fd). A guest dup2/dup3 onto one of those low
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
    if (hi >= 0) {
        *slot = hi;
        close(newfd);
    }
}

// ---- F_SETLEASE lease registry + F_NOTIFY (dnotify) directory-change monitor --------------------
// macOS has neither file leases nor dnotify, so a bare success armed nothing. We emulate as far as the host
// allows:
//   * F_SETLEASE/F_GETLEASE: validate arguments exactly like Linux (fcntl setlease) and track the lease
//     type per fd so F_GETLEASE round-trips what F_SETLEASE set. RESIDUAL: the lease-BREAK signal on a
//     conflicting cross-process open is NOT delivered -- macOS gives no rootless hook to intercept another
//     opener of the same file. Documented in syscall-compat.md.
//   * F_NOTIFY: backed by the host directory-watch primitive, drained on a lazily-spawned
//     thread that raises the requested signal (F_SETSIG signal, else the SIGIO default) in the guest -- the
//     same async delivery path POSIX timers/timerfd use (g_pending + the signalfd wake). One-shot by
//     default; re-armed each event only when DN_MULTISHOT is set.
#define DN_SIG_DEFAULT 29 // Linux SIGIO
#ifndef DN_MULTISHOT
#define DN_MULTISHOT 0x80000000u
#endif
#define DN_VALID (1u | 2u | 4u | 8u | 16u | 32u | DN_MULTISHOT) // ACCESS/MODIFY/CREATE/DELETE/RENAME/ATTRIB

static int8_t g_lease[HL_NFD];     // 0 = no lease; else lease type + 1 (F_RDLCK 0->1, F_WRLCK 1->2, F_UNLCK 2->3)
static uint8_t g_fsig[HL_NFD];     // per-fd F_SETSIG signal (0 = default); consulted by O_ASYNC + dnotify
static uint32_t g_dn_mask[HL_NFD]; // per-fd active dnotify mask (0 = no watch)
static uint8_t g_dn_sig[HL_NFD];   // signal captured for this fd's dnotify watch at arm time

static hl_host_directory g_dn_directory;
static pthread_t g_dn_thr;
static int g_dn_thr_up;
static pthread_mutex_t g_dn_lk = PTHREAD_MUTEX_INITIALIZER;

// dnotify drain thread: block on the host directory watcher and raise the armed signal in the guest.
static void *dn_loop(void *arg) {
    (void)arg;
    for (;;) {
        uint64_t token;
        int n = hl_host_directory_wait(&g_dn_directory, &token);
        if (n < 0) {
            if (errno == EINTR) continue;
            break; // watcher closed -> thread exits
        }
        if (n == 0) continue;
        int fd = (int)token;
        if (fd < 0 || fd >= HL_NFD) continue;
        pthread_mutex_lock(&g_dn_lk);
        uint32_t mask = g_dn_mask[fd];
        int sig = g_dn_sig[fd] ? g_dn_sig[fd] : DN_SIG_DEFAULT;
        if (mask && !(mask & DN_MULTISHOT)) { // one-shot: consume the watch (Linux re-arm is explicit)
            (void)hl_host_directory_remove(&g_dn_directory, token);
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

// per-process timers/dnotify are NOT inherited across fork(): a forked child's inherited watcher + drain
// thread are dead, so reset the dnotify table so the child re-arms cleanly on its own first F_NOTIFY.
static void dn_atfork_child(void) {
    hl_host_directory_abandon(&g_dn_directory);
    g_dn_thr_up = 0;
    memset(g_dn_mask, 0, sizeof g_dn_mask);
    pthread_mutex_init(&g_dn_lk, NULL);
}

// Lazily bring up the shared directory watcher + drain thread. Caller holds g_dn_lk. Returns 0 / -errno.
static int dn_init(void) {
    static int reg = 0;
    if (!reg) {
        pthread_atfork(NULL, NULL, dn_atfork_child);
        reg = 1;
    }
    if (g_dn_directory.state == NULL && hl_host_directory_init(&g_dn_directory) != 0) return -errno;
    if (!g_dn_thr_up) {
        if (pthread_create(&g_dn_thr, NULL, dn_loop, NULL) != 0) return -EAGAIN;
        g_dn_thr_up = 1;
    }
    return 0;
}

// fcntl(fd, F_NOTIFY, mask): arm/replace/remove a dnotify watch on the (directory) fd. mask 0 removes it.
static int dnotify_apply(int fd, uint32_t mask, int sig) {
    if (fd < 0 || fd >= HL_NFD) return -EBADF;
    if (mask & ~DN_VALID) return -EINVAL;
    pthread_mutex_lock(&g_dn_lk);
    int rc = 0;
    if (mask == 0) { // remove the watch
        if (g_dn_mask[fd]) {
            (void)hl_host_directory_remove(&g_dn_directory, (uint64_t)fd);
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
    uint32_t interests = mask & ~DN_MULTISHOT;
    if (hl_host_directory_set(&g_dn_directory, fd, (uint64_t)fd, interests) != 0) {
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
    // signalfd write ends are engine-private; a guest dup2/dup3 onto one must relocate it (the read ends are
    // guest fds and are NOT relocated -- a dup2 onto a signalfd read end legitimately replaces that signalfd).
    for (int i = 0; i < HL_SFD_MAX; i++)
        if (g_sfd[i].refs > 0) engine_fd_reloc(&g_sfd[i].wr, newfd);
    (void)hl_host_directory_relocate(&g_dn_directory, newfd);
    for (int i = 0; i < g_nvols; i++)
        engine_fd_reloc(&g_vols[i].fd, newfd);
}

// Vacate every engine-private fd whose NUMBER falls in [first,last] -- for a guest close_range() that would
// otherwise close the runtime's descriptors (g_root_fd etc.). Visible to fs.c/rare.c (io.c is #included first).
static void engine_fd_vacate_range(unsigned first, unsigned last) {
    int fds[2] = {g_root_fd, hl_host_directory_descriptor(&g_dn_directory)};
    for (int i = 0; i < 2; i++)
        if (fds[i] >= 0 && (unsigned)fds[i] >= first && (unsigned)fds[i] <= last) engine_fd_vacate(fds[i]);
    for (int i = 0; i < HL_SFD_MAX; i++) // signalfd write ends (engine-private)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr >= 0 && (unsigned)g_sfd[i].wr >= first && (unsigned)g_sfd[i].wr <= last)
            engine_fd_vacate(g_sfd[i].wr);
    for (int i = 0; i < HL_NFD; i++) {
        int p = g_eventfd_peer[i] - 1;
        if (p >= 0 && (unsigned)p >= first && (unsigned)p <= last) eventfd_peer_vacate(p);
    }
    for (int i = 0; i < g_nvols; i++)
        if (g_vols[i].fd >= 0 && (unsigned)g_vols[i].fd >= first && (unsigned)g_vols[i].fd <= last)
            engine_fd_vacate(g_vols[i].fd);
}

// Enforce the guest soft RLIMIT_FSIZE on a write to a regular file at absolute offset `pos` (SEEK_CUR for
// pos < 0). Linux (generic_write_check_limits): if the limit is finite and the start position is already
// at/beyond it, the write raises SIGXFSZ and returns -EFBIG; if the write would straddle the limit it is
// clamped to what fits. Returns the number of bytes the write is allowed to proceed with (0..count), or a
// negative -errno after queuing the signal. No cost when the limit is infinite (the common case).
static int64_t fsize_gate(struct cpu *c, int fd, off_t pos, uint64_t count) {
    uint64_t limit = guest_fsize_cur();
    if (limit == ~UINT64_C(0) || count == 0) return (int64_t)count;
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) return (int64_t)count; // only regular files are bounded
    if (pos < 0) {
        pos = lseek(fd, 0, SEEK_CUR);
        if (pos < 0) return (int64_t)count; // non-seekable: let the host write proceed
    }
    if ((uint64_t)pos >= limit) {
        raise_guest_signal(c, 25); // SIGXFSZ
        return -EFBIG;
    }
    uint64_t room = limit - (uint64_t)pos;
    return count > room ? (int64_t)room : (int64_t)count; // clamp a straddling write to the limit
}

// RLIMIT_FSIZE gate for a RAM-backed (memf) write. A memf file is an unlinked-while-open regular file, so
// Linux enforces the file-size limit on it exactly as for an on-disk file -- but the memf write paths serve
// the write from RAM and never reach fsize_gate (which fstat's a host fd). Mirror the same contract off the
// memf write position `pos`: at/beyond the limit -> raise SIGXFSZ and return -EFBIG; a straddling write is
// clamped to what fits. Infinite limit (the common case) is a pass-through.
static int64_t memf_fsize_gate(struct cpu *c, off_t pos, uint64_t count) {
    uint64_t limit = guest_fsize_cur();
    if (limit == ~UINT64_C(0) || count == 0 || pos < 0) return (int64_t)count;
    if ((uint64_t)pos >= limit) {
        raise_guest_signal(c, 25); // SIGXFSZ
        return -EFBIG;
    }
    uint64_t room = limit - (uint64_t)pos;
    return count > room ? (int64_t)room : (int64_t)count;
}

static ssize_t io_guest_vector_gather(uint64_t address, size_t count, void *output, size_t capacity) {
    struct iovec vectors[1024];
    if (count > 1024 || guest_iov_import(address, count, vectors) < 0) return -EFAULT;
    size_t done = 0;
    for (size_t index = 0; index < count && done < capacity; ++index) {
        size_t amount = vectors[index].iov_len;
        if (amount > capacity - done) amount = capacity - done;
        ssize_t copied =
            guest_copy_from((uint8_t *)output + done, (uint64_t)(uintptr_t)vectors[index].iov_base, amount);
        if (copied <= 0 && amount != 0) return done != 0 ? (ssize_t)done : -EFAULT;
        done += (size_t)copied;
        if ((size_t)copied != amount) break;
    }
    return (ssize_t)done;
}

static ssize_t io_guest_vector_scatter(uint64_t address, size_t count, const void *input, size_t length) {
    struct iovec vectors[1024];
    if (count > 1024 || guest_iov_import(address, count, vectors) < 0) return -EFAULT;
    size_t done = 0;
    for (size_t index = 0; index < count && done < length; ++index) {
        size_t amount = vectors[index].iov_len;
        if (amount > length - done) amount = length - done;
        ssize_t copied =
            guest_copy_to((uint64_t)(uintptr_t)vectors[index].iov_base, (const uint8_t *)input + done, amount);
        if (copied <= 0 && amount != 0) return done != 0 ? (ssize_t)done : -EFAULT;
        done += (size_t)copied;
        if ((size_t)copied != amount) break;
    }
    return (ssize_t)done;
}

static int svc_io(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                  uint64_t a5) {
    /*
     * Linux resolves the descriptor before importing a scatter/gather vector.
     * Keep that errno precedence before guest_iov_import(): otherwise a closed
     * in-range descriptor can turn readv(fd, ...) into EFAULT even when the
     * vector itself is valid. Guest descriptors are real host descriptors.
     */
    if ((nr == 65 || nr == 66 || nr == 69 || nr == 70) &&
        ((int64_t)a0 < 0 || a0 >= HL_NFD || fcntl((int)a0, F_GETFD) == -1)) {
        G_RET(c) = (uint64_t)(int64_t)(-EBADF);
        return svc_done(c);
    }
    // Scatter/gather iovcnt bound (readv/writev/preadv/pwritev). Linux (fs/read_write.c) rejects nr_segs
    // above UIO_MAXIOV(1024) with -EINVAL before touching the iovec array. The plain host path delegates
    // this check to the kernel, but the engine's RAM-backed-file (memf) and emulated-socket paths read the
    // iovec array directly and would otherwise walk past its end -- a >IOV_MAX or negative iovcnt (which
    // arrives as a huge unsigned value) then sums wild lengths and mis-reports EFBIG/0 instead of EINVAL.
    // nr_segs is an unsigned long in the kernel, so the unsigned `a2 > 1024` test also captures negatives.
    // The kernel looks up the fd first, so a bad fd must still win with EBADF: gate on the fd being open.
    if ((nr == 65 || nr == 66 || nr == 69 || nr == 70) && a2 > 1024 && (int)a0 >= 0 && fcntl((int)a0, F_GETFD) != -1) {
        G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        return svc_done(c);
    }
    // Empty scatter/gather (iovcnt == 0). Linux (fs/read_write.c: import_iovec with nr_segs 0) transfers
    // nothing and returns 0 for readv/writev/preadv/pwritev. The plain host path forwards to the host libc,
    // and BSD/macOS readv/writev reject iovcnt 0 with EINVAL -- a host-passthrough divergence from the Linux
    // ABI. Emulate the Linux zero return directly once the fd is confirmed open (a bad fd must still win with
    // EBADF, which the host path below reports on either kernel).
    if ((nr == 65 || nr == 66 || nr == 69 || nr == 70) && a2 == 0 && (int)a0 >= 0 && fcntl((int)a0, F_GETFD) != -1) {
        G_RET(c) = 0;
        return svc_done(c);
    }
    // Scatter/gather segment address validation. Linux (fs/read_write.c: import_iovec -> access_ok per
    // segment) rejects an iovec whose [base, base+len) leaves the user address window with -EFAULT, before
    // any data moves -- notably a segment whose length overflows the address space (e.g. two SSIZE_MAX
    // segments). The plain host path forwards to the host libc, where BSD/macOS readv/writev instead report
    // EINVAL for such a segment -- a host-passthrough divergence. Emulate the Linux access_ok rejection: any
    // segment whose base+len wraps or exceeds the 48-bit user ceiling can never name real guest memory, so it
    // is -EFAULT on every host. Real guest buffers live far below this ceiling, so valid I/O is untouched.
    if ((nr == 65 || nr == 66 || nr == 69 || nr == 70) && a2 > 0 && a2 <= 1024 && a1 != 0 && (int)a0 >= 0 &&
        fcntl((int)a0, F_GETFD) != -1) {
        struct iovec imported[1024];
        if (guest_iov_import(a1, (size_t)a2, imported) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            return svc_done(c);
        }
        for (int i = 0; i < (int)a2; i++) {
            uintptr_t base = (uintptr_t)imported[i].iov_base;
            uintptr_t len = (uintptr_t)imported[i].iov_len;
            if (len && (base + len < base || base + len > UINT64_C(0x0001000000000000))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
        }
    }
    // An O_PATH fd names a file but is not open for I/O -- Linux rejects the read/write family through it
    // with EBADF (fs/read_write.c). It stays valid as a dirfd for *at() and for fstat/fchdir (served by
    // svc_fs), so only the I/O syscalls are gated here.
    if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_opath[(int)a0]) {
        switch (nr) {
        case 63:
        case 64:
        case 65:
        case 66:
        case 67:
        case 68:
        case 69:
        case 70:
        case 82: /* fsync */
        case 83: /* fdatasync */ G_RET(c) = (uint64_t)(int64_t)(-EBADF); return svc_done(c);
        default: break;
        }
    }
    // /dev/full: any write fails ENOSPC (reads are served from the /dev/zero backing). Installers and
    // test suites probe this to check out-of-space handling.
    if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_devfull[(int)a0]) {
        switch (nr) {
        case 64:
        case 66:
        case 68:
        case 70:
            // A description that is not open for writing loses first, as everywhere else in the family:
            // write(fd_opened_O_RDONLY_on_/dev/full, buf, 1) is EBADF on Linux, not ENOSPC.
            G_RET(c) = (uint64_t)(int64_t)(guest_fd_rejects((int)a0, 0) ? -EBADF : -ENOSPC);
            return svc_done(c);
        default: break;
        }
    }
    // /dev/urandom + /dev/random: Linux accepts writes as entropy seeding and returns the byte count;
    // macOS EPERMs them. Swallow the write (count for write/pwrite; summed iov length for writev/pwritev).
    if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_devseed[(int)a0]) {
        // The swallow must not skip the two checks the kernel makes ahead of the write itself: the
        // description has to be open for writing (EBADF) and the source has to be readable (EFAULT).
        switch (nr) {
        case 64:
        case 66:
        case 68:
        case 70:
            if (guest_fd_rejects((int)a0, 0)) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                return svc_done(c);
            }
            break;
        default: break;
        }
        switch (nr) {
        case 64:
        case 68: // write / pwrite64: count = a2
            if (a2 && guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_READ) != (size_t)a2) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
            G_RET(c) = a2;
            return svc_done(c);
        case 66:
        case 70: { // writev / pwritev: sum the iovec lengths
            if (a2 > GUEST_IOV_STACK_MAX) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                return svc_done(c);
            }
            // The array itself lives in guest memory, so import it instead of dereferencing a1 directly.
            struct iovec vectors[GUEST_IOV_STACK_MAX];
            if (guest_iov_import(a1, (size_t)a2, vectors) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
            uint64_t total = 0;
            for (size_t index = 0; index < (size_t)a2; ++index) {
                size_t length = vectors[index].iov_len;
                uint64_t base = (uint64_t)(uintptr_t)vectors[index].iov_base;
                if (length && guest_accessible_prefix(base, length, HL_LOGICAL_VMA_READ) != length) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    return svc_done(c);
                }
                total += length;
            }
            G_RET(c) = total;
            return svc_done(c);
        }
        default: break;
        }
    }
    // Guest PROT_NONE buffer in the fd-I/O family (fd, BUF=a1, count=a2): hl force-maps guest anon pages
    // host-writable (mem.c case 222) so the host read/write does NOT fault on a guest PROT_NONE page the way
    // Linux's copy_{to,from}_user would. Reject it here with -EFAULT, exactly as Linux. Near-free when no
    // PROT_NONE region exists (g_ngna==0). read/pread WRITE the buffer, write/pwrite READ it; both fault. (read02)
    if (g_ngna) {
        switch (nr) {
        case 63:
        case 67: { // read / pread64: the kernel WRITES the buffer with byte-granular copy_to_user, so a
                   // destination straddling a guest PROT_NONE page yields a SHORT read of the good prefix
                   // (Linux only reports EFAULT when nothing at all could be copied). Clamp the count to
                   // that prefix instead of failing the whole call. (read02 still EFAULTs: prefix == 0.)
            uint64_t good = gna_prefix(a1, a2);
            if (a2 && !good) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
            a2 = good;
            break;
        }
        case 64:
        case 68: // write / pwrite64: the source buffer is read whole-request by the pipe/socket paths, and
                 // Linux reports EFAULT for the call rather than a short write, so keep this all-or-nothing.
            if (gna_hit(a1, a2)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                return svc_done(c);
            }
            break;
        case 66:   // writev / pwritev: same as write/pwrite but the source is a scatter list. Linux copies
        case 70: { // from each segment and reports EFAULT for the whole call if any segment straddles a guest
                   // PROT_NONE page (copy_from_user faults); the host writev cannot see it because hl force-maps
                   // the guest page host-readable, so reject here to match Linux. iovcnt is already bounded to
                   // [1,1024] and the array validated above, so this only inspects real segments.
            if (a1 && a2 && a2 <= 1024) {
                const struct iovec *iov = (const struct iovec *)a1;
                for (int i = 0; i < (int)a2; i++)
                    if (iov[i].iov_len && gna_hit((uint64_t)(uintptr_t)iov[i].iov_base, iov[i].iov_len)) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        return svc_done(c);
                    }
            }
            break;
        }
        default: break;
        }
    }
    switch (nr) {
    // ===================== I/O — read/write/seek (+ eventfd/timerfd/signalfd fd redirection) =====================
    case 62: {
        // lseek -- SEEK_SET/CUR/END(0/1/2) match. SEEK_DATA/SEEK_HOLE use host-native constants because
        // Darwin swaps their numeric values while Linux consumes the guest values directly.
        int whence = (int)a2;
        // Directory streams are read via getdents, backed by a private DIR* (fdopendir(dup(fd))) in the
        // plain path or a merged snapshot in the overlay path -- neither moves when the guest lseeks its
        // own fd. glibc rewinddir()/seekdir() ARE exactly this lseek, so redirect it here or the
        // enumeration never restarts (the readdir-dtype xfail: rewinddir's 2nd pass saw 0 entries).
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_nlower && g_ovldir[(int)a0][0]) {
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
    case 63: {
        int rfd = (int)a0;
        if (fcntl(rfd, F_GETFL) < 0 && errno == EBADF) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // tee(2) pushback: bytes a prior tee() peeked out of this pipe are re-served here first, in order.
        if (rfd >= 0 && rfd < HL_NFD && g_fd_pb_len[rfd]) {
            size_t want = (size_t)a2;
            size_t accessible = guest_accessible_prefix(a1, want, HL_LOGICAL_VMA_WRITE);
            if (want != 0 && accessible == 0) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            want = accessible;
            void *buffer = malloc(want == 0 ? 1 : want);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            size_t taken = pipe_pushback_take(rfd, buffer, want);
            ssize_t copied = guest_copy_to(a1, buffer, taken);
            free(buffer);
            G_RET(c) = copied == (ssize_t)taken ? taken : copied > 0 ? (uint64_t)copied : (uint64_t)(-EFAULT);
            break;
        }
        // AF_NETLINK socket read: busybox `ip` receives its RTNETLINK dump with read(2)/recvmsg;
        // drain our queued reply with the Linux MSG_PEEK/MSG_TRUNC semantics (see netns.c nl_recv).
        if (nl_is(rfd)) {
            size_t accessible = guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE);
            if (a2 && !accessible) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint8_t *buffer = malloc(accessible ? accessible : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            struct iovec iov = {buffer, accessible};
            int64_t result = nl_recv(rfd, &iov, 1, 0, NULL);
            if (result > 0) {
                size_t produced = (uint64_t)result < accessible ? (size_t)result : accessible;
                ssize_t copied = guest_copy_to(a1, buffer, produced);
                if (copied != (ssize_t)produced) result = copied > 0 ? copied : -EFAULT;
            }
            free(buffer);
            G_RET(c) = (uint64_t)result;
            break;
        }
        // RAM-backed scratch file: serve the read from memory. Unlike a host-fd read (whose kernel copyout
        // faults a bad buffer to EFAULT), this copies straight into the guest buffer, so a bad/unmapped
        // pointer must be validated here or the engine memcpy faults (access_ok).
        if (memf_get(rfd)) {
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
            ssize_t r = memf_read_pos(g_memf[rfd], buffer, accessible);
            if (r > 0) {
                ssize_t copied = guest_copy_to(a1, buffer, (size_t)r);
                if (copied != r) r = copied > 0 ? copied : -EFAULT;
            }
            free(buffer);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        // signalfd read -> struct signalfd_siginfo. Each signalfd OFD has its own self-pipe; the fd number
        // (original OR a dup, both mapped by g_sigfd_slot) is the read end, so read straight from rfd.
        if (rfd >= 0 && rfd < HL_NFD && g_sigfd_slot[rfd]) {
            // Linux needs room for at least one struct signalfd_siginfo (128 bytes); a shorter buffer is
            // EINVAL and must NOT consume a pending signal (checked before draining the wake byte).
            if (a2 < 128) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            char b;
            // drain one wake byte (one byte was written per queued instance -> keeps readability accurate)
            ssize_t pr = read(rfd, &b, 1);
            if (pr <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(pr < 0 ? -errno : -EAGAIN);
                break;
            }
            // The self-pipe only preserves INSERTION order, but signalfd drains in priority order (lowest
            // signo first, FIFO within a signo) carrying the queued siginfo (ssi_int / ssi_pid / ssi_code).
            // So ignore the byte's signo for SELECTION: scan this OFD's mask ascending for the lowest signo
            // with a queued instance and pop it. Only if nothing is queued (a host async-path wake that set
            // the bit with an empty queue) do we fall back to the byte's signo and the single-slot g_sig*.
            int sslot = g_sigfd_slot[rfd] - 1;
            uint64_t omask = g_sfd[sslot].mask;
            struct sigq_ent ent;
            int sig = 0, popped = 0;
            for (int s = 1; s < 64; s++)
                if ((omask & (1ull << s)) && sigq_pop(s, &ent)) {
                    sig = s;
                    popped = 1;
                    break;
                }
            if (!popped) {
                sig = (unsigned char)b;
                if (sig > 0 && sig < 64) __atomic_and_fetch(&g_pending, ~(1ull << (unsigned)sig), __ATOMIC_SEQ_CST);
            }
            if (a1 && a2 >= 128) {
                // a1 is a raw guest buffer we write directly -> EFAULT a bad pointer instead of faulting the engine
                uint8_t info[128] = {0};
                uint32_t signo = (uint32_t)sig, pid = (uint32_t)(popped ? ent.pid : g_sigpid[sig]);
                uint32_t uid = (uint32_t)(popped ? ent.uid : g_siguid[sig]);
                int32_t error = popped ? ent.error : g_sigerror[sig];
                int32_t code = popped ? ent.code : g_sigcode[sig];
                memcpy(info, &signo, sizeof(signo));
                memcpy(info + 4, &error, sizeof(error));
                memcpy(info + 8, &code, sizeof(code));
                memcpy(info + 12, &pid, sizeof(pid));
                memcpy(info + 16, &uid, sizeof(uid));
                uint64_t val = popped ? ent.value : g_sigval[sig];
                int32_t integer = (int32_t)val;
                memcpy(info + 44, &integer, sizeof(integer));
                memcpy(info + 48, &val, sizeof(val));
                if (guest_copy_to(a1, info, sizeof(info)) != (ssize_t)sizeof(info)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                if (!popped) {
                    g_sigerror[sig] = 0;
                    g_sigcode[sig] = 0;
                    g_sigval[sig] = 0;
                    g_sigpid[sig] = 0;
                    g_siguid[sig] = 0;
                }
            }
            G_RET(c) = 128;
            break;
        }
        // inotify read -> struct inotify_event[]
#if defined(__linux__)
        if (rfd >= 0 && rfd < HL_NFD && g_inotify[rfd] && g_inotify_raw_pos[rfd] < g_inotify_raw_len[rfd]) {
            if (a2 < 16) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE) != (size_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            size_t available = g_inotify_raw_len[rfd] - g_inotify_raw_pos[rfd];
            size_t copied = available < (size_t)a2 ? available : (size_t)a2;
            if (guest_copy_to(a1, g_inotify_raw[rfd] + g_inotify_raw_pos[rfd], copied) != (ssize_t)copied) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            g_inotify_raw_pos[rfd] += copied;
            if (g_inotify_raw_pos[rfd] == g_inotify_raw_len[rfd]) {
                free(g_inotify_raw[rfd]);
                g_inotify_raw[rfd] = NULL;
                g_inotify_raw_pos[rfd] = g_inotify_raw_len[rfd] = 0;
            }
            G_RET(c) = copied;
            break;
        }
#else
        if (rfd >= 0 && rfd < 1024 && g_inotify[rfd]) {
            // Linux needs room for at least one struct inotify_event (16-byte header); a shorter buffer is
            // EINVAL and must NOT consume the queued event (checked before any kqueue drain / snapshot diff).
            if (a2 < 16) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // The whole [a1, a1+a2) buffer is written directly by the engine below; validate it up front so a
            // bad/unmapped pointer returns -EFAULT (without consuming events) instead of faulting the engine.
            if (guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE) != (size_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint8_t *out = malloc((size_t)a2);
            if (!out) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            size_t off = 0;
            // First drain any queued rename events (IN_MOVED_FROM/IN_MOVED_TO) for this instance; the
            // snapshot diff below can only synthesize IN_CREATE/IN_DELETE, not paired moves.
            off += inomv_drain(rfd, out, (size_t)a2);
            for (int wd = 0; wd < 1024; wd++) {
                if (g_inotify_owner[wd] != rfd || !g_inotify_pending[wd]) continue;
                if (g_inotify_isdir[wd]) {
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) {
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u;
                        for (const char *p = src ? src : ""; *p;) {
                            const char *e = strchr(p, '\n');
                            size_t length = e ? (size_t)(e - p) : strlen(p);
                            if (length && !snap_has(other, p, length) && (g_inotify_mask[wd] & mask)) {
                                size_t nlen = (length + 1 + 15) & ~(size_t)15;
                                if (off + 16 + nlen > a2) break;
                                *(int32_t *)(out + off) = wd;
                                *(uint32_t *)(out + off + 4) = mask;
                                *(uint32_t *)(out + off + 8) = 0;
                                *(uint32_t *)(out + off + 12) = (uint32_t)nlen;
                                memcpy(out + off + 16, p, length);
                                memset(out + off + 16 + length, 0, nlen - length);
                                off += 16 + nlen;
                            }
                            p = e ? e + 1 : p + length;
                        }
                    }
                    free(old);
                    g_inotify_snap[wd] = cur;
                } else if (off + 16 <= a2) {
                    *(int32_t *)(out + off) = wd;
                    *(uint32_t *)(out + off + 4) = g_inotify_pending[wd] & g_inotify_mask[wd];
                    *(uint32_t *)(out + off + 8) = 0;
                    *(uint32_t *)(out + off + 12) = 0;
                    off += 16;
                }
                g_inotify_pending[wd] = 0;
            }
            struct kevent kv[32];
            struct timespec zero = {0, 0};
            int nb = g_inotify_nb[rfd];
            // If we already produced move events, poll the kqueue non-blocking so we never wait behind them.
            int n = kevent(rfd, NULL, 0, kv, 32, (nb || off > 0) ? &zero : NULL);
            if (n <= 0) {
                if (off > 0) {
                    ssize_t copied = guest_copy_to(a1, out, off);
                    G_RET(c) = copied == (ssize_t)off ? (uint64_t)off : (uint64_t)(-EFAULT);
                    free(out);
                    break;
                } // return the moves we already have
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                free(out);
                break;
            }
            for (int i = 0; i < n; i++) {
                int wd = (int)kv[i].ident;
                if (wd >= 0 && wd < 1024 && g_inotify_isdir[wd]) {
                    // directory watch: diff current entries against the snapshot -> IN_CREATE/IN_DELETE+name
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) { // pass 0 = created, pass 1 = deleted
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u; // IN_CREATE / IN_DELETE
                        for (const char *p = src ? src : ""; *p;) {
                            const char *e = strchr(p, '\n');
                            size_t l = e ? (size_t)(e - p) : strlen(p);
                            if (l && !snap_has(other, p, l) && (g_inotify_mask[wd] & mask)) {
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
            ssize_t copied = guest_copy_to(a1, out, off);
            G_RET(c) = copied == (ssize_t)off ? (uint64_t)off : (uint64_t)(-EFAULT);
            free(out);
            break;
        }
#endif
        // timerfd read -> drain timer, return count
        if (rfd >= 0 && rfd < HL_NFD && g_timerfd[rfd]) {
            // Linux needs an 8-byte buffer; a shorter read is EINVAL and must NOT drain the expiration
            // (checked before the kqueue drain so the pending tick survives an invalid short read).
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            int tslot = timerfd_slot(rfd);
            if (tslot < 0 || tslot >= HL_NFD) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (g_tfd_shared[rfd]) {
                struct timerfd_shared_state *shared = g_tfd_shared[rfd];
            timerfd_shared_retry:
                timerfd_shared_lock(shared);
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                uint64_t expirations = shared->pending;
                shared->pending = 0;
                if (shared->deadline > 0 && now_ns >= shared->deadline) {
                    if (shared->interval > 0) {
                        uint64_t elapsed = 1 + (uint64_t)((now_ns - shared->deadline) / shared->interval);
                        expirations += elapsed;
                        shared->deadline += (int64_t)elapsed * shared->interval;
                    } else {
                        expirations += 1;
                        shared->deadline = 0;
                    }
                }
                int64_t next = shared->deadline;
                int64_t interval = shared->interval;
                timerfd_shared_unlock(shared);
                struct kevent stale;
                struct timespec zero = {0, 0};
                (void)kevent(rfd, NULL, 0, &stale, 1, &zero);
                if (expirations != 0) {
                    if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                        G_RET(c) = (uint64_t)(-EFAULT);
                        break;
                    }
                    if (next > now_ns) {
                        struct kevent future;
                        EV_SET(&future, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL,
                               next - now_ns, NULL);
                        (void)kevent(rfd, &future, 1, NULL, 0, NULL);
                    }
                    g_tfd_deadline[tslot] = next;
                    g_tfd_interval[tslot] = interval;
                    if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                        G_RET(c) = (uint64_t)(-EFAULT);
                        break;
                    }
                    G_RET(c) = 8;
                    break;
                }
                if (g_tfd_nb[rfd]) {
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
                struct kevent wake;
                int waited = kevent(rfd, NULL, 0, &wake, 1, NULL);
                if (waited > 0) goto timerfd_shared_retry;
                G_RET(c) = (uint64_t)(waited < 0 ? -errno : -EAGAIN);
                break;
            }
            if (g_tfd_nb[rfd] && g_tfd_deadline[tslot] == 0 && g_tfd_pending[tslot] == 0) {
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            if (g_tfd_pending[tslot] != 0) {
                if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                uint64_t expirations = g_tfd_pending[tslot];
                g_tfd_pending[tslot] = 0;
                struct kevent pending_event;
                struct timespec zero = {0, 0};
                (void)kevent(rfd, NULL, 0, &pending_event, 1, &zero);
                if (g_tfd_interval[tslot] > 0 && g_tfd_deadline[tslot] > 0) {
                    struct timespec now;
                    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                    int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                    if (now_ns >= g_tfd_deadline[tslot]) {
                        uint64_t elapsed = 1 + (uint64_t)((now_ns - g_tfd_deadline[tslot]) / g_tfd_interval[tslot]);
                        expirations += elapsed;
                        g_tfd_deadline[tslot] += (int64_t)elapsed * g_tfd_interval[tslot];
                    }
                    struct kevent future;
                    int64_t delay = g_tfd_deadline[tslot] - now_ns;
                    if (delay < 0) delay = 0;
                    EV_SET(&future, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL, delay, NULL);
                    (void)kevent(rfd, &future, 1, NULL, 0, NULL);
                    g_tfd_first_oneshot[tslot] = 1;
                }
                if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                G_RET(c) = 8;
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
                if (guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                /*
                 * EVFILT_TIMER may report several host wakeup quanta when a busy runner services an
                 * overdue EV_ONESHOT. Linux timerfd one-shots have exactly one expiration; only a
                 * periodic timer accumulates multiple expirations.
                 */
                uint64_t expirations = g_tfd_interval[tslot] == 0 ? UINT64_C(1) : (uint64_t)kv.data;
                /*
                 * A distinct first deadline is represented by an EV_ONESHOT.  kqueue can therefore
                 * only report the first expiry, while Linux accumulates every interval that elapsed
                 * before read(2).  Derive that count from the original deadline and keep the next
                 * deadline phase-aligned instead of restarting the period at read time.
                 */
                if (g_tfd_first_oneshot[tslot] && g_tfd_interval[tslot] > 0 && g_tfd_deadline[tslot] > 0) {
                    struct timespec tnow;
                    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &tnow);
                    int64_t now_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
                    if (now_ns >= g_tfd_deadline[tslot])
                        expirations = 1 + (uint64_t)((now_ns - g_tfd_deadline[tslot]) / g_tfd_interval[tslot]);
                }
                if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
            }
            // A periodic timerfd whose first expiry (it_value) differed from its interval was armed as a
            // one-shot for that first tick (event.c case 86). Now that the first tick has been consumed,
            // re-arm the recurring periodic at the interval so subsequent expiries fire every it_interval.
            if (g_tfd_first_oneshot[tslot] && g_tfd_interval[tslot] > 0) {
                struct kevent rkv;
                struct timespec tnow;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &tnow);
                int64_t now_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
                int64_t next = g_tfd_deadline[tslot];
                if (next <= now_ns) next += ((now_ns - next) / g_tfd_interval[tslot] + 1) * g_tfd_interval[tslot];
                int64_t delay = next - now_ns;
                EV_SET(&rkv, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL, delay, NULL);
                kevent(rfd, &rkv, 1, NULL, 0, NULL);
                g_tfd_deadline[tslot] = next;
            }
            G_RET(c) = 8;
            break;
        }
        // eventfd read: return the accumulated counter, reset it, drain the readiness pipe
        if (rfd >= 0 && rfd < HL_NFD && g_eventfd_peer[rfd]) {
            int eslot = eventfd_counter_slot(rfd);
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 (the result counter) is written directly below; reject a bad/NULL pointer here, before any
            // side effect (counter reset / pipe drain). Linux read(eventfd, NULL, 8) is EFAULT, not a
            // silent 8-byte success, so a null pointer must fault too (not just an out-of-range one).
            if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
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
                    // Guest asked for non-blocking. No flag toggle is needed either way (the toggle used to
                    // race a cross-process reader — the spurious-EAGAIN bug): where the read end is
                    // host-O_NONBLOCK there is nothing to toggle, and where it is not, the drain below takes
                    // a counted byte rather than probing for one. Drain any stale readiness byte so a
                    // level-triggered epoll won't report the fd ready-forever, then return EAGAIN. The
                    // counter is zero on this branch, so the pair's invariant says no byte is owed and a
                    // host with a blocking read end takes none.
                    eventfd_drain_readiness(rfd, 0);
                    pthread_mutex_unlock(&g_eventfd_lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    goto eventfd_read_done;
                }
                // Guest wants to block. Where the read end is O_NONBLOCK a raw read would EAGAIN rather than
                // wait, so the wait is poll()'s and the read that follows only consumes the byte. Where it is
                // NOT -- a host with no per-descriptor status-flag channel, which is also a host whose poll()
                // over a mixed descriptor set does not exist -- that same one-byte read IS the wait, and it
                // is the right one: it parks the thread on exactly the pipe a writer signals through.
                pthread_mutex_unlock(&g_eventfd_lock);
                if (g_eventfd_readend_nb) {
                    struct pollfd pf = {.fd = rfd, .events = POLLIN, .revents = 0};
                    poll(&pf, 1, -1); // block until a writer signals 0->positive
                }
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
            // re-sync the pipe to "counter > 0": drain it, then re-signal one byte if still positive. Drain
            // directly with no flag toggle either way (no cross-process race). This branch is reached only
            // with a positive counter, so exactly one byte is owed.
            eventfd_drain_readiness(rfd, 1);
            if (g_eventfd_count[eslot] > 0) {
                char b = 1;
                if (write(g_eventfd_peer[rfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            if (guest_copy_to(a1, &v, sizeof(v)) != (ssize_t)sizeof(v)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 8;
        eventfd_read_done:
            break;
        }
        // /proc/<pid>/pagemap (vfs.c backs it with an empty seekable fd; g_pagemap_fd marks it): synthesize
        // one 64-bit entry per page with the PRESENT bit (63) set. The guest lseek'd to vaddr/pagesize*8 and
        // reads sequentially; advance the real fd offset so its position tracks what we "read" (LTP mmap12).
        if (rfd >= 0 && rfd < HL_NFD && g_pagemap_fd[rfd]) {
            size_t want = (size_t)a2 & ~(size_t)7; // whole 8-byte pagemap entries only
            if (want == 0) {
                G_RET(c) = 0;
                break;
            }
            if (guest_accessible_prefix(a1, want, HL_LOGICAL_VMA_WRITE) != want) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint64_t entries[512];
            size_t done = 0;
            for (size_t i = 0; i < sizeof(entries) / sizeof(entries[0]); ++i)
                entries[i] = UINT64_C(1) << 63;
            while (done < want) {
                size_t chunk = want - done < sizeof(entries) ? want - done : sizeof(entries);
                if (guest_copy_to(a1 + done, entries, chunk) != (ssize_t)chunk) {
                    G_RET(c) = done != 0 ? done : (uint64_t)(-EFAULT);
                    goto pagemap_done;
                }
                done += chunk;
            }
            lseek(rfd, (off_t)want, SEEK_CUR);
            G_RET(c) = (uint64_t)want;
        pagemap_done:
            break;
        }
        // SA_RESTART: a blocking read interrupted by a signal whose guest handler asked for restart is
        // resumed in place (the dispatcher runs the handler after the read finally returns); a handler
        // WITHOUT SA_RESTART lets EINTR through. (Well-behaved programs block in poll/select/epoll -- which
        // always return EINTR -- and only read when ready, so this never defers a needed handler.)
        ssize_t r;
        ts_wait_enter(); // 'S' while a read may block (pipe/socket/tty; a ready/regular fd returns at once)
        do {
            r = guest_fd_read(rfd, a1, (size_t)a2, 0, 0);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        // /dev/tty (or /dev/console) tty semantics: a controlling terminal has no EOF-from-emptiness, so a
        // NONBLOCKING read that came back with 0 bytes ("no input") must be EAGAIN, never EOF -- otherwise
        // readline/TUI/event-loop code reads the 0 as terminal closure and tears the terminal down. hl may
        // back /dev/tty with a host device (or /dev/null for console) that returns 0 when empty; remap it.
        if (r == 0 && a2 > 0 && rfd >= 0 && rfd < HL_NFD && g_devtty[rfd]) {
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
        if (fcntl(wfd, F_GETFL) < 0 && errno == EBADF) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // eventfd write is hoisted to the very top of the write handler: an eventfd fd is disjoint from every
        // fd type the checks below probe (oom_score_adj text file, memfd, netlink, dns, icmp, udp, memf), so
        // routing it here changes no behavior -- but it skips the memfd_seals_fd() probe further down, which
        // fstat(2)s the fd on EVERY write to detect a memfd (an eventfd is never cached as one, so it paid a
        // full host fstat per write). This is the eventfd write hot path's dominant non-syscall overhead.
        if (wfd >= 0 && wfd < HL_NFD && g_eventfd_peer[wfd]) {
            int eslot = eventfd_counter_slot(wfd);
            // eventfd_write rejects any size other than exactly 8 (fs/eventfd.c uses count != sizeof).
            // This is asymmetric with the read path, which accepts count >= 8, so a 9- or 16-byte write
            // must NOT be admitted (and must not add to the counter) the way a 9/16-byte read is served.
            if (a2 != 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 is a raw guest pointer we read directly -> validate before the deref (covers NULL too)
            uint64_t add;
            if (guest_copy_from(&add, a1, sizeof(add)) != (ssize_t)sizeof(add)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            if (add == 0xffffffffffffffffULL) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Counter bump + pipe re-signal held together under g_eventfd_lock so a concurrent read()'s
            // drain (or a peer write) can never strand the pipe readable-with-count-0 / empty-with-count>0
            // (the event-loop spin / lost-wakeup root cause -- see the _eventfd-atomicity_ note in vfs.c).
            pthread_mutex_lock(&g_eventfd_lock);
            // Linux caps the counter at ULLONG_MAX-1 (0xfffffffffffffffe). A write that would overflow that
            // maximum does NOT wrap: a nonblocking eventfd returns EAGAIN and leaves the counter unchanged
            // (a blocking one sleeps until a reader makes room -- an extreme edge hl does not model, so it
            // also returns EAGAIN rather than silently wrapping the counter to zero and losing wake state).
            if (add > 0xfffffffffffffffeULL - g_eventfd_count[eslot]) {
                pthread_mutex_unlock(&g_eventfd_lock);
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            // Captured before the bump: it is the drain's "is a readiness byte outstanding" predicate, and
            // after the bump the counter can no longer answer that question.
            int was_signalled = g_eventfd_count[eslot] > 0;
            g_eventfd_count[eslot] += add;
            // Linux wakes epoll edge-triggered waiters on EVERY write, not just the 0->positive transition.
            // A waker eventfd that is never drained (mio/tokio's cross-thread wakeup) would otherwise lose
            // its 2nd and later wakeups: the backing pipe already holds a byte, so an EV_CLEAR kqueue filter
            // never re-fires and a blocked epoll_wait hangs forever. Drain the pipe to exactly one fresh byte
            // so each write produces a new readable edge, bounded even when the reader never keeps up.
            if (add > 0) {
                // Drain to exactly one fresh byte with no flag toggle. The old toggle mutated the
                // cross-process-shared fd flags and a concurrent reader in another process observed the
                // transient O_NONBLOCK -> spurious EAGAIN.
                eventfd_drain_readiness(wfd, was_signalled);
                char b = 1;
                if (write(g_eventfd_peer[wfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            G_RET(c) = 8;
            break;
        }
        // /proc/self/oom_score_adj is guest-visible process state, not a
        // writable view of the host process. Parse the complete decimal value
        // and update the synthetic file so reads through this open description
        // and through later opens agree.
        if (wfd >= 0 && wfd < HL_NFD && !strcmp(g_proc_text_desc[wfd], "self:oom_score_adj")) {
            char value[32];
            if (!a2 || a2 >= 32 || guest_copy_from(value, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(a2 ? -EFAULT : 0);
                break;
            }
            value[a2] = 0;
            char *end = NULL;
            errno = 0;
            long parsed = strtol(value, &end, 10);
            while (end && (*end == '\n' || *end == ' ' || *end == '\t'))
                end++;
            if (errno || !end || end == value || *end || parsed < -1000 || parsed > 1000) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            char rendered[32];
            int rendered_size = snprintf(rendered, sizeof rendered, "%ld\n", parsed);
            int replaced = proc_text_replace(wfd, rendered, (size_t)rendered_size);
            if (replaced != 0) {
                G_RET(c) = (uint64_t)replaced;
                break;
            }
            g_proc_oom_score_adj = (int)parsed;
            G_RET(c) = a2;
            break;
        }
        // /proc/self/comm is WRITABLE on Linux: it renames the task exactly as prctl(PR_SET_NAME) does,
        // truncating to TASK_COMM_LEN-1 (15) characters and dropping one trailing newline, and the new name
        // is immediately visible through prctl(PR_GET_NAME) and /proc/self/{comm,status:Name,stat}. Without
        // this the write landed in the synthetic backing file and the task name never changed.
        if (wfd >= 0 && wfd < HL_NFD && !strcmp(g_proc_text_desc[wfd], "self:comm")) {
            if (!a2) {
                G_RET(c) = 0;
                break;
            }
            char name[16];
            size_t take = (size_t)a2 < sizeof name - 1 ? (size_t)a2 : sizeof name - 1;
            if (guest_copy_from(name, a1, take) != (ssize_t)take) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            name[take] = 0;
            char *nl = strchr(name, '\n');
            if (nl) *nl = 0;
            char normalized[sizeof g_procname];
            snprintf(normalized, sizeof normalized, "%.15s", name);
            char rendered[32];
            int rendered_size = snprintf(rendered, sizeof rendered, "%s\n", normalized);
            int replaced = proc_text_replace(wfd, rendered, (size_t)rendered_size);
            if (replaced != 0) {
                G_RET(c) = (uint64_t)replaced;
                break;
            }
            memcpy(g_procname, normalized, sizeof g_procname);
            set_guest_comm_name(g_procname, c->tid == 0);
            G_RET(c) = a2; // Linux consumes the whole write even when it truncated the stored name
            break;
        }
        // memfd F_SEAL_WRITE: a write to a write-sealed memfd fails EPERM (emulated seal state).
        if (wfd >= 0 && wfd < HL_NFD && (memfd_seals_fd(wfd) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        }
        // AF_NETLINK socket write: busybox `ip` (libbb) sends its RTM_GET* dump request via
        // write(2), NOT sendto/sendmsg -- so the netlink responder (which only hooked send*) never saw
        // it, no dump was queued, and the follow-up recvmsg blocked forever ("container stuck Up").
        // Route it to nl_send so the dump is synthesized exactly as for the send* path.
        if (nl_is(wfd)) {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            G_RET(c) = copied == (ssize_t)a2 ? (uint64_t)nl_send(wfd, buffer, (size_t)a2)
                                             : (uint64_t)(copied > 0 ? copied : -EFAULT);
            free(buffer);
            break;
        }
        // Container DNS: a query write(2)'d on a DNS socket (TCP DNS via write, or a connected-UDP write) is
        // parsed + answered by the host resolver (net.c/netns.c dns_send); nothing reaches the wire.
        if (wfd >= 0 && wfd < HL_NFD && g_dns_sock[wfd]) {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            G_RET(c) = copied == (ssize_t)a2 ? (uint64_t)dns_send(wfd, buffer, (size_t)a2, g_sock_stream[wfd])
                                             : (uint64_t)(copied > 0 ? copied : -EFAULT);
            free(buffer);
            break;
        }
        if (wfd >= 0 && wfd < HL_NFD && g_icmp_kind[wfd]) {
            int64_t result;
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            if (copied != (ssize_t)a2) {
                free(buffer);
                G_RET(c) = (uint64_t)(copied > 0 ? copied : -EFAULT);
                break;
            }
            if (icmp_try_send(wfd, buffer, (size_t)a2, NULL, 0, &result)) {
                free(buffer);
                G_RET(c) = (uint64_t)result;
                break;
            }
            free(buffer);
        }
        {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            if (copied != (ssize_t)a2) {
                free(buffer);
                // This udp-switch probe bounces the source for EVERY write, ahead of any look at the fd, but
                // Linux resolves the descriptor first (fdget_pos then FMODE_WRITE, both EBADF) -- so
                // write(fd_opened_O_RDONLY, NULL, 1) must be EBADF, not the EFAULT this used to report.
                G_RET(c) = (uint64_t)(copied > 0 ? copied : (guest_fd_rejects(wfd, 0) ? -EBADF : -EFAULT));
                break;
            }
            struct iovec vector = {buffer, (size_t)a2};
            int64_t result;
            if (udp_switch_write(wfd, &vector, 1, &result)) {
                free(buffer);
                G_RET(c) = (uint64_t)result;
                break;
            }
            free(buffer);
        }
        // RAM-backed scratch file: serve the write from memory (spill to the host file past the cap).
        // Copies straight from the guest buffer, so validate it (a host-fd write's kernel copyin would fault
        // a bad pointer to EFAULT; this engine memcpy would instead crash) --, access_ok.
        if (memf_get(wfd) && memf_room_or_spill(wfd, (off_t)g_memf[wfd]->pos + (off_t)a2)) {
            int64_t allowed = memf_fsize_gate(c, (off_t)g_memf[wfd]->pos, a2); // RLIMIT_FSIZE -> SIGXFSZ/EFBIG
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
            ssize_t r = memf_write_pos(g_memf[wfd], buffer, copied > 0 ? (size_t)copied : 0);
            free(buffer);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        hl_fdcache_fd_evict(wfd);
        // RLIMIT_FSIZE: a write past the soft file-size limit raises SIGXFSZ (and returns EFBIG); a straddling
        // write is clamped to the limit. No-op unless the guest set a finite limit on a regular file.
        int64_t allowed = fsize_gate(c, wfd, -1, a2);
        if (allowed < 0) {
            G_RET(c) = (uint64_t)allowed;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking write in place (see case 63)
        do {
            r = guest_fd_write(wfd, a1, (size_t)allowed, 0, 0);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        svc_sigpipe_on_epipe(c, (int64_t)G_RET(c)); // write(2) to a broken pipe/socket -> guest SIGPIPE
        break;
    }
    case 65: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_fd_pb_len[(int)a0]) { // tee(2) pushback served first
            size_t available = g_fd_pb_len[(int)a0];
            void *buffer = malloc(available == 0 ? 1 : available);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            size_t taken = pipe_pushback_take((int)a0, buffer, available);
            ssize_t copied = io_guest_vector_scatter(a1, (size_t)a2, buffer, taken);
            free(buffer);
            G_RET(c) = copied >= 0 ? (uint64_t)copied : (uint64_t)(int64_t)copied;
            break;
        }
        if (nl_is((int)a0)) { // netlink readv: drain the queued dump into the guest iov
            uint8_t buffer[65536];
            struct iovec host = {buffer, sizeof(buffer)};
            ssize_t received = nl_recv((int)a0, &host, 1, 0, NULL);
            ssize_t copied =
                received > 0 ? io_guest_vector_scatter(a1, (size_t)a2, buffer, (size_t)received) : received;
            G_RET(c) = (uint64_t)(int64_t)copied;
            break;
        }
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r;       // SA_RESTART: restart a signal-interrupted blocking readv in place (see case 63)
        ts_wait_enter(); // 'S' while readv may block
        do {
            r = guest_fd_vector((int)a0, a1, (size_t)a2, 0, 0, 1);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
        // readv
    }
    case 66: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (nl_is((int)a0)) { // netlink writev: gather the request iov + queue the dump
            uint8_t tmp[4096];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t tl = (size_t)gathered;
            nl_send((int)a0, tmp, tl);
            G_RET(c) = (uint64_t)tl;
            break;
        }
        // Container DNS: TCP DNS is commonly writev(len-prefix, query) (glibc send_vc). Gather + answer it.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
            uint8_t tmp[2048];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t tl = (size_t)gathered;
            G_RET(c) = (uint64_t)dns_send((int)a0, tmp, tl, g_sock_stream[(int)a0]);
            break;
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0]) {
            uint8_t tmp[2048];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            if (gathered < 0) {
                G_RET(c) = (uint64_t)(int64_t)gathered;
                break;
            }
            size_t size = (size_t)gathered;
            int64_t result;
            if (icmp_try_send((int)a0, tmp, size, NULL, 0, &result)) {
                G_RET(c) = (uint64_t)result;
                break;
            }
        }
        {
            int64_t result;
            uint8_t tmp[65536];
            ssize_t gathered = io_guest_vector_gather(a1, (size_t)a2, tmp, sizeof(tmp));
            struct iovec host = {tmp, gathered > 0 ? (size_t)gathered : 0};
            if (gathered < 0) {
                // Same ordering as case 64: this probe gathers before the fd is consulted, and do_writev
                // resolves the descriptor ahead of import_iovec.
                G_RET(c) = (uint64_t)(int64_t)(gathered == -EFAULT && guest_fd_rejects((int)a0, 0) ? -EBADF : gathered);
                break;
            }
            if (udp_switch_write((int)a0, &host, 1, &result)) {
                G_RET(c) = (uint64_t)result;
                break;
            }
        }
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        hl_fdcache_fd_evict((int)a0);
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking writev in place (see case 63)
        do {
            r = guest_fd_vector((int)a0, a1, (size_t)a2, 0, 0, 0);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        svc_sigpipe_on_epipe(c, (int64_t)G_RET(c)); // writev(2) to a broken pipe/socket -> guest SIGPIPE
        break;
        // writev
    }
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
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r; // SA_RESTART: restart a signal-interrupted blocking pread in place (see case 63)
        do {
            r = guest_fd_read((int)a0, a1, (size_t)a2, (off_t)a3, 1);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
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
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
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
    case 23: {
        struct fdvis_reservation fdvis;
        // dup -- a 2nd fd would share the description; flush the RAM cache so both see the real file
        memf_materialize((int)a0);
        if (fd_virt_reserve((int)a0, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        int r = nofile_gate(dup((int)a0)); // EMFILE if the new fd would be >= the guest's soft RLIMIT_NOFILE
        if (r < 0) proc_fdvis_reservation_cancel(&fdvis);
        // carry path + socket-emulation metadata to the new fd
        if (r >= 0 && r < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
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
            fd_carry_virt(r, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 24: {
        struct fdvis_reservation fdvis;
        // dup3(old,new,flags). x86's legacy dup2 arrives here rewritten to the dup3 form (see
        // translator/guest/x86_64/legacy.c) because the two calls DIVERGE on oldfd==newfd: dup3 ->
        // EINVAL, but dup2 -> returns newfd unchanged (EBADF if oldfd is invalid), with no close and no
        // CLOEXEC change. (LTP dup201) Which of the two arrived is reported OUT OF BAND -- it used to
        // ride as bit 30 of the flags, which made a guest's dup3(old,old,0x40000000) succeed where Linux
        // returns EINVAL, because no dup3 flag other than O_CLOEXEC is legal.
        unsigned d3flags = (unsigned)a2;
        int is_dup2 = G_IS_DUP2_COMPAT();
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
        if (fd_virt_reserve_at(oldfd, newfd, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        memf_materialize((int)a0); // source: a 2nd fd shares the description -> flush RAM cache
        memf_close((int)a1);       // target fd is about to be reused; drop any cache it held
        engine_fd_vacate((int)a1); // move any engine-private fd off the target before dup2 overwrites it
        fd_reset_emul((int)a1);    // dup2 atomically closes newfd -> shed ALL its emulation tables (timerfd/
                                   // eventfd/inotify/epoll/sock/...) so the reused number isn't left misrouted; the
                                   // real close is dup2's, and fd_carry_sock below repopulates from oldfd
        int r = dup2((int)a0, (int)a1);
        if (r < 0) proc_fdvis_reservation_cancel(&fdvis);
        if (r >= 0) {
            if (d3flags & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC); // O_CLOEXEC
            if ((int)a1 >= 0 && (int)a1 < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
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
                fd_carry_virt((int)a1, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 25: {
        struct fdvis_reservation fdvis;
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
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_proc_text_ro[(int)a0]) lf = 0;
            char fgetpath_buf[4096] = {0};
            int have_fgetpath = 0;
            if ((lf & 0x3) && hl_native_fd_path((int)a0, fgetpath_buf, sizeof fgetpath_buf) == 0) {
                have_fgetpath = 1;
                if (proc_text_host_path(fgetpath_buf)) lf &= ~0x3;
            }
            // Preserve the architecture's native F_GETFL representation (see G_O_LARGEFILE above).
            lf |= G_O_LARGEFILE;
            if (r & O_APPEND) lf |= 0x400;
            if (r & O_NONBLOCK) lf |= 0x800;
            // APPEND/NONBLOCK/ASYNC
            if (r & O_ASYNC) lf |= 0x2000;
#if defined(__linux__) && defined(O_DIRECT)
            // O_DIRECT is a settable status flag on Linux; the guest fd is a real host fd whose kernel
            // O_DIRECT bit is authoritative. Translate host O_DIRECT -> the guest-arch G_O_DIRECT so an
            // fd opened (or F_SETFL'd) O_DIRECT round-trips instead of being silently dropped.
            if (r & O_DIRECT) lf |= G_O_DIRECT;
#endif
            // eventfd: the host read end is kept permanently O_NONBLOCK internally, so report the guest's
            // OWN blocking/non-blocking intent (g_eventfd_gnb), not the host flag. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_eventfd_peer[(int)a0]) {
                lf = eventfd_guest_nb((int)a0) ? (lf | 0x800) : (lf & ~0x800);
            }
            int proc_text_for_log = ((int)a0 >= 0 && (int)a0 < HL_NFD && g_proc_text_ro[(int)a0]) ||
                                    (have_fgetpath && proc_text_host_path(fgetpath_buf));
            if (0 && proc_text_for_log) {
                char p[4096] = {0};
                if (have_fgetpath) {
                    snprintf(p, sizeof p, "%s", fgetpath_buf);
                } else {
                    (void)hl_native_fd_path((int)a0, p, sizeof p);
                }
                fprintf(stderr, "[HLFCNTL] pid=%d cpid=%d fd=%d mflags=0x%x lflags=0x%x path=%s\n", getpid(),
                        container_pid(), (int)a0, r, lf, p);
            }
            G_RET(c) = (uint64_t)(unsigned)lf;
            break;
        }
        // F_SETFL: Linux O_* -> macOS O_*
        if (lcmd == 4) {
            int la = (int)a2, mf = 0;
            if (la & 0x400) mf |= O_APPEND;
            if (la & 0x800) mf |= O_NONBLOCK;
            // APPEND/NONBLOCK/ASYNC
            if (la & 0x2000) mf |= O_ASYNC;
#if defined(__linux__) && defined(O_DIRECT)
            // Forward an O_DIRECT status-flag change straight to the real host fd (guest-arch G_O_DIRECT ->
            // host O_DIRECT). Previously the bit was dropped, so F_SETFL(O_DIRECT) wrongly returned success
            // without setting it (and a filesystem that rejects O_DIRECT never produced the EINVAL Linux does).
            if (la & G_O_DIRECT) mf |= O_DIRECT;
#endif
            // eventfd: record the guest's blocking/non-blocking intent in the shadow and NEVER clear the
            // host read end's O_NONBLOCK (the internal drains rely on it; clearing it would let a drain
            // block). Other flag changes still apply to the host fd. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_eventfd_peer[(int)a0]) {
                eventfd_guest_nb_set((int)a0, (la & 0x800) != 0);
                mf |= O_NONBLOCK; // keep host O_NONBLOCK on regardless of the guest's request
            }
            int r = fcntl((int)a0, F_SETFL, mf);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // F_GETLK/SETLK/SETLKW: xlate struct flock + cmd
        if (lcmd == 5 || lcmd == 6 || lcmd == 7) {
            // macOS F_GETLK=7,SETLK=8,SETLKW=9
            int mc = lcmd == 5 ? F_GETLK : lcmd == 6 ? F_SETLK : F_SETLKW;
            uint8_t lf[32];
            // Linux order (SYSCALL_DEFINE3(fcntl) -> fcntl_getlk/setlk): the fd is validated (EBADF) BEFORE the
            // flock is copied in, so a bad fd wins over a bad pointer / bad l_whence. (LTP fcntl13: fcntl(-1,...).)
            if ((int)a0 < 0 || fcntl((int)a0, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            // The Linux struct flock at a2 (fields up to lf+24) is read directly and written back for F_GETLK;
            // validate the 32-byte struct before any deref so a bad pointer returns -EFAULT, not a crash. A guest
            // PROT_NONE flock buffer (LTP fcntl13 uses one) is force-mapped host-writable by hl, but
            // host_range_mapped rejects it via its internal gna_hit check so it still EFAULTs like Linux.
            if (guest_copy_from(lf, a2, sizeof(lf)) != (ssize_t)sizeof(lf)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            // l_whence must be SEEK_SET/SEEK_CUR/SEEK_END; Linux rejects anything else with EINVAL in
            // flock_to_posix_lock -- BEFORE the fd type is consulted, so it applies to a pipe fd too. (LTP fcntl13)
            {
                short whence;
                memcpy(&whence, lf + 2, sizeof(whence));
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
                    claimed = poslk_op((int)a0, lcmd, lf, &pout);
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
                    if (lcmd == 5 && pout == 0 && guest_copy_to(a2, lf, sizeof(lf)) != (ssize_t)sizeof(lf))
                        G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                if (claimed) break; // handled in-engine (or interrupted); done
            }
            struct flock fl;
            // Linux flock: type/whence/pad/start@8/len@16/pid@24
            memset(&fl, 0, sizeof fl);
            short lt;
            memcpy(&lt, lf, sizeof(lt));
            // Linux RDLCK=0,WRLCK=1,UNLCK=2 -> macOS
            fl.l_type = lt == 0 ? F_RDLCK : lt == 1 ? F_WRLCK : F_UNLCK;
            memcpy(&fl.l_whence, lf + 2, sizeof(fl.l_whence));
            memcpy(&fl.l_start, lf + 8, sizeof(fl.l_start));
            memcpy(&fl.l_len, lf + 16, sizeof(fl.l_len));
            memcpy(&fl.l_pid, lf + 24, sizeof(fl.l_pid));
            int r = fcntl((int)a0, mc, &fl), e = errno;
            // F_GETLK writes the conflicting lock back
            if (r >= 0 && lcmd == 5) {
                short type = fl.l_type == F_RDLCK ? 0 : fl.l_type == F_WRLCK ? 1 : 2;
                int32_t pid = (int32_t)fl.l_pid;
                memcpy(lf, &type, sizeof(type));
                memcpy(lf + 2, &fl.l_whence, sizeof(fl.l_whence));
                memcpy(lf + 8, &fl.l_start, sizeof(fl.l_start));
                memcpy(lf + 16, &fl.l_len, sizeof(fl.l_len));
                memcpy(lf + 24, &pid, sizeof(pid));
                if (guest_copy_to(a2, lf, sizeof(lf)) != (ssize_t)sizeof(lf)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
            break;
        }
        // F_SETPIPE_SZ(1031)/F_GETPIPE_SZ(1032). The guest's non-O_DIRECT pipe fd is a REAL host pipe
        // (case 59 pipe()), so on a Linux host the kernel already implements these with exact Linux
        // semantics: power-of-two rounding (roundup_pow_of_two, NOT page rounding), a real capacity change
        // that the subsequent fill/EAGAIN reflects, EBUSY when shrinking below the currently buffered data,
        // and EBADF on a non-pipe fd (incl. an O_DIRECT pipe backed by a socketpair). Forward straight
        // through. The macOS build keeps the size emulation below (Darwin has no pipe-size fcntl), which
        // only records a number and never resizes the buffer, so it must stay guarded to that host.
        if (lcmd == 1031 || lcmd == 1032) {
#if defined(__linux__)
            long r = (lcmd == 1032) ? fcntl((int)a0, F_GETPIPE_SZ) : fcntl((int)a0, F_SETPIPE_SZ, (int)a2);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)errno) : (uint64_t)(unsigned)r;
            break;
#else
            // Linux's pipe_fcntl first rejects a non-pipe object with EBADF, so validate the fd is a real
            // FIFO before fabricating a size -- otherwise a regular file/socket or bad fd was reported as a
            // pipe with a plausible size.
            struct stat pst;
            if (fstat((int)a0, &pst) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (!S_ISFIFO(pst.st_mode)) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (lcmd == 1031) {
                int want = (int)a2;
                long pg = (long)hl_linux_host_page_size();
                int rounded = (int)(((want + pg - 1) / pg) * pg);
                if (rounded < (int)pg) rounded = (int)pg;
                if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_pipesz[(int)a0] = rounded;
                G_RET(c) = (uint64_t)(unsigned)rounded;
                break;
            }
            // lcmd == 1032
            {
                int sz = ((int)a0 >= 0 && (int)a0 < HL_NFD && g_pipesz[(int)a0]) ? g_pipesz[(int)a0] : 65536;
                G_RET(c) = (uint64_t)(unsigned)sz;
                break;
            }
#endif
        }
#if defined(__linux__) && defined(F_GET_RW_HINT)
        // Write life-time hints F_GET_RW_HINT(1035)/F_SET_RW_HINT(1036)/F_GET_FILE_RW_HINT(1037)/
        // F_SET_FILE_RW_HINT(1038): the arg is a pointer to a uint64 hint. The guest fd is a real host fd,
        // so forward straight through to the host kernel (which owns the actual per-inode/per-fd hint) --
        // otherwise the do_fcntl default wrongly returns EINVAL where native returns the real hint. macOS
        // has no such command, so this stays Linux-only and the default switch keeps EINVAL there.
        if (lcmd >= 1035 && lcmd <= 1038) {
            uint64_t hint = 0;
            int is_get = lcmd == 1035 || lcmd == 1037;
            if (!a2 || (!is_get && guest_copy_from(&hint, a2, sizeof(hint)) != (ssize_t)sizeof(hint)) ||
                (is_get && guest_accessible_prefix(a2, sizeof(hint), HL_LOGICAL_VMA_WRITE) != sizeof(hint))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            long r = fcntl((int)a0, lcmd, &hint);
            if (r >= 0 && is_get && guest_copy_to(a2, &hint, sizeof(hint)) != (ssize_t)sizeof(hint)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)errno) : (uint64_t)(unsigned)r;
            break;
        }
#endif
        int mcmd = lcmd;
        if (lcmd == 8)
            mcmd = F_SETOWN;
        else if (lcmd == 9)
            // owner cmds also swapped on macOS
            mcmd = F_GETOWN;
        else if (lcmd == 1030)
            mcmd = F_DUPFD_CLOEXEC;
        // memfd sealing: F_ADD_SEALS(1033) / F_GET_SEALS(1034) are honoured on an anonymous memfd (macOS has
        // no native seals, so the state + the F_SEAL_WRITE write-guard are emulated). For a NON-memfd the
        // guest fd is a real host fd, so on Linux forward the command to the host kernel, which knows whether
        // the underlying filesystem supports sealing: a regular tmpfs/shmem file reports its real seal state
        // (born F_SEAL_SEAL, so F_GET_SEALS -> 1 and a further F_ADD_SEALS -> EPERM) while a non-sealing fs
        // (ext4/overlay) answers EINVAL -- exactly like native. The old unconditional EINVAL shadowed that,
        // returning EINVAL where native tmpfs returns the real seal set. macOS keeps EINVAL (no host seals).
        else if (lcmd == 1033) { // F_ADD_SEALS(fd, seals)
            int fd = (int)a0;
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= HL_NFD || !g_memfd_is[fd]) {
#if defined(__linux__) && defined(F_ADD_SEALS)
                // A memfd not tracked here (e.g. a real host memfd whose host fd landed >= HL_NFD) would
                // otherwise let the HOST KERNEL decide the F_SEAL_WRITE-while-mapped verdict. That verdict
                // depends on whether the ENGINE happens to hold a writable host-side MAP_SHARED alias of the
                // object, which varies by host kernel/fs -- EBUSY on this VM but 0 on the CI runner (the
                // memfd-seal-busy divergence). Make F_SEAL_WRITE (0x8) deterministic from the engine's OWN
                // mapping registry, exactly like the emulated branch below: if a live MAP_SHARED mapping of
                // this object exists, refuse with EBUSY (Linux mm/shmem.c writable-mapping guard), host
                // independent. With no outstanding shared mapping, forward so the host still applies the seal
                // and reports the real seal state (F_GET_SEALS / a later writable-shared-mmap EPERM keep working).
                if (((int)a2 & 0x8) && filemap_has_shared_mapping(fd)) {
                    G_RET(c) = (uint64_t)(-EBUSY);
                    break;
                }
                int r = fcntl(fd, F_ADD_SEALS, (int)a2);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
#else
                G_RET(c) = (uint64_t)(-EINVAL);
#endif
                break;
            }
            if (g_memfd_seal[fd] & 0x1) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            } // already F_SEAL_SEAL'd
            // F_SEAL_WRITE (0x8) is refused with EBUSY while an outstanding MAP_SHARED mapping of this memfd
            // is live (Linux mm/shmem.c writable-mapping guard). F_SEAL_FUTURE_WRITE (0x10) only blocks new
            // writable maps, so it is unaffected.
            if (((int)a2 & 0x8) && !(g_memfd_seal[fd] & 0x8) && filemap_has_shared_mapping(fd)) {
                G_RET(c) = (uint64_t)(-EBUSY);
                break;
            }
            g_memfd_seal[fd] |= (int)a2 & 0x1f; // SEAL|SHRINK|GROW|WRITE|FUTURE_WRITE
            memfd_reg_set_fd(fd, g_memfd_seal[fd]);
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1034) { // F_GET_SEALS(fd)
            int fd = (int)a0;
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= HL_NFD || !g_memfd_is[fd]) {
#if defined(__linux__) && defined(F_GET_SEALS)
                int r = fcntl(fd, F_GET_SEALS);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : (uint64_t)(unsigned)r;
#else
                G_RET(c) = (uint64_t)(-EINVAL);
#endif
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
            int held = (fd < HL_NFD && g_lease[fd]) ? g_lease[fd] - 1 : 2; // stored type, else F_UNLCK
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
            // the common case. A write lease (F_WRLCK) requires the fd be the SOLE opener; hl cannot
            // enumerate other openers across guest processes, so it is tracked but its BREAK on a conflicting
            // open is never delivered (see syscall-compat.md). Both states round-trip through F_GETLEASE.
            if (arg == 0) { // F_RDLCK
                int fl = fcntl(fd, F_GETFL);
                if (fl >= 0 && (fl & O_ACCMODE) != O_RDONLY) {
                    G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
                    break;
                }
            }
            if (fd < HL_NFD) g_lease[fd] = (arg == 2) ? 0 : (int8_t)(arg + 1); // F_UNLCK clears
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1026) { // F_NOTIFY(fd, DN_* mask): arm a real host directory-change watch.
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            int sig = (fd < HL_NFD && g_fsig[fd]) ? g_fsig[fd] : 0; // F_SETSIG override, else default SIGIO
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
            if (fd < HL_NFD) g_fsig[fd] = (uint8_t)sig;
#if defined(__linux__)
            // The guest fd is a REAL host fd; forward F_SETSIG so the host's own O_ASYNC signal-driven I/O
            // delivers the requested signal (with SI_SIGIO siginfo) instead of the default SIGIO, which the
            // engine then routes to the guest. Without this the custom async signal is silently ignored and
            // an app arming F_SETSIG waits for a signal that never comes. g_fsig is still recorded for the
            // engine-serviced dnotify path (F_NOTIFY), which raises the signal itself. Errors are non-fatal.
            (void)fcntl(fd, F_SETSIG, sig);
#endif
            G_RET(c) = 0;
            break;
        } else if (lcmd == 11) { // F_GETSIG(fd): the signal set by F_SETSIG (0 = default SIGIO).
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)((fd < HL_NFD) ? g_fsig[fd] : 0);
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
        if ((lcmd == 0 || lcmd == 1030) && fd_virt_reserve((int)a0, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            goto fcntl_done;
        }
        if (lcmd == 0 || lcmd == 1030) engine_fd_vacate((int)a2);
        if ((lcmd == 0 || lcmd == 1030) && bound_sentinel_vacate((int)a2) != 0) {
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(-EMFILE);
            goto fcntl_done;
        }
        int r = fcntl((int)a0, mcmd, a2);
        if (lcmd == 0 || lcmd == 1030) r = nofile_gate(r); // F_DUPFD(_CLOEXEC): EMFILE past the guest fd cap
        if (r < 0 && (lcmd == 0 || lcmd == 1030)) proc_fdvis_reservation_cancel(&fdvis);
        if (r >= 0 && (lcmd == 0 || lcmd == 1030) && r < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
            // F_DUPFD(_CLOEXEC)
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            strcpy(g_proc_text_desc[r], g_proc_text_desc[(int)a0]);
            g_proc_text_ro[r] = g_proc_text_ro[(int)a0];
            g_pagemap_fd[r] = g_pagemap_fd[(int)a0];
            fd_carry_sock(r, (int)a0);
            fd_carry_virt(r, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
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
        int on = 0;
        if (!a2 || guest_copy_from(&on, a2, sizeof(on)) != (ssize_t)sizeof(on)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int fl = fcntl((int)a0, F_GETFL);
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
        // Validate flags exactly as Linux (fs/pipe.c): only O_CLOEXEC | O_NONBLOCK | O_DIRECT |
        // O_NOTIFICATION_PIPE are defined; any other bit -> EINVAL (mirrors eventfd2, case 19). A bogus flag
        // (e.g. 0x4) previously slipped through and pipe2 wrongly succeeded.
        if ((unsigned)fl & ~(unsigned)(0x800u | 0x80000u | (unsigned)G_O_DIRECT | 0x800000u)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // a0 receives the two result fds (8 bytes). Validate it BEFORE creating the pipe so a bad pointer
        // returns -EFAULT without leaking the freshly-opened fds (and without faulting the engine).
        if (guest_accessible_prefix(a0, 2 * sizeof(int), HL_LOGICAL_VMA_WRITE) != 2 * sizeof(int)) {
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
        if (proc_fdvis_publish_pipe_pair(fds[0], fds[1]) != 0) {
            close(fds[0]);
            close(fds[1]);
            G_RET(c) = (uint64_t)(-EMFILE);
            break;
        }
        if (guest_copy_to(a0, fds, sizeof(fds)) != (ssize_t)sizeof(fds)) {
            close(fds[0]);
            close(fds[1]);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // An O_DIRECT pipe is backed by a DGRAM socketpair (above). Like a real pipe it must report EOF
        // when the write end closes, but macOS DGRAM sockets don't -- mark both ends so close() sends a
        // zero-length EOF datagram and read() coerces the peer-closed ECONNRESET to 0. (See netns.c.)
        if ((fl & G_O_DIRECT)) {
            if (seq_ref_pair(fds[0], fds[1]) != 0) {
                int e = errno;
                proc_fdvis_close(fds[0]);
                proc_fdvis_close(fds[1]);
                close(fds[0]);
                close(fds[1]);
                G_RET(c) = (uint64_t)(-e);
                break;
            }
            if (fds[0] >= 0 && fds[0] < HL_NFD) {
                g_sock_seqpacket[fds[0]] = 1;
                g_sock_pair_peer[fds[0]] = fds[1] + 1;
            }
            if (fds[1] >= 0 && fds[1] < HL_NFD) {
                g_sock_seqpacket[fds[1]] = 1;
                g_sock_pair_peer[fds[1]] = fds[0] + 1;
            }
            sock_pair_identity_assign(fds[0], fds[1]);
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
        // Linux defines NO flags for copy_file_range: SYSCALL_DEFINE6 rejects a non-zero `flags` with
        // -EINVAL up front. The engine ignored a5 and copied the bytes anyway.
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        memf_materialize(fdin); // copy_file_range moves bytes via the real fds -> flush RAM caches first
        memf_materialize(fdout);
        size_t len = (size_t)a4, done = 0;
        int err = 0;
        off_t input_offset = 0, output_offset = 0;
        off_t *poi = a1 != 0 ? &input_offset : NULL, *poo = a3 != 0 ? &output_offset : NULL;
        // off_in (a1) / off_out (a3) are read here and written back below -> validate before any deref so a
        // bad pointer returns -EFAULT instead of faulting the engine (and before any bytes are copied).
        if (poi && guest_copy_from(poi, a1, sizeof(*poi)) != (ssize_t)sizeof(*poi)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (poo && guest_copy_from(poo, a3, sizeof(*poo)) != (ssize_t)sizeof(*poo)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        off_t oi = poi ? *poi : -1, oo = poo ? *poo : -1;
        // Linux rejects a same-file copy whose source and destination ranges overlap (EINVAL) rather than
        // corrupting the file by copying through the overlap. Compare the underlying file identity and the
        // effective start offsets (explicit offset, else the current file position for a NULL offset).
        if (len > 0) {
            struct stat si, so;
            if (fstat(fdin, &si) == 0 && fstat(fdout, &so) == 0 && si.st_dev == so.st_dev && si.st_ino == so.st_ino) {
                off_t is = poi ? *poi : lseek(fdin, 0, SEEK_CUR);
                off_t os = poo ? *poo : lseek(fdout, 0, SEEK_CUR);
                if (is >= 0 && os >= 0 && is < os + (off_t)len && os < is + (off_t)len) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
            }
        }
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
        if ((poi && guest_copy_to(a1, &oi, sizeof(oi)) != (ssize_t)sizeof(oi)) ||
            (poo && guest_copy_to(a3, &oo, sizeof(oo)) != (ssize_t)sizeof(oo))) {
            G_RET(c) = done != 0 ? (uint64_t)done : (uint64_t)(-EFAULT);
            break;
        }
        hl_fdcache_fd_evict(fdout);
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
        break;
    }
    // preadv/pwritev: struct iovec layout is identical Linux<->macOS
    case 69: {
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r = guest_fd_vector((int)a0, a1, (size_t)a2, (off_t)a3, 1, 1);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 70: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (memf_get((int)a0)) { memf_materialize((int)a0); }
        ssize_t r = guest_fd_vector((int)a0, a1, (size_t)a2, (off_t)a3, 1, 0);
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
