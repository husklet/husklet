// Extracted from service(): Filesystem -- open/openat/stat*/dir/link/perm/xattr/cwd/access, every path
// confined to the rootfs jail (overlay copy-up, /proc/self/exe synth). Returns 1 if nr was handled, 0
// otherwise. Included by service.c AFTER its local helpers (overlay_*/proc_self_exe/synth_str_fd/
// cpu_range_str it calls) and before service() -- same TU scope.

// A terminal-control syscall (tcsetpgrp/tcsetattr) issued by a process that is in a BACKGROUND process
// group raises SIGTTOU on the whole group; with the default disposition that STOPS it. During job-control
// handoff a shell's pipeline child briefly sits in a background group between its setpgid() and the
// parent's tcsetpgrp(), so a foreground command can be SIGTTOU-stopped before it even execs (the
// "[1]+ Stopped  ls | cat" hang -- the engine's in-process children lose this race more readily than a
// real kernel does). POSIX guarantees that when SIGTTOU is blocked the call simply succeeds and NO signal
// is generated -- which is exactly what a correct shell does around these calls (bash's give_terminal_to).
// So block SIGTTOU on the host for the duration of the REAL call: it never fakes the operation (the real
// tcsetpgrp/tcsetattr still runs on the real pty) and is a no-op when the guest already blocked it.
// statfs(2)/fstatfs(2) f_type + geometry fidelity inside a container. A real container's mount tree puts
// the rootfs on OVERLAYFS and the kernel pseudo-filesystems (/proc, /sys, /sys/fs/cgroup, /dev*) on their
// own magic types with the pseudo ones reporting ZERO blocks. dd resolves every guest path into ONE host
// (macOS) directory tree, so a naive host statfs stamps the SAME magic + the SAME real-disk geometry on
// every path -- so `stat -f -c %T /proc` prints the wrong type and `df -h` lists /proc & /sys with a huge
// bogus size (busybox/coreutils df hides a mount only when f_blocks==0, which the pseudo-fs must report).
// Classify by the guest ABSOLUTE path and return the Linux magic; `*zero` marks a pseudo-fs whose block/
// inode counts must be forced to 0 (proc/sysfs/cgroup2). Only used in container (g_rootfs) mode.
static int64_t guest_statfs_magic(const char *g, int *zero) {
    *zero = 0;
    if (!strcmp(g, "/proc") || !strncmp(g, "/proc/", 6)) {
        *zero = 1;
        return 0x9fa0;
    } // PROC_SUPER_MAGIC
    if (!strcmp(g, "/sys/fs/cgroup") || !strncmp(g, "/sys/fs/cgroup/", 15)) {
        *zero = 1;
        return 0x63677270;
    } // CGROUP2
    if (!strcmp(g, "/sys") || !strncmp(g, "/sys/", 5)) {
        *zero = 1;
        return 0x62656572;
    } // SYSFS_MAGIC
    if (!strcmp(g, "/dev/mqueue") || !strncmp(g, "/dev/mqueue/", 12)) return 0x19800202; // MQUEUE_MAGIC
    if (!strcmp(g, "/dev/pts") || !strncmp(g, "/dev/pts/", 9)) return 0x1cd1;            // DEVPTS_SUPER_MAGIC
    // /dev/shm is its OWN tmpfs mount in docker (separate from the /dev tmpfs); classify it explicitly so
    // `stat -f /dev/shm` names tmpfs and `df /dev/shm` shows a real (non-zero) size regardless of any future
    // change to the /dev catch-all below.
    if (!strcmp(g, "/dev/shm") || !strncmp(g, "/dev/shm/", 9)) return 0x01021994; // TMPFS_MAGIC
    if (!strcmp(g, "/dev") || !strncmp(g, "/dev/", 5)) return 0x01021994;         // TMPFS_MAGIC
    return 0x794c7630;                                                            // OVERLAYFS_SUPER_MAGIC (rootfs)
}

static void tty_ctl_block(sigset_t *saved) {
    sigset_t blk;
    sigemptyset(&blk);
    sigaddset(&blk, SIGTTOU);
    sigprocmask(SIG_BLOCK, &blk, saved);
}

static void tty_ctl_restore(const sigset_t *saved) {
    sigprocmask(SIG_SETMASK, saved, NULL);
}

// statx returns device numbers as separate major/minor u32s, whereas struct stat packs them into a
// single st_dev/st_rdev field that the guest decodes with glibc's gnu_dev_major/minor. fill_linux_stat
// copies the host dev value into st_dev/st_rdev VERBATIM, so for statx to report the SAME major:minor a
// caller would compute from fstat/newfstatat, statx must apply those very macros to that same raw value.
static inline uint32_t lin_dev_major(uint64_t dev) {
    return (uint32_t)(((dev >> 8) & 0xfffu) | ((uint32_t)(dev >> 32) & ~0xfffu));
}

static inline uint32_t lin_dev_minor(uint64_t dev) {
    return (uint32_t)((dev & 0xffu) | ((uint32_t)(dev >> 12) & ~0xffu));
}

// Overlay getdents64 snapshot cache (case 61): the merged cross-layer listing for a directory fd is taken
// once on the first getdents call and consumed across the many small reads libc makes. Keyed by guest
// fd+1 (0 == free). A slot MUST be invalidated on close() -- ovldents_drop, called from case 57 -- so a
// reused fd re-snapshots a fresh directory rather than serving the previous one's leftover tail. Without
// that, a directory read partially then closed poisoned the next directory opened on the same fd, which
// silently truncated postgres initdb's template1->template0/postgres copy (dropping ~1/4 of the catalog,
// e.g. PG_VERSION -> "base/5 is not a valid data directory" on the first client connect).
// nm/ty are heap-allocated by overlay_readdir (it grows them to the real entry count -- no 1024 cap, so
// large directories no longer truncate) and owned until freed (ovldents_free). Indexed DIRECTLY by
// guest fd (the getdents call site guarantees fd in [0,1024)); a former 16-slot table with slot-0 eviction
// broke deep `find`: a recursive walk keeps one open dir fd per level, so past 16 concurrent overlay dirs
// an ancestor's snapshot was evicted and its next getdents re-snapshotted from pos 0 -> re-descended the
// same subtree forever (loop threshold was exactly depth 16).
static struct {
    int taken; // 1 = this fd's snapshot is live
    int n, pos;
    char (*nm)[256];
    uint8_t *ty;
} g_ovldents[1024];

static void ovldents_free(int i) {
    free(g_ovldents[i].nm);
    free(g_ovldents[i].ty);
    g_ovldents[i].nm = NULL;
    g_ovldents[i].ty = NULL;
    g_ovldents[i].taken = 0;
    g_ovldents[i].n = g_ovldents[i].pos = 0;
}

static void ovldents_drop(int fd) {
    if (fd >= 0 && fd < 1024 && g_ovldents[fd].taken) ovldents_free(fd);
}

// rewinddir/seekdir on an overlay-merged dir: reset the replay cursor. pos<=0 (or out of range) restarts
// from the top; an untaken snapshot is left alone (the next getdents re-snapshots from 0). Forward-declared
// in vfs.c for the lseek handler (io.c), which is compiled into this TU before fs.c.
static void ovldents_rewind(int fd, int pos) {
    if (fd < 0 || fd >= 1024 || !g_ovldents[fd].taken) return;
    g_ovldents[fd].pos = (pos > 0 && pos <= g_ovldents[fd].n) ? pos : 0;
}

// POSIX shm / named semaphores live under /dev/shm, for which the guest /dev tmpfs has no real host tmpfs;
// glibc backs them with files there (shm_open -> /dev/shm/<name>, sem_open -> a temp /dev/shm/sem.<rnd>
// then link()ed to /dev/shm/sem.<name>). openat (case 56) redirects these to a real host file so the page
// is real and MAP_SHARED across fork; the link/rename/unlink that COMPLETE glibc's create dance must use
// the SAME backing, but the rootfs branches of those handlers resolve via jail_at into the container's
// /dev/shm and would otherwise diverge. Delegates to shm_backing_path (fscache.c): in container mode the
// backing sits inside the overlay upper's /dev/shm (per-container + visible to `ls /dev/shm`), in direct
// mode a flat /tmp file. Returns the host backing path for a /dev/shm/<name> guest path, or NULL otherwise.
static const char *shm_hostpath(const char *guest, char *buf, size_t n) {
    return shm_backing_path(guest, buf, n);
}

// a pty MASTER's termios + winsize are shared line-discipline state that Linux keeps on the master
// itself, so a program (apt/dpkg StartPtyMagic, ncurses, tmux) can get/set them on the master fd without
// ever opening the slave. macOS instead keeps that state in the tty struct, which is DESTROYED the instant
// the LAST slave fd closes -- so servicing a master's TIOCSWINSZ via a transient (open+use+close) slave
// loses the winsize immediately (a later TIOCGWINSZ on the master reads 0x0; verified on the host). We
// therefore CACHE the master's termios/winsize here keyed by the master fd, answer GETs from the cache,
// and on every SET both (a) push it to a transient slave so any *already-open* real slave sees it live and
// (b) stash it so ptm_apply_to_slave() can re-apply it when the guest later opens the real slave
// (/dev/pts/N, N == the master fd via TIOCGPTN). This reproduces exact Linux master semantics WITHOUT
// holding a slave open -- which would defeat the master read()/poll HUP-on-last-slave-close that script /
// tmux depend on to notice the child exited.
static uint8_t g_ptm_tset[DD_NFD], g_ptm_wset[DD_NFD];
static struct termios g_ptm_term[DD_NFD]; // host-form termios last set on the master
static struct winsize g_ptm_win[DD_NFD];  // winsize last set on the master

static void ptm_clear(int fd) {
    if (fd >= 0 && fd < DD_NFD) {
        g_ptm_tset[fd] = 0;
        g_ptm_wset[fd] = 0;
    }
}

// Re-apply a master's cached termios/winsize onto a freshly-opened slave fd (Linux: the slave shares the
// master's line discipline). `ptn` is the pts number, which dd defines to equal the master fd.
static void ptm_apply_to_slave(int ptn, int slavefd) {
    if (ptn < 0 || ptn >= DD_NFD || slavefd < 0) return;
    if (g_ptm_tset[ptn]) tcsetattr(slavefd, TCSANOW, &g_ptm_term[ptn]);
    if (g_ptm_wset[ptn]) ioctl(slavefd, TIOCSWINSZ, &g_ptm_win[ptn]);
}

// Tear down EVERY dd-side emulation-table entry keyed by this fd NUMBER (eventfd peer/counter/sema, timerfd,
// overlay-dir, the socket/loopback/bridge maps, epoll armed-state, flock, pidfd, RAM-scratch memf, and the
// getdents/overlay-dents caches + the path map). Shared by close(2) (case 57) AND the emulated
// close-on-exec sweep (proc.c exec_close_cloexec*). dd's execve reloads the new image IN-PROCESS, so the
// sweep hand-closes each FD_CLOEXEC descriptor -- but it used to close ONLY the real fd, leaving these tables
// stamped. A CLOEXEC eventfd thus left g_eventfd_peer[fd] set after exec; the new program (postgres) opened
// postgresql.conf onto that freed fd number and read() was misrouted to the eventfd emulation -> 0 bytes of
// real content -> `syntax error in file "postgresql.conf" line 1, near token ""` and the server never starts
// (PG16/17 only -- PG15's streaming conf reader tolerated the short read; hence the version gate). Does NOT
// close(fd) itself -- the caller owns the real fd's lifetime. Safe on a non-emulated fd (every branch is
// guarded / idempotent). Mirrors case 57's teardown exactly so close(2) semantics are unchanged.
static void fd_reset_emul(int fd) {
    if (fd >= 0 && fd < DD_NFD) {
        if (g_eventfd_peer[fd]) {
            // Refcounted teardown: a dup()'d eventfd shares the peer write end + counter slot, so only close
            // the peer and zero the shared counter when the LAST alias closes -- otherwise closing one
            // duplicate would break the object for the others (fd_carry_virt bumps the slot refcount on dup).
            int eslot = eventfd_counter_slot(fd);
            if (--g_eventfd_refs[eslot] <= 0) {
                close(g_eventfd_peer[fd] - 1);
                g_eventfd_count[eslot] = 0;
                g_eventfd_refs[eslot] = 0;
            }
            g_eventfd_peer[fd] = 0;
            g_eventfd_cslot[fd] = 0;
            g_eventfd_sema[fd] = 0;
        }
        g_timerfd[fd] = 0;
        g_tfd_deadline[fd] = 0;
        g_tfd_interval[fd] = 0;
        g_tfd_first_oneshot[fd] = 0;
        g_memfd_is[fd] = 0;
        g_memfd_seal[fd] = 0;
        g_proc_text_desc[fd][0] = 0;
        g_proc_text_ro[fd] = 0;
        g_pagemap_fd[fd] = 0;
        g_pipesz[fd] = 0;     // drop this fd's emulated F_SETPIPE_SZ so a reused number reports the default
        g_fd_cport[fd] = 0;   // drop the captured container port so getpeername on a reused fd isn't misrouted
        inotify_fd_reset(fd); // instance/watch teardown -- g_inotify[fd] used to stay stamped (stale routing)
        // signalfd: this fd was one alias of a signalfd OFD. Drop it and, when it was the LAST alias, tear the
        // OFD down (close the engine-private write end; the read end is closed by the caller's close(2)).
        if (g_sigfd_slot[fd]) {
            int sslot = g_sigfd_slot[fd] - 1;
            g_sigfd_slot[fd] = 0;
            if (--g_sfd[sslot].refs <= 0) {
                if (g_sfd[sslot].wr >= 0) close(g_sfd[sslot].wr);
                g_sfd[sslot].wr = g_sfd[sslot].rd = -1;
                g_sfd[sslot].mask = 0;
                g_sfd[sslot].refs = 0;
            }
        }
        if (g_dn_mask[fd]) dnotify_apply(fd, 0, 0); // remove this fd's dnotify (F_NOTIFY) watch before it closes
        g_lease[fd] = 0;      // release the F_SETLEASE lease this fd held (POSIX: lease dropped on close)
        g_fsig[fd] = 0;       // drop the fd's F_SETSIG signal so a reused number reports the SIGIO default
        if (g_fd_pushback[fd]) {
            free(g_fd_pushback[fd]);
            g_fd_pushback[fd] = NULL;
            g_fd_pb_len[fd] = 0;
        }
        // g_ovldir/g_opath are sized [1024], NOT [DD_NFD] (see the io.c read guards, all `fd < 1024`). The
        // enclosing `fd < DD_NFD` guard is too loose for these two: close_range(first, ~0U) — glibc's fd
        // sanitize, which erl_child_setup runs before every port fork — is clamped to fd 65535 and calls
        // fd_reset_emul() for EVERY fd in the range, so an unguarded g_ovldir[fd][0]/g_opath[fd] write here
        // stored WILD into BSS for any fd >= 1024 (fd*192 bytes past g_ovldir). That intermittently hit an
        // unmapped page (SIGSEGV/SIGBUS -> a hang when g_in_service re-faults, or SIGILL after it corrupted
        // engine state into a wild control-flow jump) — the #215 "beam.smp fork+exec control-flow corruption".
        if (fd < 1024) {
            g_ovldir[fd][0] = 0;
            g_opath[fd] = 0;
        }
        g_devfull[fd] = 0;
        g_devseed[fd] = 0;
        g_devtty[fd] = 0;
        unix_bind_clear(fd);
#ifdef __APPLE__
        if (g_devdri[fd]) dd_gpu_free_fd(fd); // release the render node's IOSurfaces on close
#endif
        g_devdri[fd] = 0;
        g_lo_port[fd] = 0;
        g_sock_stream[fd] = 0;
        g_sock_conn[fd] = 0;
        g_sock_fam[fd] = 0;
        g_sock_dgram[fd] = 0;
        seq_send_eof(fd);
        g_sock_seqpacket[fd] = 0;
        g_sock_pair_peer[fd] = 0;
        g_sock_seq_wrote[fd] = 0;
        g_sock_peer_pid[fd] = 0;
        g_sock_passcred[fd] = 0;
        g_br_port[fd] = 0;
        g_br_ip[fd] = 0;
        g_tcp_lport[fd] = 0; // drop a reused fd's stale listener so /proc/net/tcp doesn't show a ghost
        g_tcp_listen[fd] = 0;
        if (g_dns_sock[fd]) { // container DNS: close the engine-held socketpair peer
            if (g_dns_peer[fd] >= 0) close(g_dns_peer[fd]);
            g_dns_peer[fd] = -1;
            g_dns_sock[fd] = 0;
        }
        nl_close(fd); // tear down a netlink socket's socketpair peer
        // (eventfd counter/cslot/sema teardown is handled refcounted in the g_eventfd_peer block above so a
        // surviving dup keeps the shared counter; do NOT unconditionally zero the shared slot here.)
        ep_close_rehome(fd); // if this watched fd's OFD survives via a dup, re-home its epoll knote (before reset)
        ep_fd_reset(fd);
        flock_on_close(fd);
        poslk_on_close(fd); // POSIX drops all this process's fcntl record locks when any fd closes
        ptm_clear(fd);      // drop this fd's cached pty-master termios/winsize (see ptm cache below)
        pts_on_close(fd);   // free a master's devpts index (+ /dev/pts/N node) / clear a slave's stamp
    }
    pidfd_forget(fd);
    memf_close(fd);
    dirs_drop(fd);
    ovldents_drop(fd);
    fd_clear(fd);
}

