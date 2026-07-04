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
static void tty_ctl_block(sigset_t *saved) {
    sigset_t blk;
    sigemptyset(&blk);
    sigaddset(&blk, SIGTTOU);
    sigprocmask(SIG_BLOCK, &blk, saved);
}
static void tty_ctl_restore(const sigset_t *saved) { sigprocmask(SIG_SETMASK, saved, NULL); }

// #383: statx returns device numbers as separate major/minor u32s, whereas struct stat packs them into a
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
// large directories no longer truncate, #179) and owned until freed (ovldents_free). Indexed DIRECTLY by
// guest fd (the getdents call site guarantees fd in [0,1024)); a former 16-slot table with slot-0 eviction
// broke deep `find`: a recursive walk keeps one open dir fd per level, so past 16 concurrent overlay dirs
// an ancestor's snapshot was evicted and its next getdents re-snapshotted from pos 0 -> re-descended the
// same subtree forever (loop threshold was exactly depth 16, #199).
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
    if (fd >= 0 && fd < 1024 && g_ovldents[fd].taken)
        ovldents_free(fd);
}
// rewinddir/seekdir on an overlay-merged dir: reset the replay cursor. pos<=0 (or out of range) restarts
// from the top; an untaken snapshot is left alone (the next getdents re-snapshots from 0). Forward-declared
// in vfs.c for the lseek handler (io.c), which is compiled into this TU before fs.c.
static void ovldents_rewind(int fd, int pos) {
    if (fd < 0 || fd >= 1024 || !g_ovldents[fd].taken) return;
    g_ovldents[fd].pos = (pos > 0 && pos <= g_ovldents[fd].n) ? pos : 0;
}

// POSIX shm / named semaphores live under /dev/shm, for which the rootfs has no tmpfs; glibc backs them
// with files there (shm_open -> /dev/shm/<name>, sem_open -> a temp /dev/shm/sem.<rnd> then link()ed to
// /dev/shm/sem.<name>). openat (case 56) redirects these to a stable host file /tmp/.ddshm-<name> so the
// page is real and MAP_SHARED across fork. The link/rename/unlink that COMPLETE glibc's create dance must
// use the SAME backing, but the rootfs branches of those handlers resolve via jail_at into the (empty)
// <rootfs>/dev/shm and ENOENT -- breaking sem_open (python multiprocessing). Returns the host backing path
// for a /dev/shm/<name> guest path (into buf), or NULL otherwise. Same name-flattening as the openat redirect.
static const char *shm_hostpath(const char *guest, char *buf, size_t n) {
    if (!guest || strncmp(guest, "/dev/shm/", 9)) return NULL;
    int m = snprintf(buf, n, "/tmp/.ddshm-%s", guest + 9);
    if (m > (int)n - 1) m = (int)n - 1;
    for (int i = 12; i < m; i++)
        if (buf[i] == '/') buf[i] = '_';
    return buf;
}

