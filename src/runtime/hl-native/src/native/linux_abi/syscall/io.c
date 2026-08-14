#ifndef G_IS_DUP2_COMPAT
#define G_IS_DUP2_COMPAT() 0 /* aarch64 guests have no legacy dup2; every case 24 is a real dup3 */
#endif
#include "binding/vector_validation.h"
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
    hl_vfs_fd_cursor_duplicate(oldfd, newfd);
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
            process_pending_set(sig);
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

#include "io/position.c"
#include "io/vector.c"
#include "io/descriptor.c"
#include "io/stream.c"

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
        /*
         * Linux validates each vector entry in order: an entry that takes the
         * aggregate past SSIZE_MAX is EINVAL, while an earlier entry whose
         * payload range is outside user memory is EFAULT before a later entry
         * can overflow that aggregate.  Interleave the checks to preserve both
         * sides of that precedence ladder.
         */
        uint64_t total = 0;
        for (int i = 0; i < (int)a2; i++) {
            int validated = hl_guest_iov_validate((uint64_t)(uintptr_t)imported[i].iov_base,
                                                  (uint64_t)imported[i].iov_len, &total);
            if (validated != 0) {
                G_RET(c) = (uint64_t)(int64_t)validated;
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
        case 66:   // writev / pwritev: Linux can publish the readable prefix of one segment that straddles an
        case 70: { // inaccessible page. A later segment whose byte zero faults instead fails the entire call.
                   // Reject only wholly inaccessible segments here; guest_iov_range clamps a partial segment
                   // before the host sees the force-mapped tail. The array is already validated and bounded.
            if (guest_fd_rejects((int)a0, 0)) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                return svc_done(c);
            }
            if (a1 && a2 && a2 <= 1024) {
                const struct iovec *iov = (const struct iovec *)a1;
                for (int i = 0; i < (int)a2; i++)
                    if (iov[i].iov_len && guest_accessible_prefix((uint64_t)(uintptr_t)iov[i].iov_base,
                                                                  iov[i].iov_len, HL_LOGICAL_VMA_READ) == 0) {
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
    case 62: return svc_lseek(c,nr,a0,a1,a2,a3,a4,a5);
    case 63: return svc_read(c,nr,a0,a1,a2,a3,a4,a5);
    case 64: return svc_write(c,nr,a0,a1,a2,a3,a4,a5);
    case 65: return svc_readv(c,nr,a0,a1,a2,a3,a4,a5);
    case 66: return svc_writev(c,nr,a0,a1,a2,a3,a4,a5);
    case 67: return svc_pread64(c,nr,a0,a1,a2,a3,a4,a5);
    case 68: return svc_pwrite64(c,nr,a0,a1,a2,a3,a4,a5);
    case 69: return svc_preadv(c,nr,a0,a1,a2,a3,a4,a5);
    case 70: return svc_pwritev(c,nr,a0,a1,a2,a3,a4,a5);
    case 71: return svc_sendfile(c,nr,a0,a1,a2,a3,a4,a5);
    case 75: return svc_tee(c,nr,a0,a1,a2,a3,a4,a5);
    case 76: return svc_splice(c,nr,a0,a1,a2,a3,a4,a5);
    case 77: return svc_ftruncate(c,nr,a0,a1,a2,a3,a4,a5);
    case 23: return svc_dup(c,nr,a0,a1,a2,a3,a4,a5);
    case 24: return svc_dup3(c,nr,a0,a1,a2,a3,a4,a5);
    case 25: return svc_fcntl(c,nr,a0,a1,a2,a3,a4,a5);
    case 29: return svc_ioctl(c,nr,a0,a1,a2,a3,a4,a5);
    case 59: return svc_pipe2(c,nr,a0,a1,a2,a3,a4,a5);
    case 82: return svc_fsync(c,nr,a0,a1,a2,a3,a4,a5);
    case 83: return svc_fdatasync(c,nr,a0,a1,a2,a3,a4,a5);
    case 285: return svc_copy_file_range(c,nr,a0,a1,a2,a3,a4,a5);
    case 84: return svc_sync_file_range(c,nr,a0,a1,a2,a3,a4,a5);
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