// Linux *at dirfd precondition, shared by the fstatat/statx/link/symlink/rename/unlink/... family.
// For a RELATIVE path with dirfd != AT_FDCWD the kernel resolves the descriptor FIRST: EBADF if it is not an
// open fd, ENOTDIR if it is open but not a directory. dd folds the dirfd into an absolute host path via
// g_fdpath, which silently accepts a bad/regular-file dirfd -- so those errnos were never produced (fstatat
// on a non-dir dirfd wrongly "succeeded", symlinkat/linkat leaked macOS EOPNOTSUPP, statx returned EBADF for
// a non-dir dirfd). dd shares the host descriptor table, so validate against the real fd. Returns 0 (ok) or
// -errno. Absolute paths, AT_FDCWD, and the empty path (AT_EMPTY_PATH / the ENOENT case) never consult the
// dirfd. (LTP fstatat01 / statx03 / symlinkat01 / linkat01.)
static int at_dirfd_check(int dirfd, const char *raw) {
    if (!raw || !raw[0] || raw[0] == '/') return 0; // empty or absolute: the dirfd is not walked
    if (dirfd == -100 /*AT_FDCWD*/) return 0;       // cwd-relative
    struct stat ds;
    if (fstat(dirfd, &ds) < 0) return -EBADF;  // not an open descriptor
    if (!S_ISDIR(ds.st_mode)) return -ENOTDIR; // open, but not a directory
    return 0;
}

// ---- guest xattr passthrough (overlay G5) -----------------------------------------------------------
// Real overlayfs exposes a file's xattrs (file caps, SELinux labels, user.* attrs) and copies them up on
// write; dd used to stub set->ignore / get->ENODATA / list->empty, silently dropping them (a correctness
// trap -- setcap "succeeded" but getcap saw nothing). We namespace guest xattrs under `user.ddx.` on the
// host backing inode so they round-trip AND survive copy-up (ovl_copy_xattrs carries `user.ddx.*`),
// without colliding with dd's own `user.dd.*` owner attrs or host/macOS attrs. The macOS errno is mapped
// to Linux at the dispatch boundary (ENOATTR->ENODATA).
#define DDX_PFX "user.ddx."

// Host backing path for a path-based xattr op. forwrite copies a lower-only file up first (attr lands on
// the writable upper). Returns 0 (host filled) or -errno.
static int xattr_hostpath(const char *path, int nofollow, int forwrite, char *host, size_t hn) {
    if (!g_rootfs) {
        snprintf(host, hn, "%s", path ? path : "");
        return 0;
    }
    char gp[4200];
    abs_guest(-100 /*AT_FDCWD*/, path, gp, sizeof gp);
    if (g_nlower) {
        if (forwrite) {
            overlay_copyup(gp, host, hn);
            return 0;
        }
        return overlay_resolve(gp, host, hn, nofollow) ? 0 : -ENOENT;
    }
    secure_resolve(gp, host, hn, nofollow);
    return 0;
}

// setxattr with Linux XATTR_CREATE/XATTR_REPLACE semantics. macOS honours each flag alone, but rejects
// the *combination* with EINVAL — where Linux's simple_xattr_set never does: it tests XATTR_CREATE first
// (an if/else-if), so CREATE+REPLACE on an existing attr yields EEXIST, on a missing one yields ENODATA.
// So we resolve the create/replace precondition ourselves against a host existence probe and hand macOS
// a plain set (flags=0), which reproduces the kernel byte-for-byte and never leaks macOS's EINVAL.
static long ddx_set(const char *host, const char *name, const void *val, size_t sz, uint64_t lflags, int nofollow) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", DDX_PFX, name ? name : "");
    int opt = nofollow ? XATTR_NOFOLLOW : 0;
    if (lflags & 3) { // XATTR_CREATE(1) | XATTR_REPLACE(2)
        int exists = getxattr(host, hn, NULL, 0, 0, opt) >= 0;
        if ((lflags & 1) && exists) return -EEXIST;   // XATTR_CREATE on an existing attr
        if ((lflags & 2) && !exists) return -ENOATTR; // XATTR_REPLACE on a missing attr -> ENODATA (m2l)
    }
    return setxattr(host, hn, val, sz, 0, opt) < 0 ? -errno : 0;
}

static long ddx_get(const char *host, const char *name, void *val, size_t sz, int opt) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", DDX_PFX, name ? name : "");
    ssize_t r = getxattr(host, hn, val, sz, 0, opt);
    return r < 0 ? -errno : r;
}

static long ddx_remove(const char *host, const char *name, int opt) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", DDX_PFX, name ? name : "");
    return removexattr(host, hn, opt) < 0 ? -errno : 0;
}

// List only the guest-visible (user.ddx.*) attrs, prefix stripped, into the guest buffer. sz==0 -> size.
static long ddx_list(const char *host, char *out, size_t sz, int opt) {
    char raw[65536];
    ssize_t n = listxattr(host, raw, sizeof raw, opt);
    if (n < 0) return -errno;
    size_t need = 0, pl = strlen(DDX_PFX);
    for (ssize_t i = 0; i < n;) {
        const char *nm = raw + i;
        size_t l = strlen(nm);
        i += l + 1;
        if (l > pl && !strncmp(nm, DDX_PFX, pl)) {
            const char *g = nm + pl;
            size_t gl = strlen(g) + 1;
            if (sz) {
                if (need + gl > sz) return -ERANGE;
                memcpy(out + need, g, gl);
            }
            need += gl;
        }
    }
    return (long)need;
}

// mount(2). The historical stub returned 0 unconditionally, so a container entrypoint's `mount --bind`,
// `mount -t tmpfs`, and `mount -o remount,ro` silently did NOTHING -- wrong dir content and, worse, an
// UNENFORCED read-only mount (a silent correctness/security hole). Implement the cases an entrypoint
// actually issues against dd's vfs: bind = a bind-vol alias to the source's host backing; tmpfs/ramfs = a
// fresh empty host scratch dir; remount,ro = enforce RO (whole-rootfs g_rootfs_ro / a bind-vol's ro flag /
// a per-subtree path-based RO list). Pseudo-filesystems dd already synthesizes (proc/sysfs/cgroup/devpts/
// mqueue/...) are a genuine no-op success (they ARE present at their mount point). Anything dd cannot
// materialize returns the honest Linux errno instead of a fake 0. MS_RDONLY=1 REMOUNT=0x20 BIND=0x1000.
static int64_t svc_mount(struct cpu *c, uint64_t a_src, uint64_t a_tgt, uint64_t a_fstype, uint64_t a_flags) {
    (void)c;
    if (!g_rootfs) return 0; // bare (no-jail) mode: nothing to alias into -> keep the legacy success
    const char *src = (const char *)a_src, *tgtraw = (const char *)a_tgt, *fstype = (const char *)a_fstype;
    unsigned long fl = (unsigned long)a_flags;
    if (!tgtraw || guest_bad_ptr((uintptr_t)tgtraw, 1)) return -EFAULT;
    char tgt[4200];
    guest_abspath_at(-100, tgtraw, tgt, sizeof tgt); // guest-absolute, lexically normalized
    if (tgt[0] != '/') return -EINVAL;

    // MS_REMOUNT: change an existing mount's flags. Enforce read-only; ignore other churn.
    if (fl & 0x20) {
        int vi = jail_match(tgt);
        if (fl & 0x1) { // remount,ro
            if (!strcmp(tgt, "/")) {
                g_rootfs_ro = 1;
                return 0;
            }
            if (vi >= 0) {
                g_vols[vi].ro = 1;
                return 0;
            }
            return ro_subpath_add(tgt) == 0 ? 0 : -ENOMEM;
        }
        // remount,rw (relax where cleanly possible; a path-based RO subtree can't be un-listed race-free).
        if (!strcmp(tgt, "/"))
            g_rootfs_ro = 0;
        else if (vi >= 0)
            g_vols[vi].ro = 0;
        return 0;
    }

    // MS_BIND: alias the target subtree to the source path's host backing.
    if (fl & 0x1000) {
        if (!src || guest_bad_ptr((uintptr_t)src, 1)) return -EFAULT;
        char sabs[4200], shost[4200];
        guest_abspath_at(-100, src, sabs, sizeof sabs);
        if (sabs[0] != '/') return -EINVAL;
        if (!secure_resolve(sabs, shost, sizeof shost, 0)) return -EACCES; // escaped the jail
        struct stat st;
        if (stat(shost, &st) != 0) return -ENOENT; // Linux: bind of a missing source -> ENOENT
        return rt_add_vol(tgt, shost, (fl & 0x1) ? 1 : 0);
    }

    // A named filesystem type.
    if (fstype && guest_bad_ptr((uintptr_t)fstype, 1)) return -EFAULT;
    char ft[64];
    ft[0] = 0;
    if (fstype)
        for (size_t k = 0; k < sizeof ft - 1 && fstype[k]; k++) {
            ft[k] = fstype[k];
            ft[k + 1] = 0;
        }
    // Pseudo-filesystems dd already serves at their canonical mount points -> a real no-op success.
    if (!strcmp(ft, "proc") || !strcmp(ft, "sysfs") || !strcmp(ft, "cgroup") || !strcmp(ft, "cgroup2") ||
        !strcmp(ft, "devpts") || !strcmp(ft, "mqueue") || !strcmp(ft, "devtmpfs") || !strcmp(ft, "debugfs") ||
        !strcmp(ft, "securityfs") || !strcmp(ft, "tracefs") || !strcmp(ft, "configfs") || !strcmp(ft, "bpf") ||
        !strcmp(ft, "fusectl") || !strcmp(ft, "pstore") || !strcmp(ft, "sysctl"))
        return 0;
    // tmpfs / ramfs: back the mount point with a fresh, empty host scratch dir.
    if (!strcmp(ft, "tmpfs") || !strcmp(ft, "ramfs")) {
        char tmpl[] = "/tmp/.ddtmpfsXXXXXX";
        if (!mkdtemp(tmpl)) return -errno;
        int64_t r = rt_add_vol(tgt, tmpl, (fl & 0x1) ? 1 : 0);
        if (r < 0) rmdir(tmpl);
        return r;
    }
    if (ft[0] == 0) return -EINVAL; // mount without a type, bind, or remount is invalid
    // A real block/overlay/nfs/... filesystem dd cannot materialize -> the honest errno, NOT a fake 0 that
    // would leave the mount point showing the wrong (still-unmounted) content.
    return -ENODEV;
}