// Tear down EVERY dd-side emulation-table entry keyed by this fd NUMBER (eventfd peer/counter/sema, timerfd,
// overlay-dir, the socket/loopback/bridge maps, epoll armed-state, flock, pidfd, RAM-scratch memf, and the
// getdents/overlay-dents caches + the path map). Shared by close(2) (case 57) AND the emulated
// close-on-exec sweep (proc.c exec_close_cloexec*). #282: dd's execve reloads the new image IN-PROCESS, so the
// sweep hand-closes each FD_CLOEXEC descriptor -- but it used to close ONLY the real fd, leaving these tables
// stamped. A CLOEXEC eventfd thus left g_eventfd_peer[fd] set after exec; the new program (postgres) opened
// postgresql.conf onto that freed fd number and read() was misrouted to the eventfd emulation -> 0 bytes of
// real content -> `syntax error in file "postgresql.conf" line 1, near token ""` and the server never starts
// (PG16/17 only -- PG15's streaming conf reader tolerated the short read; hence the version gate). Does NOT
// close(fd) itself -- the caller owns the real fd's lifetime. Safe on a non-emulated fd (every branch is
// guarded / idempotent). Mirrors case 57's teardown exactly so close(2) semantics are unchanged.
static void fd_reset_emul(int fd) {
    if (fd >= 0 && fd < 1024) {
        if (g_eventfd_peer[fd]) {
            close(g_eventfd_peer[fd] - 1);
            g_eventfd_peer[fd] = 0;
        }
        g_timerfd[fd] = 0;
        g_tfd_deadline[fd] = 0;
        g_tfd_interval[fd] = 0;
        g_memfd_is[fd] = 0;
        g_memfd_seal[fd] = 0;
        g_inotify_owner[fd] = 0;
        if (g_fd_pushback[fd]) { free(g_fd_pushback[fd]); g_fd_pushback[fd] = NULL; g_fd_pb_len[fd] = 0; }
        g_ovldir[fd][0] = 0;
        g_opath[fd] = 0;
        g_devfull[fd] = 0;
        g_lo_port[fd] = 0;
        g_sock_stream[fd] = 0;
        g_sock_dgram[fd] = 0;
        seq_send_eof(fd);
        g_sock_seqpacket[fd] = 0;
        g_br_port[fd] = 0;
        g_br_ip[fd] = 0;
        if (g_dns_sock[fd]) { // container DNS: close the engine-held socketpair peer
            if (g_dns_peer[fd] >= 0) close(g_dns_peer[fd]);
            g_dns_peer[fd] = -1;
            g_dns_sock[fd] = 0;
        }
        nl_close(fd); // #289: tear down a netlink socket's socketpair peer
        g_eventfd_count[fd] = 0;
        g_eventfd_sema[fd] = 0;
        ep_fd_reset(fd);
        flock_on_close(fd);
        poslk_on_close(fd); // #340: POSIX drops all this process's fcntl record locks when any fd closes
    }
    pidfd_forget(fd);
    memf_close(fd);
    dirs_drop(fd);
    ovldents_drop(fd);
    fd_clear(fd);
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
static int ddx_opt(uint64_t flags, int nofollow) {
    int o = nofollow ? XATTR_NOFOLLOW : 0;
    if (flags & 1) o |= XATTR_CREATE;  // Linux XATTR_CREATE
    if (flags & 2) o |= XATTR_REPLACE; // Linux XATTR_REPLACE
    return o;
}
static long ddx_set(const char *host, const char *name, const void *val, size_t sz, int opt) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", DDX_PFX, name ? name : "");
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

static int svc_fs(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                  uint64_t a4, uint64_t a5) {
    switch (nr) {
    // ===================== Filesystem — open/stat/dir/link/perm/xattr/cwd, all path-confined to the rootfs jail
    // =====================
    // setxattr(5)/lsetxattr(6)/fsetxattr(7): a0=path|fd, a1=name, a2=val, a3=size, a4=flags
    case 5:
    case 6:
    case 7: {
        char host[4300];
        int e;
        if (nr == 7) e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else e = xattr_hostpath((const char *)a0, nr == 6, 1, host, sizeof host);
        if (e < 0) { G_RET(c) = (uint64_t)(int64_t)e; break; }
        G_RET(c) = (uint64_t)(int64_t)ddx_set(host, (const char *)a1, (const void *)a2, (size_t)a3, ddx_opt(a4, nr == 6));
        break;
    }
    // getxattr(8)/lgetxattr(9)/fgetxattr(10): a0=path|fd, a1=name, a2=val, a3=size
    case 8:
    case 9:
    case 10: {
        char host[4300];
        int e;
        if (nr == 10) e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else e = xattr_hostpath((const char *)a0, nr == 9, 0, host, sizeof host);
        if (e < 0) { G_RET(c) = (uint64_t)(int64_t)e; break; }
        G_RET(c) = (uint64_t)(int64_t)ddx_get(host, (const char *)a1, (void *)a2, (size_t)a3, nr == 9 ? XATTR_NOFOLLOW : 0);
        break;
    }
    // listxattr(11)/llistxattr(12)/flistxattr(13): a0=path|fd, a1=list, a2=size
    case 11:
    case 12:
    case 13: {
        char host[4300];
        int e;
        if (nr == 13) e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else e = xattr_hostpath((const char *)a0, nr == 12, 0, host, sizeof host);
        if (e < 0) { G_RET(c) = (uint64_t)(int64_t)e; break; }
        G_RET(c) = (uint64_t)(int64_t)ddx_list(host, (char *)a1, (size_t)a2, nr == 12 ? XATTR_NOFOLLOW : 0);
        break;
    }
    // removexattr(14)/lremovexattr(15)/fremovexattr(16): a0=path|fd, a1=name
    case 14:
    case 15:
    case 16: {
        char host[4300];
        int e;
        if (nr == 16) e = fcntl((int)a0, F_GETPATH, host) == 0 ? 0 : -EBADF;
        else e = xattr_hostpath((const char *)a0, nr == 15, 1, host, sizeof host);
        if (e < 0) { G_RET(c) = (uint64_t)(int64_t)e; break; }
        G_RET(c) = (uint64_t)(int64_t)ddx_remove(host, (const char *)a1, nr == 15 ? XATTR_NOFOLLOW : 0);
        break;
    }
    case 17: {
        if (g_rootfs) {
            // getcwd -> the GUEST cwd (not the host path)
            size_t l = strlen(g_cwd);
            if (a0 && l + 1 <= a1) {
                if (!host_range_mapped((uintptr_t)a0, l + 1)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
                memcpy((void *)a0, g_cwd, l + 1);
                G_RET(c) = l + 1;
            } else
                G_RET(c) = (uint64_t)(-ERANGE);
            break;
        }
        if (a0 && !host_range_mapped((uintptr_t)a0, (size_t)a1)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
        if (getcwd((char *)a0, (size_t)a1))
            G_RET(c) = strlen((char *)a0) + 1;
        else
            G_RET(c) = (uint64_t)(-errno);
        break;
    }
    // ioctl(fd, req, arg) -- Linux req# -> macOS
    case 29: {
        int fd = (int)a0;
        // Truncate the ioctl request to 32 bits (Linux `cmd` is unsigned int). musl declares ioctl's request
        // as `int`, so a read-direction request with the direction bit set (e.g. TIOCGPTN 0x80045430) arrives
        // SIGN-EXTENDED as 0xffffffff80045430 and would miss its switch case -> ENOTTY (glibc zero-extends, so
        // it worked there). This makes both forms match, fixing musl tmux/script/openpty and any high-bit ioctl. (#219)
        unsigned long rq = (uint32_t)a1;
        void *arg = (void *)a2;
        // macOS pty MASTERS reject every termios/winsize ioctl with ENOTTY -- unlike Linux, where the master
        // accepts them and they act on the shared line discipline (dpkg's openpty master does TIOCSWINSZ +
        // tcsetattr(TCSANOW) on it; that ENOTTY is apt's "Setting TIOCSWINSZ for master fd N failed" and the
        // debconf frontend-fallback that follows). termios + winsize are properties of the pty PAIR, so when
        // the request targets a master (not itself a tty, but ptsname() resolves its slave) we retarget the
        // op to a transient slave fd -- giving the guest exact Linux master semantics on x86 and arm alike.
        int tfd = fd, pts_slave = -1;
        switch (rq) {
        case 0x5401: case 0x5402: case 0x5403: case 0x5404:               // TCGETS / TCSETS{,W,F}
        case 0x5413: case 0x5414:                                         // TIOCGWINSZ / TIOCSWINSZ
        case 0x802c542a: case 0x402c542b: case 0x402c542c: case 0x402c542d: // TCGETS2 / TCSETS2{,W,F}
            if (!isatty(fd)) {
                char *sn = ptsname(fd);
                if (sn) { pts_slave = open(sn, O_RDWR | O_NOCTTY); if (pts_slave >= 0) tfd = pts_slave; }
            }
            break;
        default: break;
        }
        switch (rq) {
        case 0x5401: {
            struct termios t;
            if (tcgetattr(tfd, &t) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
                // TCGETS
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
            tty_ctl_block(&sv); // a bg-group tcsetattr would otherwise SIGTTOU-stop the caller
            G_RET(c) = tcsetattr(tfd, act, &t) < 0 ? (uint64_t)(-errno) : 0;
            tty_ctl_restore(&sv);
            break;
        }
        case 0x802c542a: {
            struct termios t;
            if (tcgetattr(tfd, &t) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
                // TCGETS2 (glibc aarch64 uses this)
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
            G_RET(c) = tcsetattr(tfd, act, &t) < 0 ? (uint64_t)(-errno) : 0;
            tty_ctl_restore(&sv);
            break;
        }
        case 0x5413:
            G_RET(c) = ioctl(tfd, TIOCGWINSZ, arg) < 0 ? (uint64_t)(-errno) : 0;
            // TIOCGWINSZ (struct same)
            break;
        // TIOCSWINSZ
        case 0x5414: G_RET(c) = ioctl(tfd, TIOCSWINSZ, arg) < 0 ? (uint64_t)(-errno) : 0; break;
        case 0x80045430:
            if (arg && fd >= 0 && fd < 1024) *(uint32_t *)arg = (uint32_t)fd;
            G_RET(c) = 0;
            // TIOCGPTN -> pts# = master fd
            break;
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
            if (!sn) { G_RET(c) = (uint64_t)(int64_t)(-(errno ? errno : EINVAL)); break; }
            int mf = ((int)a2 & 0x3) | O_NOCTTY;      // access mode (shared values) + no controlling tty
            if (a2 & 0x80000) mf |= O_CLOEXEC;        // honor Linux O_CLOEXEC on the returned fd
            int s = open(sn, mf);
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
            pid_t fg = isatty(fd) ? tcgetpgrp(fd) : -1;
            if (fg <= 0) fg = getpgrp();
            if (g_init_hostpid && fg == g_init_hostpid) fg = 1; // init's real group -> guest pgid 1
            if (arg) *(int *)arg = (int)fg;
            G_RET(c) = 0;
            break;
        }
        case 0x5410: { // tcsetpgrp
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
            // Socket ioctls (SIOCGIF*, #294): answer from the shared lo+eth0 model (netns.c) when `fd`
            // is a socket; otherwise ENOTTY.
            int64_t r;
            if (net_ioctl(fd, rq, (uint8_t *)arg, &r)) { G_RET(c) = (uint64_t)r; break; }
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
            if (g_nlower) { // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker first
                char gpm[4200];
                abs_guest((int)a0, (const char *)a1, gpm, sizeof gpm);
                overlay_clear_whiteout(gpm);
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
                if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1); // #255: dropped-cred creator owns it
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
            if (newfile_stamp_wanted()) newfile_stamp_path(p, 1); // #255
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
                if (newfile_stamp_wanted()) newfile_stamp_path(hp, 1); // #255: dropped-cred creator owns the dir
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
        if (r >= 0 && newfile_stamp_wanted()) newfile_stamp_path(p, 1); // #255
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // unlinkat(dirfd, path, flags) -- confined
    case 35: {
        // shm/sem files are flat host files under /tmp (see shm_hostpath); sem_unlink/shm_unlink and glibc's
        // temp-file cleanup must hit that backing, not the jail's <rootfs>/dev/shm. AT_REMOVEDIR never applies.
        char shb[300];
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
            // file and made dpkg abort "package control info contained directory" (#254). Match Linux:
            // rmdir a non-directory -> ENOTDIR; unlink a directory -> EISDIR.
            struct stat lst;
            int isdir = lstat(host, &lst) == 0 && S_ISDIR(lst.st_mode);
            if ((a2 & 0x200) && !isdir) { G_RET(c) = (uint64_t)(int64_t)(-ENOTDIR); break; }
            if (!(a2 & 0x200) && isdir) { G_RET(c) = (uint64_t)(int64_t)(-EISDIR); break; }
            // rmdir must fail ENOTEMPTY on a non-empty MERGED dir. The upper-only branch below lets the
            // kernel enforce this, but a lower-backed dir is whiteout-masked unconditionally -- so it would
            // wrongly "succeed" and hide live lower children. Check the merged listing first (overlay_readdir
            // always includes "." and ".." -> a count > 2 means the directory still has real children).
            if ((a2 & 0x200) && isdir) {
                char(*nm)[256] = NULL;
                uint8_t *ty = NULL;
                int nent = overlay_readdir(gp, &nm, &ty);
                free(nm);
                free(ty);
                if (nent > 2) { G_RET(c) = (uint64_t)(int64_t)(-ENOTEMPTY); break; }
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
                if (try_adopt && S_ISREG(ps.st_mode)) { adev = (uint64_t)ps.st_dev; aino = (uint64_t)ps.st_ino; }
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
            if (try_adopt && S_ISREG(ps.st_mode)) { adev = (uint64_t)ps.st_dev; aino = (uint64_t)ps.st_ino; }
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
        if (jail_ro_at((int)a1, (const char *)a2)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        const char *target =
            // target is the link CONTENT (unresolved); follow-time confinement guards it
            (const char *)a0;
        if (g_rootfs) {
            if (g_nlower) { // recreating a whiteout'd name -> clear its stale `.wh.NAME` marker first
                char gpm[4200];
                abs_guest((int)a1, (const char *)a2, gpm, sizeof gpm);
                overlay_clear_whiteout(gpm);
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
        // glibc's sem_open/shm_open creation links a temp /dev/shm/sem.<rnd> to the final /dev/shm/<name>;
        // both ends are shm-backed host files under /tmp, so link them directly (the jail branch below would
        // resolve them into the empty <rootfs>/dev/shm and ENOENT).
        char lob[300], lnb[300];
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
        char rob[300], rnb[300];
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
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : 0;
            break;
        }
        char ob[4200], nb[4200];
        const char *op = atpath((int)a0, (const char *)a1, ob, sizeof ob, 0);
        const char *np = atpath((int)a2, (const char *)a3, nb, sizeof nb, 0);
        G_RET(c) = renameatx_np(ATFD(a0), op, ATFD(a2), np, rxflags) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 40:
    case 39:
    // mount / umount2 / pivot_root -> ok
    case 41: G_RET(c) = 0; break;
    case 43:
    case 44: {
        // statfs(path,buf)/fstatfs(fd,buf): wrap the host call, then TRANSLATE the macOS struct statfs
        // into the Linux struct statfs layout (all 8-byte fields on 64-bit; f_fsid is two 32-bit words).
        struct statfs hs;
        int r;
        if (nr == 43) {
            char pb[4200];
            const char *p = atpath(-100, (const char *)a0, pb, sizeof pb, 0);
            r = statfs(p, &hs);
        } else {
            r = fstatfs((int)a0, &hs);
        }
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        uint8_t *b = (uint8_t *)a1;
        if (!host_range_mapped((uintptr_t)a1, 120)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
        memset(b, 0, 120);
        *(int64_t *)(b + 0) = 0x01021994;              // f_type (TMPFS_MAGIC; geometry is what matters)
        *(int64_t *)(b + 8) = (int64_t)hs.f_bsize;     // f_bsize
        *(uint64_t *)(b + 16) = (uint64_t)hs.f_blocks; // f_blocks
        *(uint64_t *)(b + 24) = (uint64_t)hs.f_bfree;  // f_bfree
        *(uint64_t *)(b + 32) = (uint64_t)hs.f_bavail; // f_bavail
        *(uint64_t *)(b + 40) = (uint64_t)hs.f_files;  // f_files
        *(uint64_t *)(b + 48) = (uint64_t)hs.f_ffree;  // f_ffree
        *(int32_t *)(b + 56) = hs.f_fsid.val[0];       // f_fsid[0]
        *(int32_t *)(b + 60) = hs.f_fsid.val[1];       // f_fsid[1]
        *(int64_t *)(b + 64) = 255;                    // f_namelen (NAME_MAX)
        *(int64_t *)(b + 72) = (int64_t)hs.f_bsize;    // f_frsize
        *(int64_t *)(b + 80) = 0;                      // f_flags
        G_RET(c) = 0;
        break;
    }
    case 46: {
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
        // fallocate(fd,mode,offset,len). FALLOC_FL_PUNCH_HOLE(2)|KEEP_SIZE(1): deallocate+zero a range
        // via macOS F_PUNCHHOLE (file stays the same size, the range reads as zeros).
        memf_materialize((int)a0); // rare on scratch: flush RAM cache, then use the host fallocate path
        int mode = (int)a1;
        off_t off = (off_t)a2, len = (off_t)a3;
        if (mode & 2) {
#ifdef F_PUNCHHOLE
            struct fpunchhole fph;
            memset(&fph, 0, sizeof fph);
            fph.fp_offset = off;
            fph.fp_length = len;
            int r = fcntl((int)a0, F_PUNCHHOLE, &fph);
            fd_evict((int)a0);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
#else
            G_RET(c) = (uint64_t)(-EOPNOTSUPP);
#endif
            break;
        }
        struct stat s;
        // plain fallocate: extend (no shrink)
        off_t end = off + len;
        if (fstat((int)a0, &s) == 0 && s.st_size < end && ftruncate((int)a0, end) < 0) {}
        fd_evict((int)a0);
        G_RET(c) = 0;
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
        if (r == 0 && (int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0]) fc_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 53:
    // fchmodat(dirfd,path,mode,flags) / fchmodat2
    case 452: {
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
            // BUG #181: the host chown is a rootless no-op; persist the guest-set owner as an xattr on
            // the backing file so a later stat reports it (not the #156 cuid/cgid default). -1 = keep.
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
        fchown((int)a0, (uid_t)a1, (gid_t)a2);
        chown_xattr_set_fd((int)a0, (int)(int32_t)(uint32_t)a1, (int)(int32_t)(uint32_t)a2);
        // the guest-owner xattr just changed -> drop this path's cached stat so a later stat reports it
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_fdpath[(int)a0][0]) fc_evict_path(g_fdpath[(int)a0]);
        G_RET(c) = 0;
        break;
        // fchown(fd,uid,gid) -- best-effort
    }
    // openat2(dirfd, path, open_how*, size): unpack open_how { u64 flags; u64 mode; u64 resolve; } into
    // the openat arg positions, then share the full openat path (O_* xlate, overlay, jail). The RESOLVE_*
    // restriction flags are advisory here -- the rootfs jail already confines every resolution.
    case 437: {
        uint64_t *how = (uint64_t *)a2;
        a2 = how ? how[0] : 0; // open_how.flags -> openat flags
        a3 = how ? how[1] : 0; // open_how.mode  -> openat mode
    } /* fall through to openat */
    case 56: {
        // openat -- Linux O_* -> macOS O_* (they differ!)
        int lf = (int)a2, mf = lf & 0x3;
        // O_PATH (Linux 0x200000, arch-independent): the fd only NAMES the file -- fstat / *at dirfd /
        // fchdir work through it, but read/write are rejected EBADF. macOS has no O_PATH, so we open a
        // normal read fd (O_RDONLY, +O_DIRECTORY for a dir) for the metadata ops and record the flag so the
        // I/O family (svc_io) returns EBADF. Marked on every open-success path below.
        int is_opath = (lf & 0x200000) != 0;
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
            if (fd >= 0 && fd < 1024) {
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
            // materialization below (proc_dir_try_open) serves it and tags the fd's guest path (#370).
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
            // /sys/class/net (#289): interface introspection. Directory opens (the class dir + per-iface
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
            }
            // device nodes -> host devices (rootfs has no real /dev)
            if (rp && !strncmp(rp, "/dev/", 5)) {
                const char *hd = dev_node_hostpath(rp);
                if (hd) {
                    int d = open(hd, mf);
                    // /dev/full is backed by /dev/zero for reads; flag the fd so its writes fail ENOSPC.
                    if (d >= 0 && d < 1024) g_devfull[d] = !strcmp(rp, "/dev/full");
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
        // #255: when a runtime-dropped process (gosu postgres) O_CREATs a file, the new inode must be
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
            char hp[300];
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
            if (rp && !strcmp(rp, "/dev/ptmx")) {
                int m = posix_openpt(O_RDWR | O_NOCTTY);
                if (m >= 0) {
                    grantpt(m);
                    unlockpt(m);
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
            if (rp && !strncmp(rp, "/dev/pts/", 9) && rp[9] >= '0' && rp[9] <= '9') {
                char *sn = ptsname(atoi(rp + 9));
                if (!sn) {
                    G_RET(c) = (uint64_t)(int64_t)(-2);
                    break;
                    // ENOENT
                }
                int s = open(sn, mf);
                G_RET(c) = s < 0 ? (uint64_t)(-errno) : (uint64_t)s;
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
            // #255: after copy-up, `host` (the upper path) exists iff the file was already present in the
            // overlay -> a missing upper means this open will CREATE it fresh; stamp its owner post-open.
            int nf_new = nf_want && access(host, F_OK) != 0;
            // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (host table larger).
            int r = nofile_gate(open(host, mf | ((lf & G_O_NOFOLLOW) ? O_NOFOLLOW : 0), (mode_t)a3));
            if (r >= 0 && nf_new) newfile_stamp_fd(r);
            if (r >= 0 && r < 1024) g_opath[r] = is_opath;
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
                // keyblock (the first key) was re-read -> BADSIG on apt-get update over a layered image (#391).
                char gdir[4200];
                if (have_canon) guest_from_host(gpa, gdir, sizeof gdir);
                else snprintf(gdir, sizeof gdir, "%s", gp);
                struct stat dst;
                if (r < 1024 && !jail_is_vol(gdir) && fstat(r, &dst) == 0 && S_ISDIR(dst.st_mode))
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
            // ALSO exclude O_NOFOLLOW (#372): the cache stores the CANONICAL (symlink-followed) host
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
                if (r >= 0 && r < 1024) g_opath[r] = is_opath;
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
            // #255: probe pre-existence (relative to the resolved parent) so we stamp ONLY a fresh create.
            int nf_new = nf_want && faccessat(pfd, fin, F_OK, AT_SYMLINK_NOFOLLOW) != 0;
            int r = openat(pfd, fin, mf | O_NOFOLLOW, (mode_t)a3);
            int e = errno;
            close(pfd);
            r = nofile_gate(r); // fd past the guest's soft RLIMIT_NOFILE -> EMFILE (host table is far larger)
            if (r < 0 && errno == EMFILE) e = EMFILE;
            if (r >= 0 && nf_new) newfile_stamp_fd(r);
            if (r >= 0 && r < 1024) g_opath[r] = is_opath;
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
        const char *p = atpath((int)a0, (const char *)a1, pb, sizeof pb, 0);
        int nf_new = nf_want && faccessat(ATFD(a0), p, F_OK, AT_SYMLINK_NOFOLLOW) != 0; // #255: stamp only fresh
        // Gate the new fd against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the shared host fd
        // table is far larger; engine-private fds are hoisted above 1<<20, so the guest limit is emulated).
        int r = nofile_gate(openat(ATFD(a0), p, mf, (mode_t)a3));
        if (r >= 0 && nf_new) newfile_stamp_fd(r);
        if (r >= 0 && r < 1024) g_opath[r] = is_opath;
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
        // still open, so a blocked peer recv wakes and returns 0. Shared with the execve CLOEXEC sweep (#282).
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
        if (g_nlower && fd >= 0 && fd < 1024 && g_ovldir[fd][0]) {
            // snapshot cache is indexed directly by guest fd (no slot table -> no eviction thrash, #199)
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
                *(uint64_t *)(ld + 0) = g_ovldents[fd].pos + 1;
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
        // Match every /proc magic link on the GUEST-ABSOLUTE path, so readlink("/proc/self/exe"),
        // readlinkat(AT_FDCWD, "proc/self/exe") from "/", and readlinkat(pid_dirfd, "exe") agree
        // byte-exactly (#317/#370). Paths that don't land in /proc (or /dev/fd) keep the raw pointer,
        // so the real-resolution fallback below is byte-identical for ordinary symlinks.
        char gpb[4200];
        const char *gp = p;
        if (p) {
            guest_abspath_at((int)a0, p, gpb, sizeof gpb);
            if (!strcmp(gpb, "/proc") || !strncmp(gpb, "/proc/", 6) || !strncmp(gpb, "/dev/fd/", 8) ||
                !strncmp(gpb, "/dev/std", 8))
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
                else if (pfn >= 0 && pfn < 1024 && g_eventfd_peer[pfn])
                    sl = snprintf(syn, sizeof syn, "anon_inode:[eventfd]");
                else if (pfn >= 0 && pfn < 1024 && g_timerfd[pfn])
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
                if (!strcmp(leaf, "cwd"))
                    tgt = (!g_rootfs && getcwd(cwb, sizeof cwb)) ? cwb : (g_cwd[0] ? g_cwd : "/");
                size_t l = strlen(tgt);
                if (l > bs) l = bs;
                memcpy(buf, tgt, l);
                G_RET(c) = (uint64_t)l;
                break;
            }
            // /proc/[self|pid]/ns/<name> -> "<name>:[<inode>]" namespace links (nsenter/iproute2 read these;
            // the inode constants are the kernel's initial-namespace values -- stable and plausible).
            if (leaf && !strncmp(leaf, "ns/", 3) && leaf[3]) {
                static const struct { const char *nm; unsigned ino; } NS[] = {
                    {"cgroup", 4026531835u}, {"ipc", 4026531839u},  {"mnt", 4026531841u},
                    {"net", 4026531840u},    {"pid", 4026531836u},  {"pid_for_children", 4026531836u},
                    {"time", 4026531834u},   {"time_for_children", 4026531834u},
                    {"user", 4026531837u},   {"uts", 4026531838u},  {0, 0}};
                int nsdone = 0;
                for (int i = 0; NS[i].nm; i++)
                    if (!strcmp(leaf + 3, NS[i].nm)) {
                        char nsb[64];
                        int nl = snprintf(nsb, sizeof nsb, "%s:[%u]", NS[i].nm, NS[i].ino);
                        size_t l = (size_t)nl > bs ? bs : (size_t)nl;
                        memcpy(buf, nsb, l);
                        G_RET(c) = (uint64_t)l;
                        nsdone = 1;
                        break;
                    }
                if (nsdone) break;
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
            // "no such path" but EINVAL as "ordinary component" (#370 completeness).
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
            // #317: a result atpath left RELATIVE (bare mode, no rootfs) must resolve against the CALLER's
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
        const char *raw = (const char *)a1, *p = atpath((int)a0, raw, pb, sizeof pb, (a3 & 0x100) ? 1 : 0);
        {
            const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
            // A dirfd-RELATIVE name (fstatat(pid_dirfd, "exe")) that lands in /proc must hit the same
            // magic-link synthesis as its absolute spelling (#317 consistency; bare mode included, where
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
                // the engine fills it directly) -> a bad pointer is -EFAULT, matching Linux's copyout (#395).
                if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
                    // engine fills it -> a bad pointer is -EFAULT, matching Linux's copyout ordering (#395).
                    if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
            // non-synth path falls through to the generic handler below with Linux's normal ordering (#395).
            {
                struct stat synth_s;
                if (synth_stat_raw(gp, &synth_s)) {
                    if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
                if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
        // guest-chown xattr lives on the host backing file: read via fd for AT_EMPTY_PATH, else by path.
        // The stat succeeded above, so validate the guest buffer here (copyout-last) -> bad ptr = -EFAULT.
        if (!host_range_mapped((uintptr_t)a2, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
        // The guest stat buffer is filled DIRECTLY by the engine; validate it (after the fd/stat succeeds,
        // so a bad fd still reports EBADF first, matching Linux's copyout-last ordering) so a bad pointer
        // returns -EFAULT instead of faulting the engine (#395, access_ok).
        if (!host_range_mapped((uintptr_t)a1, GUEST_LINUX_STAT_BYTES)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
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
        struct timespec *ts = (struct timespec *)a2;
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
        // Validate the guest pointers before any deref: a bad path or result buffer must return -EFAULT,
        // not fault the engine (guest memory is identity-mapped host memory). atpath/raw[0] read the path;
        // the 256-byte struct statx is written to a4 below.
        if (raw && !host_addr_mapped((uintptr_t)raw)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
        if (!host_range_mapped((uintptr_t)a4, 256)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
        const char *p = atpath((int)a0, raw, pb, sizeof pb, nofollow);
        int rc, empty = (raw && !raw[0] && (a2 & 0x1000));
        const char *gp = (g_rootfs && !strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) ? p + g_rootfs_canon_len : p;
        // Track the host backing file so ownership virtualization reads the SAME guest-chown xattr that
        // fstat/newfstatat do (#383): xpath = the host path we stat'd, or xfd = the fd for AT_EMPTY_PATH;
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
            int rr = esf                            ? memf_fstat((int)a0, &s)
                     : empty                        ? fstat((int)a0, &s)
                                                    : fstatat(ATFD(a0), p, &s, nofollow ? AT_SYMLINK_NOFOLLOW : 0);
            rc = rr < 0 ? -errno : 0;
            if (rc == 0) {
                if (empty) xfd = (int)a0; // AT_EMPTY_PATH: xattr lives on the fd's backing file
                else xpath = p;
            }
        }
        if (rc < 0) {
            G_RET(c) = (uint64_t)(int64_t)rc;
            break;
        }
        // Route ownership through the SHARED virtualization (cuid/cgid default + #181 guest-chown xattr via
        // the #382 cache) so statx's uid/gid are byte-identical to fstat/newfstatat for the same file.
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
        int empty = (a4 & 0x1000);     // AT_EMPTY_PATH
        int nofollow = !(a4 & 0x400);  // default: don't dereference the final symlink (AT_SYMLINK_FOLLOW=0x400)
        struct stat s;
        char pb[4200];
        int rr;
        if (empty && memf_get((int)a0)) rr = memf_fstat((int)a0, &s);
        else if (empty) rr = fstat((int)a0, &s);
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
        if (!host_range_mapped((uintptr_t)a2, need + 8)) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
        uint64_t dev = (uint64_t)s.st_dev, ino = (uint64_t)s.st_ino;
        *(uint32_t *)(fh + 0) = need; // handle_bytes
        *(int32_t *)(fh + 4) = 1;     // handle_type (stable, arbitrary)
        memcpy(fh + 8, &dev, 8);
        memcpy(fh + 16, &ino, 8);
        if (a3) {
            if (!host_range_mapped((uintptr_t)a3, sizeof(int))) { G_RET(c) = (uint64_t)(int64_t)(-EFAULT); break; }
            *(int *)a3 = (int)s.st_dev; // mount_id
        }
        G_RET(c) = 0;
        break;
    }
    // faccessat2(dirfd,path,mode,flags) -- glibc access() uses it; same path/confinement, flags ignored
    case 439:
    case 48: {
        char pb[4200];
        // /proc/[self|pid]/exe magic symlink -> access the actual executable (matched on the
        // guest-absolute path so dirfd-relative and cwd-relative spellings work too, #317/#370)
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
    default:
        return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
