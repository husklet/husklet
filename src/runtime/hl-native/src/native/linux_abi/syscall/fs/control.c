#include "../../host_errno.h"

static int ioctl_terminal_request(struct cpu *c, int fd, unsigned long rq, void *arg, uint64_t a2, int tfd,
                                  int is_master) {
    switch (rq) {
    case 0x5401: {
        struct termios t;
        // TCGETS
        if (is_master && g_ptm_tset[fd]) {
            // A master's termios is engine state, so the guest's own image answers exactly.
            memcpy(arg, g_ptm_image[fd], 36);
            G_RET(c) = 0;
            break;
        }
        if (tcgetattr(tfd, &t) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        termios_m2l(&t, (uint8_t *)arg);
        terminal_termios_apply_recall(tfd, (uint8_t *)arg);
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
        int r = tcsetattr(tfd, act, &t); // push live to any open real slave (best effort on a master)
        if (is_master) {
            g_ptm_term[fd] = t;
            memcpy(g_ptm_image[fd], arg, 36);
            g_ptm_tset[fd] = 1;
            G_RET(c) = 0;
        } // master always accepts
        else {
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            if (r == 0) terminal_termios_observe_set(tfd, (const uint8_t *)arg);
        }
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
        // termios2's leading 36 bytes are the termios image, so the same guest-authored view applies.
        if (is_master && g_ptm_tset[fd])
            memcpy(arg, g_ptm_image[fd], 36);
        else
            terminal_termios_apply_recall(tfd, (uint8_t *)arg);
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
        int r = tcsetattr(tfd, act, &t);
        if (is_master) {
            g_ptm_term[fd] = t;
            memcpy(g_ptm_image[fd], arg, 36);
            g_ptm_tset[fd] = 1;
            G_RET(c) = 0;
        } else {
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            if (r == 0) terminal_termios_observe_set(tfd, (const uint8_t *)arg);
        }
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
    // TCSBRK -- tcdrain (arg != 0) / tcsendbreak (arg == 0). The third argument is passed by value, not
    // through a pointer, so it arrives in a2 directly. A master retargets to its transient slave above.
    case 0x5409: {
        int r = (int)a2 == 0 ? tcsendbreak(tfd, 0) : tcdrain(tfd);
        G_RET(c) = is_master ? 0 : (r < 0 ? (uint64_t)(-errno) : 0);
        break;
    }
    // TCSBRKP -- always send a break of the given duration (by-value arg).
    case 0x5425: {
        int r = tcsendbreak(tfd, (int)a2);
        G_RET(c) = is_master ? 0 : (r < 0 ? (uint64_t)(-errno) : 0);
        break;
    }
    // TCFLSH -- discard queued I/O. Linux queue selector (0=TCIFLUSH,1=TCOFLUSH,2=TCIOFLUSH) maps to the
    // host's symbolic tcflush() constants (their raw values differ on macOS).
    case 0x540b: {
        int q = (int)a2;
        int hq = q == 0 ? TCIFLUSH : q == 1 ? TCOFLUSH : TCIOFLUSH;
        int r = tcflush(tfd, hq);
        G_RET(c) = is_master ? 0 : (r < 0 ? (uint64_t)(-errno) : 0);
        break;
    }
    // TCXONC -- suspend/restart transmission (tcflow). Linux action (0=TCOOFF..3=TCION) maps to the host's
    // symbolic tcflow() constants.
    case 0x540a: {
        int act = (int)a2;
        int hact = act == 0 ? TCOOFF : act == 1 ? TCOON : act == 2 ? TCIOFF : TCION;
        int r = tcflow(tfd, hact);
        G_RET(c) = is_master ? 0 : (r < 0 ? (uint64_t)(-errno) : 0);
        break;
    }
    default: return 0;
    }
    return 1;
}

static void ioctl_descriptor_request(struct cpu *c, int fd, unsigned long rq, void *arg, uint64_t a2) {
    switch (rq) {
    case 0x80045430: {
        // TIOCGPTN -> the Linux devpts index N hl assigned this master at /dev/ptmx-open time (ptsname(3)
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
    // FIONREAD (SIOCINQ): bytes available to read. A guest AF_INET/AF_INET6 STREAM socket
    // (g_sock_stream) is backed by an AF_UNIX socket on the private loopback/bridge (see lo_swap).
    // The host AF_UNIX SIOCINQ does NOT subtract the bytes already consumed from the partially-read
    // head skb (a kernel quirk), so after a short read it over-reports -- a guest that expects TCP
    // SIOCINQ (nginx, redis, anything polling queued bytes) gets the wrong count. Recompute the true
    // readable count with a non-consuming MSG_PEEK bounded by SO_RCVBUF; this is also exactly correct
    // for a genuinely-AF_INET stream socket, so it applies uniformly to guest INET streams.
    case 0x541b:
        if (arg && fd >= 0 && fd < HL_NFD && g_sock_stream[fd]) {
            int rcvbuf = 0;
            socklen_t rl = sizeof rcvbuf;
            if (getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, &rl) == 0 && rcvbuf > 0) {
                if (rcvbuf > (16 << 20)) rcvbuf = 16 << 20; // cap the transient peek buffer at 16MB
                char *scratch = malloc((size_t)rcvbuf);
                if (scratch) {
                    ssize_t pk = recv(fd, scratch, (size_t)rcvbuf, MSG_PEEK | MSG_DONTWAIT);
                    free(scratch);
                    if (pk >= 0) {
                        *(int *)arg = (int)pk;
                        G_RET(c) = 0;
                        break;
                    }
                    if (HL_HOST_ERRNO_WOULD_BLOCK(errno)) {
                        *(int *)arg = 0; // nothing queued (no readable bytes) -> 0, like TCP SIOCINQ
                        G_RET(c) = 0;
                        break;
                    }
                    // Any other error (ENOTCONN on a listening socket, etc.): fall back to the host ioctl.
                }
            }
        }
        G_RET(c) = ioctl(fd, FIONREAD, arg) < 0 ? (uint64_t)(-errno) : 0;
        break;
    // SIOCOUTQ (TIOCOUTQ, 0x5411): bytes still queued in the send buffer. Linux answers it on any TCP
    // socket, but a guest INET stream socket backed by the AF_UNIX switch has its host ioctl rejected
    // as ENOTTY. Forward to the host (a real AF_UNIX/AF_INET socket does answer it); if the host still
    // rejects it for a tracked stream socket, report 0 (a drained switch socket holds no unsent bytes).
    case 0x5411:
        if (fd >= 0 && fd < HL_NFD && g_sock_stream[fd]) {
#if defined(__linux__)
            if (ioctl(fd, 0x5411, arg) == 0) {
                G_RET(c) = 0;
                break;
            }
#endif
            if (arg) *(int *)arg = 0;
            G_RET(c) = 0;
            break;
        }
        G_RET(c) = ioctl(fd, 0x5411, arg) < 0 ? (uint64_t)(-errno) : 0;
        break;
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
        int guest_fg;
        if (hl_linux_pidmap_guest_checked(&g_pgidmap, (int)fg, &guest_fg) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
            break;
        }
        if (!hl_linux_pidmap_is_active(&g_pgidmap) && g_init_hostpid && fg == g_init_hostpid) guest_fg = 1;
        if (arg) *(int *)arg = guest_fg;
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
        int host_pg;
        if (pg > 0 && hl_linux_pidmap_host_checked(&g_pgidmap, (int)pg, &host_pg) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (pg > 0) pg = (pid_t)host_pg;
        if (!hl_linux_pidmap_is_active(&g_pgidmap) && pg == 1 && g_init_hostpid) pg = g_init_hostpid;
        if (isatty(fd) && pg > 0) {
            // A pipeline leader calls tcsetpgrp while still in a background group (the parent shell sets
            // the foreground group concurrently); without blocking SIGTTOU here the host kernel would
            // STOP it mid-handoff -> the foreground command freezes ("[1]+ Stopped"). Block SIGTTOU so
            // the real tcsetpgrp installs the fg group cleanly (kernel still routes ^C/^Z afterwards).
            sigset_t sv;
            if (tty_ctl_block(&sv) != 0) {
                G_RET(c) = (uint64_t)(int64_t)(-errno);
                break;
            }
            int control_result = tcsetpgrp(fd, pg);
            int control_error = control_result == 0 ? 0 : errno;
            if (tty_ctl_restore(&sv) != 0 && control_result == 0) {
                G_RET(c) = (uint64_t)(int64_t)(-errno);
                break;
            }
            if (control_result != 0) {
                G_RET(c) = (uint64_t)(int64_t)(-control_error);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // TIOCSCTTY -- acquire the controlling terminal for real when `fd` is a tty (best effort; the
    // login_tty in the daemon usually already did this for the session leader), then report success so
    // an interactive shell's job-control setup never warns.
    case 0x540e:
        if (isatty(fd)) (void)ioctl(fd, TIOCSCTTY, 0);
        G_RET(c) = 0;
        break;
    // TIOCNOTTY -- drop the controlling terminal. A guest daemonizing without setsid (or a session
    // leader detaching the tty) issues it on its ctty fd; the guest task is a real host process whose
    // ctty is the real host pty, so forward to the real fd. The host kernel drops the binding (and, for
    // a session leader, delivers SIGHUP+SIGCONT to the old foreground group), after which /dev/tty
    // faithfully surfaces ENXIO. Without this the guest kept a phantom ctty and /dev/tty stayed open.
    case 0x5422:
#ifdef TIOCNOTTY
        if (!isatty(fd)) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
            break;
        }
        G_RET(c) = ioctl(fd, TIOCNOTTY, 0) < 0 ? (uint64_t)(int64_t)(-errno) : 0;
#else
        G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
#endif
        break;
    // TIOCGSID -- session id of the terminal (tcgetsid(3) drives this). The guest's children are real
    // host processes in the engine's session, so the kernel's own tty->session binding is authoritative:
    // forward to the real fd and translate only the INIT's identity (its real host session id -> guest 1),
    // matching the TIOCGPGRP virtualization above so getsid(0) and TIOCGSID agree. A non-tty / non-ctty
    // fd faithfully surfaces the host ENOTTY.
    case 0x5429: {
#ifdef TIOCGSID
        pid_t sid = 0;
        if (ioctl(fd, TIOCGSID, &sid) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        int guest_sid;
        if (hl_linux_pidmap_guest_checked(&g_sidmap, (int)sid, &guest_sid) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOTTY);
            break;
        }
        if (!hl_linux_pidmap_is_active(&g_sidmap) && g_init_hostpid && sid == g_init_hostpid) guest_sid = 1;
        if (arg) *(int *)arg = guest_sid;
        G_RET(c) = 0;
#else
        G_RET(c) = (uint64_t)(-25); // ENOTTY (host lacks TIOCGSID, e.g. Darwin build)
#endif
        break;
    }
    // TIOCPKT -- packet mode on a pty master (script(1), expect, sshd, tmux use it to observe slave-side
    // flush/flow/termios changes). The master is a real host pty fd, so the kernel frames the control byte
    // into subsequent master reads once enabled; just forward the enable/disable to the real fd.
    case 0x5420:
#ifdef TIOCPKT
        G_RET(c) = ioctl(fd, TIOCPKT, arg) < 0 ? (uint64_t)(int64_t)(-errno) : 0;
#else
        G_RET(c) = (uint64_t)(-25); // ENOTTY (host lacks TIOCPKT, e.g. Darwin build)
#endif
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
}

static void svc_fs_control_29(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                              uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 29: {
        int fd = (int)a0;
        // Truncate the ioctl request to 32 bits (Linux `cmd` is unsigned int). musl declares ioctl's request
        // as `int`, so a read-direction request with the direction bit set (e.g. TIOCGPTN 0x80045430) arrives
        // SIGN-EXTENDED as 0xffffffff80045430 and would miss its switch case -> ENOTTY (glibc zero-extends, so
        // it worked there). This makes both forms match, fixing musl tmux/script/openpty and any high-bit ioctl.
        unsigned long rq = (uint32_t)a1;
        uint8_t ioctl_argument[256] = {0};
        uint8_t ioctl_nested[80] = {0};
        uint64_t ioctl_nested_guest = 0;
        size_t ioctl_nested_size = 0;
        size_t ioctl_size = (rq >> 16) & 0x3fffu;
        int ioctl_input = ((rq >> 30) & 1u) != 0;
        int ioctl_output = ((rq >> 31) & 1u) != 0;
        switch (rq) {
        case 0x5401:
            ioctl_size = 36;
            ioctl_output = 1;
            break;
        case 0x5402:
        case 0x5403:
        case 0x5404:
            ioctl_size = 36;
            ioctl_input = 1;
            break;
        case 0x5413:
            ioctl_size = sizeof(struct winsize);
            ioctl_output = 1;
            break;
        case 0x5414:
            ioctl_size = sizeof(struct winsize);
            ioctl_input = 1;
            break;
        case 0x5421:
        case 0x5410:
        case 0x5420:
            ioctl_size = sizeof(int);
            ioctl_input = 1;
            break;
        case 0x541b:
        case 0x5411:
        case 0x540f:
        case 0x5429:
            ioctl_size = sizeof(int);
            ioctl_output = 1;
            break;
        default:
            if (rq >= 0x8900 && rq <= 0x89ff) ioctl_size = 40, ioctl_input = ioctl_output = 1;
            break;
        }
        if (ioctl_size > sizeof ioctl_argument) {
            G_RET(c) = (uint64_t)(int64_t)(-E2BIG);
            break;
        }
        if (ioctl_input && ioctl_size && guest_copy_from(ioctl_argument, a2, ioctl_size) != (ssize_t)ioctl_size) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (rq == 0x8912 && ioctl_size >= 16) {
            int32_t capacity;
            memcpy(&capacity, ioctl_argument, sizeof capacity);
            memcpy(&ioctl_nested_guest, ioctl_argument + 8, sizeof ioctl_nested_guest);
            if (capacity > 0 && ioctl_nested_guest) {
                ioctl_nested_size = (size_t)capacity < sizeof ioctl_nested ? (size_t)capacity : sizeof ioctl_nested;
                if (guest_accessible_prefix(ioctl_nested_guest, ioctl_nested_size, HL_LOGICAL_VMA_WRITE) !=
                    ioctl_nested_size) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                uint64_t local_nested = (uint64_t)(uintptr_t)ioctl_nested;
                memcpy(ioctl_argument + 8, &local_nested, sizeof local_nested);
            }
        }
        void *arg = ioctl_size ? ioctl_argument : (void *)a2;
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
        // The GET/SET on a master answers from / writes to hl's per-master termios+winsize cache (g_ptm_*);
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
        case 0x5409:     // TCSBRK  (tcdrain / tcsendbreak)
        case 0x540a:     // TCXONC  (tcflow)
        case 0x540b:     // TCFLSH  (tcflush)
        case 0x5425:     // TCSBRKP (tcsendbreak with duration)
        {
            // "Is fd a pty master?" -- apt/dpkg StartPtyMagic does TIOCSWINSZ + tcsetattr(TCSANOW) on the
            // master WITHOUT ever opening the slave, and a macOS master ENOTTYs every termios/winsize ioctl,
            // so mis-detecting the master here is exactly ("Setting TIOCSWINSZ/TCSANOW for master fd N
            // failed"). Consult hl's AUTHORITATIVE devpts registry first (pts_fd_is_master, stamped at
            // /dev/ptmx-open time): it is the source of truth for every master hl handed out and costs no
            // host syscall. ptsname(fd) is kept as an independent confirmation AND to resolve the host
            // slave device so a SET can be live-pushed to a transient slave. A slave or ordinary tty is
            // neither (bit clear, ptsname==NULL) -> operate on fd directly, which is correct there. (Prior
            // code detected the master by ptsname ALONE; adding the registry makes detection authoritative
            // rather than dependent solely on a host heuristic, so a master hl tracks is recognized even if
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
        if (!ioctl_terminal_request(c, fd, rq, arg, a2, tfd, is_master)) ioctl_descriptor_request(c, fd, rq, arg, a2);
        if (pts_slave >= 0) close(pts_slave); // transient slave used to service a master's termios/winsize op
        if ((int64_t)G_RET(c) >= 0 && rq == 0x8912 && ioctl_nested_guest) {
            int32_t produced;
            memcpy(&produced, ioctl_argument, sizeof produced);
            size_t copied = produced <= 0                          ? 0
                            : (size_t)produced < ioctl_nested_size ? (size_t)produced
                                                                   : ioctl_nested_size;
            if (copied && guest_copy_to(ioctl_nested_guest, ioctl_nested, copied) != (ssize_t)copied)
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            memcpy(ioctl_argument + 8, &ioctl_nested_guest, sizeof ioctl_nested_guest);
        }
        if ((int64_t)G_RET(c) >= 0 && ioctl_output && ioctl_size &&
            guest_copy_to(a2, ioctl_argument, ioctl_size) != (ssize_t)ioctl_size)
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
        break;
    }
    default: break;
    }
}

static int svc_fs_control(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                          uint64_t a5) {
    switch (nr) {
    case 29: svc_fs_control_29(c, nr, a0, a1, a2, a3, a4, a5); return 1;
    default: return 0;
    }
}