static int svc_fs(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                  uint64_t a5) {
    switch (nr) {
    // ===================== Filesystem — open/stat/dir/link/perm/xattr/cwd, all path-confined to the rootfs jail
    // =====================
    // setxattr(5)/lsetxattr(6)/fsetxattr(7): a0=path|fd, a1=name, a2=val, a3=size, a4=flags
    case 5:
    case 6:
    case 7: {
        char host[4300];
        int e;
        if (nr == 7)
            e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 6, 1, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)ddx_set(host, (const char *)a1, (const void *)a2, (size_t)a3, a4, nr == 6);
        break;
    }
    // getxattr(8)/lgetxattr(9)/fgetxattr(10): a0=path|fd, a1=name, a2=val, a3=size
    case 8:
    case 9:
    case 10: {
        char host[4300];
        int e;
        if (nr == 10)
            e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 9, 0, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) =
            (uint64_t)(int64_t)ddx_get(host, (const char *)a1, (void *)a2, (size_t)a3, nr == 9 ? XATTR_NOFOLLOW : 0);
        break;
    }
    // listxattr(11)/llistxattr(12)/flistxattr(13): a0=path|fd, a1=list, a2=size
    case 11:
    case 12:
    case 13: {
        char host[4300];
        int e;
        if (nr == 13)
            e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 12, 0, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)ddx_list(host, (char *)a1, (size_t)a2, nr == 12 ? XATTR_NOFOLLOW : 0);
        break;
    }
    // removexattr(14)/lremovexattr(15)/fremovexattr(16): a0=path|fd, a1=name
    case 14:
    case 15:
    case 16: {
        char host[4300];
        int e;
        if (nr == 16)
            e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else
            e = xattr_hostpath((const char *)a0, nr == 15, 1, host, sizeof host);
        if (e < 0) {
            G_RET(c) = (uint64_t)(int64_t)e;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)ddx_remove(host, (const char *)a1, nr == 15 ? XATTR_NOFOLLOW : 0);
        break;
    }
    case 17: {
        // getcwd(BUF, size). Resolve the guest cwd into an ENGINE-local buffer first, then apply the exact
        // kernel order (fs/dcache.c SYSCALL_DEFINE2(getcwd)): the path length is compared to `size` BEFORE any
        // copy_to_user, so a too-small buffer is -ERANGE regardless of BUF's validity, and only when the path
        // FITS does the copy run -> -EFAULT on a NULL/bad BUF. The old code passed the guest BUF straight to
        // the host getcwd(BUF,size): a NULL/huge-size probe (LTP getcwd01 case 2: buf=NULL,size=(size_t)-1)
        // made libc getcwd write through NULL -> SIGSEGV in the engine instead of returning EFAULT.
        char cwbuf[4200];
        const char *cw;
        if (g_rootfs) {
            cw = g_cwd[0] ? g_cwd : "/"; // the GUEST cwd (not the host path)
        } else {
            // bare mode: the engine chdir()s for real, so the live host cwd IS the guest cwd
            if (!getcwd(cwbuf, sizeof cwbuf)) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            cw = cwbuf;
        }
        size_t len = strlen(cw) + 1; // path length INCLUDING the terminating NUL, exactly like the kernel
        if (len > (size_t)a1) {
            G_RET(c) = (uint64_t)(-ERANGE);
            break;
        }
        if (!a0 || !host_range_mapped((uintptr_t)a0, len)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        memcpy((void *)a0, cw, len);
        G_RET(c) = len;
        break;
    }
    // ioctl(fd, req, arg) -- Linux req# -> macOS
    case 29: {
        int fd = (int)a0;
        // Truncate the ioctl request to 32 bits (Linux `cmd` is unsigned int). musl declares ioctl's request
        // as `int`, so a read-direction request with the direction bit set (e.g. TIOCGPTN 0x80045430) arrives
        // SIGN-EXTENDED as 0xffffffff80045430 and would miss its switch case -> ENOTTY (glibc zero-extends, so
        // it worked there). This makes both forms match, fixing musl tmux/script/openpty and any high-bit ioctl.
        unsigned long rq = (uint32_t)a1;
        void *arg = (void *)a2;
        // GPU rung 2 (opt-in): the dd render node services DD_IOCTL_GPU_ALLOC (host-IOSurface buffer).
        // Gated on the per-fd render-node tag, so no other fd/ioctl is affected.
        if (gpu_iosurface_on() && fd >= 0 && fd < DD_NFD && g_devdri[fd] && rq == DD_IOCTL_GPU_ALLOC) {
            G_RET(c) = (uint64_t)dd_gpu_alloc(fd, arg);
            break;
        }
        // DRM ioctls chromium/Mesa(kms_swrast) issue on the render node once it opens the discovered node:
        // drmGetVersion (VERSION), GET_CAP, SET_CLIENT_CAP, and the dumb-buffer + PRIME set that Mesa's
        // software GBM winsys uses to allocate/map/export the window buffer (CREATE/MAP/DESTROY_DUMB,
        // GEM_CLOSE, PRIME_HANDLE_TO_FD). Gated on the render-node tag.
        if (gpu_iosurface_on() && fd >= 0 && fd < DD_NFD && g_devdri[fd] &&
            (rq == 0xc0406400 || rq == 0xc0106c0c || rq == 0x400c6d0d || rq == 0xc02064b2 ||
             rq == 0xc01064b3 || rq == 0xc00464b4 || rq == 0x40086409 || rq == 0xc00c642d)) {
            int64_t rr = drm_synth_ioctl(fd, rq, arg);
            G_RET(c) = (uint64_t)rr;
            break;
        }
        // macOS pty MASTERS reject every termios/winsize ioctl with ENOTTY -- unlike Linux, where the master
        // accepts them and they act on the shared line discipline (apt/dpkg's StartPtyMagic does TIOCSWINSZ +
        // tcsetattr(TCSANOW) on the master; that ENOTTY is apt's "Setting TIOCSWINSZ for master fd N failed"
        // / "Setting in Start via TCSANOW ... failed" and the debconf frontend cascade that follows). termios
        // + winsize are properties of the pty PAIR, so when the request targets a master we retarget the op to
        // a transient slave fd -- giving the guest exact Linux master semantics on x86 and arm alike.
        //
        // A master is detected by ptsname(fd) resolving its slave device -- NOT by isatty(). On macOS
        // isatty() returns 1 for a pty master (it is a tty-class char device) even though every termios ioctl
        // on it fails ENOTTY, so the old `if (!isatty(fd))` gate skipped the retarget for exactly the masters
        // that need it (-- apt never opens the slave, so nothing masked it; ext_posix/pty passed only
        // because it opened the slave first, which happens to flip isatty). ptsname()!=NULL is the precise
        // "fd is a pty master" test: a slave or ordinary tty returns NULL (ENOTTY) and we operate on fd
        // directly, which is correct -- those accept termios/winsize as-is.
        // The GET/SET on a master answers from / writes to dd's per-master termios+winsize cache (g_ptm_*);
        // a SET is also pushed to a transient slave so any real slave the guest ALREADY holds open sees it
        // live, and re-applied via ptm_apply_to_slave() when the guest later opens the slave.
        int tfd = fd, pts_slave = -1, is_master = 0;
        switch (rq) {
        case 0x5401:
        case 0x5402:
        case 0x5403:
        case 0x5404: // TCGETS / TCSETS{,W,F}
        case 0x5413:
        case 0x5414: // TIOCGWINSZ / TIOCSWINSZ
        case 0x802c542a:
        case 0x402c542b:
        case 0x402c542c:
        case 0x402c542d: // TCGETS2 / TCSETS2{,W,F}
        {
            // "Is fd a pty master?" -- apt/dpkg StartPtyMagic does TIOCSWINSZ + tcsetattr(TCSANOW) on the
            // master WITHOUT ever opening the slave, and a macOS master ENOTTYs every termios/winsize ioctl,
            // so mis-detecting the master here is exactly ("Setting TIOCSWINSZ/TCSANOW for master fd N
            // failed"). Consult dd's AUTHORITATIVE devpts registry first (pts_fd_is_master, stamped at
            // /dev/ptmx-open time): it is the source of truth for every master dd handed out and costs no
            // host syscall. ptsname(fd) is kept as an independent confirmation AND to resolve the host
            // slave device so a SET can be live-pushed to a transient slave. A slave or ordinary tty is
            // neither (bit clear, ptsname==NULL) -> operate on fd directly, which is correct there. (Prior
            // code detected the master by ptsname ALONE; adding the registry makes detection authoritative
            // rather than dependent solely on a host heuristic, so a master dd tracks is recognized even if
            // ptsname ever fails to resolve it.)
            is_master = pts_fd_is_master(fd);
            char *sn = ptsname(fd); // non-NULL only for a real host pty master
            if (sn) is_master = 1;
            if (is_master && sn) {
                pts_slave = open(sn, O_RDWR | O_NOCTTY);
                if (pts_slave >= 0) tfd = pts_slave;
            }
        } break;
        default: break;
        }
        switch (rq) {
        case 0x5401: {
            struct termios t;
            // TCGETS
            if (is_master && g_ptm_tset[fd])
                t = g_ptm_term[fd]; // master keeps its own termios (Linux)
            else if (tcgetattr(tfd, &t) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            termios_m2l(&t, (uint8_t *)arg);
            G_RET(c) = 0;
            break;
        }
        case 0x5402:
        case 0x5403:
        case 0x5404: {
            struct termios t;
            // TCSETS/W/F
            termios_l2m((const uint8_t *)arg, &t);
            int act = rq == 0x5402 ? TCSANOW : rq == 0x5403 ? TCSADRAIN : TCSAFLUSH;
            sigset_t sv;
            tty_ctl_block(&sv);              // a bg-group tcsetattr would otherwise SIGTTOU-stop the caller
            int r = tcsetattr(tfd, act, &t); // push live to any open real slave (best effort on a master)
            tty_ctl_restore(&sv);
            if (is_master) {
                g_ptm_term[fd] = t;
                g_ptm_tset[fd] = 1;
                G_RET(c) = 0;
            } // master always accepts
            else
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        case 0x802c542a: {
            struct termios t;
            // TCGETS2 (glibc aarch64 uses this)
            if (is_master && g_ptm_tset[fd])
                t = g_ptm_term[fd];
            else if (tcgetattr(tfd, &t) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            termios_m2l(&t, (uint8_t *)arg);
            *(uint32_t *)((uint8_t *)arg + 36) = (uint32_t)cfgetispeed(&t);
            *(uint32_t *)((uint8_t *)arg + 40) = (uint32_t)cfgetospeed(&t);
            G_RET(c) = 0;
            break;
        }
        case 0x402c542b:
        case 0x402c542c:
        case 0x402c542d: {
            struct termios t;
            // TCSETS2/W2/F2
            termios_l2m((const uint8_t *)arg, &t);
            cfsetispeed(&t, *(uint32_t *)((const uint8_t *)arg + 36));
            cfsetospeed(&t, *(uint32_t *)((const uint8_t *)arg + 40));
            int act = rq == 0x402c542b ? TCSANOW : rq == 0x402c542c ? TCSADRAIN : TCSAFLUSH;
            sigset_t sv;
            tty_ctl_block(&sv); // a bg-group tcsetattr would otherwise SIGTTOU-stop the caller
            int r = tcsetattr(tfd, act, &t);
            tty_ctl_restore(&sv);
            if (is_master) {
                g_ptm_term[fd] = t;
                g_ptm_tset[fd] = 1;
                G_RET(c) = 0;
            } else
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        case 0x5413: // TIOCGWINSZ (struct same on all)
            if (is_master && g_ptm_wset[fd]) {
                if (arg) *(struct winsize *)arg = g_ptm_win[fd];
                G_RET(c) = 0;
            } else
                G_RET(c) = ioctl(tfd, TIOCGWINSZ, arg) < 0 ? (uint64_t)(-errno) : 0;
            break;
        case 0x5414: {                           // TIOCSWINSZ
            int r = ioctl(tfd, TIOCSWINSZ, arg); // live-push to any open real slave
            if (is_master) {
                if (arg) g_ptm_win[fd] = *(struct winsize *)arg;
                g_ptm_wset[fd] = 1;
                G_RET(c) = 0;
            } else
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        case 0x80045430: {
            // TIOCGPTN -> the Linux devpts index N dd assigned this master at /dev/ptmx-open time (ptsname(3)
            // and musl/glibc openpty build "/dev/pts/N" from it). Fall back to the fd for an untracked master.
            int n = pts_index_of_master(fd);
            if (n < 0) n = fd;
            if (arg) *(uint32_t *)arg = (uint32_t)n;
            G_RET(c) = 0;
            break;
        }
        // TIOCSPTLCK (unlockpt done at open)
        case 0x40045431: G_RET(c) = 0; break;
        // TIOCGPTPEER (_IO('T',0x41) == 0x5441; no direction bit, so it arrives unchanged under both musl
        // and glibc) -- glibc's openpty() opens the slave in a SINGLE call from the master fd instead of the
        // ptsname()+open() dance. `fd` is the master and (as TIOCGPTN reports) IS the pts#, so ptsname(fd)
        // resolves the host slave device -- the exact path the `/dev/pts/N` open uses. `arg` carries open
        // flags (O_RDWR|O_NOCTTY, glibc may OR in O_CLOEXEC 0x80000); open the slave and RETURN the new fd,
        // like a dup/open. (musl's openpty takes a different ptsname route and never issues this.)
        case 0x5441: {
            char *sn = ptsname(fd);
            if (!sn) {
                G_RET(c) = (uint64_t)(int64_t)(-(errno ? errno : EINVAL));
                break;
            }
            int mf = ((int)a2 & 0x3) | O_NOCTTY; // access mode (shared values) + no controlling tty
            if (a2 & 0x80000) mf |= O_CLOEXEC;   // honor Linux O_CLOEXEC on the returned fd
            int s = open(sn, mf);
            if (s >= 0) {
                ptm_apply_to_slave(fd, s); // slave inherits the master's cached termios/winsize
                int n = pts_index_of_master(fd);
                if (n >= 0) pts_note_slave(s, n); // stamp the slave's /dev/pts/N identity + publish node
            }
            G_RET(c) = s < 0 ? (uint64_t)(-errno) : (uint64_t)s;
            break;
        }
        case 0x5421: {
            // FIONBIO
            int on = arg ? *(int *)arg : 0, fl = fcntl(fd, F_GETFL);
            fl = on ? (fl | O_NONBLOCK) : (fl & ~O_NONBLOCK);
            G_RET(c) = fcntl(fd, F_SETFL, fl) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // FIONREAD
        case 0x541b: G_RET(c) = ioctl(fd, FIONREAD, arg) < 0 ? (uint64_t)(-errno) : 0; break;
        // FIOCLEX
        case 0x5451: G_RET(c) = fcntl(fd, F_SETFD, FD_CLOEXEC) < 0 ? (uint64_t)(-errno) : 0; break;
        case 0x5450: {
            int fl = fcntl(fd, F_GETFD);
            G_RET(c) = fcntl(fd, F_SETFD, fl & ~FD_CLOEXEC) < 0 ? (uint64_t)(-errno) : 0;
            break;
            // FIONCLEX
        }
        // TIOCGPGRP/TIOCSPGRP -- REAL job control. The guest's children are real host processes (clone = host
        // fork) in the engine's session (the daemon's login_tty made the engine the pty's session leader), so
        // the kernel's own pty foreground-group machinery applies to them: a child placed in the foreground
        // really IS the fg group -> not SIGTTIN/SIGTTOU-frozen, and Ctrl-C/Ctrl-Z reach it. Two things make it
        // work: (1) here we virtualize only the INIT's identity -- the guest sees getpid()==1 while its real
        // host pgid is g_init_hostpid -- translating just that pair and passing real child pgids straight
        // through to the real host tcget/tcsetpgrp; (2) rt_sigprocmask mirrors the terminal-stop signals onto
        // the host mask, so bash's background tcsetpgrp handoff isn't SIG_DFL-stopped by the host kernel.
        case 0x540f: { // tcgetpgrp
            // Linux: a non-tty fd (regular file, pipe, socket) fails ENOTTY. The old getpgrp() fallback
            // faked success and let terminal detection treat a plain file as a controlling terminal.
            if (!isatty(fd)) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
                break;
            }
            pid_t fg = tcgetpgrp(fd);
            if (fg <= 0) fg = getpgrp();
            if (g_init_hostpid && fg == g_init_hostpid) fg = 1; // init's real group -> guest pgid 1
            if (arg) *(int *)arg = (int)fg;
            G_RET(c) = 0;
            break;
        }
        case 0x5410: { // tcsetpgrp
            // Linux: a non-tty fd fails ENOTTY rather than silently accepting a foreground-group set.
            if (!isatty(fd)) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
                break;
            }
            pid_t pg = arg ? *(int *)arg : 0;
            if (pg == 1 && g_init_hostpid) pg = g_init_hostpid; // guest pgid 1 -> init's real host group
            if (isatty(fd) && pg > 0) {
                // A pipeline leader calls tcsetpgrp while still in a background group (the parent shell sets
                // the foreground group concurrently); without blocking SIGTTOU here the host kernel would
                // STOP it mid-handoff -> the foreground command freezes ("[1]+ Stopped"). Block SIGTTOU so
                // the real tcsetpgrp installs the fg group cleanly (kernel still routes ^C/^Z afterwards).
                sigset_t sv;
                tty_ctl_block(&sv);
                (void)tcsetpgrp(fd, pg);
                tty_ctl_restore(&sv);
            }
            G_RET(c) = 0; // never surface an error -> bash never warns
            break;
        }
        // TIOCSCTTY -- acquire the controlling terminal for real when `fd` is a tty (best effort; the
        // login_tty in the daemon usually already did this for the session leader), then report success so
        // an interactive shell's job-control setup never warns.
        case 0x540e:
            if (isatty(fd)) (void)ioctl(fd, TIOCSCTTY, 0);
            G_RET(c) = 0;
            break;
        default: {
            // Socket ioctls (SIOCGIF*): answer from the shared lo+eth0 model (netns.c) when `fd`
            // is a socket; otherwise ENOTTY.
            int64_t r;
            if (net_ioctl(fd, rq, (uint8_t *)arg, &r)) {
                G_RET(c) = (uint64_t)r;
                break;
            }
            G_RET(c) = (uint64_t)(-25); // ENOTTY
            break;
        }
        }
        if (pts_slave >= 0) close(pts_slave); // transient slave used to service a master's termios/winsize op
        break;
    }
    // mknodat(dirfd, path, mode, dev)
    case 33: {
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (g_rootfs) {
            if (g_nlower) {
                char gpm[4200];
                abs_guest((int)a0, (const char *)a1, gpm, sizeof gpm);
                // Merged-view errno the upper-only host mknodat can't produce (lower name -> EEXIST; a
                // lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT). Before whiteout clear.
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                overlay_clear_whiteout(gpm); // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker
            }
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = mknodat(pfd, fin, (mode_t)a2, (dev_t)a3), e = errno;
            char dp[4200];
            if (r >= 0 && fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                mc_evict(hp);
                ac_evict(hp);
                if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1); // dropped-cred creator owns it
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = mknodat(ATFD(a0), p, (mode_t)a2, (dev_t)a3);
        if (r >= 0) {
            mc_evict(p);
            ac_evict(p);
            if (newfile_stamp_wanted()) newfile_stamp_path(p, 1);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // mkdirat(dirfd, path, mode) -- confined
    case 34: {
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (g_rootfs) {
            // OVERLAY: recreating a name a lower still provides -> drop any stale `.wh.NAME` whiteout first
            // (else the new dir can be hidden by an order-dependent readdir dedup), and if a lower dir of the
            // same name exists, mark the new upper dir OPAQUE so the lower's stale children never re-surface.
            char gpm[4200];
            int had_lower_dir = 0;
            if (g_nlower) {
                abs_guest((int)a0, (const char *)a1, gpm, sizeof gpm);
                // Merged-view errno the upper-only host mkdirat can't produce (a lower still provides the
                // name -> EEXIST; a lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT).
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                overlay_clear_whiteout(gpm);
                had_lower_dir = overlay_lower_has_dir(gpm);
            }
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = mkdirat(pfd, fin, (mode_t)a2), e = errno;
            char dp[4200];
            if (r >= 0 && fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                mc_evict(hp);
                ac_evict(hp);
                if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1); // dropped-cred creator owns the dir
            }
            close(pfd);
            if (r >= 0 && had_lower_dir) overlay_set_opaque(gpm);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = mkdirat(ATFD(a0), p, (mode_t)a2);
        mc_evict(p);
        // namespace change -> evict
        ac_evict(p);
        if (r >= 0 && newfile_stamp_wanted()) newfile_stamp_path(p, 1);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // unlinkat(dirfd, path, flags) -- confined
    case 35: {
        // Linux rejects unknown flag bits (only AT_REMOVEDIR=0x200 is valid) with EINVAL BEFORE any
        // path resolution or removal -- otherwise a corrupted/probed flag value would silently fall
        // through and delete the target. This check precedes the EFAULT path check to match the kernel.
        if (a2 & ~0x200u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // a bad pathname pointer -> EFAULT (getname copy_from_user), before any resolution; and a
        // relative path under a bad/non-dir dirfd -> EBADF/ENOTDIR. (LTP unlink07.) guest_bad_ptr also faults
        // a PROT_NONE guard page (tst_get_bad_addr), which dd force-maps host-readable.
        if (!a1 || guest_bad_ptr(a1, 1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        // shm/sem files are flat host files under /tmp (see shm_hostpath); sem_unlink/shm_unlink and glibc's
        // temp-file cleanup must hit that backing, not the jail's <rootfs>/dev/shm. AT_REMOVEDIR never applies.
        char shb[4224];
        const char *shp = shm_hostpath((const char *)a1, shb, sizeof shb);
        if (shp) {
            G_RET(c) = unlink(shp) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        // RAM-backed scratch adoption: SQLite et al. open a temp file O_CREAT|O_EXCL then unlink it while
        // still open (delete-on-close). After this unlink drops its last link the file is anonymous, so we
        // may adopt it into RAM. Cheap pre-filter (avoid the fd scan on ordinary unlinks): a temp-dir path
        // or the sqlite "etilqs_" prefix, and not a directory removal. dev/ino is captured (per branch,
        // through the same resolution the unlink uses) right before the unlink and matched after.
        int try_adopt = 0;
        if (!memf_disabled() && !(a2 & 0x200)) {
            char gp[4200];
            abs_guest((int)a0, (const char *)a1, gp, sizeof gp);
            const char *base = strrchr(gp, '/');
            base = base ? base + 1 : gp;
            try_adopt = !strncmp(gp, "/tmp/", 5) || !strncmp(gp, "/var/tmp/", 9) || strstr(base, "etilqs_") != 0;
        }
        // OVERLAY: delete. A name a read-only lower still provides must be MASKED with a .wh.NAME whiteout
        // (overlay_whiteout also drops any upper copy) so it stays hidden. An UPPER-ONLY file has no lower to
        // mask, so it is simply removed with NO whiteout -- a spurious .wh.NAME would otherwise linger in the
        // parent and hide a later re-create of that same name (apt's http method deletes partial/X after a
        // failed fetch, then re-creates and renames it -> the stale whiteout ENOENTed the rename source).
        if (g_rootfs && g_nlower) {
            char gp[4200];
            abs_guest((int)a0, (const char *)a1, gp, sizeof gp);
            char host[4300];
            if (!overlay_resolve(gp, host, sizeof host, 1)) {
                G_RET(c) = (uint64_t)(-2);
                break;
                // ENOENT
            }
            // Enforce rmdir/unlink type semantics against the MERGED target BEFORE touching it. The
            // non-overlay branches pass AT_REMOVEDIR straight to unlinkat() so the kernel does this, but
            // the overlay path used remove()/overlay_whiteout() which pick unlink-vs-rmdir by the target's
            // OWN type -- so rmdir() wrongly succeeded on a regular file (and unlink() on a directory). dpkg
            // probes a control file's type with `rmdir(f) == 0`: the wrongly-successful rmdir deleted the
            // file and made dpkg abort "package control info contained directory". Match Linux:
            // rmdir a non-directory -> ENOTDIR; unlink a directory -> EISDIR.
            struct stat lst;
            int isdir = lstat(host, &lst) == 0 && S_ISDIR(lst.st_mode);
            if ((a2 & 0x200) && !isdir) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOTDIR);
                break;
            }
            if (!(a2 & 0x200) && isdir) {
                G_RET(c) = (uint64_t)(int64_t)(-EISDIR);
                break;
            }
            // rmdir must fail ENOTEMPTY on a non-empty MERGED dir. The upper-only branch below lets the
            // kernel enforce this, but a lower-backed dir is whiteout-masked unconditionally -- so it would
            // wrongly "succeed" and hide live lower children. Check the merged listing first (overlay_readdir
            // always includes "." and ".." -> a count > 2 means the directory still has real children).
            if ((a2 & 0x200) && isdir) {
                char (*nm)[256] = NULL;
                uint8_t *ty = NULL;
                int nent = overlay_readdir(gp, &nm, &ty);
                free(nm);
                free(ty);
                if (nent > 2) {
                    G_RET(c) = (uint64_t)(int64_t)(-ENOTEMPTY);
                    break;
                }
            }
            if (overlay_lower_has(gp)) {
                overlay_whiteout(gp);
                G_RET(c) = 0;
            } else {
                // upper-only -> remove with the CORRECT op (rmdir for a dir, unlink for a file) so the
                // kernel still enforces ENOTDIR/EISDIR/ENOTEMPTY exactly as Linux would.
                int r = (a2 & 0x200) ? rmdir(host) : unlink(host);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            }
            // Invalidate the stat/access/readlink caches for the removed path: `host` is the merged-resolve
            // host path, the SAME key case 79/48 memoize under, so a follow-up `test -e`/stat sees it gone
            // (mirrors the non-overlay branch below). Without this a removed upper entry kept reporting as
            // present via a stale mc_ hit even though it no longer appears in a readdir.
            mc_evict(host);
            ac_evict(host);
            rl_evict(host);
            // hardlink coherence: removing one link drops the sibling links' nlink -- evict their cached
            // stats by inode (lst was captured before the removal, so nlink>=2 means aliases still exist).
            if (S_ISREG(lst.st_mode) && lst.st_nlink >= 2) mc_evict_ino(lst.st_dev, lst.st_ino);
            break;
        }
        if (g_rootfs) {
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            // Capture the pre-unlink identity: (dev,ino) drives the delete-on-close adopt AND the hardlink
            // nlink-coherence eviction below; st_nlink>=2 means other links alias this inode.
            uint64_t adev = 0, aino = 0, nlink = 0;
            struct stat ps;
            if (fstatat(pfd, fin, &ps, AT_SYMLINK_NOFOLLOW) == 0) {
                nlink = (uint64_t)ps.st_nlink;
                if (try_adopt && S_ISREG(ps.st_mode)) {
                    adev = (uint64_t)ps.st_dev;
                    aino = (uint64_t)ps.st_ino;
                }
            }
            // AT_REMOVEDIR: linux 0x200
            int r = unlinkat(pfd, fin, (a2 & 0x200) ? AT_REMOVEDIR : 0), e = errno;
            char dp[4200];
            if (r >= 0 && fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                mc_evict(hp);
                ac_evict(hp);
                rl_evict(hp);
            }
            close(pfd);
            if (r >= 0 && aino) memf_try_adopt(adev, aino);
            if (r >= 0 && nlink >= 2) mc_evict_ino((dev_t)ps.st_dev, (ino_t)ps.st_ino);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        // unlink: never follow the final symlink (remove the link itself, not its target).
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 1);
        uint64_t adev = 0, aino = 0, nlink = 0;
        struct stat ps;
        if (fstatat(ATFD(a0), p, &ps, AT_SYMLINK_NOFOLLOW) == 0) {
            nlink = (uint64_t)ps.st_nlink;
            if (try_adopt && S_ISREG(ps.st_mode)) {
                adev = (uint64_t)ps.st_dev;
                aino = (uint64_t)ps.st_ino;
            }
        }
        int r = unlinkat(ATFD(a0), p, (a2 & 0x200) ? AT_REMOVEDIR : 0);
        mc_evict(p);
        ac_evict(p);
        rl_evict(p);
        if (r >= 0 && aino) memf_try_adopt(adev, aino);
        if (r >= 0 && nlink >= 2) mc_evict_ino((dev_t)ps.st_dev, (ino_t)ps.st_ino);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // symlinkat(target, newdirfd, linkpath) -- the link is CREATED at (newdirfd, linkpath)
    case 36: {
        // a relative linkpath under a bad/non-dir newdirfd -> EBADF/ENOTDIR (dd's g_fdpath fold used to
        // leak macOS EOPNOTSUPP for a non-dir dirfd). (LTP symlinkat01.)
        {
            int adc = at_dirfd_check((int)a1, (const char *)a2);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        if (jail_ro_at((int)a1, (const char *)a2)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        const char *target =
            // target is the link CONTENT (unresolved); follow-time confinement guards it
            (const char *)a0;
        if (g_rootfs) {
            if (g_nlower) {
                char gpm[4200];
                abs_guest((int)a1, (const char *)a2, gpm, sizeof gpm);
                // Merged-view errno the upper-only host symlinkat can't produce (lower name -> EEXIST; a
                // lower-only non-dir ancestor -> ENOTDIR; missing ancestor -> ENOENT). Before whiteout clear.
                int pc = overlay_create_precheck(gpm);
                if (pc) {
                    G_RET(c) = (uint64_t)(int64_t)pc;
                    break;
                }
                overlay_clear_whiteout(gpm); // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker
            }
            char fin[512];
            int pfd = jail_at((int)a1, (const char *)a2, fin, sizeof fin, 1);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = symlinkat(target, pfd, fin), e = errno;
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a1, (const char *)a2, pb, sizeof pb, 0);
        G_RET(c) = symlinkat(target, ATFD(a1), p) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // linkat(odir,opath,ndir,npath,flags) -- writes both ends (new link + source link count)
    case 37: {
        // reject unknown linkat flag bits with EINVAL (valid: AT_SYMLINK_FOLLOW 0x400 | AT_EMPTY_PATH
        // 0x1000). dd otherwise ignored the flags and the link wrongly succeeded. (LTP linkat01 case 22.)
        if (a4 & ~(uint64_t)0x1400) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // a relative old/new path under a bad/non-dir dirfd -> EBADF/ENOTDIR, before any resolution
        // (dd's g_fdpath fold leaked macOS EOPNOTSUPP for a non-dir dirfd). (LTP linkat01 cases 8/9.)
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (!adc) adc = at_dirfd_check((int)a2, (const char *)a3);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        // A hardlink whose SOURCE lives on a dd-synthetic pseudo-filesystem (/proc, /sys, /dev) crosses a
        // device boundary -> EXDEV, exactly as on Linux where those are separate mounts. (LTP linkat01 case 20.)
        {
            char sgp[4200];
            abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
            if (!strncmp(sgp, "/proc/", 6) || !strncmp(sgp, "/sys/", 5) || !strncmp(sgp, "/dev/", 5)) {
                char dgp[4200];
                abs_guest((int)a2, (const char *)a3, dgp, sizeof dgp);
                // only when the destination is NOT on the same pseudo-fs (a shm/sem /dev link is handled below)
                if (strncmp(dgp, "/dev/shm/", 9)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EXDEV);
                    break;
                }
            }
        }
        // glibc's sem_open/shm_open creation links a temp /dev/shm/sem.<rnd> to the final /dev/shm/<name>;
        // both ends are shm-backed host files under /tmp, so link them directly (the jail branch below would
        // resolve them into the empty <rootfs>/dev/shm and ENOENT).
        char lob[4224], lnb[4224];
        const char *loh = shm_hostpath((const char *)a1, lob, sizeof lob);
        const char *lnh = shm_hostpath((const char *)a3, lnb, sizeof lnb);
        if (loh && lnh) {
            G_RET(c) = link(loh, lnh) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1) || jail_ro_at((int)a2, (const char *)a3)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        int fl = (a4 & 0x400) ? AT_SYMLINK_FOLLOW : 0;
        if (g_rootfs) {
            // Copy a lower-only SOURCE up first. jail_at(create=1) resolves the source to its UPPER parent
            // dir, but a file that still lives only in the read-only lower is absent there, so linkat would
            // ENOENT (dpkg backs up e.g. /usr/bin/perl via link() on every package upgrade). Mirrors what
            // rename(2) already does for its source. No-op outside overlay mode / when already up.
            overlay_copyup_at((int)a0, (const char *)a1);
            // both ends confined via TOCTOU-free resolver
            char ofin[512], nfin[512];
            int opfd = jail_at((int)a0, (const char *)a1, ofin, sizeof ofin, 1);
            if (opfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)opfd;
                break;
            }
            int npfd = jail_at((int)a2, (const char *)a3, nfin, sizeof nfin, 1);
            if (npfd < 0) {
                close(opfd);
                G_RET(c) = (uint64_t)(int64_t)npfd;
                break;
            }
            int r = linkat(opfd, ofin, npfd, nfin, fl), e = errno;
            // the new link bumped the shared inode's nlink -> the source path's cached stat is now stale.
            if (r == 0) {
                struct stat ls;
                if (fstatat(npfd, nfin, &ls, AT_SYMLINK_NOFOLLOW) == 0) mc_evict_ino(ls.st_dev, ls.st_ino);
            }
            close(opfd);
            close(npfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char ob[4200], nb[4200];
        const char *op = atpath((int)a0, (const char *)a1, ob, sizeof ob, 0);
        const char *np = atpath((int)a2, (const char *)a3, nb, sizeof nb, 0);
        int r = linkat(ATFD(a0), op, ATFD(a2), np, fl);
        if (r == 0) {
            struct stat ls;
            if (fstatat(ATFD(a2), np, &ls, AT_SYMLINK_NOFOLLOW) == 0) mc_evict_ino(ls.st_dev, ls.st_ino);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 38:
    // renameat(38) / renameat2(276): translate the renameat2 flags onto macOS renameatx_np --
    // RENAME_NOREPLACE(1)->RENAME_EXCL (fail if dst exists), RENAME_EXCHANGE(2)->RENAME_SWAP (atomic swap).
    case 276: {
        // renameat2 flag validation (LTP renameat201). Valid flags are RENAME_NOREPLACE(1) |
        // RENAME_EXCHANGE(2) | RENAME_WHITEOUT(4); any unknown bit -> EINVAL, and RENAME_EXCHANGE is exclusive
        // of NOREPLACE and WHITEOUT. Checked before touching the fs (Linux orders this ahead of the path walk).
        if (nr == 276) {
            int lf = (int)a4;
            if ((lf & ~0x7) || ((lf & 2) && (lf & (1 | 4)))) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
        }
        // A relative old/new path under a bad/non-dir dirfd -> EBADF/ENOTDIR.
        {
            int adc = at_dirfd_check((int)a0, (const char *)a1);
            if (!adc) adc = at_dirfd_check((int)a2, (const char *)a3);
            if (adc) {
                G_RET(c) = (uint64_t)(int64_t)adc;
                break;
            }
        }
        if (jail_ro_at((int)a0, (const char *)a1) || jail_ro_at((int)a2, (const char *)a3)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        // inotify: a rename generates IN_MOVED_FROM(src)/IN_MOVED_TO(dst) with a shared cookie on any watch
        // covering the source / destination directory. Queue them now (before the move) so a watch's read()
        // can pair them -- the snapshot diff cannot. No-op when nothing watches either directory.
        inotify_notify_move((int)a0, (const char *)a1, (int)a2, (const char *)a3);
        unsigned int rxflags = 0;
        if (nr == 276) {
            int lf = (int)a4;
            if (lf & 1) rxflags |= RENAME_EXCL;
            if (lf & 2) rxflags |= RENAME_SWAP;
        }
        // shm/sem create that renames (rather than links) a temp /dev/shm file to the final name: both ends
        // are shm-backed host files under /tmp, so rename them directly (the jail branch would ENOENT them).
        char rob[4224], rnb[4224];
        const char *roh = shm_hostpath((const char *)a1, rob, sizeof rob);
        const char *rnh = shm_hostpath((const char *)a3, rnb, sizeof rnb);
        if (roh && rnh) {
            G_RET(c) = renameatx_np(AT_FDCWD, roh, AT_FDCWD, rnh, rxflags) < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        if (g_rootfs) {
            // both ends confined (TOCTOU-free). Copy a lower-only SOURCE up first so renameatx_np finds it in
            // the writable upper (jail_at already materializes the dest's upper parent via overlay_mkparents).
            // RECURSIVE for a lower-only directory: the whole subtree must be in the upper before the move,
            // else the rename moves an EMPTY dir and loses the contents. For an EXCHANGE, the DEST must also
            // be copied up (both ends land in the upper before the atomic swap).
            overlay_copyup_at_tree((int)a0, (const char *)a1);
            if (rxflags & RENAME_SWAP) overlay_copyup_at_tree((int)a2, (const char *)a3);
            char ofin[512], nfin[512];
            int opfd = jail_at((int)a0, (const char *)a1, ofin, sizeof ofin, 1);
            if (opfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)opfd;
                break;
            }
            int npfd = jail_at((int)a2, (const char *)a3, nfin, sizeof nfin, 1);
            if (npfd < 0) {
                close(opfd);
                G_RET(c) = (uint64_t)(int64_t)npfd;
                break;
            }
            char dp[4200];
            if (fcntl(opfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, ofin);
                mc_evict(hp);
                ac_evict(hp);
            }
            int r = renameatx_np(opfd, ofin, npfd, nfin, rxflags), e = errno;
            close(opfd);
            close(npfd);
            // Overlay: a plain move (not RENAME_EXCHANGE) of a file the image lower still provides leaves the
            // copied-up upper source moved away but the lower copy exposed -> the source would re-appear. Drop
            // a whiteout at the source so it stays gone (real overlayfs rename semantics). No-op outside overlay.
            if (r == 0 && !(rxflags & RENAME_SWAP)) {
                char sgp[4200];
                abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
                if (overlay_lower_has(sgp)) overlay_whiteout(sgp);
                // RENAME_WHITEOUT: Linux additionally leaves a whiteout char device (0,0) at the source.
                // Record it so lstat(src) reports that char device (synth_stat_raw); overlay_whiteout above
                // already dropped the union `.wh.` marker when a lower entry needed masking.
                if (nr == 276 && ((int)a4 & 4)) whiteout_note(sgp);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char ob[4200], nb[4200];
        const char *op = atpath((int)a0, (const char *)a1, ob, sizeof ob, 0);
        const char *np = atpath((int)a2, (const char *)a3, nb, sizeof nb, 0);
        int rr = renameatx_np(ATFD(a0), op, ATFD(a2), np, rxflags);
        // RENAME_WHITEOUT (non-overlay): record the source as a whiteout char device (0,0) so lstat(src)
        // reports it, matching Linux -- macOS cannot mknod a real device node rootless.
        if (rr == 0 && nr == 276 && ((int)a4 & 4)) {
            char sgp[4200];
            abs_guest((int)a0, (const char *)a1, sgp, sizeof sgp);
            whiteout_note(sgp);
        }
        G_RET(c) = rr < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // mount(source,target,fstype,flags,data): implement bind/tmpfs/remount,ro against dd's vfs (svc_mount);
    // a real no-op stub silently gave wrong content + unenforced RO.
    case 40: G_RET(c) = (uint64_t)svc_mount(c, a0, a1, a2, a3); break;
    // umount2(target,flags): detach a runtime bind/tmpfs volume mounted exactly there. A pseudo-mount dd
    // keeps serving (not a registered volume) stays present -> success (unmounting it is a harmless no-op
    // in dd's model; the content is synthetic, not backed by the removed mount).
    case 39: {
        if (!g_rootfs) {
            G_RET(c) = 0;
            break;
        }
        if (!a0 || guest_bad_ptr(a0, 1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        char utgt[4200];
        guest_abspath_at(-100, (const char *)a0, utgt, sizeof utgt);
        rt_del_vol(utgt); // -EINVAL if not a registered volume; treated as a no-op success below
        G_RET(c) = 0;
        break;
    }
    // pivot_root(new_root,put_old): re-root the guest at new_root, confined within the rootfs jail (modeled
    // as a chroot -- dd has one root fd; put_old is not separately materialized). Validate new_root exists
    // as a directory so a bad target reports ENOENT/ENOTDIR instead of a fake success.
    case 41: {
        if (!g_rootfs) {
            G_RET(c) = 0;
            break;
        }
        if (!a0 || guest_bad_ptr(a0, 1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
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
        rc_reset();
        G_RET(c) = 0;
        break;
    }
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
            // the buffer is examined (LTP statfs02 "bad path"). guest_bad_ptr catches a PROT_NONE page.
            if (!a0 || guest_bad_ptr(a0, 1)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            char pb[4200];
            const char *p = atpath(-100, (const char *)a0, pb, sizeof pb, 0);
            guest_abspath_at(-100, (const char *)a0, gpath, sizeof gpath); // guest-absolute path (both modes)
            r = statfs(p, &hs);
            // A SYNTHETIC proc/sys/cgroup leaf (its content is served by the /proc·/sys synth, not the image)
            // has no host file to statfs -> ENOENT. But Linux reports the pseudo-fs magic + geometry for these
            // paths, and tools (UseContainerSupport, magic-based pseudo-fs detection) rely on it. If dd
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
            if (g_rootfs && (int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0])
                guest_from_host(g_fdpath[(int)a0], gpath, sizeof gpath);
        }
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        uint8_t *b = (uint8_t *)a1;
        // The result buffer must be writable -> EFAULT on a bad/unmapped/PROT_NONE pointer (LTP statfs02
        // "bad buf"; the engine fills this buffer itself, so guard before the writes below).
        if (guest_bad_ptr((uintptr_t)a1, 120)) {
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
        memset(b, 0, 120);
        *(int64_t *)(b + 0) = f_type;               // f_type (Linux fs magic for the resolved mount)
        *(int64_t *)(b + 8) = (int64_t)hs.f_bsize;  // f_bsize
        *(uint64_t *)(b + 16) = blocks;             // f_blocks
        *(uint64_t *)(b + 24) = bfree;              // f_bfree
        *(uint64_t *)(b + 32) = bavail;             // f_bavail
        *(uint64_t *)(b + 40) = files;              // f_files
        *(uint64_t *)(b + 48) = ffree;              // f_ffree
        *(int32_t *)(b + 56) = hs.f_fsid.val[0];    // f_fsid[0]
        *(int32_t *)(b + 60) = hs.f_fsid.val[1];    // f_fsid[1]
        *(int64_t *)(b + 64) = 255;                 // f_namelen (NAME_MAX)
        *(int64_t *)(b + 72) = (int64_t)hs.f_bsize; // f_frsize
        // f_flags: Linux exposes the mount flags (ST_VALID + mount options). dd's mounts are all relatime;
        // the pseudo-fs + tmpfs mounts (/proc /sys /dev /dev/shm) are nosuid,nodev,noexec (per mountinfo).
        // Reporting 0 made ST_NOSUID/NODEV/NOEXEC/RDONLY probes see a false mount view.
        int64_t f_flags = 0;
        if (classify) {
            f_flags = 0x0020 | 0x1000; // ST_VALID | ST_RELATIME
            if (!strncmp(gpath, "/proc", 5) || !strncmp(gpath, "/sys", 4) || !strncmp(gpath, "/dev", 4))
                f_flags |= 0x0002 | 0x0004 | 0x0008; // ST_NOSUID | ST_NODEV | ST_NOEXEC
        }
        *(int64_t *)(b + 80) = f_flags; // f_flags
        G_RET(c) = 0;
        break;
    }
    case 46: {
        // memfd sealing: F_SEAL_SHRINK(0x2) blocks a size-reducing ftruncate, F_SEAL_GROW(0x4) blocks a
        // size-increasing one -> EPERM (matching the write/pwrite F_SEAL_WRITE guards). A sealed shared
        // buffer must not be resized under a receiver (SIGBUS/OOB). Compare against the CURRENT size.
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && (memfd_seals_fd((int)a0) & 0x6)) {
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
        int r = ftruncate((int)a0, (off_t)a1);
        fd_evict((int)a0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
        // ftruncate
    }
    case 47: {
        // fallocate(fd,mode,offset,len). Implements the range MODES Linux defines -- the historical stub
        // ignored ZERO_RANGE/COLLAPSE_RANGE/INSERT_RANGE (fell through to a plain no-op extend) and
        // swallowed the ftruncate ENOSPC, i.e. it reported reservation/modification SUCCESS while the data
        // was untouched (silent corruption + a faked space reservation). macOS has no fallocate(2), so:
        // PUNCH_HOLE via F_PUNCHHOLE; ZERO_RANGE via a zero-fill; COLLAPSE/INSERT via a correct read-shift-
        // write (macOS truly can't collapse/insert a plain file, so do the shift by hand rather than no-op).
        // FALLOC_FL: KEEP_SIZE=0x01 PUNCH_HOLE=0x02 COLLAPSE_RANGE=0x08 ZERO_RANGE=0x10 INSERT_RANGE=0x20
        //            UNSHARE_RANGE=0x40. memfd seals: SHRINK(0x2) blocks a size-reducing op, GROW(0x4) a
        //            size-increasing one, WRITE(0x8) any data modification -> EPERM.
        int fd = (int)a0, mode = (int)a1;
        off_t off = (off_t)a2, len = (off_t)a3;
        if (off < 0 || len <= 0 || (mode & ~0x7f)) { // negative/zero range or unknown mode bit -> EINVAL
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Linux mode-combination validity (vfs_fallocate/do_fallocate): the range ops must be used
        // exclusively, otherwise the historical "dispatch on the first matching bit" logic silently ran one
        // op and dropped the rest (e.g. COLLAPSE|KEEP_SIZE shrank the file, ZERO|COLLAPSE overwrote bytes).
        if (((mode & 0x02) && !(mode & 0x01)) ||           // PUNCH_HOLE requires KEEP_SIZE
            ((mode & 0x08) && (mode & ~0x08)) ||           // COLLAPSE_RANGE must be used alone
            ((mode & 0x20) && (mode & ~0x20)) ||           // INSERT_RANGE must be used alone
            ((mode & 0x40) && (mode & ~(0x40 | 0x01)))) {  // UNSHARE_RANGE only with KEEP_SIZE
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // off+len overflow (or wrap through zero) -> EFBIG, matching vfs_fallocate's wrap check. off>=0 and
        // len>0 here, so the sum overflows iff off > INT64_MAX - len.
        if (off > (off_t)0x7fffffffffffffffLL - len) {
            G_RET(c) = (uint64_t)(-EFBIG);
            break;
        }
        int seal = (fd >= 0 && fd < DD_NFD) ? memfd_seals_fd(fd) : 0;
        memf_materialize(fd); // flush any RAM cache; every branch below works on the real host fd
        struct stat s;
        if (fstat(fd, &s) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        off_t cur = s.st_size;
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
            fd_evict(fd);
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
            fd_evict(fd);
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
            fd_evict(fd);
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
            fd_evict(fd);
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
            fd_evict(fd);
            G_RET(c) = 0;
        }
        break;
    fallocate_done:
        fd_evict(fd);
        break;
    }
    case 49: {
        char pb[4200];
        // chdir (confined; tracks guest cwd)
        const char *p = atpath(-100, (const char *)a0, pb, sizeof pb, 0);
        if (chdir(p) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // Track the guest cwd from the host path the dir resolved to (handles the upper, any lower, or a
        // volume) -- relative/"."/AT_FDCWD resolution joins g_cwd, so a stale value sends `ls` to the wrong dir.
        if (g_rootfs) guest_from_host(p, g_cwd, sizeof g_cwd);
        G_RET(c) = 0;
        break;
    }
    case 50: {
        if (fchdir((int)a0) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
            // fchdir (tracks guest cwd)
        }
        if (g_rootfs && (int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0])
            guest_from_host(g_fdpath[(int)a0], g_cwd, sizeof g_cwd);
        G_RET(c) = 0;
        break;
    }
    // fchmod(fd, mode) -- like fchmodat, the new mode must invalidate this file's cached stat, or a
    // subsequent stat() of the same path serves the stale pre-chmod mode from the mc cache (the fd's
    // canonical host path in g_fdpath is the SAME key case 79 memoizes under).
    case 52: {
        int r = fchmod((int)a0, (mode_t)a1);
        if (r == 0 && (int)a0 >= 0 && (int)a0 < DD_NFD && g_fdpath[(int)a0][0]) fc_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 53:
    // fchmodat(dirfd,path,mode,flags) / fchmodat2
    case 452: {
        // A pathname pointer outside the accessible address space -> EFAULT (kernel getname
        // copy_from_user), before the dirfd/target is examined (LTP fchmodat02 "invalid address").
        // guest_bad_ptr catches the PROT_NONE tst_get_bad_addr page; the reads below (jail/atpath) would
        // otherwise consume garbage from dd's force-mapped shadow of that page and mis-report the error.
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
        if (g_rootfs) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = fchmodat(pfd, fin, (mode_t)a2, 0), e = errno;
            char dp[4200];
            if (r >= 0 && fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                mc_evict(hp);
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = fchmodat(ATFD(a0), p, (mode_t)a2, 0);
        if (r >= 0) mc_evict(p);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // fchownat(dirfd,path,uid,gid,flags) -- best-effort (rootless)
    case 54: {
        // Linux validates the flag word before touching the path: only AT_SYMLINK_NOFOLLOW (0x100) and
        // AT_EMPTY_PATH (0x1000) are defined; any other bit is EINVAL. dd emulates a root container, so an
        // actual ownership change is faked-success (a real host chown is a rootless no-op), but a syntactically
        // invalid call must still fail exactly as Linux does rather than silently mutate the owner xattr.
        if (a4 & ~0x1100u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (g_rootfs) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, (a4 & 0x100) ? 1 : 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int nofollow = (a4 & 0x100) ? 1 : 0;
            fchownat(pfd, fin, (uid_t)a2, (gid_t)a3, nofollow ? AT_SYMLINK_NOFOLLOW : 0);
            // the host chown is a rootless no-op; persist the guest-set owner as an xattr on
            // the backing file so a later stat reports it (not the cuid/cgid default). -1 = keep.
            char dp[4200];
            if (fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                chown_xattr_set_path(hp, (int)(int32_t)(uint32_t)a2, (int)(int32_t)(uint32_t)a3, nofollow);
            }
            close(pfd);
            G_RET(c) = 0;
            break;
            // EPERM on the host -> faked OK
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        fchownat(ATFD(a0), p, (uid_t)a2, (gid_t)a3, 0);
        chown_xattr_set_path(p, (int)(int32_t)(uint32_t)a2, (int)(int32_t)(uint32_t)a3, 0);
        G_RET(c) = 0;
        break;
    }
    case 55: {
        // A genuinely invalid fd must fail EBADF like Linux -- don't fake-success and poison the owner
        // xattr on a descriptor that isn't open. (Host EPERM from the rootless chown is still faked OK,
        // matching the emulated root container.)
        if (fchown((int)a0, (uid_t)a1, (gid_t)a2) < 0 && errno == EBADF) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        chown_xattr_set_fd((int)a0, (int)(int32_t)(uint32_t)a1, (int)(int32_t)(uint32_t)a2);
        // the guest-owner xattr just changed -> drop this path's cached stat so a later stat reports it
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0]) fc_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = 0;
        break;
        // fchown(fd,uid,gid) -- best-effort
    }
    // openat2(dirfd, path, open_how*, size): unpack open_how { u64 flags; u64 mode; u64 resolve; } into
    // the openat arg positions, then share the full openat path (O_* xlate, overlay, jail). Linux validates
    // the ABI up front, so we do too: NULL how -> EFAULT, size < v0 (24) -> EINVAL, size > PAGE_SIZE or
    // non-zero extension bytes -> E2BIG, unknown resolve bits / mode>07777 / mode set without a create flag
    // -> EINVAL. RESOLVE_NO_SYMLINKS is enforced as O_NOFOLLOW (ELOOP on a symlink final component); the
    // rootfs jail already confines every resolution so the containment RESOLVE_* bits stay advisory.
    case 437: {
        uint64_t how_ptr = a2, usize = a3;
        if (!how_ptr || guest_bad_ptr(how_ptr, 8)) {
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
        if (guest_bad_ptr(how_ptr, usize)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const uint8_t *hb = (const uint8_t *)how_ptr;
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
        const uint64_t *how = (const uint64_t *)how_ptr;
        uint64_t oflags = how[0], omode = how[1], resolve = how[2];
        // RESOLVE_* valid mask: NO_XDEV|NO_MAGICLINKS|NO_SYMLINKS|BENEATH|IN_ROOT|CACHED = 0x3f
        if ((resolve & ~0x3fULL) || (omode & ~07777ULL) ||
            (omode && !(oflags & (0x40ULL /*O_CREAT*/ | 0x400000ULL /*__O_TMPFILE*/)))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (resolve & 0x04ULL /*RESOLVE_NO_SYMLINKS*/)
            oflags |= (uint64_t)G_O_NOFOLLOW;
        a2 = oflags; // open_how.flags -> openat flags
        a3 = omode;  // open_how.mode  -> openat mode
    } /* fall through to openat */
    case 56: {
        // openat -- Linux O_* -> macOS O_* (they differ!)
        int lf = (int)a2, mf = lf & 0x3;
        // O_PATH (Linux 0x200000, arch-independent): the fd only NAMES the file -- fstat / *at dirfd /
        // fchdir work through it, but read/write are rejected EBADF. macOS has no O_PATH, so we open a
        // normal read fd (O_RDONLY, +O_DIRECTORY for a dir) for the metadata ops and record the flag so the
        // I/O family (svc_io) returns EBADF. Marked on every open-success path below.
        int is_opath = (lf & 0x200000) != 0;
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
        if (((lf & 3) || (lf & 0x40) || (lf & 0x200) || (lf & 0x400)) && jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        // O_TMPFILE (the __O_TMPFILE bit 0x400000 is arch-independent): create an unnamed, auto-cleaned
        // regular file inside the named directory by making one + immediately unlinking it (macOS has no
        // O_TMPFILE). The fd is a normal RW file with link count 0.
        if (lf & 0x400000) {
            char pb[4200];
            const char *dir = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
            int dfd = open(dir, O_RDONLY | O_DIRECTORY);
            if (dfd < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int fd = -1, e = ENOENT;
            for (int t = 0; t < 64; t++) {
                char nm[40];
                snprintf(nm, sizeof nm, ".dd_tmpfile_%d_%d", (int)getpid(), rand());
                fd = openat(dfd, nm, O_CREAT | O_EXCL | O_RDWR, (mode_t)(a3 ? a3 : 0600));
                e = errno;
                if (fd >= 0) {
                    unlinkat(dfd, nm, 0);
                    break;
                }
                if (e != EEXIST) break;
            }
            close(dfd);
            if (fd >= 0 && fd < DD_NFD) {
                g_fdpath[fd][0] = 0;   // anonymous: no tracked path
                memf_attach(fd, 0, 0); // O_TMPFILE is unambiguously private scratch -> back it with RAM
            }
            G_RET(c) = fd < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)fd;
            break;
        }
        {
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
                    break;
                }
                int md = proc_masked_open(rp);
                if (md != -2) {
                    if (md >= 0 && (lf & 0x80000)) fcntl(md, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
                    G_RET(c) = md < 0 ? (uint64_t)(-errno) : (uint64_t)md;
                    break;
                }
            }
            // Synthetic non-pid directories whose direct leaves already exist but whose DIRECTORY was not
            // enumerable: /proc/net, /proc/[self|pid]/ns, /sys/fs/cgroup, /sys/class/block, /sys/block,
            // cpuN/topology, and /dev/fd (== /proc/self/fd). A directory walk of these now sees their entries.
            if (rp) {
                int md = synth_misc_dir_open(rp);
                if (md != -2) {
                    if (md >= 0 && (lf & 0x80000)) fcntl(md, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
                    if (md >= 0 && md < DD_NFD) g_opath[md] = is_opath;             // O_PATH fd -> I/O EBADF
                    G_RET(c) = md < 0 ? (uint64_t)(-errno) : (uint64_t)md;
                    break;
                }
            }
            // opendir("/proc"): materialize the process table (numeric pid dir per live container process
            // + the synthesized static files) so getdents enumerates the whole container -- `ps`/top/htop
            // read this to find processes. Without it the empty rootfs /proc dir yielded an empty table.
            if (rp && g_rootfs && (!strcmp(rp, "/proc") || !strcmp(rp, "/proc/"))) {
                int d = proc_root_dir_open();
                if (d >= 0) {
                    G_RET(c) = (uint64_t)d;
                    break;
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
                    break;
                }
                // /proc/[self|pid]/exe -> open the actual guest executable (the magic symlink target)
                char ep[1024];
                if (proc_self_exe(rp, ep, sizeof ep)) {
                    char hb[4200];
                    const char *hp = xresolve_overlay(ep, hb, sizeof hb);
                    int ef = open(hp, O_RDONLY);
                    if (ef >= 0 && (lf & 0x80000)) fcntl(ef, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
                    G_RET(c) = ef < 0 ? (uint64_t)(-errno) : (uint64_t)ef;
                    break;
                }
                // /proc/[self|pid]/auxv (rustix/libc read it)
                if (strstr(rp, "/auxv")) {
                    char tn[] = "/tmp/.ddauxvXXXXXX";
                    int afd = mkstemp(tn);
                    if (afd >= 0) {
                        unlink(tn);
                        if (write(afd, g_auxv_data, g_auxv_len) < 0) {}
                        lseek(afd, 0, SEEK_SET);
                    }
                    G_RET(c) = afd < 0 ? (uint64_t)(-errno) : (uint64_t)afd;
                    break;
                }
                // cpuinfo/meminfo/stat/mounts/uptime/loadavg/version
                int pf = proc_open(rp);
                if (pf != -2) {
                    G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
                    break;
                }
            }
            // cgroup v2 limit files (JVM/Go self-size on these)
            if (rp && !strncmp(rp, "/sys/fs/cgroup/", 15)) {
                int pf = proc_open(rp);
                if (pf != -2) {
                    G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
                    break;
                }
            }
            // /sys/class/net: interface introspection. Directory opens (the class dir + per-iface
            // dirs) materialize a temp dir for getdents; attribute files are served by proc_open.
            if (rp && !strncmp(rp, "/sys/class/net", 14)) {
                int d = sysnet_dir_open(rp);
                if (d != -2) {
                    if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
                    G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                    break;
                }
                int pf = proc_open(rp);
                if (pf != -2) {
                    G_RET(c) = pf < 0 ? (uint64_t)(-errno) : (uint64_t)pf;
                    break;
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
                    break;
                }
                // cpuN/topology/<core_id|physical_package_id|thread_siblings[_list]|...>: lscpu/util-linux
                // reconstruct sockets/cores/threads from these; an ENOENT makes lscpu mis-count (dd-only).
                char tb[96];
                int tn = syscpu_topology_content(rp, tb, sizeof tb);
                if (tn >= 0) {
                    int d = synth_str_fd(tb);
                    G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                    break;
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
                    if (d >= 0 && d < DD_NFD) g_opath[d] = is_opath;               // O_PATH fd -> I/O EBADF
                    G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                    break;
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
                    break;
                }
            }
            // GPU rung 2 (opt-in): the DRM render-node discovery + allocation path. Inert unless
            // DD_GPU_IOSURFACE is set (existing workloads never touch /dev/dri or the DRM sysfs).
            if (rp && gpu_iosurface_on()) {
                int drm_minor = -1;
                // synthesize /dev/dri/{renderD128,card0} as a placeholder fd tagged as a render node; its
                // DD_IOCTL_GPU_ALLOC / DRM ioctls (fs.c case 29) service the IOSurface + version/cap probes.
                if (!strncmp(rp, "/dev/dri/", 9) && drm_dev_minor(rp + 9, &drm_minor)) {
                    int d = open("/dev/null", O_RDWR); // backing is irrelevant; the ioctl carries the payload
                    if (d >= 0 && d < DD_NFD) g_devdri[d] = (uint8_t)(drm_minor + 1); // minor+1 (fstat fixup)
                    if (d >= 0 && (lf & 0x80000)) fcntl(d, F_SETFD, FD_CLOEXEC); // honor O_CLOEXEC
                    G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                    break;
                }
                // the minimal DRM sysfs tree libdrm's drmGetDevices2 walks: opendir the class/drm dirs,
                // and serve uevent/dev attribute files as content fds.
                if (!strncmp(rp, "/sys/dev/char/226:", 18) || !strncmp(rp, "/sys/class/drm", 14)) {
                    int dd = drm_dir_open(rp);
                    if (dd != -2) {
                        if (dd >= 0 && (lf & 0x80000)) fcntl(dd, F_SETFD, FD_CLOEXEC);
                        if (dd >= 0 && dd < 1024) g_opath[dd] = is_opath;
                        G_RET(c) = dd < 0 ? (uint64_t)(-errno) : (uint64_t)dd;
                        break;
                    }
                    char cb[256];
                    int cn = drm_synth_content(rp, cb, sizeof cb);
                    if (cn >= 0) {
                        int d = synth_str_fd(cb);
                        G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                        break;
                    }
                }
            }
            // device nodes -> host devices (rootfs has no real /dev)
            if (rp && !strncmp(rp, "/dev/", 5)) {
                const char *hd = dev_node_hostpath(rp);
                if (hd) {
                    int d = open(hd, mf);
                    // /dev/full is backed by /dev/zero for reads; flag the fd so its writes fail ENOSPC.
                    if (d >= 0 && d < DD_NFD) g_devfull[d] = !strcmp(rp, "/dev/full");
                    // /dev/tty (and /dev/console, backed by /dev/null): tty read semantics -- a nonblocking
                    // empty read is EAGAIN, never EOF (see g_devtty). Flag the fd so svc_io maps 0->EAGAIN.
                    if (d >= 0 && d < DD_NFD)
                        g_devtty[d] = (!strcmp(rp, "/dev/tty") || !strcmp(rp, "/dev/console"));
                    // This dev open uses only the access mode (mf gains O_NONBLOCK below, after this block),
                    // so propagate the guest's O_NONBLOCK onto the tty fd now -- both so its host reads are
                    // genuinely nonblocking and so F_GETFL reflects it for the 0->EAGAIN remap in svc_io.
                    if (d >= 0 && d < DD_NFD && g_devtty[d] && (lf & 0x800)) {
                        int gf = fcntl(d, F_GETFL);
                        if (gf >= 0) fcntl(d, F_SETFL, gf | O_NONBLOCK);
                    }
                    // /dev/urandom + /dev/random accept seed writes on Linux (macOS EPERMs); flag the fd.
                    if (d >= 0 && d < DD_NFD)
                        g_devseed[d] = (!strcmp(rp, "/dev/urandom") || !strcmp(rp, "/dev/random"));
                    G_RET(c) = d < 0 ? (uint64_t)(-errno) : (uint64_t)d;
                    break;
                }
            }
        }
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
            int pfn = procfd_num((const char *)a1);
            if (pfn < 0) pfn = dev_std_fd((const char *)a1);
            if (pfn >= 0) {
                memf_materialize(pfn); // reopen-by-fd would expose the real file -> flush RAM cache first
                char gp[4200];
                int r = -1;
                if (fcntl(pfn, F_GETPATH, gp) == 0 && gp[0]) r = open(gp, mf & ~(O_EXCL | O_CREAT), (mode_t)a3);
                if (r < 0) r = dup(pfn); // anonymous/pipe/socket fd -> share the description
                if (r >= 0) {
                    char tp[4200];
                    if (fcntl(r, F_GETPATH, tp) == 0) fd_setpath(r, tp);
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
                    int s = (fcntl(a, F_GETPATH, hp) == 0) ? open(hp, mf) : dup(a);
                    G_RET(c) = s < 0 ? (uint64_t)(-errno) : (uint64_t)s;
                    break;
                }
            }
            // a guest-created pty slave /dev/pts/N. Intercepted HERE, ahead of the overlay resolver, so
            // the freshly-allocated slave (which has no rootfs backing file) is never an ENOENT miss. N is the
            // devpts index dd assigned the master (pts_alloc); ptsname(master) yields the host slave device.
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
                int s = open(sn, mf);
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
        // OVERLAY: resolve across layers (upper shadows lowers)
        if (g_rootfs && g_nlower) {
            char gp[4200];
            abs_guest((int)a0, (const char *)a1, gp, sizeof gp);
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
            int r = nofile_gate(open(host, mf | ((lf & G_O_NOFOLLOW) ? O_NOFOLLOW : 0), (mode_t)a3));
            if (r >= 0 && nf_new) newfile_stamp_fd(r);
            if (r >= 0 && r < DD_NFD) g_opath[r] = is_opath;
            if (r >= 0) {
                char gpa[4200];
                int have_canon = fcntl(r, F_GETPATH, gpa) == 0;
                if (have_canon) {
                    fd_setpath(r, gpa);
                    if (isw) {
                        mc_evict(gpa);
                        rl_evict(gpa);
                        ac_evict(gpa);
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
                char gdir[4200];
                if (have_canon)
                    guest_from_host(gpa, gdir, sizeof gdir);
                else
                    snprintf(gdir, sizeof gdir, "%s", gp);
                struct stat dst;
                if (r < DD_NFD && !jail_is_vol(gdir) && fstat(r, &dst) == 0 && S_ISDIR(dst.st_mode))
                    snprintf(g_ovldir[r], sizeof g_ovldir[r], "%s", gdir);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
            break;
        }
        // TOCTOU-free per-component resolve in the jail
        if (g_rootfs) {
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
            if (cacheable && oc_lookup(gkey, hostc, sizeof hostc)) {
                // ONE atomic open replaces the per-component walk; hostc is already canonical+symlink-free.
                int r = open(hostc, mf | O_NOFOLLOW, (mode_t)a3);
                int e = errno;
                r = nofile_gate(r); // fd past the guest's soft RLIMIT_NOFILE -> EMFILE
                if (r < 0 && errno == EMFILE) e = EMFILE;
                if (r >= 0 && r < DD_NFD) g_opath[r] = is_opath;
                if (r >= 0) {
                    fd_setpath(r, hostc);
                    if (lf & 3) { // write-open: keep the metadata caches coherent (same as the walk path)
                        mc_evict(hostc);
                        rl_evict(hostc);
                        ac_evict(hostc);
                    }
                }
                G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
                break;
            }
            char fin[512];
            // resolve following the final symlink unless the guest asked O_NOFOLLOW (per-arch bit)
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, (lf & G_O_NOFOLLOW) != 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            // fin is resolved -> O_NOFOLLOW safe
            // probe pre-existence (relative to the resolved parent) so we stamp ONLY a fresh create.
            int nf_new = nf_want && faccessat(pfd, fin, F_OK, AT_SYMLINK_NOFOLLOW) != 0;
            // O_PATH|O_NOFOLLOW on a symlink -> open the LINK via O_SYMLINK (else O_NOFOLLOW ELOOPs); a
            // regular O_NOFOLLOW open keeps ELOOPing on a symlink as Linux does.
            int r = openat(pfd, fin, mf | (osymlink ? O_SYMLINK : O_NOFOLLOW), (mode_t)a3);
            int e = errno;
            close(pfd);
            r = nofile_gate(r); // fd past the guest's soft RLIMIT_NOFILE -> EMFILE (host table is far larger)
            if (r < 0 && errno == EMFILE) e = EMFILE;
            if (r >= 0 && nf_new) newfile_stamp_fd(r);
            if (r >= 0 && r < DD_NFD) g_opath[r] = is_opath;
            if (r >= 0) {
                char gp[4200];
                // canonical host path for tracking
                if (fcntl(r, F_GETPATH, gp) == 0) {
                    fd_setpath(r, gp);
                    if ((lf & 3) || (lf & 0x40) || (lf & 0x200)) {
                        mc_evict(gp);
                        rl_evict(gp);
                        ac_evict(gp);
                    }
                    // W4D: memoize this walk's result (gp = F_GETPATH = canonical in-jail host path) so the
                    // next open of the same guest path is a single open(). oc_store re-checks in-jail+epoch.
                    if (cacheable) oc_store(gkey, gp);
                }
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
            break;
        }
        char pb[4200];
        // no jail
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, osymlink ? 1 : 0);
        int nf_new = nf_want && faccessat(ATFD(a0), p, F_OK, AT_SYMLINK_NOFOLLOW) != 0; // stamp only fresh
        // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the shared host fd
        // table is far larger; engine-private fds are hoisted above 1<<20, so the guest limit is emulated).
        // O_PATH|O_NOFOLLOW on a symlink -> O_SYMLINK opens the link itself.
        int r = nofile_gate(openat(ATFD(a0), p, mf | osymlink, (mode_t)a3));
        if (r >= 0 && nf_new) newfile_stamp_fd(r);
        if (r >= 0 && r < DD_NFD) g_opath[r] = is_opath;
        if (r >= 0) {
            fd_setpath(r, p);
            if ((lf & 3) || (lf & 0x40) || (lf & 0x200)) {
                mc_evict(p);
                rl_evict(p);
                ac_evict(p);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 57: {
        int cf = (int)a0;
        engine_fd_vacate(cf); // guest close must not clobber an engine-private fd (g_root_fd etc.) on this number
        // Drop every dd-side emulation-table entry for this fd (eventfd peer/timerfd/overlay-dir/socket/epoll/
        // flock/pidfd/memf/getdents caches/path) BEFORE the real close, so a reused number can't be misrouted.
        // SEQPACKET/O_DIRECT-pipe EOF is injected here (inside fd_reset_emul's seq_send_eof) while this end is
        // still open, so a blocked peer recv wakes and returns 0. Shared with the execve CLOEXEC sweep.
        fd_reset_emul(cf);
        int r = close(cf);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
        // close: -errno on fail
    }
    // getdents64
    case 61: {
        int fd = (int)a0;
        // OVERLAY: merged listing across layers
        if (g_nlower && fd >= 0 && fd < DD_NFD && g_ovldir[fd][0]) {
            // snapshot cache is indexed directly by guest fd (no slot table -> no eviction thrash)
            if (!g_ovldents[fd].taken) {
                g_ovldents[fd].taken = 1;
                g_ovldents[fd].pos = 0;
                g_ovldents[fd].n = overlay_readdir(g_ovldir[fd], &g_ovldents[fd].nm, &g_ovldents[fd].ty);
            }
            uint8_t *out = (uint8_t *)a1;
            size_t o = 0;
            while (g_ovldents[fd].pos < g_ovldents[fd].n) {
                const char *nm = g_ovldents[fd].nm[g_ovldents[fd].pos];
                size_t nl = strlen(nm), lr = (19 + nl + 1 + 7) & ~7ull;
                if (o + lr > (size_t)a2) break;
                uint8_t *ld = out + o;
                // REAL inode: stat the merged entry (its host backing across upper/lowers), so `ls -i`,
                // `find -inum`, and hardlink detection work on a layered image. The old `pos+1` fabricated a
                // unique per-position number -> every entry looked like a distinct inode (hardlinks/du/rsync
                // dedup broke). Fall back to pos+1 only if the entry can't be stat'd.
                uint64_t d_ino = (uint64_t)g_ovldents[fd].pos + 1;
                if (nl < 200) {
                    char egp[4300], ehp[4300];
                    int gl = snprintf(egp, sizeof egp, "%s/%s", g_ovldir[fd], nm);
                    if (gl > 0 && (size_t)gl < sizeof egp) {
                        const char *eh = xresolve_overlay(egp, ehp, sizeof ehp);
                        struct stat est;
                        if (eh && lstat(eh, &est) == 0) d_ino = (uint64_t)est.st_ino;
                    }
                }
                *(uint64_t *)(ld + 0) = d_ino;
                *(uint64_t *)(ld + 8) = o + lr;
                *(uint16_t *)(ld + 16) = (uint16_t)lr;
                *(ld + 18) = g_ovldents[fd].ty[g_ovldents[fd].pos];
                memcpy(ld + 19, nm, nl);
                ld[19 + nl] = 0;
                o += lr;
                g_ovldents[fd].pos++;
            }
            // exhausted -> free the snapshot (releases the heap arrays too)
            if (o == 0) ovldents_free(fd);
            G_RET(c) = (uint64_t)o;
            break;
        }
        DIR *dir = NULL;
        for (int i = 0; i < g_ndirs; i++)
            if (g_dirs[i].fd == fd) {
                dir = g_dirs[i].d;
                break;
            }
        if (!dir) {
            dir = fdopendir(dup(fd));
            if (!dir) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (g_ndirs < 64) {
                g_dirs[g_ndirs].fd = fd;
                g_dirs[g_ndirs].d = dir;
                g_ndirs++;
            }
        }
        uint8_t *out = (uint8_t *)a1;
        size_t o = 0;
        struct dirent *de;
        long pos = telldir(dir);
        while ((de = readdir(dir))) {
            size_t nl = strlen(de->d_name), lr = (19 + nl + 1 + 7) & ~7ull;
            if (o + lr > (size_t)a2) {
                seekdir(dir, pos);
                break;
            }
            uint8_t *ld = out + o;
            *(uint64_t *)(ld + 0) = de->d_ino;
            *(uint64_t *)(ld + 8) = o + lr;
            *(uint16_t *)(ld + 16) = (uint16_t)lr;
            *(ld + 18) = de->d_type;
            memcpy(ld + 19, de->d_name, nl);
            ld[19 + nl] = 0;
            o += lr;
            pos = telldir(dir);
        }
        G_RET(c) = o;
        break;
    }
    // readlinkat(dirfd, path, buf, bufsiz)
    case 78: {
        const char *p = (const char *)a1;
        char *buf = (char *)a2;
        size_t bs = (size_t)a3;
        // Linux validates the buffer size FIRST: bufsiz <= 0 is EINVAL even for a nonexistent path.
        if ((int64_t)a3 <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // AT_EMPTY_PATH form: readlinkat(dirfd, "", buf, sz) with an EMPTY pathname operates on the file the
        // DIRFD itself names -- an O_PATH|O_NOFOLLOW fd opened directly on a symlink. macOS has no
        // AT_EMPTY_PATH (and passing "" to host readlinkat yields ENOTDIR/ENOENT), so recover the fd's own
        // host path via F_GETPATH and readlink THAT link. (-- LTP readlinkat01 dir_fd2/emptypath;
        // AT_FDCWD is excluded: an empty path there is a genuine ENOENT, handled by the normal path below.)
        if (p && !p[0] && (int)a0 >= 0) {
            char fp[4200];
            if (fcntl((int)a0, F_GETPATH, fp) == 0) {
                ssize_t r = readlink(fp, buf, bs);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
            } else {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            }
            break;
        }
        // Match every /proc magic link on the GUEST-ABSOLUTE path, so readlink("/proc/self/exe"),
        // readlinkat(AT_FDCWD, "proc/self/exe") from "/", and readlinkat(pid_dirfd, "exe") agree
        // byte-exactly. Paths that don't land in /proc (or /dev/fd) keep the raw pointer,
        // so the real-resolution fallback below is byte-identical for ordinary symlinks.
        char gpb[4200];
        const char *gp = p;
        if (p) {
            guest_abspath_at((int)a0, p, gpb, sizeof gpb);
            if (!strcmp(gpb, "/proc") || !strncmp(gpb, "/proc/", 6) || !strncmp(gpb, "/dev/fd/", 8) ||
                !strncmp(gpb, "/dev/std", 8) || !strncmp(gpb, "/sys/dev/char/226:", 18) ||
                !strncmp(gpb, "/sys/class/drm", 14))
                gp = gpb;
        }
        // /proc/self (and /proc/thread-self) are magic symlinks to the caller's own pid -- readlink returns
        // the decimal pid. `ls -l /proc` readlinks it now that /proc lists a "self" entry.
        if (p && (!strcmp(gp, "/proc/self") || !strcmp(gp, "/proc/thread-self"))) {
            char num[16];
            int l = snprintf(num, sizeof num, "%d", container_pid());
            if ((size_t)l > bs) l = (int)bs;
            memcpy(buf, num, (size_t)l);
            G_RET(c) = (uint64_t)l;
            break;
        }
        // /proc/mounts is itself a symlink to self/mounts (glibc/util-linux realpath it before parsing).
        if (p && !strcmp(gp, "/proc/mounts")) {
            static const char *const mt = "self/mounts";
            size_t l = strlen(mt);
            if (l > bs) l = bs;
            memcpy(buf, mt, l);
            G_RET(c) = (uint64_t)l;
            break;
        }
        // /proc/self/fd/N -> the path host fd N currently points at (recovered via F_GETPATH on macOS).
        int pfn = procfd_num(gp);
        if (pfn >= 0) {
            if (eventfd_peer_is_engine_fd(pfn)) {
                G_RET(c) = (uint64_t)(-ENOENT);
                break;
            }
            // a guest-created pty. Its slave must readlink to /dev/pts/N (never the host /dev/ttysNNN)
            // so ttyname(3)/`ls -l /proc/self/fd` resolve the Linux path; its master to the /dev/ptmx
            // multiplexer. Checked ahead of F_GETPATH, which would otherwise leak the host device name.
            {
                int pn = pts_index_of_fd(pfn);
                if (pn >= 0) {
                    char nm[32];
                    int l = pts_fd_is_master(pfn) ? snprintf(nm, sizeof nm, "/dev/ptmx")
                                                  : snprintf(nm, sizeof nm, "/dev/pts/%d", pn);
                    if ((size_t)l > bs) l = (int)bs;
                    memcpy(buf, nm, (size_t)l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
            }
            // The controlling terminal (stdio pty from `docker run -t`) is named /dev/pts/0 in the
            // container -- return that instead of leaking the host pty device (mac /dev/ttysNNN), so
            // ttyname(3)/`tty`/`ps` resolve a device that actually exists in the guest.
            if (fd_is_ctty(pfn)) {
                static const char *const cn = "/dev/pts/0";
                size_t l = strlen(cn);
                if (l > bs) l = bs;
                memcpy(buf, cn, l);
                G_RET(c) = l;
                break;
            }
            char gp[4200];
            if (fcntl(pfn, F_GETPATH, gp) != 0) {
                // A pathless fd (pipe/socket/eventfd/timerfd/anon inode): Linux still resolves
                // /proc/self/fd/N to a synthetic "pipe:[ino]" / "socket:[ino]" / "anon_inode:[...]" name --
                // never EBADF for an OPEN fd. Reproduce that so `ls -l /proc/self/fd`, lsof, and Go's
                // os.Readlink on a pipe fd work instead of erroring.
                if (fcntl(pfn, F_GETFD) < 0) {
                    // Linux: the /proc/self/fd entry for a CLOSED fd simply doesn't exist -> ENOENT
                    // (EBADF is only for a bad dirfd argument, never for the named link).
                    G_RET(c) = (uint64_t)(-ENOENT);
                    break;
                }
                struct stat ss;
                int have = fstat(pfn, &ss) == 0;
                char syn[64];
                int sl;
                if (have && S_ISFIFO(ss.st_mode))
                    sl = snprintf(syn, sizeof syn, "pipe:[%llu]", (unsigned long long)ss.st_ino);
                else if (have && S_ISSOCK(ss.st_mode))
                    sl = snprintf(syn, sizeof syn, "socket:[%llu]", (unsigned long long)ss.st_ino);
                else if (pfn >= 0 && pfn < DD_NFD && g_eventfd_peer[pfn])
                    sl = snprintf(syn, sizeof syn, "anon_inode:[eventfd]");
                else if (pfn >= 0 && pfn < DD_NFD && g_timerfd[pfn])
                    sl = snprintf(syn, sizeof syn, "anon_inode:[timerfd]");
                else
                    sl = snprintf(syn, sizeof syn, "anon_inode:inode");
                size_t l = (size_t)sl > bs ? bs : (size_t)sl;
                memcpy(buf, syn, l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            // map the host path back into the guest's view (strip the rootfs prefix if jailed)
            const char *gpath =
                (g_rootfs && !strncmp(gp, g_rootfs_canon, g_rootfs_canon_len)) ? gp + g_rootfs_canon_len : gp;
            if (!gpath[0]) gpath = "/";
            size_t l = strlen(gpath);
            if (l > bs) l = bs;
            memcpy(buf, gpath, l);
            G_RET(c) = l;
            break;
        }
        // /proc/[self|pid]/root and /proc/[self|pid]/cwd are magic symlinks: root -> the container's "/",
        // cwd -> the process's current working dir (Go/Rust path code and some init resolve these).
        if (p) {
            const char *leaf = proc_self_leaf(gp);
            if (leaf && (!strcmp(leaf, "root") || !strcmp(leaf, "cwd"))) {
                char cwb[4200];
                const char *tgt = "/";
                // bare mode (no rootfs): the engine chdir()s for real, so the live host cwd IS the guest cwd
                if (!strcmp(leaf, "cwd")) tgt = (!g_rootfs && getcwd(cwb, sizeof cwb)) ? cwb : (g_cwd[0] ? g_cwd : "/");
                size_t l = strlen(tgt);
                if (l > bs) l = bs;
                memcpy(buf, tgt, l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            // /proc/[self|pid]/ns/<name> -> "<name>:[<inode>]" namespace links (nsenter/iproute2 read these;
            // the inode constants are the kernel's initial-namespace values -- stable and plausible).
            if (leaf && !strncmp(leaf, "ns/", 3) && leaf[3]) {
                char nsb[64];
                int nl = ns_link_target(leaf + 3, nsb, sizeof nsb);
                if (nl >= 0) {
                    size_t l = (size_t)nl > bs ? bs : (size_t)nl;
                    memcpy(buf, nsb, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
            }
        }
        // Peer /proc/<pid>/ns/<name>: a container is a single namespace set, so a LIVE peer process's
        // namespace links readlink to the SAME "<name>:[<inode>]" values as self (lsns/nsenter inspect
        // live children by peer pid). proc_self_leaf matches only our own pid, so cover foreign pids here.
        if (p) {
            int peer = -1, hp = 0;
            const char *aleaf = proc_any_leaf(gp, &peer);
            if (aleaf && !strncmp(aleaf, "ns/", 3) && aleaf[3] && proc_pid_member(peer, &hp)) {
                char nsb[64];
                int nl = ns_link_target(aleaf + 3, nsb, sizeof nsb);
                if (nl >= 0) {
                    size_t l = (size_t)nl > bs ? bs : (size_t)nl;
                    memcpy(buf, nsb, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
            }
            // Peer /proc/<pid>/fd/<N> -> the fd's target (symlink-target view), read from the peer's libproc
            // fd table (its fds live in another dd worker process; procfd_num rejected the foreign pid above).
            // A closed/absent peer fd -> ENOENT. Opening the link stays deferred (needs cross-process fd
            // passing). proc_self_leaf matched only our own pid, so cover foreign pids here.
            if (aleaf && !strncmp(aleaf, "fd/", 3) && aleaf[3] && proc_pid_member(peer, &hp)) {
                int isnum = 1;
                for (const char *t = aleaf + 3; *t; t++)
                    if (*t < '0' || *t > '9') isnum = 0;
                if (isnum) {
                    char tgt[4200];
                    int tl = proc_fd_link_pid(hp, atoi(aleaf + 3), tgt, sizeof tgt);
                    if (tl < 0) {
                        G_RET(c) = (uint64_t)(-ENOENT);
                        break;
                    }
                    size_t l = (size_t)tl > bs ? bs : (size_t)tl;
                    memcpy(buf, tgt, l);
                    G_RET(c) = (uint64_t)l;
                    break;
                }
            }
        }
        // DRM sysfs symlinks (subsystem/device classification) libdrm's drmGetDevices2 readlinks.
        if (p && gpu_iosurface_on() &&
            (!strncmp(gp, "/sys/dev/char/226:", 18) || !strncmp(gp, "/sys/class/drm", 14))) {
            int dl = drm_synth_readlink(gp, buf, bs);
            if (dl >= 0) {
                G_RET(c) = (uint64_t)dl;
                break;
            }
        }
        char ep[1024];
        if (proc_self_exe(gp, ep, sizeof ep)) {
            size_t l = strlen(ep);
            if (l > bs) l = bs;
            memcpy(buf, ep, l);
            G_RET(c) = l;
        } else {
            // A path that EXISTS in the synthesized /proc (or cgroup /sys) view but is not one of the
            // magic links above is a regular file/dir there -> EINVAL, exactly like Linux. It must NOT
            // fall through to ENOENT: glibc/musl realpath() readlink every component and treat ENOENT as
            // "no such path" but EINVAL as "ordinary component" (completeness).
            struct stat ss;
            if (p && gp != p &&
                (!strcmp(gp, "/proc") || !strncmp(gp, "/proc/", 6) || !strncmp(gp, "/sys/fs/cgroup/", 15)) &&
                (!strcmp(gp, "/proc") || (synth_stat_raw(gp, &ss) && !S_ISLNK(ss.st_mode)))) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            char pb[4200];
            // Resolve through atpath (overlay-aware, nofollow=read the link itself, dirfd-relative confined):
            // a bare xlate() only consults the writable upper, so readlink of a lower-only path (e.g. a
            // PATH-launched binary in a read-only image layer) hit a non-existent upper path and returned
            // ENOENT instead of EINVAL -- breaking musl/glibc realpath(), which readlinks each path prefix
            // and treats ENOENT as "no such path" (PostgreSQL find_my_exec: "could not resolve path ...").
            const char *rp = atpath((int)a0, p, pb, sizeof pb, 1);
            // a result atpath left RELATIVE (bare mode, no rootfs) must resolve against the CALLER's
            // dirfd, not the engine cwd -- readlink(2) on it silently used the host cwd, so a dirfd-relative
            // link came back ENOENT/garbage. An absolute result ignores the dirfd, as before.
            int rel = rp && rp[0] != '/';
            int rc, len;
            if (!rel && rl_lookup(rp, &rc, buf, bs, &len)) {
                G_RET(c) = rc < 0 ? (uint64_t)(int64_t)rc : (uint64_t)len;
                break;
            }
            ssize_t r = readlinkat(rel ? ATFD(a0) : AT_FDCWD, rp, buf, bs);
            // Cache only absolute keys, and only UNTRUNCATED reads: r == bs may be a clipped read whose
            // stored text would poison a later full-buffer readlink of the same path with the short length.
            if (!rel && (r < 0 || (size_t)r < bs)) rl_store(rp, r < 0 ? -errno : (int)r, buf, r < 0 ? 0 : (int)r);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        }
        break;
    }
    case 79: {
        struct stat s;
        // newfstatat(dfd, path, buf, flags)
        char pb[4200];
        // AT_SYMLINK_NOFOLLOW (0x100): lstat -- resolve the final component WITHOUT following it.
        const char *raw = (const char *)a1;
        // reject unknown flag bits with EINVAL, and validate the dirfd (EBADF/ENOTDIR) for a relative
        // path -- both BEFORE resolving, matching the kernel. Valid flags: AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT
        // | AT_EMPTY_PATH = 0x1900. dd's g_fdpath fold otherwise accepts a bad/non-dir dirfd. (LTP fstatat01.)
        if (a3 & ~(uint64_t)0x1900) {
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
        const char *p = atpath((int)a0, raw, pb, sizeof pb, (a3 & 0x100) ? 1 : 0);
        {
            const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
            // A dirfd-RELATIVE name (fstatat(pid_dirfd, "exe")) that lands in /proc must hit the same
            // magic-link synthesis as its absolute spelling (consistency; bare mode included, where
            // atpath leaves the raw relative path untouched).
            char gsyn[4200];
            if (raw && raw[0] && raw[0] != '/') {
                guest_abspath_at((int)a0, raw, gsyn, sizeof gsyn);
                if (!strncmp(gsyn, "/proc/", 6)) gp = gsyn;
            }
            char ep[1024];
            if (proc_self_exe(gp, ep, sizeof ep)) {
                struct stat es;
                // The magic /proc/self/exe always "exists", so validate the guest stat buffer now (before
                // the engine fills it directly) -> a bad pointer is -EFAULT, matching Linux's copyout.
                if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                if (a3 & 0x100) { // lstat: report the magic symlink itself (Linux: st_size == 0)
                    memset(&es, 0, sizeof es);
                    es.st_mode = S_IFLNK | 0777;
                    es.st_size = 0;
                    es.st_nlink = 1;
                    fill_linux_stat((uint8_t *)a2, &es, NULL, -1); // synth /proc/self/exe symlink
                    G_RET(c) = 0;
                    break;
                }
                // stat (follow): stat the actual executable file through the jail
                char hb[4200];
                const char *hp = xresolve_overlay(ep, hb, sizeof hb);
                if (stat(hp, &es) == 0) {
                    fill_linux_stat((uint8_t *)a2, &es, hp, -1);
                    G_RET(c) = 0;
                    break;
                }
                // file unexpectedly missing -> fall through to the generic ENOENT path
            }
            // /proc/[self|pid]/{root,cwd} magic symlinks: lstat reports the link, stat follows to the dir.
            {
                const char *sleaf = proc_self_leaf(gp);
                if (sleaf && (!strcmp(sleaf, "root") || !strcmp(sleaf, "cwd"))) {
                    // Magic /proc/self/{root,cwd} always resolves; validate the guest stat buffer before the
                    // engine fills it -> a bad pointer is -EFAULT, matching Linux's copyout ordering.
                    if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                    char cwb[4200];
                    const char *tgt = "/";
                    // bare mode: the engine chdir()s for real, so the live host cwd IS the guest cwd
                    if (!strcmp(sleaf, "cwd"))
                        tgt = (!g_rootfs && getcwd(cwb, sizeof cwb)) ? cwb : (g_cwd[0] ? g_cwd : "/");
                    struct stat es;
                    if (a3 & 0x100) { // lstat: the symlink itself (Linux: st_size == 0)
                        memset(&es, 0, sizeof es);
                        es.st_mode = S_IFLNK | 0777;
                        es.st_size = 0;
                        es.st_nlink = 1;
                        fill_linux_stat((uint8_t *)a2, &es, NULL, -1);
                        G_RET(c) = 0;
                        break;
                    }
                    char hb[4200];
                    const char *hp = xresolve_overlay(tgt, hb, sizeof hb);
                    if (stat(hp, &es) == 0) {
                        fill_linux_stat((uint8_t *)a2, &es, hp, -1);
                        G_RET(c) = 0;
                        break;
                    }
                }
            }
            // synthesized /proc or /sys file: split synth_stat so we only validate the guest buffer once we
            // KNOW it is a synth path (which "exists") -> a bad pointer is -EFAULT on copyout, and a
            // non-synth path falls through to the generic handler below with Linux's normal ordering.
            {
                struct stat synth_s;
                if (synth_stat_raw(gp, &synth_s)) {
                    if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                    fill_linux_stat((uint8_t *)a2, &synth_s, NULL, -1);
                    G_RET(c) = 0;
                    break;
                }
            }
        }
        // cacheable: named path, follow
        if (raw && raw[0] && !(a3 & 0x100)) {
            int rc;
            if (!mc_lookup(p, &rc, &s)) {
                int r = fstatat(ATFD(a0), p, &s, 0);
                rc = r < 0 ? -errno : 0;
                mc_store(p, rc, &s);
            }
            // Validate the guest buffer only after a successful stat (copyout-last: a bad path still
            // reports its own errno first, matching Linux) -> a bad pointer is -EFAULT, not an engine fault.
            if (rc == 0) {
                if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                fill_linux_stat((uint8_t *)a2, &s, p, -1);
            }
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        // AT_EMPTY_PATH -> fstat(dfd)
        int empty_self = (raw && !raw[0] && (a3 & 0x1000));
        int r = (empty_self && memf_get((int)a0)) ? memf_fstat((int)a0, &s)
                : empty_self                      ? fstat((int)a0, &s)
                                                  : fstatat(ATFD(a0), p, &s, AT_SYMLINK_NOFOLLOW);
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (empty_self) drm_fd_stat_fixup((int)a0, &s); // render-node fstatat("",AT_EMPTY_PATH) -> char dev
        // guest-chown xattr lives on the host backing file: read via fd for AT_EMPTY_PATH, else by path.
        // The stat succeeded above, so validate the guest buffer here (copyout-last) -> bad ptr = -EFAULT.
        if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        fill_linux_stat((uint8_t *)a2, &s, empty_self ? NULL : p, empty_self ? (int)a0 : -1);
        G_RET(c) = 0;
        break;
    }
    case 80: {
        // fstat(fd, buf)
        struct stat s;
        int sr = memf_get((int)a0) ? memf_fstat((int)a0, &s) : fstat((int)a0, &s);
        if (sr < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        drm_fd_stat_fixup((int)a0, &s); // render-node fd -> char dev 226:<minor> (else /dev/null's rdev leaks)
        // The guest stat buffer is filled DIRECTLY by the engine; validate it (after the fd/stat succeeds,
        // so a bad fd still reports EBADF first, matching Linux's copyout-last ordering) so a bad pointer
        // returns -EFAULT instead of faulting the engine (access_ok).
        if (!host_range_mapped((uintptr_t)a1, GUEST_LINUX_STAT_BYTES)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        fill_linux_stat((uint8_t *)a1, &s, NULL, (int)a0);
        G_RET(c) = 0;
        break;
    }
    case 81:
        sync();
        G_RET(c) = 0;
        // sync
        break;
    // syncfs(fd): no macOS syncfs -> flush this fd then sync the system. RAM-backed scratch is a no-op.
    case 267:
        if (!memf_get((int)a0)) {
            fsync((int)a0);
            sync();
        }
        G_RET(c) = 0;
        break;
    // utimensat(dirfd, path, times, flags)
    case 88: {
        // Linux rejects unknown flag bits (only AT_SYMLINK_NOFOLLOW=0x100 is valid) with EINVAL before
        // touching the file -- otherwise a bad flag value would still update the timestamps.
        if (a3 & ~0x100u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        struct timespec *ts = (struct timespec *)a2;
        struct timespec lts[2];
        // Linux and macOS disagree on the tv_nsec "special" sentinels: Linux UTIME_NOW = 0x3fffffff /
        // UTIME_OMIT = 0x3ffffffe, but the host (macOS) wants UTIME_NOW = -1 / UTIME_OMIT = -2. The host
        // utimensat/futimens only honor the macOS values, so a guest passing the Linux sentinels (glibc's
        // futimens/utimensat, and dd's own utime/utimes/futimesat -> utimensat rewrites whenever a field is
        // "set to now") would otherwise write the raw 0x3ffffffe nanoseconds instead of omitting/now-ing the
        // field. Copy out to a local (never mutate guest memory) and translate both slots. a2==NULL stays
        // NULL (= set both to now). EFAULT a bad non-NULL times pointer (we now dereference it in-engine).
        if (a2) {
            if (guest_bad_ptr((uintptr_t)a2, sizeof(struct timespec) * 2)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            lts[0] = ts[0];
            lts[1] = ts[1];
            for (int i = 0; i < 2; i++) {
                if (lts[i].tv_nsec == 0x3fffffff)
                    lts[i].tv_nsec = UTIME_NOW; // Linux UTIME_NOW  -> macOS
                else if (lts[i].tv_nsec == 0x3ffffffe)
                    lts[i].tv_nsec = UTIME_OMIT; // Linux UTIME_OMIT -> macOS
            }
            ts = lts;
        }
        if (!a1) {
            G_RET(c) = futimens((int)a0, ts) < 0 ? (uint64_t)(-errno) : 0;
            break;
            // path NULL -> futimens(fd)
        }
        if (jail_ro_at((int)a0, (const char *)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        if (g_rootfs) {
            overlay_copyup_at((int)a0, (const char *)a1); // bring a lower-only target up so jail_at finds it
            char fin[512];
            int pfd = jail_at((int)a0, (const char *)a1, fin, sizeof fin, (a3 & 0x100) ? 1 : 0);
            if (pfd < 0) {
                G_RET(c) = (uint64_t)(int64_t)pfd;
                break;
            }
            int r = utimensat(pfd, fin, ts, (a3 & 0x100) ? AT_SYMLINK_NOFOLLOW : 0), e = errno;
            char dp[4200];
            if (r >= 0 && fcntl(pfd, F_GETPATH, dp) == 0) {
                char hp[4400];
                snprintf(hp, sizeof hp, "%s/%s", dp, fin);
                mc_evict(hp);
                // mtime changed
            }
            close(pfd);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char pb[4200];
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int r = utimensat(ATFD(a0), p, ts, (a3 & 0x100) ? AT_SYMLINK_NOFOLLOW : 0);
        if (r >= 0) mc_evict(p);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // umask -> old mask
    case 166: G_RET(c) = (uint64_t)umask((mode_t)a0); break;
    // fadvise64 -- advisory no-op
    case 223: G_RET(c) = 0; break;
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
        // Validate the guest pointers before any deref: a bad path or result buffer must return -EFAULT, not
        // fault the engine. guest_bad_ptr (not host_addr_mapped) also faults a PROT_NONE guard page -- the LTP
        // tst_get_bad_addr idiom -- which dd force-maps host-readable (and zero-filled, so raw[0] must NOT be
        // consulted here or the guard page reads as an empty "" path). host_addr_mapped wrongly passed it.
        if (raw && guest_bad_ptr((uintptr_t)raw, 1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (!host_range_mapped((uintptr_t)a4, 256)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const char *p = atpath((int)a0, raw, pb, sizeof pb, nofollow);
        int rc, empty = (raw && !raw[0] && (a2 & 0x1000));
        const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
        // Track the host backing file so ownership virtualization reads the SAME guest-chown xattr that
        // fstat/newfstatat do: xpath = the host path we stat'd, or xfd = the fd for AT_EMPTY_PATH;
        // both stay NULL/-1 for synthetic entries (no backing file -> cuid/cgid default applies).
        const char *xpath = NULL;
        int xfd = -1;
        char ep[1024];
        if (proc_self_exe(gp, ep, sizeof ep)) {
            // /proc/[self|pid]/exe magic symlink -> the running executable
            if (nofollow) { // the magic symlink itself (Linux: st_size == 0)
                memset(&s, 0, sizeof s);
                s.st_mode = S_IFLNK | 0777;
                s.st_size = 0;
                s.st_nlink = 1;
                rc = 0;
            } else {
                char hb[4200];
                const char *hp = xresolve_overlay(ep, hb, sizeof hb);
                rc = stat(hp, &s) == 0 ? 0 : -errno;
                if (rc == 0) xpath = hp;
            }
        } else if (synth_stat_raw(gp, &s)) {
            rc = 0;
            // synth /proc or /sys -> fill from s below (synthetic: no backing file, xpath/xfd stay NULL/-1)
        }
        // cacheable (only the follow case -- the path cache doesn't distinguish follow vs nofollow)
        else if (raw && raw[0] && !empty && !nofollow) {
            if (!mc_lookup(p, &rc, &s)) {
                int rr = fstatat(ATFD(a0), p, &s, 0);
                rc = rr < 0 ? -errno : 0;
                mc_store(p, rc, &s);
            }
            if (rc == 0) xpath = p;
        } else {
            int esf = empty && memf_get((int)a0);
            int rr = esf     ? memf_fstat((int)a0, &s)
                     : empty ? fstat((int)a0, &s)
                             : fstatat(ATFD(a0), p, &s, nofollow ? AT_SYMLINK_NOFOLLOW : 0);
            rc = rr < 0 ? -errno : 0;
            if (rc == 0) {
                if (empty)
                    xfd = (int)a0; // AT_EMPTY_PATH: xattr lives on the fd's backing file
                else
                    xpath = p;
            }
        }
        if (rc < 0) {
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        if (empty) drm_fd_stat_fixup((int)a0, &s); // render-node statx("",AT_EMPTY_PATH) -> char dev 226:<minor>
        // Route ownership through the SHARED virtualization (cuid/cgid default + guest-chown xattr via
        // the cache) so statx's uid/gid are byte-identical to fstat/newfstatat for the same file.
        uint32_t vuid, vgid;
        stat_virt_ids(&s, xpath, xfd, &vuid, &vgid);
        uint8_t *d = (uint8_t *)a4;
        // struct statx (Linux uapi offsets). We fill STATX_BASIC_STATS | STATX_BTIME.
        memset(d, 0, 256);
        // stx_mask @0 = basic(0x7ff) | btime(0x800); stx_blksize @4
        *(uint32_t *)(d + 0) = 0x7ff | 0x800;
        *(uint32_t *)(d + 4) = 4096;
        // stx_nlink @16 (raw, matching fill_linux_stat)
        *(uint32_t *)(d + 16) = (uint32_t)s.st_nlink;
        // stx_uid @20  stx_gid @24 (virtualized)
        *(uint32_t *)(d + 20) = vuid;
        *(uint32_t *)(d + 24) = vgid;
        // stx_mode @28
        *(uint16_t *)(d + 28) = (uint16_t)s.st_mode;
        // stx_ino @32
        *(uint64_t *)(d + 32) = s.st_ino;
        // stx_size @40
        *(uint64_t *)(d + 40) = (uint64_t)s.st_size;
        // stx_blocks @48
        *(uint64_t *)(d + 48) = (uint64_t)s.st_blocks;
        // stx_{atime,btime,ctime,mtime} @64/80/96/112: {s64 tv_sec; u32 tv_nsec} each 16 bytes
        *(int64_t *)(d + 64) = (int64_t)s.st_atimespec.tv_sec;
        *(uint32_t *)(d + 72) = (uint32_t)s.st_atimespec.tv_nsec;
        *(int64_t *)(d + 80) = (int64_t)s.st_birthtimespec.tv_sec;
        *(uint32_t *)(d + 88) = (uint32_t)s.st_birthtimespec.tv_nsec;
        *(int64_t *)(d + 96) = (int64_t)s.st_ctimespec.tv_sec;
        *(uint32_t *)(d + 104) = (uint32_t)s.st_ctimespec.tv_nsec;
        *(int64_t *)(d + 112) = (int64_t)s.st_mtimespec.tv_sec;
        *(uint32_t *)(d + 120) = (uint32_t)s.st_mtimespec.tv_nsec;
        // stx_rdev_major @128 / minor @132, stx_dev_major @136 / minor @140 -- decoded from the SAME raw
        // dev values fill_linux_stat packs into st_rdev/st_dev, so a caller sees identical major:minor.
        *(uint32_t *)(d + 128) = lin_dev_major((uint64_t)s.st_rdev);
        *(uint32_t *)(d + 132) = lin_dev_minor((uint64_t)s.st_rdev);
        *(uint32_t *)(d + 136) = lin_dev_major((uint64_t)s.st_dev);
        *(uint32_t *)(d + 140) = lin_dev_minor((uint64_t)s.st_dev);
        G_RET(c) = 0;
        break;
    }
    // name_to_handle_at(dfd, path, file_handle*, mount_id*, flags): macOS has no FS file handles, so
    // synthesize a stable 16-byte handle from st_dev+st_ino (round-trips file identity). file_handle is
    // { u32 handle_bytes; i32 handle_type; u8 f_handle[]; }; handle_bytes is the buffer size on input
    // and is rewritten to the produced size (-EOVERFLOW if the caller's buffer is too small).
    case 264: {
        uint8_t *fh = (uint8_t *)a2;
        if (!fh || !host_range_mapped((uintptr_t)a2, 4)) { // handle_bytes read/write below
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
        if (*(uint32_t *)(fh + 0) < need) {
            *(uint32_t *)(fh + 0) = need;
            G_RET(c) = (uint64_t)(int64_t)(-EOVERFLOW);
            break;
        }
        if (!host_range_mapped((uintptr_t)a2, need + 8)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t dev = (uint64_t)s.st_dev, ino = (uint64_t)s.st_ino;
        *(uint32_t *)(fh + 0) = need; // handle_bytes
        *(int32_t *)(fh + 4) = 1;     // handle_type (stable, arbitrary)
        memcpy(fh + 8, &dev, 8);
        memcpy(fh + 16, &ino, 8);
        if (a3) {
            if (!host_range_mapped((uintptr_t)a3, sizeof(int))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            *(int *)a3 = (int)s.st_dev; // mount_id
        }
        G_RET(c) = 0;
        break;
    }
    // faccessat2(dirfd,path,mode,flags) -- glibc access() uses it; same path/confinement, flags ignored
    case 439:
    case 48: {
        char pb[4200];
        // Linux: an empty pathname is ENOENT for faccessat(48), and for faccessat2(439) unless
        // AT_EMPTY_PATH(0x1000) is set. dd used to resolve "" to the rootfs root (a searchable dir) and
        // report it executable, so `[ -x "$(command -v missing)" ]` (dash's `command -v` yields "" for a
        // missing command) wrongly passed and ran a nonexistent `update-menus` -> exit 127. That is the
        // dh_installmenu postinst guard in fish/lynx/many packages, so it broke `dpkg --configure`.
        if (!a1 || !((const char *)a1)[0]) {
            if (!(nr == 439 && (a3 & 0x1000))) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                break;
            }
        }
        // /proc/[self|pid]/exe magic symlink -> access the actual executable (matched on the
        // guest-absolute path so dirfd-relative and cwd-relative spellings work too)
        char ep[1024], gsyn48[4200];
        const char *gp48 = (const char *)a1;
        if (gp48 && gp48[0] && gp48[0] != '/') {
            guest_abspath_at((int)a0, gp48, gsyn48, sizeof gsyn48);
            if (!strncmp(gsyn48, "/proc/", 6)) gp48 = gsyn48;
        }
        if (proc_self_exe(gp48, ep, sizeof ep)) {
            char hb[4200];
            const char *hp = xresolve_overlay(ep, hb, sizeof hb);
            int r = access(hp, (int)a2);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // pseudo /dev char devices (open() backs them with a host node) must also test as present: e.g.
        // libgcrypt probes access("/dev/urandom",R_OK) to pick its RNG module -- an ENOENT there aborts
        // gpgv and breaks `apt-get update`. Test the host device with the requested mode.
        {
            const char *hd = dev_node_hostpath((const char *)a1);
            if (hd) {
                int r = access(hd, (int)a2);
                G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // faccessat
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        {
            const char *gp = (g_rootfs && p && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
            if (gp48 && gp48 != (const char *)a1 && !strncmp(gp48, "/proc/", 6)) gp = gp48;
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
            if (!ac_lookup(p, &rc)) {
                int r = faccessat(ATFD(a0), p, 0, 0);
                rc = r < 0 ? -errno : 0;
                ac_store(p, rc);
            }
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        int r = faccessat(ATFD(a0), p, (int)a2, 0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
