// dd/runtime/os/linux/container -- termios (Linux<->macOS) + NET-ns private loopback (127/8 -> AF_UNIX).

#include <netdb.h> // container DNS: getaddrinfo/getnameinfo via the macOS host resolver (dns_* below)

// ---- termios: Linux <-> macOS. Different field width (4 vs 8B flags), bit values, and c_cc order.
// Linux struct termios (TCGETS): c_iflag/oflag/cflag/lflag @0,4,8,12 (u32); c_line@16; c_cc[19]@17.
static const uint32_t TIO_I[][2] = {{0x1, IGNBRK},  {0x2, BRKINT},   {0x4, IGNPAR},    {0x8, PARMRK},  {0x10, INPCK},
                                    {0x20, ISTRIP}, {0x40, INLCR},   {0x80, IGNCR},    {0x100, ICRNL}, {0x400, IXON},
                                    {0x800, IXANY}, {0x1000, IXOFF}, {0x2000, IMAXBEL}};
static const uint32_t TIO_O[][2] = {{0x1, OPOST}, {0x4, ONLCR}, {0x8, OCRNL}, {0x10, ONOCR}, {0x20, ONLRET}};
static const uint32_t TIO_C[][2] = {{0x40, CSTOPB},  {0x80, CREAD},  {0x100, PARENB},
                                    {0x200, PARODD}, {0x400, HUPCL}, {0x800, CLOCAL}};
static const uint32_t TIO_L[][2] = {{0x1, ISIG},    {0x2, ICANON},  {0x8, ECHO},     {0x10, ECHOE},   {0x20, ECHOK},
                                    {0x40, ECHONL}, {0x80, NOFLSH}, {0x100, TOSTOP}, {0x8000, IEXTEN}};
static const int CC_L2M[17] = {VINTR, VQUIT, VERASE, VKILL, VEOF, VTIME, VMIN, -1, VSTART,
                               // Linux c_cc index -> macOS index
                               VSTOP, VSUSP, VEOL, VREPRINT, VDISCARD, VWERASE, VLNEXT, VEOL2};

// bind()/connect() an AF_UNIX socket at host path `host`. macOS sun_path is only 104 bytes, but a container
// overlay upper socket path ($HOME/.dd/containers/<64-hex>/upper/.../.s.PGSQL.5432) can exceed that -- a
// plain snprintf into sun_path SILENTLY TRUNCATES, so bind creates the inode at the wrong (short) path and
// the guest's later stat/chmod/connect (which resolve the FULL path) ENOENT it. When `host` fits, bind/
// connect directly (byte-identical to before). When it overflows, split dir/base, fchdir into the parent,
// and operate on the SHORT basename (.s.PGSQL.5432, mysqld.sock, ...) so the inode lands at -- and is dialed
// from -- exactly the full path the overlay resolver produces. `connecting`: 0 = bind, 1 = connect.
static int unix_sock_at(int fd, const char *host, int connecting) {
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    if (strlen(host) < sizeof un.sun_path) {
        snprintf(un.sun_path, sizeof un.sun_path, "%s", host);
        return connecting ? connect(fd, (struct sockaddr *)&un, sizeof un)
                          : bind(fd, (struct sockaddr *)&un, sizeof un);
    }
    char dir[1024];
    snprintf(dir, sizeof dir, "%s", host);
    char *sl = strrchr(dir, '/');
    if (!sl || !sl[1] || strlen(sl + 1) >= sizeof un.sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    snprintf(un.sun_path, sizeof un.sun_path, "%s", sl + 1);
    *sl = 0;
    int pfd = open(dir[0] ? dir : "/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (pfd < 0) return -1;
    int cwd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (cwd < 0) {
        close(pfd);
        return -1;
    }
    int rc = -1, e = 0;
    if (fchdir(pfd) == 0) {
        rc = connecting ? connect(fd, (struct sockaddr *)&un, sizeof un) : bind(fd, (struct sockaddr *)&un, sizeof un);
        e = errno;
        fchdir(cwd);
    } else {
        e = errno;
    }
    close(cwd);
    close(pfd);
    errno = e;
    return rc;
}

// AF_UNIX DATAGRAM send to a host pathname `host`, path-length safe (fchdir-shortens past macOS's 104-byte
// sun_path, exactly like unix_sock_at above). Used by sendto/sendmsg when a container's datagram dest is an
// AF_UNIX PATHNAME (e.g. syslog to /dev/log): the socket inode lives at the overlay-resolved host path, which
// a plain sockaddr_un would truncate. `mh` carries the payload iov/control; we only own msg_name. Returns
// bytes sent (>=0) or -1 with errno.
static int64_t unix_dgram_sendmsg_at(int fd, const char *host, struct msghdr *mh, int flags) {
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    if (strlen(host) < sizeof un.sun_path) {
        snprintf(un.sun_path, sizeof un.sun_path, "%s", host);
        mh->msg_name = &un;
        mh->msg_namelen = sizeof un;
        return sendmsg(fd, mh, flags);
    }
    char dir[1024];
    snprintf(dir, sizeof dir, "%s", host);
    char *sl = strrchr(dir, '/');
    if (!sl || !sl[1] || strlen(sl + 1) >= sizeof un.sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    snprintf(un.sun_path, sizeof un.sun_path, "%s", sl + 1);
    *sl = 0;
    int pfd = open(dir[0] ? dir : "/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (pfd < 0) return -1;
    int cwd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (cwd < 0) {
        close(pfd);
        return -1;
    }
    int64_t rc = -1;
    int e = 0;
    if (fchdir(pfd) == 0) {
        mh->msg_name = &un;
        mh->msg_namelen = sizeof un;
        rc = sendmsg(fd, mh, flags);
        e = errno;
        fchdir(cwd);
    } else {
        e = errno;
    }
    close(cwd);
    close(pfd);
    errno = e;
    return rc;
}

static uint32_t map_bits(uint32_t v, const uint32_t t[][2], int n, int fwd) {
    uint32_t o = 0;
    for (int i = 0; i < n; i++) {
        if (fwd) {
            if (v & t[i][0]) o |= t[i][1];
        } else {
            if (v & t[i][1]) o |= t[i][0];
        }
    }
    return o;
}

static void termios_l2m(const uint8_t *L, struct termios *M) {
    memset(M, 0, sizeof *M);
    uint32_t li = *(uint32_t *)(L + 0), lo = *(uint32_t *)(L + 4), lc = *(uint32_t *)(L + 8),
             ll = *(uint32_t *)(L + 12);
    M->c_iflag = map_bits(li, TIO_I, 13, 1);
    M->c_oflag = map_bits(lo, TIO_O, 5, 1);
    M->c_cflag = map_bits(lc, TIO_C, 6, 1);
    M->c_lflag = map_bits(ll, TIO_L, 9, 1);
    int csz = lc & 0x30;
    M->c_cflag |= (csz == 0x30 ? CS8 : csz == 0x20 ? CS7 : csz == 0x10 ? CS6 : CS5);
    const uint8_t *lcc = L + 17;
    for (int i = 0; i < 17; i++)
        if (CC_L2M[i] >= 0) M->c_cc[CC_L2M[i]] = lcc[i];
}

static void termios_m2l(const struct termios *M, uint8_t *L) {
    memset(L, 0, 36);
    uint32_t li = map_bits((uint32_t)M->c_iflag, TIO_I, 13, 0), lo = map_bits((uint32_t)M->c_oflag, TIO_O, 5, 0);
    uint32_t lc = map_bits((uint32_t)M->c_cflag, TIO_C, 6, 0), ll = map_bits((uint32_t)M->c_lflag, TIO_L, 9, 0);
    int csz = M->c_cflag & CSIZE;
    lc |= (csz == CS8 ? 0x30 : csz == CS7 ? 0x20 : csz == CS6 ? 0x10 : 0);
    *(uint32_t *)(L + 0) = li;
    *(uint32_t *)(L + 4) = lo;
    *(uint32_t *)(L + 8) = lc;
    *(uint32_t *)(L + 12) = ll;
    uint8_t *lcc = L + 17;
    for (int i = 0; i < 17; i++)
        if (CC_L2M[i] >= 0) lcc[i] = M->c_cc[CC_L2M[i]];
}

// Linux MSG_* -> macOS MSG_* (they differ for TRUNC/DONTWAIT/EOR/WAITALL).
static int msgflags_l2m(int lf) {
    // OOB/PEEK/DONTROUTE identical
    int mf = lf & (0x1 | 0x2 | 0x4);
    // MSG_TRUNC
    if (lf & 0x20) mf |= 0x10;
    // MSG_DONTWAIT
    if (lf & 0x40) mf |= 0x80;
    // MSG_EOR
    if (lf & 0x80) mf |= 0x8;
    // MSG_WAITALL
    if (lf & 0x100) mf |= 0x40;
    return mf;
}

// macOS MSG_* -> Linux MSG_* (inverse of msgflags_l2m; used for recvmsg msg_flags writeback). The
// returned-flags set differs: notably MSG_CTRUNC is macOS 0x20 / Linux 0x8, MSG_TRUNC macOS 0x10 /
// Linux 0x20, MSG_EOR macOS 0x8 / Linux 0x80. OOB/DONTROUTE map straight through.
static int msgflags_m2l(int mf) {
    // MSG_OOB(0x1)/MSG_DONTROUTE(0x4) identical; MSG_PEEK isn't a returned flag but is harmless
    int lf = mf & (0x1 | 0x2 | 0x4);
    // MSG_TRUNC: macOS 0x10 -> Linux 0x20
    if (mf & 0x10) lf |= 0x20;
    // MSG_CTRUNC: macOS 0x20 -> Linux 0x8
    if (mf & 0x20) lf |= 0x8;
    // MSG_EOR: macOS 0x8 -> Linux 0x80
    if (mf & 0x8) lf |= 0x80;
    return lf;
}

// ---- SCM ancillary data: Linux<->macOS cmsg framing translation (SOL_SOCKET/SCM_RIGHTS fd passing).
// dd uses host fds directly as guest fds, so the fd integers in an SCM_RIGHTS payload need no remap --
// only the cmsg framing differs: Linux hdr=16B (8B len @0, int level @8, int type @12), 8-byte align,
// SOL_SOCKET=1; macOS hdr=12B (4B len @0, int level @4, int type @8), 4-byte align, SOL_SOCKET=0xffff.
#define LX_CMSG_ALIGN(n) (((n) + 7u) & ~(size_t)7u) // Linux: 8-byte align
#define LX_CMSGHDR 16u                              // Linux cmsg header: 8(len)+4(level)+4(type)
#define LX_SOL_SOCKET 1
#define DD_CMSG_EVENTFD_MAGIC 0xddefd001u

struct dd_cmsg_eventfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t slot;
    uint32_t sema;
    uint32_t nb; // guest EFD_NONBLOCK intent (g_eventfd_gnb) — the host fd is always O_NONBLOCK internally
};

static __thread int g_cmsg_tmpfds[1024];
static __thread int g_cmsg_ntmpfds;

static void cmsg_tmpfds_close(void) {
    for (int i = 0; i < g_cmsg_ntmpfds; i++)
        if (g_cmsg_tmpfds[i] >= 0) close(g_cmsg_tmpfds[i]);
    g_cmsg_ntmpfds = 0;
}

static int cmsg_level_l2m(int lv) {
    return lv == LX_SOL_SOCKET ? SOL_SOCKET : lv;
}

static int cmsg_level_m2l(int lv) {
    return lv == SOL_SOCKET ? LX_SOL_SOCKET : lv;
}

static int cmsg_eventfd_marker(const struct dd_cmsg_eventfd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.ddcmsgXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    g_cmsg_tmpfds[g_cmsg_ntmpfds++] = fd;
    return fd;
}

static int cmsg_fd_is_write_sideband(int fd) {
    if (fd < 0) return 0;
    int fl = fcntl(fd, F_GETFL);
    if (fl < 0) return 0;
    if ((fl & O_ACCMODE) != O_WRONLY) return 0;
    if (!(fl & O_NONBLOCK)) return 0;
    struct stat st;
    if (fstat(fd, &st) != 0) return 0;
    return S_ISFIFO(st.st_mode);
}

static int cmsg_read_eventfd_marker(int fd, struct dd_cmsg_eventfd_meta *m) {
    if (fd < 0 || !m) return 0;
    memset(m, 0, sizeof *m);
    if (pread(fd, m, sizeof *m, 0) != (ssize_t)sizeof *m) return 0;
    return m->magic == DD_CMSG_EVENTFD_MAGIC;
}


static int cmsg_import_eventfd_trailer(int *fds, int nfds) {
    if (!fds || nfds <= 2) return nfds;
    int cap = nfds / 3 + 1;
    int *hidden = calloc((size_t)cap, sizeof(int));
    int *marker_fd = calloc((size_t)cap, sizeof(int));
    struct dd_cmsg_eventfd_meta *metas = calloc((size_t)cap, sizeof(*metas));
    if (!hidden || !marker_fd || !metas) {
        free(hidden);
        free(marker_fd);
        free(metas);
        return nfds;
    }
    int nmeta = 0;
    int visible = nfds;
    while (visible >= 3 && nmeta < cap) {
        int h = fds[visible - 2];
        int marker = fds[visible - 1];
        struct dd_cmsg_eventfd_meta m;
        if (!cmsg_fd_is_write_sideband(h)) break;
        if (!cmsg_read_eventfd_marker(marker, &m)) break;
        hidden[nmeta] = h;
        marker_fd[nmeta] = marker;
        metas[nmeta] = m;
        nmeta++;
        visible -= 2;
    }
    if (!nmeta) {
        free(hidden);
        free(marker_fd);
        free(metas);
        return nfds;
    }
    for (int i = 0; i < nmeta; i++)
        if (metas[i].ordinal >= (uint32_t)visible) {
            free(hidden);
            free(marker_fd);
            free(metas);
            return nfds;
        }
    for (int i = 0; i < nmeta; i++) {
        int h = hidden[i];
        int marker = marker_fd[i];
        struct dd_cmsg_eventfd_meta *m = &metas[i];
        int pub = fds[m->ordinal];
        if (pub >= 0 && pub < DD_NFD) {
            g_eventfd_peer[pub] = h + 1;
            g_eventfd_cslot[pub] = (int)m->slot + 1;
            g_eventfd_sema[pub] = (uint8_t)(m->sema != 0);
            g_eventfd_gnb[pub] = (uint8_t)(m->nb != 0); // carry the guest blocking/non-blocking intent
            // The imported read end must be host-O_NONBLOCK too (internal drains rely on it); the sender set
            // the write-side, but ensure the received public fd is non-blocking regardless of its origin.
            {
                int fl = fcntl(pub, F_GETFL);
                if (fl >= 0 && !(fl & O_NONBLOCK)) fcntl(pub, F_SETFL, fl | O_NONBLOCK);
            }
        } else {
            close(h);
        }
        close(marker);
    }
    free(hidden);
    free(marker_fd);
    free(metas);
    return visible;
}

static void cmsg_note_recv_sock_fd(int fd);

// guest(Linux) control buf -> host(macOS) control buf. Returns host bytes written (<=cap), 0/none,
// or -1 with *errp set. A partial ancillary conversion must never be sent: silently dropping SCM_RIGHTS
// fds leaves higher-level protocols like Mojo with a successful data message but missing handles.
static ssize_t cmsg_l2m(const uint8_t *g, size_t glen, uint8_t *h, size_t cap, int *errp) {
    if (errp) *errp = 0;
    cmsg_tmpfds_close();
    size_t go = 0, ho = 0;
    while (go + LX_CMSGHDR <= glen) {
        uint64_t clen = *(const uint64_t *)(g + go); // Linux cmsg_len (8B)
        int lvl = *(const int *)(g + go + 8);
        int typ = *(const int *)(g + go + 12);
        if (clen < LX_CMSGHDR || go + clen > glen) {
            if (errp) *errp = EINVAL;
            return -1;
        }
        size_t dlen = (size_t)clen - LX_CMSGHDR;          // payload bytes (e.g. N*4 fds)
        int *combo = NULL;
        int combo_cap = 0;
        int combo_n = 0;
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS && dlen >= sizeof(int)) {
            const int *fds = (const int *)(g + go + LX_CMSGHDR);
            int nfds = (int)(dlen / sizeof(int));
            if (nfds > 253) {
                if (errp) *errp = EINVAL;
                return -1;
            }
            combo_cap = nfds * 3; // visible fd + possible eventfd write-side fd + marker fd
            combo = malloc((size_t)combo_cap * sizeof(int));
            if (!combo) {
                if (errp) *errp = ENOMEM;
                return -1;
            }
            for (int i = 0; i < nfds; i++) {
                combo[combo_n++] = fds[i];
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd < 0 || fd >= DD_NFD || !g_eventfd_peer[fd]) continue;
                if (combo_n + 2 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                int hidden = g_eventfd_peer[fd] - 1;
                int fl = fcntl(hidden, F_GETFL);
                if (fl >= 0) fcntl(hidden, F_SETFL, fl | O_NONBLOCK);
                fcntl(hidden, F_SETFD, FD_CLOEXEC);
                struct dd_cmsg_eventfd_meta m = {
                    .magic = DD_CMSG_EVENTFD_MAGIC,
                    .ordinal = (uint32_t)i,
                    .slot = (uint32_t)eventfd_counter_slot(fd),
                    .sema = (uint32_t)(g_eventfd_sema[fd] != 0),
                    .nb = (uint32_t)(g_eventfd_gnb[fd] != 0),
                };
                int marker = cmsg_eventfd_marker(&m);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = hidden;
                combo[combo_n++] = marker;
            }
            dlen = (size_t)combo_n * sizeof(int);
        }
        size_t need = CMSG_SPACE(dlen);
        if (ho + need > cap) {
            free(combo);
            if (errp) *errp = EMSGSIZE;
            return -1;
        }
        struct cmsghdr ch;
        memset(&ch, 0, sizeof ch);
        ch.cmsg_len = CMSG_LEN(dlen); // macOS 12+dlen
        ch.cmsg_level = cmsg_level_l2m(lvl);
        ch.cmsg_type = typ; // SCM_RIGHTS==1 on both
        memcpy(h + ho, &ch, sizeof ch);
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS && combo_n > 0)
            memcpy(CMSG_DATA((struct cmsghdr *)(h + ho)), combo, dlen);
        else
            memcpy(CMSG_DATA((struct cmsghdr *)(h + ho)), g + go + LX_CMSGHDR, dlen);
        free(combo);
        ho += need;
        go += LX_CMSG_ALIGN(clen);
    }
    return (ssize_t)ho;
}

// host(macOS) control buf -> guest(Linux) control buf, appending at `off`. Returns Linux bytes written
// (<=cap; stops at the guest-buffer boundary, leaving the kernel's MSG_CTRUNC in mh->msg_flags to be
// translated).
static ssize_t cmsg_m2l(const struct msghdr *mh, uint8_t *g, size_t cap, size_t off, int *truncp) {
    if (truncp) *truncp = 0;
    size_t go = off;
    for (struct cmsghdr *c = CMSG_FIRSTHDR((struct msghdr *)mh); c; c = CMSG_NXTHDR((struct msghdr *)mh, c)) {
        if (c->cmsg_len < CMSG_LEN(0)) break;
        size_t dlen = (size_t)c->cmsg_len - CMSG_LEN(0); // payload bytes (macOS hdr=12)
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS && dlen >= sizeof(int)) {
            int nfds = (int)(dlen / sizeof(int));
            int *fds = (int *)CMSG_DATA(c);
            int visible = cmsg_import_eventfd_trailer(fds, nfds);
            for (int i = 0; i < visible; i++) {
                cmsg_note_recv_sock_fd(fds[i]);
            }
            dlen = (size_t)visible * sizeof(int);
        }
        size_t need = LX_CMSG_ALIGN(LX_CMSGHDR + dlen);
        if (go + LX_CMSGHDR + dlen > cap) {
            if (truncp) *truncp = 1;
            break;
        }
        *(uint64_t *)(g + go) = (uint64_t)(LX_CMSGHDR + dlen); // Linux cmsg_len
        *(int *)(g + go + 8) = cmsg_level_m2l(c->cmsg_level);
        *(int *)(g + go + 12) = c->cmsg_type;
        memcpy(g + go + LX_CMSGHDR, CMSG_DATA(c), dlen);
        go += need;
    }
    return (ssize_t)go;
}

static void cmsg_lx_set_cloexec_fds(uint8_t *g, size_t glen) {
    size_t go = 0;
    while (go + LX_CMSGHDR <= glen) {
        uint64_t clen = *(uint64_t *)(g + go);
        int lvl = *(int *)(g + go + 8);
        int typ = *(int *)(g + go + 12);
        if (clen < LX_CMSGHDR || go + clen > glen) break;
        if (lvl == LX_SOL_SOCKET && typ == SCM_RIGHTS) {
            size_t dlen = (size_t)clen - LX_CMSGHDR;
            int *fds = (int *)(g + go + LX_CMSGHDR);
            for (size_t i = 0; i + sizeof(int) <= dlen; i += sizeof(int)) {
                int fd = fds[i / sizeof(int)];
                if (fd >= 0) fcntl(fd, F_SETFD, FD_CLOEXEC);
            }
        }
        go += LX_CMSG_ALIGN(clen);
    }
}

// Append a synthesized Linux SCM_CREDENTIALS record (SOL_SOCKET / type 2, struct ucred {pid,uid,gid}) at
// offset `off` in the guest control buffer, for a socket with SO_PASSCRED enabled (macOS has neither the
// option nor the auto-attached cmsg). Returns the new offset (8-aligned), or `off` unchanged if there is no
// room -- the caller then flags MSG_CTRUNC. See g_sock_passcred / case 212.
static size_t cmsg_add_cred(uint8_t *g, size_t off, size_t cap, int pid, int uid, int gid) {
    size_t need = LX_CMSGHDR + 12; // ucred = 3 x u32 = 12
    if (off + need > cap) return off;
    *(uint64_t *)(g + off) = (uint64_t)need;   // Linux cmsg_len (payload + 16B hdr)
    *(int *)(g + off + 8) = LX_SOL_SOCKET;     // cmsg_level = SOL_SOCKET(1)
    *(int *)(g + off + 12) = 2;                // cmsg_type  = SCM_CREDENTIALS(2)
    *(uint32_t *)(g + off + 16) = (uint32_t)pid;
    *(uint32_t *)(g + off + 20) = (uint32_t)uid;
    *(uint32_t *)(g + off + 24) = (uint32_t)gid;
    return off + LX_CMSG_ALIGN(need);
}

// SOL_SOCKET option name: Linux -> macOS (they differ). -1 = ignore (unsupported here).
static int so_opt_l2m(int o) {
    switch (o) {
    // SO_DEBUG
    case 1: return 0x0001;
    // SO_REUSEADDR
    case 2: return 0x0004;
    // SO_ERROR  (async-connect completion!)
    case 4: return 0x1007;
    // SO_DONTROUTE
    case 5: return 0x0010;
    // SO_BROADCAST
    case 6: return 0x0020;
    // SO_SNDBUF
    case 7: return 0x1001;
    // SO_RCVBUF
    case 8: return 0x1002;
    // SO_KEEPALIVE
    case 9: return 0x0008;
    // SO_OOBINLINE
    case 10: return 0x0100;
    // SO_LINGER (struct linger: same layout)
    case 13: return 0x0080;
    // SO_REUSEPORT
    case 15: return 0x0200;
    // SO_ACCEPTCONN
    case 30: return 0x0002;
    // SO_TYPE
    case 3: return 0x1008;
    // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) are handled at the call site (case 208/209: real timeval translation +
    // arming); every other unknown SOL_SOCKET optname -> ignore here.
    default: return -1;
    }
}

// IPPROTO_TCP optname Linux -> macOS. CRITICAL: these numbers diverge, and a raw pass-through maps
// Linux TCP_KEEPIDLE(4)/TCP_CORK(3) onto macOS TCP_NOPUSH(4)/TCP_NOOPT(3) — TCP_NOPUSH *corks* the
// socket so a server's reply is never delivered until close (breaks redis & every keepalive-setting
// server). Map the known ones; ignore (-1) unknown rather than pass through and accidentally cork.
static int tcp_opt_l2m(int o) {
    switch (o) {
    case 1: return 0x01;  // TCP_NODELAY  (same)
    case 2: return 0x02;  // TCP_MAXSEG   (same)
    case 3: return 0x04;  // Linux TCP_CORK     -> macOS TCP_NOPUSH (deliberate; guest asked to cork)
    case 4: return 0x10;  // Linux TCP_KEEPIDLE -> macOS TCP_KEEPALIVE (seconds)
    case 5: return 0x101; // Linux TCP_KEEPINTVL-> macOS TCP_KEEPINTVL
    case 6: return 0x102; // Linux TCP_KEEPCNT  -> macOS TCP_KEEPCNT
    default: return -1;   // unknown -> ignore (never pass a Linux number straight to macOS IPPROTO_TCP)
    }
}

// ---- NET namespace: per-container private loopback. A container's explicit 127.0.0.0/8 TCP sockets
// are routed to AF_UNIX sockets under a per-namespace host dir the guest can't name (it's path-jailed),
// so each container's localhost is isolated from the host + other containers. 0.0.0.0/external stay
// host-passthrough (so `-p` publishing still works). Off when g_netns[0]==0.
// host dir for this container's loopback unix sockets ("" = no isolation)
static char g_netns[200];
// fd -> the loopback port it's bound/connected to (0 = not a private-lo socket)
static uint16_t g_lo_port[DD_NFD];
// fd -> 1 if this private-lo socket is AF_INET6 (so getsockname/getpeername/accept report a sockaddr_in6
// with ::1 instead of an AF_INET 127.0.0.1). The unix-socket switch is keyed by port only, so a v6 server
// and a v4 client on the same loopback port still rendezvous (dual-stack); this only picks the address
// family reported back to the guest.
static uint8_t g_lo_v6[DD_NFD];
// fd -> 1 if created SOCK_STREAM (only those get loopback isolation)
static uint8_t g_sock_stream[DD_NFD];
// fd -> 1 once a stream connect() SUCCEEDED on it. Linux keeps a connected stream socket in SS_CONNECTED
// (a second connect -> EISCONN) until close, even after the peer sends FIN; macOS drops the peer
// association after FIN so getpeername() there returns ENOTCONN. This sticky flag lets connect(203) report
// EISCONN faithfully (LTP connect01). Cleared on close (fd_reset_emul) and at socket()/accept re-init.
static uint8_t g_sock_conn[DD_NFD];
// fd -> 1 if created AF_INET SOCK_DGRAM (only those get the published-UDP switch redirect, below)
static uint8_t g_sock_dgram[DD_NFD];
// fd -> the socket's guest (Linux) address family, recorded at socket()/accept() so connect(203)/bind(200)
// can validate the guest sockaddr's sa_family against it (EAFNOSUPPORT) without a getsockname() probe --
// which is unreliable after a prior failed connect on the same fd. 0 = untracked (best-effort fallback).
static uint16_t g_sock_fam[DD_NFD];
// fd -> 1 if this DGRAM socket emulates a connection-oriented endpoint that owes its peer an EOF on
// close (a SEQPACKET socketpair or an O_DIRECT "packet" pipe). macOS DGRAM sockets never deliver EOF on
// peer close, so close() injects a zero-length EOF datagram and recv/read coerce ECONNRESET -> 0 for these.
static uint8_t g_sock_seqpacket[DD_NFD];

// fd -> (its socketpair/O_DIRECT-pipe PARTNER fd + 1); 0 = no known partner. Recorded for both ends at
// socketpair(SEQPACKET)/pipe2(O_DIRECT) so close() can tell a genuine last-local close (inject the synthetic
// EOF) from a parent dropping the child's fork-inherited peer end while it still holds its OWN end (must NOT
// inject: Linux delivers no EOF while the child still references that end, and our zero-length datagram would
// otherwise land in our own retained end's queue and be misread as a premature EOF -- exactly what broke
// Chromium's Mojo SEQPACKET handshake). Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_pair_peer[DD_NFD];

// fd -> a DISTINCT synthetic peer pid stamped on both ends at socketpair() creation (0 = none). macOS
// captures LOCAL_PEERPID at socketpair-creation time and reports the CREATOR's pid on BOTH ends, never
// updating it on fork -- so the process that CREATED the pair (Chromium's browser == container init, guest
// pid 1) reads its OWN pid as the peer credential for every forked child (renderer/GPU), which the
// SCM_CREDENTIALS/SO_PEERCRED self-fallback then collapsed to container_pid() == guest 1 for ALL of them.
// Mojo uses that peer pid as the ports node identity, so every child collided on node "1" and the ports
// node-merge never finalized (transport up, but OnChannelConnected never fired -> the child's IO thread
// blocked in recvmsg -> the 15s EnsureConnected watchdog killed it, before any Wayland/xdg_surface/frame).
// Stamping each end with a unique id (>= 1<<30, above Linux PID_MAX ~4M so it never aliases a real guest
// pid Mojo also tracks) gives every child a distinct, non-self node identity whenever LOCAL_PEERPID
// degenerates to self. Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_peer_pid[DD_NFD];

// fd -> 1 if THIS process has actually SENT on this SEQPACKET/O_DIRECT-pipe endpoint (0 = never used it).
// The synthetic peer-EOF injected on close (seq_send_eof) may fire ONLY for an endpoint this process wrote
// to -- never for one it merely inherited across fork and never used. Rationale (the Chromium Mojo wall):
// when the browser forks a renderer/GPU child, that child inherits ALL the browser's open fds, including the
// browser's SEND end of a Mojo channel meant for a DIFFERENT child. The bystander never sends on it, but on
// startup it CLOSES the inherited copy -- and the old close-time EOF injection then delivered a spurious
// zero-length "EOF" datagram into the peer end, which is the REAL target child's LIVE recv queue. That child
// read the premature 0 as end-of-channel and gave up ("Terminating after 15 seconds with no connection"),
// which in turn dropped its channel ref so the browser's next invitation send got ECONNRESET. A per-fd
// "did I ever write here" gate makes a never-used inherited end silent (correct: a bystander's close signals
// nothing on Linux either), while a genuine writer's close still injects the EOF a blocked reader needs
// (rustc/make jobserver, O_DIRECT pipe). RESET on fork (a child starts having-written nothing -- seq_wrote_
// after_fork), carried on dup (fd_carry_sock), cleared on close (fd_reset_emul).
static uint8_t g_sock_seq_wrote[DD_NFD];

static void seq_mark_wrote(int fd) {
    if (fd >= 0 && fd < DD_NFD && g_sock_seqpacket[fd]) g_sock_seq_wrote[fd] = 1;
}

// A forked child inherits the parent's open channel ends but has WRITTEN to none of them yet: clear the
// whole "wrote" table so a bystander child that closes inherited channel fds injects no spurious peer-EOF.
static void seq_wrote_after_fork(void) { memset(g_sock_seq_wrote, 0, sizeof g_sock_seq_wrote); }

static int sock_alloc_synth_peer(void) {
    static int ctr = 0x40000000; // 1<<30
    int v = __atomic_add_fetch(&ctr, 1, __ATOMIC_RELAXED);
    if (v >= 0x7fff0000 || v < 0x40000000) { // never wrap into a real-pid / negative range
        __atomic_store_n(&ctr, 0x40000000, __ATOMIC_RELAXED);
        v = 0x40000000;
    }
    return v;
}

static int seq_is(int fd) {
    return fd >= 0 && fd < DD_NFD && g_sock_seqpacket[fd];
}

// fd -> 1 if the guest enabled SO_PASSCRED on this AF_UNIX socket. macOS has no SO_PASSCRED/SCM_CREDENTIALS,
// so we record the request and synthesize the peer-credentials ancillary record on each recvmsg (below).
// Chromium's Mojo NodeChannel sets SO_PASSCRED and REQUIRES an SCM_CREDENTIALS cmsg on the bootstrap message
// -- without it the receiver logs "missing credentials" and aborts. Carried on dup, reset on close.
static uint8_t g_sock_passcred[DD_NFD];

static void cmsg_note_recv_sock_fd(int fd) {
    if (fd < 0 || fd >= DD_NFD) return;
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISSOCK(st.st_mode)) return;

    int ty = 0;
    socklen_t tyl = sizeof ty;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &ty, &tyl) != 0) ty = 0;

    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    memset(&ss, 0, sizeof ss);
    if (getsockname(fd, (struct sockaddr *)&ss, &sl) != 0 || ss.ss_family != AF_UNIX) return;

    g_sock_fam[fd] = AF_UNIX;
    g_sock_stream[fd] = (ty == SOCK_STREAM);
    g_sock_dgram[fd] = (ty == SOCK_DGRAM);
    if (!g_sock_peer_pid[fd]) g_sock_peer_pid[fd] = sock_alloc_synth_peer();
    g_sock_passcred[fd] = 1;
}

// fd -> guest-requested TCP bind port (host order), 0 = none. Set at bind(200) for AF_INET/INET6 stream
// sockets; consumed by the /proc/net/tcp[6] synth to surface a LISTEN row (see netns_tcp_* below).
static uint16_t g_tcp_lport[DD_NFD];
static uint32_t g_tcp_laddr[DD_NFD];     // fd -> raw __be32 v4 bind addr (0.0.0.0 -> 0), printed %08X kernel-style
static uint8_t g_tcp_l6[DD_NFD];         // fd -> 1 if the bind was AF_INET6 (row goes in /proc/net/tcp6)
static uint8_t g_tcp_laddr6[DD_NFD][16]; // fd -> 16-byte v6 bind addr
static uint8_t g_tcp_listen[DD_NFD];     // fd -> 1 once listen(2) succeeded (row is emitted only then)

// Wake any peer blocked on this DGRAM endpoint with a zero-length EOF datagram (queued after pending data,
// so order is preserved). Best-effort: a no-op for non-SEQPACKET fds and harmless if the peer is gone.
static void seq_send_eof(int fd) {
    if (!seq_is(fd)) return;
    // Only a genuine WRITER on this endpoint may synthesize the peer's EOF on close. An endpoint this process
    // merely inherited across fork and never wrote to belongs to another process's channel topology -- a
    // bystander renderer/GPU child closing the browser's inherited Mojo channel-send end must NOT dump a
    // zero-length "EOF" into the real target child's live recv queue (the Chromium Mojo handshake wall). On
    // Linux a bystander's close signals nothing to the peer either, so staying silent is Linux-exact; a real
    // writer's last close still injects the EOF a blocked reader needs (jobserver / O_DIRECT pipe). See
    // g_sock_seq_wrote for the full rationale.
    if (fd < 0 || fd >= DD_NFD || !g_sock_seq_wrote[fd]) return;
    // Suppress the synthetic EOF when this pair's OTHER end is still open in THIS process: we're a parent
    // dropping a fork-inherited peer copy (Linux delivers no EOF while the fork child still holds that end),
    // and injecting our zero-length "EOF" datagram here would deliver it to our OWN retained end -- a later
    // recv there would misread it as a premature end-of-stream (this is the Chromium Mojo handshake wall).
    // A real last-local close -- where we no longer hold the partner -- still injects, so a blocked peer recv
    // (e.g. a jobserver/O_DIRECT-pipe reader that never gets ECONNRESET on macOS) still wakes and returns 0.
    int p = g_sock_pair_peer[fd];
    if (p > 0) {
        int peer = p - 1;
        if (peer >= 0 && peer < DD_NFD && g_sock_seqpacket[peer]) return; // partner still held here -> no self-EOF
    }
    send(fd, "", 0, 0);
}

static int lo_on(void) {
    return g_netns[0] != 0;
}

static int lo_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 8 && *(uint16_t *)sa == AF_INET && sa[4] == 127;
    // 127.x.x.x
}

// ---- IPv6 loopback: same private-namespace redirect as 127/8, for AF_INET6 (::/::1). The guest passes a
// Linux sockaddr_in6 { u16 family(==10); u16 port@2; u32 flow@4; u8 addr[16]@8; u32 scope@24 }; the family
// VALUE is the Linux one (10), not macOS AF_INET6 (30). The 16-byte addr @8 is in6addr_loopback (::1, 15
// zero bytes + 0x01) or in6addr_any (::, all zero). Routing both to the per-container loopback dir keeps a
// dual-stack server's v6 bind isolated instead of escaping to the real host stack (and v6 has no bridge).
#define LX_AF_INET6_FAM 10

static int in6_all_zero(const uint8_t *a, int n) {
    for (int i = 0; i < n; i++)
        if (a[i]) return 0;
    return 1;
}

static int in6_is_loopback(const uint8_t *a) {
    return in6_all_zero(a, 15) && a[15] == 1;
}

static int in6_is_any(const uint8_t *a) {
    return in6_all_zero(a, 16);
}

// connect(dest): v6 loopback if AF_INET6 and dest is ::1 (mirrors lo_is: only the explicit loopback addr)
static int lo6_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 24 && *(const uint16_t *)sa == LX_AF_INET6_FAM && in6_is_loopback(sa + 8);
}

static int br_on(void); // defined below (per-network bridge on); used by the v6 bind classifiers here

// bind(addr): v6 loopback if AF_INET6 and addr is ::1, OR :: (unspecified) ONLY when the bridge is off.
// `::1` always stays on the private per-container loopback. `::` (dual-stack "any", as busybox nc / many
// servers bind when listening) is the v6 analogue of IPv4 0.0.0.0: with a user network attached it must
// defer to the bridge (br6_any_is below) so a peer container can reach it, instead of landing on the
// isolated loopback (where a cross-container connect would ENOENT). Mirrors lo_any_is's `&& !br_on()`.
static int lo6_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 24 || *(const uint16_t *)sa != LX_AF_INET6_FAM) return 0;
    if (in6_is_loopback(sa + 8)) return 1;
    return in6_is_any(sa + 8) && !br_on();
}

// bind(::): the IPv6 unspecified address, routed to the per-network bridge (== IPv4 0.0.0.0's br path) so
// a dual-stack listener that binds `::` is reachable by peer containers over the AF_UNIX switch. Only when
// a user network is attached (br_on()); with no bridge, `::` is handled by lo6_any_is (isolated loopback).
static int br6_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 24 || *(const uint16_t *)sa != LX_AF_INET6_FAM) return 0;
    return in6_is_any(sa + 8) && br_on();
}

static void lo_path(uint16_t port, char *out, size_t n) {
    snprintf(out, n, "%s/p%u", g_netns, (unsigned)port);
}

// Allocate an ephemeral loopback port for a bind(127.0.0.1:0). The kernel would assign a real port;
// under the unix-socket emulation we instead pick a port whose `p<port>` path is still free so that a
// later getsockname()/connect() round-trips to the same socket. (Without this, port 0 collapsed to a
// fixed sentinel and the client connected to a path that was never bound -> ENOENT.)
static uint16_t lo_alloc_ephemeral(void) {
    static uint16_t next; // seeded once per process; the path-existence check guards collisions
    if (next < 1024) next = (uint16_t)(20000 + (getpid() & 0x3fff));
    for (int tries = 0; tries < 45000; tries++) {
        uint16_t cand = next++;
        if (next < 1024) next = 1024; // wrapped through 0
        if (cand < 1024) continue;    // stay out of the privileged range
        char path[200];
        lo_path(cand, path, sizeof path);
        if (access(path, F_OK) != 0) return cand; // unbound -> usable
    }
    return 0;
}

// Swap the AF_INET socket at `fd` for a fresh AF_UNIX SOCK_STREAM one (keeping the fd number + flags).
static int lo_swap(int fd) {
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    int u = socket(AF_UNIX, SOCK_STREAM, 0);
    if (u < 0) return -1;
    if (u != fd) {
        if (dup2(u, fd) < 0) {
            close(u);
            return -1;
        }
        close(u);
    }
    // keep non-blocking (async connect)
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    return 0;
}

// report AF_INET 127.0.0.1:port back to the guest
static void fill_inet_lo(uint8_t *sa, socklen_t *l, uint16_t port) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(port);
    *(uint32_t *)(sa + 4) = 0x0100007fu;
    // 127.0.0.1, zero-pad
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// report AF_INET6 ::1:port back to the guest (Linux sockaddr_in6 layout; family value 10). Mirrors
// fill_inet_lo: reports the loopback addr regardless of whether the socket bound :: or ::1 (apps key off
// the port; cf. the v4 path reporting 127.0.0.1 for a 0.0.0.0 bind).
static void fill_inet6_lo(uint8_t *sa, socklen_t *l, uint16_t port) {
    if (!sa) return;
    memset(sa, 0, 28);                       // family/port/flow/addr/scope
    *(uint16_t *)(sa + 0) = LX_AF_INET6_FAM; // 10
    *(uint16_t *)(sa + 2) = htons(port);     // port (BE) @2
    sa[8 + 15] = 1;                          // addr @8 = ::1 (in6addr_loopback)
    if (l) *l = 28;
}

// ---- NET bridge (2A "virtual switch"): per-USER-NETWORK rendezvous for container<->container traffic.
// Generalizes the loopback redirect from "127/8 -> per-container dir" to "this user network's subnet ->
// SHARED per-network dir". A guest TCP socket whose peer is ANOTHER container's IP on the same user
// network (same /16 as our own DD_IP, and not 127/8) is routed to an AF_UNIX socket at
//   /tmp/.ddbr-<DD_NETBR>/<ip>:<port>
// The listening container (bind 0.0.0.0:<port> or its own IP) LISTENS on /tmp/.ddbr-<netid>/<ownip>:<port>;
// a peer connect(<ownip>:<port>) dials the same path. Because every container on the host is a JIT
// process under the same user, the two AF_UNIX endpoints rendezvous with no bridge / TUN / root. The dir
// is keyed by <netid> (mode 0700, the guest is path-jailed) so other networks never share sockets. The
// 127/8 loopback path (g_netns / lo_*) is untouched and stays per-container; only non-127 in-subnet
// AF_INET is bridged. Off when g_netbr[0]==0 || g_myip==0.
static char g_netbr[200];        // shared per-network rendezvous dir ("" = bridge off)
static uint32_t g_myip;          // this endpoint's IP, network byte order (0 = unset)
static uint16_t g_br_port[DD_NFD]; // fd -> virtual port of a bridge socket (0 = not a bridge socket)
static uint32_t g_br_ip[DD_NFD]; // fd -> virtual IP (network order) reported via getsockname/getpeername
static int g_br_init;

// Carry the per-fd socket-emulation metadata (SOCK_STREAM-ness, loopback/bridge port + ip) from `src`
// to `dst` when an fd is duplicated/moved (dup/dup3/fcntl F_DUPFD). Without this, a guest that creates a
// TCP socket then relocates it to another fd number (e.g. busybox's xmove_fd -> a fixed low fd) loses the
// `g_sock_stream` flag that gates the loopback + per-network bridge bind/connect redirection, so its
// AF_INET traffic silently falls through to host passthrough and never rendezvous with a peer container.
static void fd_carry_sock(int dst, int src) {
    if (dst < 0 || dst >= DD_NFD || src < 0 || src >= DD_NFD) return;
    g_sock_stream[dst] = g_sock_stream[src];
    g_sock_dgram[dst] = g_sock_dgram[src];
    g_sock_seqpacket[dst] = g_sock_seqpacket[src];
    g_sock_pair_peer[dst] = g_sock_pair_peer[src]; // dup aliases the same end -> same partner
    g_sock_seq_wrote[dst] = g_sock_seq_wrote[src]; // ... and the "did this process write here" EOF-gate
    g_sock_peer_pid[dst] = g_sock_peer_pid[src];   // ... and the same synthetic peer node identity
    g_sock_passcred[dst] = g_sock_passcred[src];
    g_sock_conn[dst] = g_sock_conn[src];
    g_sock_fam[dst] = g_sock_fam[src];
    g_lo_port[dst] = g_lo_port[src];
    g_lo_v6[dst] = g_lo_v6[src];
    g_br_port[dst] = g_br_port[src];
    g_br_ip[dst] = g_br_ip[src];
    g_tcp_lport[dst] = g_tcp_lport[src];
    g_tcp_laddr[dst] = g_tcp_laddr[src];
    g_tcp_l6[dst] = g_tcp_l6[src];
    g_tcp_listen[dst] = g_tcp_listen[src];
    memcpy(g_tcp_laddr6[dst], g_tcp_laddr6[src], 16);
}

// ---- listening-TCP introspection (ss/netstat -l): a socket the guest bind()+listen()s MUST appear in
// /proc/net/tcp[6] with state 0A (TCP_LISTEN). dd translates the guest's AF_INET(6) bind onto a host
// AF_UNIX switch (or a real host bind in passthrough), so the synthesized /proc/net/tcp table -- which
// runs no real IP stack -- has to remember the guest-requested (addr,port) itself. bind(200) records it;
// listen(201) arms g_tcp_listen; the vfs synth walks these to emit the LISTEN rows. Cleared on
// close/socket-reinit (fd_reset_emul) so a reused fd never reports a stale listener.
// Note: g_tcp_lport/laddr/l6/listen/laddr6 are declared up top alongside the other per-fd socket arrays.
static void netns_tcp_bind_note(int fd, uint16_t port_host, int v6, uint32_t addr4_be, const uint8_t *addr6) {
    if (fd < 0 || fd >= DD_NFD) return;
    g_tcp_lport[fd] = port_host;
    g_tcp_laddr[fd] = addr4_be; // raw __be32 as it sits in memory (printed %08X, kernel-style)
    g_tcp_l6[fd] = (uint8_t)!!v6;
    g_tcp_listen[fd] = 0; // a fresh bind is not yet listening
    if (v6 && addr6)
        memcpy(g_tcp_laddr6[fd], addr6, 16);
    else
        memset(g_tcp_laddr6[fd], 0, 16);
}

static void netns_tcp_listen_note(int fd) {
    if (fd >= 0 && fd < DD_NFD && g_tcp_lport[fd]) g_tcp_listen[fd] = 1;
}

// Emit the LISTEN rows for the v4 (v6==0) or v6 (v6==1) table into `out` (<=cap). Returns bytes written.
// Row layout mirrors the kernel's tcp4_seq/tcp6_seq: sl, local_address:port, rem 0, st 0A, queues 0,
// uid 0, a synthetic-but-stable inode, refcount 1. Values a real ss/netstat parses positionally.
static int netns_tcp_emit(char *out, size_t cap, int v6) {
    int off = 0, sl = 0;
    for (int fd = 0; fd < DD_NFD && off < (int)cap - 256; fd++) {
        if (!g_tcp_listen[fd] || !g_tcp_lport[fd]) continue;
        if ((int)g_tcp_l6[fd] != !!v6) continue;
        unsigned long ino = 100000UL + (unsigned)fd; // stable within a run; distinct per listener
        if (v6) {
            const uint8_t *a = g_tcp_laddr6[fd];
            char h[33];
            for (int i = 0; i < 16; i++)
                snprintf(h + i * 2, 3, "%02x", a[i]);
            off +=
                snprintf(out + off, cap - off,
                         "%4d: %s:%04X 00000000000000000000000000000000:0000 0A "
                         "00000000:00000000 00:00000000 00000000     0        0 %lu 1 0000000000000000 100 0 0 10 0\n",
                         sl++, h, g_tcp_lport[fd], ino);
        } else {
            off +=
                snprintf(out + off, cap - off,
                         "%4d: %08X:%04X 00000000:0000 0A "
                         "00000000:00000000 00:00000000 00000000     0        0 %lu 1 0000000000000000 100 0 0 10 0\n",
                         sl++, g_tcp_laddr[fd], g_tcp_lport[fd], ino);
        }
    }
    return off;
}

// dotted-quad -> network-order u32 (bytes a.b.c.d), 0 on parse failure
static uint32_t br_parse_ip(const char *s) {
    unsigned a = 0, b = 0, cc = 0, d = 0;
    if (sscanf(s, "%u.%u.%u.%u", &a, &b, &cc, &d) != 4) return 0;
    if (a > 255 || b > 255 || cc > 255 || d > 255) return 0;
    return (uint32_t)(a | (b << 8) | (cc << 16) | (d << 24));
}

// Lazy, self-contained env ingestion (mirrors the net_isolate getenv pattern in service.c case 203), so
// the bridge needs no edit to the per-target startup code: DD_NETBR=<netid>, DD_IP=<dotted-quad>.
static void br_init(void) {
    if (g_br_init) return;
    g_br_init = 1;
    const char *nbr = getenv("DD_NETBR");
    if (nbr && nbr[0]) {
        snprintf(g_netbr, sizeof g_netbr, "/tmp/.ddbr-%.40s", nbr);
        mkdir(g_netbr, 0700); // shared per-network dir; EEXIST is fine (peers share it)
    }
    const char *dip = getenv("DD_IP");
    if (dip && dip[0]) g_myip = br_parse_ip(dip);
}

static int br_on(void) {
    if (!g_br_init) br_init();
    return g_netbr[0] != 0 && g_myip != 0;
}

// dest IP is on our user network (same /16 as g_myip == first two octets a.b)
static int br_in_subnet(uint32_t ip_be) {
    return (ip_be & 0x0000ffffu) == (g_myip & 0x0000ffffu);
}

// connect(dest): bridge if AF_INET, non-127, in our subnet
static int br_connect_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return 0;
    return br_in_subnet(*(uint32_t *)(sa + 4));
}

// bind(addr): bridge if AF_INET, non-127, and 0.0.0.0 (ANY) / our own IP / in-subnet
static int br_bind_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return 0;
    uint32_t ip = *(uint32_t *)(sa + 4);
    return ip == 0 || ip == g_myip || br_in_subnet(ip);
}

// A STREAM bind the private loopback should own. Explicit 127/8 always; INADDR_ANY (0.0.0.0) too when the
// bridge is OFF -- "any" includes loopback, and with no virtual switch the only reachable address under
// isolation IS loopback, so a server that binds 0.0.0.0 (redis/memcached/nats default) must land on lo_path
// or a same-container 127.0.0.1 connect finds nothing (ENOENT). In bridge mode 0.0.0.0 stays on br_path
// (cross-container + publish); the loopback connect path falls back to our own br endpoint there.
static int lo_any_is(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(const uint16_t *)sa != AF_INET) return 0;
    if (sa[4] == 127) return 1;
    return *(const uint32_t *)(sa + 4) == 0 && !br_on();
}

// rendezvous path for <ip_be>:<port> under the shared per-network dir
static void br_path(uint32_t ip_be, uint16_t port, char *out, size_t n) {
    const uint8_t *b = (const uint8_t *)&ip_be;
    snprintf(out, n, "%s/%u.%u.%u.%u:%u", g_netbr, b[0], b[1], b[2], b[3], (unsigned)port);
}

// bind(:0) on the bridge -> a free, round-trippable ephemeral port keyed by OUR ip (cf. lo_alloc_ephemeral)
static uint16_t br_alloc_ephemeral(void) {
    static uint16_t next;
    if (next < 1024) next = (uint16_t)(20000 + (getpid() & 0x3fff));
    for (int tries = 0; tries < 45000; tries++) {
        uint16_t cand = next++;
        if (next < 1024) next = 1024;
        if (cand < 1024) continue;
        char path[200];
        br_path(g_myip, cand, path, sizeof path);
        if (access(path, F_OK) != 0) return cand;
    }
    return 0;
}

// report the VIRTUAL AF_INET <ip_be>:<port> (not the AF_UNIX path) back to the guest
static void fill_inet_br(uint8_t *sa, socklen_t *l, uint32_t ip_be, uint16_t port) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(port);
    *(uint32_t *)(sa + 4) = ip_be;
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// ---- Published-port host forwarder (`docker run -p HOST:CONTAINER`) -----------------------------
// A guest that binds+listens on a published container port does so on the AF_UNIX virtual switch
// (br_path for a 0.0.0.0/eth0 bind, lo_path for a 127.0.0.1 bind) -- reachable by peer containers, but
// NOT by a host process dialing localhost:HOST_PORT, because nothing on the host listens on an AF_INET
// socket for HOST_PORT. This bridges that gap: when the guest listen()s on a published port we start a
// REAL host AF_INET listener on 0.0.0.0:HOST_PORT (matching docker's default publish address, which the
// daemon also reports via NetworkSettings.Ports), and for each accepted host connection we dial the
// guest's AF_UNIX switch socket and relay bytes both ways. The guest's own accept() returns the relayed
// connection exactly as if a peer container had connected, so container<->container, egress, the switch
// and --network none/host are completely untouched -- this purely ADDS the host->container path.
// (Uses g_portmap/pm_host from state.c, included before this file in the same TU.) TCP only for now;
// published UDP is a follow-up (the guest's UDP path isn't switch-redirected today).

// Is `cport` a published container port? (pm_host() returns cport on a miss, so it can't answer this.)
static int pm_published(uint16_t cport) {
    for (int i = 0; i < g_nportmap; i++)
        if (g_portmap[i].cport == cport) return 1;
    return 0;
}

// Host ports we've already spun up a TCP / UDP forwarder for (idempotent across re-listen()/re-bind under
// SO_REUSEADDR). A port is marked BEFORE its forwarder thread is created and the thread UNMARKS it if its
// own bind() fails, so a transient EADDRINUSE doesn't permanently disable forwarding for that port (F7).
static uint16_t g_fwd_started[32];
static int g_nfwd;
static uint16_t g_udp_fwd_started[32];
static int g_nudpfwd;

static void fwd_unmark(uint16_t *arr, int *n, uint16_t hport) {
    for (int i = 0; i < *n; i++)
        if (arr[i] == hport) {
            arr[i] = arr[--(*n)];
            return;
        }
}

// One relay connection: pump bytes between host TCP fd `a` and switch AF_UNIX fd `b` until either EOF.
struct fwd_relay {
    int a, b;
};

// Copy one ready direction src->dst. Returns 0 to keep going, -1 to tear the whole connection down.
// On src EOF we half-close dst (shutdown SHUT_WR) so the peer sees the FIN and can finish its reply,
// and mark that direction done; the connection ends only once BOTH directions have closed.
static int fwd_pump(int src, int dst, int *src_done, char *buf, size_t cap) {
    ssize_t n = read(src, buf, cap);
    if (n == 0) {
        shutdown(dst, SHUT_WR);
        *src_done = 1;
        return 0;
    }
    if (n < 0) {
        if (errno == EINTR || errno == EAGAIN) return 0;
        return -1;
    }
    for (ssize_t off = 0; off < n;) {
        ssize_t w = write(dst, buf + off, (size_t)(n - off));
        if (w <= 0) {
            if (w < 0 && (errno == EINTR || errno == EAGAIN)) continue;
            return -1;
        }
        off += w;
    }
    return 0;
}

static void *fwd_relay_thread(void *p) {
    struct fwd_relay r = *(struct fwd_relay *)p;
    free(p);
    char buf[65536];
    int a_done = 0, b_done = 0; // host->guest / guest->host directions closed
    while (!a_done || !b_done) {
        struct pollfd pf[2] = {{r.a, a_done ? 0 : POLLIN, 0}, {r.b, b_done ? 0 : POLLIN, 0}};
        if (poll(pf, 2, -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (!a_done && (pf[0].revents & (POLLIN | POLLHUP | POLLERR)))
            if (fwd_pump(r.a, r.b, &a_done, buf, sizeof buf) < 0) break;
        if (!b_done && (pf[1].revents & (POLLIN | POLLHUP | POLLERR)))
            if (fwd_pump(r.b, r.a, &b_done, buf, sizeof buf) < 0) break;
    }
    close(r.a);
    close(r.b);
    return NULL;
}

static int switch_dial(const char *path); // defined below; gap-tolerant AF_UNIX switch dial

struct fwd_listen {
    uint16_t hport;
    char upath[200]; // full switch path; truncated into sun_path exactly as the guest's bind did
};

static void *fwd_listen_thread(void *p) {
    struct fwd_listen fl = *(struct fwd_listen *)p;
    free(p);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    if (ls < 0) return NULL;
    int on = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in sin;
    memset(&sin, 0, sizeof sin);
    sin.sin_family = AF_INET;
    sin.sin_port = htons(fl.hport);
    sin.sin_addr.s_addr = htonl(INADDR_ANY); // 0.0.0.0:HOST_PORT (docker's default publish address)
    if (bind(ls, (struct sockaddr *)&sin, sizeof sin) < 0 || listen(ls, 128) < 0) {
        close(ls); // host port busy (e.g. another container already published it) -> no forwarding
        fwd_unmark(g_fwd_started, &g_nfwd, fl.hport); // transient busy: allow a later listen() to retry (F7)
        return NULL;
    }
    for (;;) {
        int hc = accept(ls, NULL, NULL);
        if (hc < 0) {
            if (errno == EINTR) continue;
            break;
        }
        // Dial the guest's switch listen socket (same truncation the guest used when it bound it),
        // retrying briefly across a re-listen gap: a published server looping `nc -l -w N` rebinds the
        // switch inode between connections, so a host connection that lands in the gap sees ENOENT (inode
        // gone) or ECONNREFUSED (stale inode, nothing accepting). Recreate + retry for ~600ms (mirrors TCP
        // SYN retransmit), then drop the host connection. A genuinely-dead guest still fails after the cap.
        int gc = switch_dial(fl.upath);
        if (gc < 0) {
            close(hc);
            continue;
        }
        struct fwd_relay *fr = malloc(sizeof *fr);
        if (!fr) {
            close(gc);
            close(hc);
            continue;
        }
        fr->a = hc;
        fr->b = gc;
        pthread_t t;
        if (pthread_create(&t, NULL, fwd_relay_thread, fr) != 0) {
            free(fr);
            close(gc);
            close(hc);
            continue;
        }
        pthread_detach(t);
    }
    close(ls);
    return NULL;
}

// Dial an AF_UNIX switch socket at `path`, retrying briefly across a peer's re-listen gap: a server
// looping `nc -l -w N` (or any accept-one-then-rebind pattern) unbinds+rebinds the switch inode between
// connections, so a dial that lands in that window sees ENOENT (inode gone) or ECONNREFUSED (stale inode,
// nothing accepting yet). Recreate the socket + retry for ~600ms (mirrors TCP SYN retransmission across a
// transient backlog gap); a genuinely-absent peer still fails after the cap. Returns a connected fd or -1.
// A connection that immediately HUPs with no readable data is a peer that's mid-exit: a `-w N` listener
// whose accept-window just closed (busybox `nc -l -w 1` loops align their 1-second boundary with the
// scenario's `sleep 1`, so a single-shot client connects exactly as the current listener is exiting). It
// accepts nothing and the socket closes with 0 bytes. Distinguish that from a live connection (data
// pending, or a client-first protocol where the server waits for the request) by a brief poll: only a
// POLLHUP/POLLERR WITHOUT POLLIN means "dead on arrival" -> retry a fresh listener. Anything else is live.
static int switch_dead_on_arrival(int fd) {
    struct pollfd pf = {fd, POLLIN, 0};
    int pr = poll(&pf, 1, 40); // returns at once when readable/closed; ~40ms only for a truly-idle live peer
    if (pr <= 0) return 0;     // idle but live: a client-first protocol (server awaits the request) or slow
    // Readable: could be real data OR a peer-closed EOF. PEEK (consumes nothing, so the guest's later read
    // still sees any data): 0 bytes == the peer closed with nothing to serve == dead on arrival.
    char b[1];
    ssize_t n = recv(fd, b, 1, MSG_PEEK | MSG_DONTWAIT);
    if (n == 0) return 1;                                             // clean EOF, no data -> dead
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return 0; // spurious wake, live
    if (n < 0) return 1;                                              // ECONNRESET/ECONNREFUSED -> dead
    return 0;                                                         // real data pending -> live
}

static int switch_dial(const char *path) {
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", path);
    for (int attempt = 0; attempt < 60; attempt++) {
        int fd = socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd < 0) return -1;
        if (connect(fd, (struct sockaddr *)&un, sizeof un) == 0) {
            // Peer mid-exit guard: a `-w N` listener whose window just closed accepts nothing and the
            // connection HUPs with no data. Detect that and retry a fresh listener; a live peer (data
            // pending, or a client-first published service) is kept.
            if (!switch_dead_on_arrival(fd)) return fd;
            close(fd);
        } else {
            int e = errno;
            close(fd);
            if (e != ENOENT && e != ECONNREFUSED) return -1;
        }
        struct timespec ts = {0, 20000000}; // 20ms
        nanosleep(&ts, NULL);
    }
    return -1;
}

// Is the daemon owning the process-independent host->container TCP forwarder? When set
// (DD_PUBLISH_DAEMON=1), the engine must NOT open its own in-process host AF_INET listener — that listener
// lived in whichever guest process called listen(), so a prefork / re-listening server tore it down on
// every rebind and two guest processes raced EADDRINUSE. The daemon's listener (dd-daemon/containers/
// ports.rs) outlives every guest process and dials this container's switch inode per connection instead.
// The guest-side bind/listen->switch redirect + getsockname->cport reporting below are UNCHANGED (the
// daemon relies on them). Cached (env is fixed for the process).
static int g_hostfwd_daemon = -1;

static int hostfwd_by_daemon(void) {
    if (g_hostfwd_daemon < 0) g_hostfwd_daemon = (getenv("DD_PUBLISH_DAEMON") != NULL);
    return g_hostfwd_daemon;
}

// Called from listen(): if `fd` is a published switch-backed listening socket, start its host forwarder.
static void fwd_maybe_start(int fd) {
    if (hostfwd_by_daemon()) return; // daemon owns the TCP host listener -> don't race it
    if (fd < 0 || fd >= DD_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        br_path(g_br_ip[fd], cport, upath, sizeof upath);
    } else if (g_lo_port[fd]) {
        cport = g_lo_port[fd];
        lo_path(cport, upath, sizeof upath);
    } else
        return; // real host bind (no switch redirect) -> already natively reachable, nothing to do
    if (!pm_published(cport)) return; // not a published port
    uint16_t hport = pm_host(cport);
    for (int i = 0; i < g_nfwd; i++)
        if (g_fwd_started[i] == hport) return; // already forwarding this host port
    if (g_nfwd >= 32) return;
    struct fwd_listen *fl = malloc(sizeof *fl);
    if (!fl) return;
    fl->hport = hport;
    snprintf(fl->upath, sizeof fl->upath, "%s", upath);
    g_fwd_started[g_nfwd++] = hport; // mark BEFORE create (closes the dedup window); thread unmarks on bind fail
    pthread_t t;
    if (pthread_create(&t, NULL, fwd_listen_thread, fl) != 0) {
        free(fl);
        g_nfwd--;
        return;
    }
    pthread_detach(t);
}

// ---- Published-port host UDP forwarder (`docker run -p HOST:CONTAINER/udp`) ----------------------
// UDP has no listen()/accept(): a guest UDP server bind()s a datagram socket on the virtual switch
// (AF_UNIX SOCK_DGRAM at br_path/lo_path, set up in the bind hook below) and recvfrom()s it. As with
// TCP nothing on the host owns an AF_INET socket for HOST_PORT, so a host process sending to
// localhost:HOST_PORT never reaches the guest. This bridges that gap with a real host
// AF_INET/SOCK_DGRAM socket on 0.0.0.0:HOST_PORT that relays datagrams to/from the guest's switch
// socket. Because UDP is connectionless, replies must route back to the right host client: we give
// EACH distinct host client its own guest-facing AF_UNIX/SOCK_DGRAM socket bound to a unique synthetic
// path, and send the client's datagrams to the guest FROM that socket. The guest's recvfrom() then sees
// that synthetic path as the source address, and a standard server that sendto()s its reply back to the
// recvfrom source lands on exactly that per-client socket -- which the forwarder maps back to the host
// client. So per-peer reply routing falls out of the normal UDP request/reply pattern, with no change
// to the guest's sendto/recvfrom path (AF_UNIX addresses pass through service.c's sa_l2m/sa_m2l raw).
// Scoped to PUBLISHED ports only (pm_published): non-published UDP is left entirely untouched (real
// host bind / egress / DNS unchanged), and it is a no-op on --network host/none (br_on()/lo_on() off),
// mirroring the TCP forwarder's guards. TCP publishing, container<->container and egress are unaffected.

// Swap the AF_INET socket at `fd` for a fresh AF_UNIX SOCK_DGRAM one (keeping the fd number + flags).
// Mirrors lo_swap() but for datagram sockets (the switch-backed UDP server socket).
static int udp_swap(int fd) {
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    int u = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (u < 0) return -1;
    if (u != fd) {
        if (dup2(u, fd) < 0) {
            close(u);
            return -1;
        }
        close(u);
    }
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    return 0;
}

#define UDP_FWD_MAXPEERS 64

struct udp_peer {
    struct sockaddr_storage caddr; // host client addr (macOS layout, as recvfrom delivered it)
    socklen_t calen;
    int gs; // guest-facing AF_UNIX/SOCK_DGRAM socket (bound to its own path,
            // connected to the guest switch socket) -- this client's identity
    int used;
};

struct udp_fwd {
    uint16_t hport;
    char upath[200]; // guest switch datagram socket path (br_/lo_path, as the guest bound)
    char pdir[80];   // dir holding this forwarder's synthetic per-client socket paths
    int hs;          // host AF_INET/SOCK_DGRAM socket on 0.0.0.0:hport
    struct udp_peer peers[UDP_FWD_MAXPEERS];
    int npeers;
    unsigned pseq; // monotonic id for unique synthetic peer paths (+ ring eviction)
};

// Find the peer slot for host client (sa,sl), or create one (its own AF_UNIX dgram socket bound to a
// fresh synthetic path and connected to the guest switch socket). Returns the guest-facing fd or -1.
static int udp_peer_get(struct udp_fwd *f, const struct sockaddr *sa, socklen_t sl) {
    for (int i = 0; i < f->npeers; i++)
        if (f->peers[i].used && f->peers[i].calen == sl && memcmp(&f->peers[i].caddr, sa, sl) == 0)
            return f->peers[i].gs;
    int slot, appended = 0;
    if (f->npeers < UDP_FWD_MAXPEERS) {
        slot = f->npeers;
        appended = 1;
    } else { // table full: evict a slot round-robin (oldest-ish) so new clients still work
        slot = (int)(f->pseq % UDP_FWD_MAXPEERS);
        if (f->peers[slot].used) close(f->peers[slot].gs);
        f->peers[slot].used = 0;
    }
    // On any failure after an eviction (!appended) we already cleared peers[slot].used above; also drop the
    // slot from npeers so we don't leave a permanently-dead slot in the table (F5). (Append never bumped
    // npeers yet -- it is incremented only on success below -- so its failures need no adjustment.)
    int gs = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (gs < 0) {
        if (!appended) f->npeers--;
        return -1;
    }
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s/%u", f->pdir, f->pseq++);
    unlink(un.sun_path);
    if (bind(gs, (struct sockaddr *)&un, sizeof un) < 0) {
        close(gs);
        if (!appended) f->npeers--;
        return -1;
    }
    struct sockaddr_un gu;
    memset(&gu, 0, sizeof gu);
    gu.sun_family = AF_UNIX;
    snprintf(gu.sun_path, sizeof gu.sun_path, "%s", f->upath);
    if (connect(gs, (struct sockaddr *)&gu, sizeof gu) < 0) {
        close(gs);
        unlink(un.sun_path);
        if (!appended) f->npeers--;
        return -1;
    }
    if (appended) f->npeers++;
    f->peers[slot].used = 1;
    f->peers[slot].gs = gs;
    f->peers[slot].calen = sl;
    memcpy(&f->peers[slot].caddr, sa, sl < sizeof f->peers[slot].caddr ? sl : sizeof f->peers[slot].caddr);
    return gs;
}

static void *udp_fwd_thread(void *p) {
    struct udp_fwd *f = (struct udp_fwd *)p; // heap-owned by this thread for its lifetime
    int hs = socket(AF_INET, SOCK_DGRAM, 0);
    if (hs < 0) {
        fwd_unmark(g_udp_fwd_started, &g_nudpfwd, f->hport);
        free(f);
        return NULL;
    }
    int on = 1;
    setsockopt(hs, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in sin;
    memset(&sin, 0, sizeof sin);
    sin.sin_family = AF_INET;
    sin.sin_port = htons(f->hport);
    sin.sin_addr.s_addr = htonl(INADDR_ANY);                 // 0.0.0.0:HOST_PORT (docker's default publish address)
    if (bind(hs, (struct sockaddr *)&sin, sizeof sin) < 0) { // host port busy -> no forwarding
        close(hs);
        fwd_unmark(g_udp_fwd_started, &g_nudpfwd, f->hport); // transient busy: allow a later bind() to retry (F7)
        free(f);
        return NULL;
    }
    f->hs = hs;
    snprintf(f->pdir, sizeof f->pdir, "/tmp/.ddudp.%d.%u", (int)getpid(), (unsigned)f->hport);
    mkdir(f->pdir, 0700);
    char buf[65536];
    for (;;) {
        struct pollfd pf[1 + UDP_FWD_MAXPEERS];
        pf[0].fd = hs;
        pf[0].events = POLLIN;
        pf[0].revents = 0;
        int n = 1;
        for (int i = 0; i < f->npeers; i++) {
            if (!f->peers[i].used) continue;
            pf[n].fd = f->peers[i].gs;
            pf[n].events = POLLIN;
            pf[n].revents = 0;
            n++;
        }
        if (poll(pf, n, -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        // host client -> guest: per-client guest-facing socket preserves the reply path
        if (pf[0].revents & (POLLIN | POLLERR)) {
            struct sockaddr_storage ca;
            socklen_t cl = sizeof ca;
            ssize_t r = recvfrom(hs, buf, sizeof buf, 0, (struct sockaddr *)&ca, &cl);
            if (r >= 0) {
                int gs = udp_peer_get(f, (struct sockaddr *)&ca, cl);
                if (gs >= 0) send(gs, buf, (size_t)r, 0); // connected to the guest switch socket
            }
        }
        // guest replies -> back out to the originating host client (the socket that received it)
        for (int i = 0; i < f->npeers; i++) {
            if (!f->peers[i].used) continue;
            int hit = 0;
            for (int j = 1; j < n; j++)
                if (pf[j].fd == f->peers[i].gs && (pf[j].revents & (POLLIN | POLLERR))) {
                    hit = 1;
                    break;
                }
            if (!hit) continue;
            // Non-blocking: the pre-poll pf[] readiness can match a REUSED fd-number of an evicted+recreated
            // peer socket; a blocking recv() would then hang forever. EAGAIN -> spurious wakeup, just skip (F4).
            ssize_t r = recv(f->peers[i].gs, buf, sizeof buf, MSG_DONTWAIT);
            if (r >= 0) sendto(hs, buf, (size_t)r, 0, (struct sockaddr *)&f->peers[i].caddr, f->peers[i].calen);
        }
    }
    for (int i = 0; i < f->npeers; i++)
        if (f->peers[i].used) close(f->peers[i].gs);
    close(hs);
    free(f);
    return NULL;
}

// Called from the UDP bind hook once a published switch-backed datagram socket is bound: start its host
// forwarder. Mirrors fwd_maybe_start() but triggers at bind (UDP has no listen) and keys off g_*_port,
// which the bind hook just set on this fd.
static void udp_fwd_maybe_start(int fd) {
    if (fd < 0 || fd >= DD_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        br_path(g_br_ip[fd], cport, upath, sizeof upath);
    } else if (g_lo_port[fd]) {
        cport = g_lo_port[fd];
        lo_path(cport, upath, sizeof upath);
    } else
        return;
    if (!pm_published(cport)) return;
    uint16_t hport = pm_host(cport);
    for (int i = 0; i < g_nudpfwd; i++)
        if (g_udp_fwd_started[i] == hport) return; // already forwarding this host port
    if (g_nudpfwd >= 32) return;
    struct udp_fwd *f = (struct udp_fwd *)calloc(1, sizeof *f);
    if (!f) return;
    f->hport = hport;
    snprintf(f->upath, sizeof f->upath, "%s", upath);
    g_udp_fwd_started[g_nudpfwd++] = hport; // mark BEFORE create; thread unmarks on bind fail (F7)
    pthread_t t;
    if (pthread_create(&t, NULL, udp_fwd_thread, f) != 0) {
        free(f);
        g_nudpfwd--;
        return;
    }
    pthread_detach(t);
}

// UDP bind hook: if `fd` is an AF_INET datagram socket binding a PUBLISHED container port on the bridge
// (0.0.0.0/own-ip/in-subnet) or private loopback, swap it onto an AF_UNIX/SOCK_DGRAM switch socket and
// start the host->guest forwarder. Returns 1 if handled (result in *out), 0 to let the caller bind
// normally (non-published UDP, non-switch nets, or anything not AF_INET datagram -> untouched).
static int udp_bind_maybe(int fd, const uint8_t *sa, socklen_t l, int64_t *out) {
    if (fd < 0 || fd >= DD_NFD || !g_sock_dgram[fd]) return 0;
    uint16_t cport;
    char up[200];
    uint32_t myip = 0;
    if (br_on() && br_bind_is(sa, l)) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0; // only explicit, published ports get switched
        myip = g_myip;
        br_path(myip, cport, up, sizeof up); // we always listen on OUR endpoint IP
    } else if (lo_on() && lo_is(sa, l)) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0;
        lo_path(cport, up, sizeof up);
    } else {
        return 0;
    }
    if (udp_swap(fd) < 0) {
        *out = -errno;
        return 1;
    }
    unlink(up);
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", up);
    int r = bind(fd, (struct sockaddr *)&un, sizeof un);
    if (r == 0) {
        if (myip) {
            g_br_port[fd] = cport;
            g_br_ip[fd] = myip;
        } else {
            g_lo_port[fd] = cport;
        }
        udp_fwd_maybe_start(fd);
    }
    *out = r < 0 ? -errno : 0;
    return 1;
}

// ===== Linux <-> macOS sockaddr translation (AF_INET / AF_INET6) — gate: NOSOCKADDR=1 =====
// The non-isolated socket paths (real host TCP/UDP via bind/connect/accept/getsockname/getpeername/
// sendto/recvfrom/sendmsg) used to hand the guest's *Linux*-layout sockaddr straight to a macOS
// syscall (and vice-versa on output). The two layouts differ in the first two bytes:
//   Linux  sockaddr_in  = { u16 sin_family;  u16 sin_port; u32 sin_addr;  u8 pad[8] }   (AF_INET =2)
//   macOS  sockaddr_in  = { u8 sin_len; u8 sin_family; u16 sin_port; u32 sin_addr; ... } (AF_INET =2)
//   Linux  sockaddr_in6 = { u16 sin6_family; u16 port; u32 flow; u8 addr[16]; u32 scope}(AF_INET6=10)
//   macOS  sockaddr_in6 = { u8 len; u8 sin6_family; u16 port; u32 flow; u8 addr[16]; u32 scope}(=30)
// So a guest AF_INET(2) read as macOS becomes sin_len=2/sin_family=0 (AF_UNSPEC) -> the server never
// really binds; and host output read back as Linux yields sin_family = 0x0210 = 528 (garbage). AF_INET6
// additionally differs in the family *value* (10 vs 30). port/addr/flow/scope share offsets+encoding
// (network byte order) so only family/len need fixing. AF_UNIX and other families pass through.
#define LX_AF_INET 2
#define LX_AF_INET6 10
static int g_saxl = -1;

static int saxl_on(void) {
    if (g_saxl < 0) g_saxl = getenv("NOSOCKADDR") ? 0 : 1;
    return g_saxl;
}

// guest domain (Linux) -> host (macOS), for socket()/socketpair(). AF_INET(2)/AF_UNIX(1) match.
static int af_l2m(int d) {
    return (saxl_on() && d == LX_AF_INET6) ? AF_INET6 : d;
}

// guest(Linux) sockaddr -> host(macOS) into `out`; returns host socklen, or -1 if not AF_INET/INET6
// (or gated off) — caller then uses the original guest pointer/len unchanged.
static socklen_t sa_l2m(const uint8_t *g, socklen_t glen, struct sockaddr_storage *out) {
    if (!saxl_on() || !g || glen < 2) return (socklen_t)-1;
    int fam = *(const uint16_t *)g;
    if (fam == LX_AF_INET && glen >= 8) {
        struct sockaddr_in *m = (struct sockaddr_in *)out;
        memset(m, 0, sizeof *m);
        m->sin_len = sizeof *m;
        m->sin_family = AF_INET;
        memcpy(&m->sin_port, g + 2, 2); // port (BE), same offset
        memcpy(&m->sin_addr, g + 4, 4); // addr (BE), same offset
        return (socklen_t)sizeof *m;    // 16
    }
    if (fam == LX_AF_INET6 && glen >= 24) {
        struct sockaddr_in6 *m = (struct sockaddr_in6 *)out;
        memset(m, 0, sizeof *m);
        m->sin6_len = sizeof *m;
        m->sin6_family = AF_INET6;
        memcpy(&m->sin6_port, g + 2, 2);
        memcpy(&m->sin6_flowinfo, g + 4, 4);
        memcpy(&m->sin6_addr, g + 8, 16);
        if (glen >= 28) memcpy(&m->sin6_scope_id, g + 24, 4);
        return (socklen_t)sizeof *m; // 28
    }
    return (socklen_t)-1;
}

// host(macOS) sockaddr -> guest(Linux) layout written to `g` (capacity gcap, may be 0/NULL). Returns
// the FULL Linux length of the address (16/28) even if it exceeds gcap (Linux truncates the copy but
// reports the full length via *addrlen), or -1 if not AF_INET/INET6 (caller copies raw host bytes).
static int sa_m2l(const struct sockaddr *m, uint8_t *g, socklen_t gcap) {
    if (!saxl_on() || !m) return -1;
    if (m->sa_family == AF_INET) {
        const struct sockaddr_in *s = (const struct sockaddr_in *)m;
        uint8_t t[16];
        memset(t, 0, sizeof t);
        *(uint16_t *)t = LX_AF_INET;
        memcpy(t + 2, &s->sin_port, 2);
        memcpy(t + 4, &s->sin_addr, 4);
        if (g && gcap) memcpy(g, t, gcap < 16 ? gcap : 16);
        return 16;
    }
    if (m->sa_family == AF_INET6) {
        const struct sockaddr_in6 *s = (const struct sockaddr_in6 *)m;
        uint8_t t[28];
        memset(t, 0, sizeof t);
        *(uint16_t *)t = LX_AF_INET6;
        memcpy(t + 2, &s->sin6_port, 2);
        memcpy(t + 4, &s->sin6_flowinfo, 4);
        memcpy(t + 8, &s->sin6_addr, 16);
        memcpy(t + 24, &s->sin6_scope_id, 4);
        if (g && gcap) memcpy(g, t, gcap < 28 ? gcap : 28);
        return 28;
    }
    return -1;
}

// ---- Per-workspace VPN egress redirect (docs/VPN.md option b) ---------------------------------------
// When DD_EGRESS_SOCKS="host:port" is armed, the guest's genuine external TCP connect()s are funneled
// through that SOCKS5 proxy (the front-end of a per-workspace userspace tunnel) instead of dialing the
// destination directly from the host's default routing domain. When the env var is ABSENT the whole
// feature is inert: egress_should_redirect() returns 0 up front and net.c runs its normal, byte-for-byte
// unchanged direct connect(). Only genuine external AF_INET/AF_INET6 destinations are candidates — the
// lo/bridge/DNS/AF_UNIX classifiers in net.c have already peeled those off before the Real-host-connect
// site that calls us, and we re-guard loopback here as a safety net so a VPN never captures 127/8 or ::1.
static const char *g_egress_socks = NULL; // "host:port"; stays NULL once probed-as-unset
static int g_egress_probed = 0;
static const char *egress_socks(void) {
    if (!g_egress_probed) {
        g_egress_probed = 1;
        const char *e = getenv("DD_EGRESS_SOCKS");
        g_egress_socks = (e && *e) ? e : NULL;
    }
    return g_egress_socks;
}
// Should this (already-translated macOS) destination be tunneled? 0 unless the redirect is armed AND the
// address is a genuine external IPv4/IPv6 (never loopback/unspecified/link-local).
static int egress_should_redirect(const struct sockaddr *m) {
    if (!egress_socks() || !m) return 0;
    if (m->sa_family == AF_INET) {
        uint32_t a = ntohl(((const struct sockaddr_in *)m)->sin_addr.s_addr);
        return !((a >> 24) == 127 || a == 0);
    }
    if (m->sa_family == AF_INET6) {
        const struct in6_addr *a6 = &((const struct sockaddr_in6 *)m)->sin6_addr;
        return !(IN6_IS_ADDR_LOOPBACK(a6) || IN6_IS_ADDR_UNSPECIFIED(a6) || IN6_IS_ADDR_LINKLOCAL(a6));
    }
    return 0;
}

// #261 — IPv4-only container network: dd models eth0 exactly like Docker's default bridge does — one IPv4
// address, NO global IPv6 address, and an empty IPv6 routing table (see the RTM_GETADDR/RTM_GETROUTE dumps
// in nl_emit_dump: eth0 gets only an AF_INET addr, and the only v6 addr is ::1 on lo). So a genuine external
// (global-unicast) IPv6 destination has NO ROUTE and, on a real container kernel, connect()/sendto() to it
// fails *immediately* with ENETUNREACH. dd must reproduce that instead of forwarding the dial to the
// underlying host's v6 stack: on a mac whose v6 path to the destination is black-holed (orbstack NAT is
// v4-only), that forward hangs the guest for the full ~2-minute connect timeout, and a happy-eyeballs client
// (apt, curl, glibc) that tried the AAAA first never falls back — the exact #261 stall. Failing fast is what
// lets it fall back to IPv4 in milliseconds, so `apt-get update` Just Works without Acquire::ForceIPv4.
// The AAAA record itself is still returned by the embedded resolver (dns_build_response), matching Docker's
// embedded DNS which also serves AAAA on a v4-only network — the guest learns the v6 addr, tries it, and is
// bounced instantly. Loopback (::1 -> private lo), link-local, unspecified, and the bridge/DNS classes are
// all peeled off before the direct-connect site, so only true external v6 reaches this predicate.
// Gate NOV6UNREACH=1 restores the old host-passthrough for A/B. When DD_EGRESS_SOCKS is armed the v6 dest is
// tunneled through the proxy instead (egress_should_redirect handles AF_INET6), so this is consulted only on
// the direct path, after that redirect has had its chance.
static int v6_no_route(const struct sockaddr *m) {
    static int off = -1;
    if (off < 0) off = (getenv("NOV6UNREACH") != NULL);
    if (off) return 0;
    if (!m || m->sa_family != AF_INET6) return 0;
    const struct in6_addr *a6 = &((const struct sockaddr_in6 *)m)->sin6_addr;
    return !(IN6_IS_ADDR_LOOPBACK(a6) || IN6_IS_ADDR_UNSPECIFIED(a6) || IN6_IS_ADDR_LINKLOCAL(a6));
}
// Blocking read/write of exactly `n` bytes (EINTR-retried); 0 = ok, -1 = errno set (peer close = ECONNRESET).
static int egress_io_write(int fd, const void *buf, size_t n) {
    const uint8_t *p = buf;
    size_t off = 0;
    while (off < n) {
        ssize_t w = write(fd, p + off, n - off);
        if (w > 0) { off += (size_t)w; continue; }
        if (w < 0 && errno == EINTR) continue;
        if (w == 0) errno = ECONNRESET;
        return -1;
    }
    return 0;
}
static int egress_io_read(int fd, void *buf, size_t n) {
    uint8_t *p = buf;
    size_t off = 0;
    while (off < n) {
        ssize_t r = read(fd, p + off, n - off);
        if (r > 0) { off += (size_t)r; continue; }
        if (r < 0 && errno == EINTR) continue;
        if (r == 0) errno = ECONNRESET; // proxy closed mid-handshake
        return -1;
    }
    return 0;
}
// Perform a SOCKS5 CONNECT to the macOS destination `m` over the guest socket `fd`, dialing the proxy in
// DD_EGRESS_SOCKS. Returns 0 (fd now relayed to the real dest through the tunnel) or -1/errno mirroring
// connect(). The guest fd is put in blocking mode for the short handshake and its O_NONBLOCK is restored
// after (a non-blocking guest that we return 0 to simply sees connect() complete immediately — legal).
static int egress_connect(int fd, const struct sockaddr *m, socklen_t mlen) {
    (void)mlen;
    const char *hp = egress_socks();
    if (!hp) { errno = EINVAL; return -1; }
    // Parse the proxy "host:port" (host is an IPv4 loopback literal, e.g. 127.30.0.1).
    const char *colon = strrchr(hp, ':');
    if (!colon || colon == hp) { errno = EINVAL; return -1; }
    char host[64];
    size_t hl = (size_t)(colon - hp);
    if (hl >= sizeof host) { errno = EINVAL; return -1; }
    memcpy(host, hp, hl);
    host[hl] = 0;
    int port = atoi(colon + 1);
    if (port <= 0 || port > 65535) { errno = EINVAL; return -1; }
    struct sockaddr_in px;
    memset(&px, 0, sizeof px);
    px.sin_len = sizeof px;
    px.sin_family = AF_INET;
    px.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &px.sin_addr) != 1) { errno = EINVAL; return -1; }

    int fl = fcntl(fd, F_GETFL);
    int was_nb = (fl >= 0 && (fl & O_NONBLOCK));
    if (was_nb) fcntl(fd, F_SETFL, fl & ~O_NONBLOCK);

    int rc = -1, e = 0;
    do {
        int cr;
        do {
            cr = connect(fd, (struct sockaddr *)&px, sizeof px);
        } while (cr < 0 && errno == EINTR);
        if (cr != 0) { e = errno; break; } // proxy down -> report as the connect error
        // SOCKS5 greeting: VER=5, NMETHODS=1, METHOD=0 (no auth).
        uint8_t greet[3] = {0x05, 0x01, 0x00};
        if (egress_io_write(fd, greet, 3) != 0) { e = errno; break; }
        uint8_t sel[2];
        if (egress_io_read(fd, sel, 2) != 0) { e = errno; break; }
        if (sel[0] != 0x05 || sel[1] != 0x00) { e = ECONNREFUSED; break; } // no acceptable auth method
        // CONNECT request: VER, CMD=1, RSV=0, ATYP, addr, port(BE) — copied verbatim from the macOS sockaddr.
        uint8_t req[22];
        int n = 0;
        req[n++] = 0x05;
        req[n++] = 0x01;
        req[n++] = 0x00;
        if (m->sa_family == AF_INET) {
            const struct sockaddr_in *s4 = (const struct sockaddr_in *)m;
            req[n++] = 0x01;
            memcpy(req + n, &s4->sin_addr, 4);
            n += 4;
            memcpy(req + n, &s4->sin_port, 2);
            n += 2;
        } else {
            const struct sockaddr_in6 *s6 = (const struct sockaddr_in6 *)m;
            req[n++] = 0x04;
            memcpy(req + n, &s6->sin6_addr, 16);
            n += 16;
            memcpy(req + n, &s6->sin6_port, 2);
            n += 2;
        }
        if (egress_io_write(fd, req, (size_t)n) != 0) { e = errno; break; }
        // Reply: VER, REP, RSV, ATYP, bound-addr, bound-port. Read the 4-byte header, then drain the bound
        // address (length per ATYP) and 2-byte port so the fd is left clean at the start of the relayed stream.
        uint8_t rep[4];
        if (egress_io_read(fd, rep, 4) != 0) { e = errno; break; }
        if (rep[1] != 0x00) { // SOCKS reply code -> map to a connect-style errno
            switch (rep[1]) {
                case 0x03: e = ENETUNREACH; break;  // network unreachable
                case 0x04: e = EHOSTUNREACH; break; // host unreachable
                case 0x05: e = ECONNREFUSED; break; // connection refused
                case 0x06: e = ETIMEDOUT; break;    // TTL expired
                default: e = ECONNREFUSED; break;   // general/ruleset/unsupported failure
            }
            break;
        }
        int skip = (rep[3] == 0x01) ? 4 : (rep[3] == 0x04) ? 16 : -1;
        if (rep[3] == 0x03) { // domain name: 1-byte length prefix
            uint8_t l;
            if (egress_io_read(fd, &l, 1) != 0) { e = errno; break; }
            skip = l;
        }
        if (skip < 0) { e = ECONNREFUSED; break; } // unknown ATYP
        uint8_t junk[256];
        if (skip > 0 && egress_io_read(fd, junk, (size_t)skip) != 0) { e = errno; break; }
        if (egress_io_read(fd, junk, 2) != 0) { e = errno; break; } // bound port
        rc = 0;
    } while (0);

    if (was_nb) fcntl(fd, F_SETFL, fl); // restore the guest's non-blocking flag
    if (rc != 0) {
        errno = e ? e : ECONNREFUSED;
        return -1;
    }
    return 0;
}

// host(macOS) AF_UNIX sockaddr -> guest(Linux) layout. The two structs disagree in the leading bytes:
//   Linux  sockaddr_un = { u16 sun_family;             char sun_path[108] }  (AF_UNIX = 1)
//   macOS  sockaddr_un = { u8 sun_len; u8 sun_family;  char sun_path[104] }  (AF_UNIX = 1)
// A raw byte copy (the old non-INET fallback) made the guest read sun_family as sun_len|(AF_UNIX<<8)
// (e.g. 272/362), so a genuine AF_UNIX peer/name looked like an unknown family -> postgres classified a
// unix-socket client as a TCP "host" and rejected it (no pg_hba host entry). Rewrite to a 2-byte family
// and, for a bound pathname socket, reverse-map the host path (upper/lower/volume) back to the guest path
// so getsockname/getpeername report the path the guest actually bound -- not its on-disk overlay location.
// Returns the full Linux address length (Linux reports it even past gcap), or -1 if `m` is not AF_UNIX.
static int sa_un_m2l(const struct sockaddr *m, socklen_t mlen, uint8_t *g, socklen_t gcap) {
    if (!saxl_on() || !m || m->sa_family != AF_UNIX) return -1;
    const struct sockaddr_un *u = (const struct sockaddr_un *)m;
    size_t off = offsetof(struct sockaddr_un, sun_path);
    size_t hplen = (size_t)mlen > off ? (size_t)mlen - off : 0; // path bytes the host reported (no NUL guarantee)
    char hpath[256];
    size_t i = 0;
    for (; i < hplen && i + 1 < sizeof hpath && u->sun_path[i]; i++)
        hpath[i] = u->sun_path[i];
    hpath[i] = 0;
    char gpath[256];
    if (hpath[0] == '/' && g_rootfs)
        guest_from_host(hpath, gpath, sizeof gpath); // overlay host path -> guest-visible path
    else
        snprintf(gpath, sizeof gpath, "%s", hpath); // unnamed/autobind (empty) or non-jail: pass through
    uint8_t t[2 + sizeof gpath];
    *(uint16_t *)t = AF_UNIX;
    size_t pl = strlen(gpath);
    memcpy(t + 2, gpath, pl);
    t[2 + pl] = 0;
    int llen = pl ? (int)(2 + pl + 1) : 2; // pathname: family + path + NUL; unnamed: just the family
    if (g && gcap) memcpy(g, t, (size_t)gcap < (size_t)llen ? gcap : (size_t)llen);
    return llen;
}

// ---- abstract-namespace AF_UNIX (sun_path[0]=='\0'): macOS has no abstract namespace, so map the
// abstract name to a real filesystem socket under a per-namespace dir keyed by DD_NETNS (same key as
// ipc_ns_key), so two guests in one container rendezvous and different containers stay isolated. The
// guest socket is already a real host AF_UNIX socket (case 198), so only the ADDRESS is rewritten.
static char g_absdir[200];
static int g_abs_init;

static void abs_init(void) {
    if (g_abs_init) return;
    g_abs_init = 1;
    const char *ns = getenv("DD_NETNS"); // same key used by ipc_ns_key (service.c)
    snprintf(g_absdir, sizeof g_absdir, "/tmp/.ddabs-%.40s", (ns && ns[0]) ? ns : "default");
    mkdir(g_absdir, 0700); // EEXIST fine; peers share it (0700, guest is path-jailed)
}

// Is this guest sockaddr an abstract AF_UNIX addr? family u16==AF_UNIX, sun_path[0]==NUL, name>=1B.
static int abs_is(const uint8_t *sa, socklen_t l) {
    return sa && l > 3 && *(const uint16_t *)sa == AF_UNIX && sa[2] == 0; // sun_path[0] @ offset 2
}

// Is this guest sockaddr an AF_UNIX *pathname* addr (a filesystem socket, not abstract/autobind)?
static int unix_path_is(const uint8_t *sa, socklen_t l) {
    return sa && l > 3 && *(const uint16_t *)sa == AF_UNIX && sa[2] != 0; // sun_path[0] @ offset 2
}

// Copy a guest sockaddr_un's sun_path (NUL- or addrlen-bounded) into `out` as a C string.
static void unix_path_copy(const uint8_t *sa, socklen_t l, char *out, size_t n) {
    size_t pl = (size_t)l > 2 ? (size_t)l - 2 : 0; // bytes after the 2-byte family
    size_t i = 0;
    for (; i + 1 < n && i < pl && sa[2 + i]; i++)
        out[i] = (char)sa[2 + i];
    out[i] = 0;
}

// Map abstract name (bytes sa+3 .. for namelen=l-3) to a filesystem path. Hex when it fits macOS
// sun_path[104], else FNV-1a hash (long D-Bus/X11/systemd names overflow); the name may hold NULs,
// '/', non-printables, so hex/hash makes a safe single path component (no traversal).
static void abs_path(const uint8_t *sa, socklen_t l, char *out, size_t n) {
    abs_init();
    const uint8_t *nm = sa + 3;
    size_t nl = (size_t)l - 3;
    size_t dl = strlen(g_absdir);
    if (dl + 1 + nl * 2 + 1 <= n && dl + 1 + nl * 2 < 104) { // full hex (unambiguous, fits sun_path)
        char hx[210];
        static const char *H = "0123456789abcdef";
        for (size_t i = 0; i < nl; i++) {
            hx[2 * i] = H[nm[i] >> 4];
            hx[2 * i + 1] = H[nm[i] & 15];
        }
        hx[2 * nl] = 0;
        snprintf(out, n, "%s/%s", g_absdir, hx);
    } else { // hash fallback (FNV-1a) keeps the path bounded
        uint64_t h = 1469598103934665603ull;
        for (size_t i = 0; i < nl; i++) {
            h ^= nm[i];
            h *= 1099511628211ull;
        }
        snprintf(out, n, "%s/h%016llx", g_absdir, (unsigned long long)h);
    }
}

// ===== AF_NETLINK / NETLINK_ROUTE: a minimal RTNETLINK responder ==========================
// macOS has no AF_NETLINK, so socket(AF_NETLINK,...) returned EAFNOSUPPORT and every interface-
// enumeration path (getifaddrs via glibc/musl, go-sockaddr, `ip`, ifconfig, minio, consul)
// failed with "Address family not supported". dd models exactly two interfaces (lo + eth0; see
// netif_* in state.c). A guest netlink socket is backed by an AF_UNIX SOCK_DGRAM socketpair: the
// guest holds one end; when it sends an RTM_GET* dump request we parse the nlmsghdr and WRITE the
// synthesized dump into OUR peer end, which queues on the guest end so the guest's ordinary
// recv/recvmsg/poll reads it back -- no read-side blocking or extra threads. We only synthesize the
// three dumps real enumeration uses (RTM_GETLINK / RTM_GETADDR / RTM_GETROUTE); any other request
// just gets an NLMSG_DONE so nothing hangs.
#define LX_AF_NETLINK 16
#define NL_RTM_NEWLINK 16
#define NL_RTM_GETLINK 18
#define NL_RTM_NEWADDR 20
#define NL_RTM_GETADDR 22
#define NL_RTM_NEWROUTE 24
#define NL_RTM_GETROUTE 26
#define NL_NLMSG_DONE 3
#define NL_NLM_F_MULTI 2
// guest netlink fd -> our peer socketpair fd, stored +1 (0 = not a netlink socket). Mirrors the
// g_eventfd_peer +1 convention so close()/fd_reset_emul can tear the peer down.
static int g_nl_peer[DD_NFD];

static int nl_is(int fd) {
    return fd >= 0 && fd < DD_NFD && g_nl_peer[fd];
}

// close a netlink fd's peer (called from fd_reset_emul on the guest close). Idempotent.
static void nl_close(int fd) {
    if (fd >= 0 && fd < DD_NFD && g_nl_peer[fd]) {
        close(g_nl_peer[fd] - 1);
        g_nl_peer[fd] = 0;
    }
}

// socket(AF_NETLINK,...): back it with an AF_UNIX SOCK_DGRAM socketpair. Any requested type
// (SOCK_RAW/SOCK_DGRAM) collapses to SOCK_DGRAM (AF_UNIX has no SOCK_RAW). Returns the guest fd or
// -errno. Honors SOCK_CLOEXEC(0x80000)/SOCK_NONBLOCK(0x800) on the guest end.
static int nl_open(int type, int proto) {
    (void)proto; // only NETLINK_ROUTE is modelled; others still get a working (empty-dump) socket
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) < 0) return -errno;
    int g = sv[0], peer = sv[1];
    if (g < 0 || g >= DD_NFD) { // untracked fd range -> can't route sends; refuse cleanly
        close(g);
        close(peer);
        return -EMFILE;
    }
    if (type & 0x80000) fcntl(g, F_SETFD, FD_CLOEXEC);
    if (type & 0x800) fcntl(g, F_SETFL, O_NONBLOCK);
    fcntl(peer, F_SETFD, FD_CLOEXEC); // keep our end out of a guest execve
    g_nl_peer[g] = peer + 1;
    return g;
}

// getsockname on a netlink fd: report a sockaddr_nl { u16 family; u16 pad; u32 pid; u32 groups } with
// pid = getpid() (the port id our dump replies also stamp in nlmsg_pid, so go's pid check matches).
static void nl_getsockname(uint8_t *sa, socklen_t *sl) {
    if (sa && sl && *sl >= 12) {
        memset(sa, 0, 12);
        *(uint16_t *)(sa + 0) = LX_AF_NETLINK;
        *(uint32_t *)(sa + 4) = (uint32_t)getpid();
        *sl = 12;
    } else if (sl)
        *sl = 12;
}

// Fill a Linux sockaddr_nl "from the kernel" (pid 0) as a recv source address; 12 bytes.
static void nl_fill_src(uint8_t *sa, socklen_t cap) {
    if (!sa || cap < 12) return;
    memset(sa, 0, 12);
    *(uint16_t *)(sa + 0) = LX_AF_NETLINK; // nl_pid=0 => from kernel (glibc/go accept only pid 0 source)
}

// rtattr append: { u16 rta_len; u16 rta_type; data } padded to RTA_ALIGN(4).
static void nl_put_attr(uint8_t *b, size_t *o, uint16_t type, const void *data, uint16_t dlen) {
    *(uint16_t *)(b + *o) = (uint16_t)(4 + dlen);
    *(uint16_t *)(b + *o + 2) = type;
    if (data && dlen) memcpy(b + *o + 4, data, dlen);
    *o += (size_t)((4 + dlen + 3) & ~3);
}

// begin an nlmsg (16-byte header); returns its offset for nl_end() to backpatch nlmsg_len.
static size_t nl_begin(uint8_t *b, size_t *o, uint16_t type, uint32_t seq) {
    size_t h = *o;
    memset(b + h, 0, 16);
    *(uint16_t *)(b + h + 4) = type;
    *(uint16_t *)(b + h + 6) = NL_NLM_F_MULTI;
    *(uint32_t *)(b + h + 8) = seq;
    *(uint32_t *)(b + h + 12) = (uint32_t)getpid();
    *o = h + 16;
    return h;
}

static void nl_end(uint8_t *b, size_t *o, size_t h) {
    *(uint32_t *)(b + h) = (uint32_t)(*o - h); // nlmsg_len (unpadded); attrs already 4-aligned
    *o = (*o + 3) & ~(size_t)3;
}

// one RTM_NEWLINK message
static void nl_link(uint8_t *b, size_t *o, uint32_t seq, const char *name, int idx, uint16_t iftype, uint32_t flags,
                    const uint8_t *mac, uint32_t mtu, const uint8_t *bcast) {
    size_t h = nl_begin(b, o, NL_RTM_NEWLINK, seq);
    uint8_t *ii = b + *o; // ifinfomsg (16B): family,pad,type(2),index(4),flags(4),change(4)
    memset(ii, 0, 16);
    *(uint16_t *)(ii + 2) = iftype;
    *(int32_t *)(ii + 4) = idx;
    *(uint32_t *)(ii + 8) = flags;
    *(uint32_t *)(ii + 12) = 0xffffffffu;
    *o += 16;
    nl_put_attr(b, o, 3, name, (uint16_t)(strlen(name) + 1)); // IFLA_IFNAME
    nl_put_attr(b, o, 1, mac, 6);                             // IFLA_ADDRESS
    nl_put_attr(b, o, 2, bcast, 6);                           // IFLA_BROADCAST
    uint32_t v = mtu;
    nl_put_attr(b, o, 4, &v, 4);      // IFLA_MTU
    v = (iftype == 772) ? 0u : 1000u; // IFLA_TXQLEN
    nl_put_attr(b, o, 13, &v, 4);
    uint8_t op = 6, lm = 0;        // IF_OPER_UP / IFLA_LINKMODE
    nl_put_attr(b, o, 16, &op, 1); // IFLA_OPERSTATE
    nl_put_attr(b, o, 17, &lm, 1); // IFLA_LINKMODE
    nl_end(b, o, h);
}

// one RTM_NEWADDR message (v4: alen=4; v6: alen=16). bcast!=NULL adds IFA_BROADCAST (v4 eth0 only).
static void nl_addr(uint8_t *b, size_t *o, uint32_t seq, uint8_t family, uint8_t prefix, uint8_t scope, int idx,
                    const char *label, const void *addr, int alen, const void *bcast) {
    size_t h = nl_begin(b, o, NL_RTM_NEWADDR, seq);
    uint8_t *ia = b + *o; // ifaddrmsg (8B): family,prefixlen,flags,scope,index(4)
    memset(ia, 0, 8);
    ia[0] = family;
    ia[1] = prefix;
    ia[3] = scope;
    *(uint32_t *)(ia + 4) = (uint32_t)idx;
    *o += 8;
    nl_put_attr(b, o, 1, addr, (uint16_t)alen);                            // IFA_ADDRESS
    nl_put_attr(b, o, 2, addr, (uint16_t)alen);                            // IFA_LOCAL
    if (bcast) nl_put_attr(b, o, 4, bcast, (uint16_t)alen);                // IFA_BROADCAST
    if (label) nl_put_attr(b, o, 3, label, (uint16_t)(strlen(label) + 1)); // IFA_LABEL
    nl_end(b, o, h);
}

// one RTM_NEWROUTE message
static void nl_route(uint8_t *b, size_t *o, uint32_t seq, uint8_t dst_len, uint8_t scope, uint8_t type,
                     const uint32_t *dst, const uint32_t *gw, const uint32_t *prefsrc, int oif) {
    size_t h = nl_begin(b, o, NL_RTM_NEWROUTE, seq);
    uint8_t *rm = b + *o; // rtmsg (12B): family,dst_len,src_len,tos,table,protocol,scope,type,flags(4)
    memset(rm, 0, 12);
    rm[0] = 2; // AF_INET
    rm[1] = dst_len;
    rm[4] = 254; // RT_TABLE_MAIN
    rm[5] = 3;   // RTPROT_BOOT
    rm[6] = scope;
    rm[7] = type; // RTN_UNICAST=1
    *o += 12;
    if (dst) nl_put_attr(b, o, 1, dst, 4); // RTA_DST
    if (oif) {
        uint32_t v = (uint32_t)oif;
        nl_put_attr(b, o, 4, &v, 4);
    } // RTA_OIF
    if (gw) nl_put_attr(b, o, 5, gw, 4);           // RTA_GATEWAY
    if (prefsrc) nl_put_attr(b, o, 7, prefsrc, 4); // RTA_PREFSRC
    nl_end(b, o, h);
}

static void nl_done(uint8_t *b, size_t *o, uint32_t seq) {
    uint8_t *h = b + *o;
    memset(h, 0, 16);
    *(uint32_t *)(h + 0) = 16;
    *(uint16_t *)(h + 4) = NL_NLMSG_DONE;
    *(uint16_t *)(h + 6) = NL_NLM_F_MULTI;
    *(uint32_t *)(h + 8) = seq;
    *(uint32_t *)(h + 12) = (uint32_t)getpid();
    *o += 16;
}

// Build + queue (one datagram to `peer`) the dump for request `type` with echoed `seq`.
static void nl_emit_dump(int peer, uint16_t type, uint32_t seq) {
    uint8_t out[4096];
    size_t o = 0;
    if (type == NL_RTM_GETLINK) {
        uint8_t zero6[6] = {0}, ff6[6] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff}, mac[6];
        netif_eth0_mac(mac);
        nl_link(out, &o, seq, "lo", 1, 772 /*ARPHRD_LOOPBACK*/, 0x10049u /*UP|LOOP|RUN|LOWER_UP*/, zero6, 65536, zero6);
        // --network none: loopback-only, so eth0 is absent from the link dump (`ip link` sees just lo).
        if (!net_isolate())
            nl_link(out, &o, seq, "eth0", 2, 1 /*ARPHRD_ETHER*/, 0x11043u /*UP|BCAST|RUN|MCAST|LOWER_UP*/, mac, 1500,
                    ff6);
    } else if (type == NL_RTM_GETADDR) {
        uint32_t lo4 = 0x0100007fu; // 127.0.0.1
        uint8_t lo6[16] = {0};
        lo6[15] = 1; // ::1
        uint32_t e4 = netif_eth0_ip(), eb = netif_eth0_bcast();
        nl_addr(out, &o, seq, 2 /*AF_INET*/, 8, 254 /*RT_SCOPE_HOST*/, 1, "lo", &lo4, 4, NULL);
        nl_addr(out, &o, seq, 10 /*AF_INET6*/, 128, 254, 1, NULL, lo6, 16, NULL);
        if (!net_isolate()) // --network none: no eth0 address
            nl_addr(out, &o, seq, 2, (uint8_t)netif_eth0_prefix(), 0 /*RT_SCOPE_UNIVERSE*/, 2, "eth0", &e4, 4, &eb);
    } else if (type == NL_RTM_GETROUTE) {
        if (!net_isolate()) { // --network none: no eth0 routes (loopback carries no L3 routing table)
            uint32_t net = netif_eth0_net(), gw = netif_eth0_gw(), src = netif_eth0_ip();
            nl_route(out, &o, seq, 0, 0 /*UNIVERSE*/, 1, NULL, &gw, NULL, 2); // default via gw dev eth0
            nl_route(out, &o, seq, (uint8_t)netif_eth0_prefix(), 253 /*LINK*/, 1, &net, NULL, &src, 2); // subnet
        }
    }
    // (unknown request types fall through to just NLMSG_DONE -> an empty, harmless dump)
    nl_done(out, &o, seq);
    ssize_t w = send(peer, out, o, 0); // one datagram; guest reads it via its own recv/recvmsg
    (void)w;
}

// Copy `n` bytes from `src` into the guest scatter buffer (iov array). Returns bytes actually copied.
static size_t nl_scatter(const uint8_t *src, size_t n, struct iovec *iov, int iovn) {
    size_t off = 0;
    for (int i = 0; i < iovn && off < n; i++) {
        size_t take = iov[i].iov_len;
        if (take > n - off) take = n - off;
        if (take && iov[i].iov_base) memcpy(iov[i].iov_base, src + off, take);
        off += take;
    }
    return off;
}

// Receive one queued netlink datagram into the guest iov, emulating the Linux MSG_PEEK / MSG_TRUNC
// semantics that macOS lacks. Two macOS gaps break busybox `ip`/libnetlink here:
//   (1) recv(...,MSG_TRUNC) on Linux returns the datagram's TRUE length (not the copied length) so a
//       caller can size a buffer; macOS ignores MSG_TRUNC on input and returns only what it copied.
//   (2) macOS short-circuits ANY zero-length receive to 0 without touching the queue, so busybox's
//       "peek the size first" idiom -- recvmsg(fd, {iov_len=0}, MSG_PEEK|MSG_TRUNC) -- reports 0, so it
//       reads nothing, never advances past the request, and its recv-loop spins/blocks forever.
// We first PEEK the whole datagram into a host scratch (buffer >= our <=4KB dumps, so the host recv
// returns the real length even on macOS), then honor the guest's flags precisely: MSG_PEEK leaves the
// datagram queued; a real read consumes it (excess discarded, as for any DGRAM); MSG_TRUNC makes the
// return value the true length. *msgflags (if set) gets Linux MSG_TRUNC when the copy was truncated.
// gflags are Linux MSG_* flags. Returns bytes (per Linux) or -errno.
static int64_t nl_recv(int fd, struct iovec *iov, int iovn, int gflags, int *msgflags) {
    uint8_t hb[8192]; // dumps are <=4096 (see nl_emit_dump's out[]); big enough to peek the full length
    ssize_t truelen;
    int hpeek = MSG_PEEK | ((gflags & 0x40 /* Linux MSG_DONTWAIT */) ? MSG_DONTWAIT : 0);
    int hread = (gflags & 0x40 /* Linux MSG_DONTWAIT */) ? MSG_DONTWAIT : 0;
    do {
        truelen = recv(fd, hb, sizeof hb, hpeek);
    } while (truelen < 0 && errno == EINTR);
    if (truelen < 0) {
        if (msgflags) *msgflags = 0;
        return -errno;
    }
    size_t cap = 0;
    for (int i = 0; i < iovn; i++)
        cap += iov[i].iov_len;
    size_t copylen = (size_t)truelen < cap ? (size_t)truelen : cap;
    if (!(gflags & 0x2 /*MSG_PEEK*/)) { // real read: consume the whole datagram (rest discarded, DGRAM)
        ssize_t consumed;
        do {
            consumed = recv(fd, hb, sizeof hb, hread);
        } while (consumed < 0 && errno == EINTR);
        if (consumed < 0) {
            if (msgflags) *msgflags = 0;
            return -errno;
        }
    }
    size_t got = nl_scatter(hb, copylen, iov, iovn);
    if (msgflags) *msgflags = ((size_t)truelen > got) ? 0x20 /*Linux MSG_TRUNC*/ : 0;
    return (gflags & 0x20 /*MSG_TRUNC*/) ? (int64_t)truelen : (int64_t)got;
}

// Queue one NLMSG_ERROR (type 2) reply to `peer` for the request whose 16-byte header is at `req_hdr`.
// Linux nlmsgerr = { s32 error; nlmsghdr orig_request; }: error<0 is an errno, error==0 is a plain ACK
// (sent only when the request set NLM_F_ACK). We echo the request's seq/header exactly as the kernel does
// so libnetlink/glibc match the reply to the outstanding request.
static void nl_error(int peer, const uint8_t *req_hdr, int err) {
    uint8_t out[36];
    memset(out, 0, sizeof out);
    *(uint32_t *)(out + 0) = 36;                              // nlmsg_len = hdr(16) + err(4) + echoed hdr(16)
    *(uint16_t *)(out + 4) = 2;                               // NLMSG_ERROR
    *(uint16_t *)(out + 6) = 0;                               // nlmsg_flags
    *(uint32_t *)(out + 8) = *(const uint32_t *)(req_hdr + 8); // echo the request's seq
    *(uint32_t *)(out + 12) = (uint32_t)getpid();            // nlmsg_pid == our port id (matches the dumps)
    *(int32_t *)(out + 16) = (int32_t)err;                   // nlmsgerr.error (negative errno; 0 == ACK)
    memcpy(out + 20, req_hdr, 16);                           // nlmsgerr.msg = echoed request header
    ssize_t w = send(peer, out, sizeof out, 0);
    (void)w;
}

// A send on a netlink fd: walk the request's nlmsghdr(s) and queue each one's reply. Returns bytes
// consumed (== len; requests are tiny) so the guest's send returns success.
static int64_t nl_send(int fd, const uint8_t *buf, size_t len) {
    int peer = g_nl_peer[fd] - 1;
    size_t off = 0;
    while (off + 16 <= len) {
        uint32_t nlen = *(const uint32_t *)(buf + off);
        uint16_t ntype = *(const uint16_t *)(buf + off + 4);
        uint32_t nseq = *(const uint32_t *)(buf + off + 8);
        // RTM message groups run base+0=NEW, +1=DEL, +2=GET, +3=SET. A GET (type%4==2) is a read the dump
        // responder answers; a NEW/DEL/SET (type%4!=2) is a MODIFICATION dd has no writable netlink stack to
        // apply. Reply to modifications with a real NLMSG_ERROR (-EPERM) instead of the old phantom empty
        // NLMSG_DONE, so `ip addr/route add`/SETLINK fail loudly rather than silently succeeding unchanged.
        if (ntype >= 16 && (ntype % 4) != 2)
            nl_error(peer, buf + off, -EPERM);
        else
            nl_emit_dump(peer, ntype, nseq);
        if (nlen < 16 || off + ((nlen + 3) & ~3u) <= off) break; // malformed -> stop
        off += (nlen + 3) & ~3u;
    }
    return (int64_t)len;
}

// ===== Socket ioctls (SIOCGIF*): the ioctl half of the shared lo+eth0 model ================
// busybox `ifconfig` (and any getifaddrs-free tool) enumerates via socket ioctls, not netlink: an
// AF_INET SOCK_DGRAM socket + SIOCGIFCONF/SIOCGIFADDR/SIOCGIFFLAGS/... . macOS has these too, but its
// kernel knows nothing of our synthesized container interfaces, so it returned ENOTTY -> "ioctl 0x8912
// failed: Not a tty". We answer them from the SAME lo+eth0 model the netlink responder + procfs use
// (netif_* in state.c), writing the Linux struct layouts directly (guest expects Linux sockaddr_in:
// family u16 @0). Dispatched from the ioctl handler (fs.c) for socket fds; guest result pointers are
// range-checked (-EFAULT) since we memcpy into them directly rather than via a bounds-checking syscall.
#define LX_IFNAMSIZ 16
#define LX_IFREQ_SZ 40 // sizeof(struct ifreq) on 64-bit Linux: name[16] + 24-byte union

// The two modelled interfaces, filled per slot (0=lo, 1=eth0). All IPv4 fields are network-order held
// as a host u32 (a | b<<8 | c<<16 | d<<24), matching netif_eth0_ip()'s encoding.
struct nif {
    const char *name;
    int index, mtu;
    uint32_t ip, mask, bcast;
    uint16_t flags, arphrd;
    uint8_t mac[6];
};

// prefixlen -> IPv4 netmask (network-order-as-host-u32). /16 -> 255.255.0.0 (0x0000ffff).
static uint32_t netif_mask_be(int prefix) {
    uint8_t m[4] = {0, 0, 0, 0};
    for (int i = 0; i < 4; i++) {
        int bits = prefix - i * 8;
        m[i] = bits >= 8 ? 0xff : bits > 0 ? (uint8_t)(0xff << (8 - bits)) : 0;
    }
    return (uint32_t)(m[0] | (m[1] << 8) | (m[2] << 16) | (m[3] << 24));
}

static void nif_get(int slot, struct nif *o) {
    memset(o, 0, sizeof *o);
    if (slot == 0) { // lo: 127.0.0.1/8, UP|LOOPBACK|RUNNING
        o->name = "lo";
        o->index = 1;
        o->ip = 0x0100007fu;   // 127.0.0.1
        o->mask = 0x000000ffu; // 255.0.0.0 (/8)
        o->mtu = 65536;
        o->flags = 0x49; // IFF_UP|IFF_LOOPBACK|IFF_RUNNING
        o->arphrd = 772; // ARPHRD_LOOPBACK
    } else {             // eth0: the bridge IP, UP|BROADCAST|RUNNING|MULTICAST
        o->name = "eth0";
        o->index = 2;
        o->ip = netif_eth0_ip();
        o->mask = netif_mask_be(netif_eth0_prefix());
        o->bcast = netif_eth0_bcast();
        o->mtu = 1500;
        o->flags = 0x1043; // IFF_UP|IFF_BROADCAST|IFF_RUNNING|IFF_MULTICAST
        o->arphrd = 1;     // ARPHRD_ETHER
        netif_eth0_mac(o->mac);
    }
}

static int nif_by_name(const char *name, struct nif *o) {
    for (int i = 0; i < 2; i++) {
        nif_get(i, o);
        if (strcmp(o->name, name) == 0) return 1;
    }
    return 0;
}

static int nif_by_index(int idx, struct nif *o) {
    for (int i = 0; i < 2; i++) {
        nif_get(i, o);
        if (o->index == idx) return 1;
    }
    return 0;
}

// Write a Linux sockaddr_in { u16 family=AF_INET(2); u16 port=0; u32 addr; u8 pad[8] } into the 24-byte
// ifreq union at `u` (whole union cleared so stale caller bytes don't leak).
static void ifr_set_in(uint8_t *u, uint32_t addr_be) {
    memset(u, 0, 24);
    *(uint16_t *)(u + 0) = 2; // AF_INET (Linux value)
    *(uint32_t *)(u + 4) = addr_be;
}

// Handle a socket ioctl against the lo+eth0 model. Returns 1 if `rq` is one we own (result in *out,
// 0 or -errno), 0 to let the caller fall through (non-socket-ioctl request).
static int net_ioctl(int fd, unsigned long rq, uint8_t *arg, int64_t *out) {
    if (rq < 0x8900 || rq > 0x89ff) return 0; // not a socket-device (SIOC*) ioctl -> caller's normal path
    // Must be a socket (Linux returns ENOTTY for these on a non-socket fd).
    {
        int ty;
        socklen_t tl = sizeof ty;
        if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &ty, &tl) < 0) {
            *out = -ENOTTY;
            return 1;
        }
    }
    if (rq == 0x8942) { // SIOCETHTOOL: busybox `ip link` probes it per interface. A real kernel answers
        // it (driver/link info); we don't model ethtool, but must not FAIL -- busybox prints
        // "ioctl 0x8942 failed" on ANY error (incl. EOPNOTSUPP). Report success, leaving the guest's
        // ethtool struct as it pre-zeroed it (plain `ip link`/`ip addr` display nothing from it), so the
        // output matches real docker's clean listing.
        *out = 0;
        return 1;
    }
    switch (rq) {
    case 0x8912: // SIOCGIFCONF
    case 0x8910: // SIOCGIFNAME
    case 0x8913: // SIOCGIFFLAGS
    case 0x8915: // SIOCGIFADDR
    case 0x8919: // SIOCGIFBRDADDR
    case 0x891b: // SIOCGIFNETMASK
    case 0x8921: // SIOCGIFMTU
    case 0x8927: // SIOCGIFHWADDR
    case 0x8933: // SIOCGIFINDEX
        break;
    default:
        // A socket ioctl we don't model (e.g. SIOCETHTOOL 0x8942). Report EOPNOTSUPP like a kernel that
        // lacks the op -- NOT ENOTTY: busybox `ip` prints "ioctl 0x.. failed" on any error except
        // EOPNOTSUPP, which it silently tolerates (matching real docker's clean `ip link` output). We
        // return the macOS ENOTSUP(45) value: svc_done's host->Linux errno xlate maps it to Linux
        // EOPNOTSUPP(95) (whereas macOS EOPNOTSUPP(102) would wrongly map to EINVAL).
        *out = -ENOTSUP;
        return 1;
    }
    if (rq == 0x8912) { // SIOCGIFCONF: fill an ifreq array (one per interface with an AF_INET addr)
        if (!host_range_mapped((uintptr_t)arg, 16)) {
            *out = -EFAULT;
            return 1;
        }
        int32_t ifc_len = *(int32_t *)(arg + 0);
        uint8_t *buf = (uint8_t *)*(uint64_t *)(arg + 8);
        int total = net_isolate() ? 1 : 2; // lo (+ eth0 unless --network none)
        if (!buf) {
            *(int32_t *)(arg + 0) = total * LX_IFREQ_SZ;
            *out = 0;
            return 1;
        } // size probe
        int maxn = ifc_len / LX_IFREQ_SZ;
        int n = maxn < total ? maxn : total;
        if (n > 0 && !host_range_mapped((uintptr_t)buf, (size_t)n * LX_IFREQ_SZ)) {
            *out = -EFAULT;
            return 1;
        }
        for (int i = 0; i < n; i++) {
            struct nif nif;
            nif_get(i, &nif);
            uint8_t *e = buf + (size_t)i * LX_IFREQ_SZ;
            memset(e, 0, LX_IFREQ_SZ);
            snprintf((char *)e, LX_IFNAMSIZ, "%s", nif.name);
            ifr_set_in(e + LX_IFNAMSIZ, nif.ip);
        }
        *(int32_t *)(arg + 0) = n * LX_IFREQ_SZ;
        *out = 0;
        return 1;
    }
    // The remaining requests all operate on a single struct ifreq.
    if (!host_range_mapped((uintptr_t)arg, LX_IFREQ_SZ)) {
        *out = -EFAULT;
        return 1;
    }
    struct nif nif;
    uint8_t *u = arg + LX_IFNAMSIZ; // the ifreq union
    if (rq == 0x8910) {             // SIOCGIFNAME: index -> name
        if (!nif_by_index(*(int32_t *)u, &nif)) {
            *out = -ENODEV;
            return 1;
        }
        memset(arg, 0, LX_IFNAMSIZ);
        snprintf((char *)arg, LX_IFNAMSIZ, "%s", nif.name);
        *out = 0;
        return 1;
    }
    char name[LX_IFNAMSIZ + 1];
    memcpy(name, arg, LX_IFNAMSIZ);
    name[LX_IFNAMSIZ] = 0;
    if (!nif_by_name(name, &nif)) {
        *out = -ENODEV;
        return 1;
    }
    switch (rq) {
    case 0x8915: ifr_set_in(u, nif.ip); break;    // SIOCGIFADDR
    case 0x8919: ifr_set_in(u, nif.bcast); break; // SIOCGIFBRDADDR
    case 0x891b: ifr_set_in(u, nif.mask); break;  // SIOCGIFNETMASK
    case 0x8913:
        memset(u, 0, 24);
        *(int16_t *)u = (int16_t)nif.flags;
        break; // SIOCGIFFLAGS
    case 0x8921:
        memset(u, 0, 24);
        *(int32_t *)u = nif.mtu;
        break; // SIOCGIFMTU
    case 0x8933:
        memset(u, 0, 24);
        *(int32_t *)u = nif.index;
        break;   // SIOCGIFINDEX
    case 0x8927: // SIOCGIFHWADDR: sockaddr { u16 sa_family=ARPHRD_*; u8 mac[6] ... }
        memset(u, 0, 24);
        *(uint16_t *)u = nif.arphrd;
        memcpy(u + 2, nif.mac, 6);
        break;
    }
    *out = 0;
    return 1;
}

// ============================ Container DNS (embedded nameserver -> host resolver) ============================
// The container's /etc/resolv.conf (provisioned by the daemon) points at 127.0.0.11 -- dd's embedded
// nameserver, the same address Docker uses. glibc/musl in the guest then send DNS as UDP (default) or TCP
// (fallback) to 127.0.0.11:53. We intercept those sends here, parse the query, resolve it via the macOS
// host resolver (getaddrinfo / getnameinfo -- which honor the host's system DNS, INCLUDING a corporate
// VPN's split-DNS, exactly like the ddcli-mac container), synthesize a wire-format DNS response, and make
// it readable on the guest socket. The guest fd is swapped to one end of an AF_UNIX socketpair; the
// response is written into the engine-held peer end, so poll/select/epoll + recv/read all see a real fd
// with real buffered data (no polling/timeout hacks). recvfrom/recvmsg report the source as 127.0.0.11:53
// so glibc/musl's anti-spoofing "answer came from the nameserver we asked" check passes.
//
// Coverage: A (1), AAAA (28), PTR (12, in-addr.arpa + ip6.arpa reverse), CNAME chains (flattened -- the
// resolved addresses are returned under the queried name, which is what getaddrinfo/gethostbyname consume),
// NXDOMAIN (name has no address of any family), NODATA (name exists but not this type), SERVFAIL (transient
// resolver error), multiple answers, TTL. Other qtypes (MX/TXT/SRV/SOA/NS/CAA/HTTPS/SVCB...) return NOERROR
// with no answer (NODATA), so a client falls back to A/AAAA -- see the tracked-remaining note in the report.
#define DD_DNS_NS 0x0b00007fu    // 127.0.0.11, network byte order (bytes 7f 00 00 0b == LE u32 0x0b00007f)
static uint8_t g_dns_sock[DD_NFD]; // fd -> 1 if this fd is an intercepted, socketpair-backed DNS socket
static int g_dns_peer[DD_NFD];     // fd -> engine-held socketpair end we write synthesized responses into

// DNS interception is off under --network none (DD_NET_ISOLATE): Docker's null network has no resolver, so
// :53 to 127.0.0.11 is left to fall through to the (dead) host loopback and name resolution fails, matching.
static int g_dns_off = -1;

static int dns_enabled(void) {
    if (g_dns_off < 0) g_dns_off = (getenv("DD_NET_ISOLATE") != NULL);
    return !g_dns_off;
}

// A Linux sockaddr_in destined for the embedded nameserver 127.0.0.11:53 (family value 2 == macOS AF_INET).
static int dns_dest_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 8 && *(const uint16_t *)sa == AF_INET && *(const uint16_t *)(sa + 2) == htons(53) &&
           *(const uint32_t *)(sa + 4) == DD_DNS_NS;
}

// Report the nameserver's address (127.0.0.11:53) back to the guest as the packet source / peer.
static void dns_fill_ns(uint8_t *sa, socklen_t *l) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(53);
    *(uint32_t *)(sa + 4) = DD_DNS_NS;
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// Swap the guest's AF_INET DNS socket for one end of an AF_UNIX socketpair (keeping the fd number + flags);
// stash the other end so send handlers can push synthesized responses into it. Idempotent per fd.
static int dns_swap(int fd, int stream) {
    if (fd < 0 || fd >= DD_NFD) return -1;
    if (g_dns_sock[fd]) return 0;
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    int sv[2];
    if (socketpair(AF_UNIX, stream ? SOCK_STREAM : SOCK_DGRAM, 0, sv) < 0) return -1;
    if (sv[0] != fd) {
        if (dup2(sv[0], fd) < 0) {
            close(sv[0]);
            close(sv[1]);
            return -1;
        }
        close(sv[0]);
    }
    if (fl >= 0 && (fl & O_NONBLOCK)) fcntl(fd, F_SETFL, O_NONBLOCK);
    if (df >= 0 && (df & FD_CLOEXEC)) fcntl(fd, F_SETFD, FD_CLOEXEC);
    fcntl(sv[1], F_SETFD, FD_CLOEXEC); // engine end never leaks across a guest execve
    g_dns_peer[fd] = sv[1];
    g_dns_sock[fd] = 1;
    return 0;
}

// Encode a dotted host name into DNS wire label format (len-prefixed labels + a 0 terminator). -1 if it
// wouldn't fit. A trailing dot / empty labels are skipped.
static int dns_enc_name(uint8_t *out, int cap, const char *name) {
    int o = 0;
    const char *p = name;
    while (*p) {
        const char *dot = strchr(p, '.');
        int len = dot ? (int)(dot - p) : (int)strlen(p);
        if (len > 63) return -1;
        if (len == 0) {
            if (dot) {
                p = dot + 1;
                continue;
            }
            break;
        }
        if (o + 1 + len >= cap) return -1;
        out[o++] = (uint8_t)len;
        memcpy(out + o, p, len);
        o += len;
        if (!dot) break;
        p = dot + 1;
    }
    if (o + 1 > cap) return -1;
    out[o++] = 0;
    return o;
}

// Decode a DNS name in a QUESTION (no compression) at msg[off] into a dotted string; return the number of
// wire bytes consumed (incl the 0 terminator), or -1 on malformation.
static int dns_dec_qname(const uint8_t *msg, int len, int off, char *name, int ncap) {
    int no = 0, o = off;
    while (o < len) {
        int c = msg[o];
        if (c == 0) {
            o++;
            name[no < ncap ? no : ncap - 1] = 0;
            return o - off;
        }
        if (c & 0xc0) return -1; // a query name is never compressed
        o++;
        if (o + c > len) return -1;
        if (no && no < ncap - 1) name[no++] = '.';
        for (int i = 0; i < c && no < ncap - 1; i++)
            name[no++] = (char)msg[o + i];
        o += c;
    }
    return -1;
}

static int dns_hexval(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
}

static int dns_ci_suffix(const char *s, const char *suf) {
    size_t ls = strlen(s), lf = strlen(suf);
    return ls >= lf && strcasecmp(s + ls - lf, suf) == 0;
}

// Append one resource record header (name = compression pointer to the question at offset 12) + rdata.
static int dns_put_rr(uint8_t *a, int ao, int cap, uint16_t type, const uint8_t *rdata, int rdlen) {
    if (ao + 12 + rdlen > cap) return -1;
    a[ao++] = 0xc0;
    a[ao++] = 0x0c; // NAME -> ptr to the question's QNAME
    a[ao++] = (uint8_t)(type >> 8);
    a[ao++] = (uint8_t)type;
    a[ao++] = 0;
    a[ao++] = 1; // CLASS IN
    a[ao++] = 0;
    a[ao++] = 0;
    a[ao++] = 0;
    a[ao++] = 30; // TTL = 30s
    a[ao++] = (uint8_t)(rdlen >> 8);
    a[ao++] = (uint8_t)rdlen;
    memcpy(a + ao, rdata, rdlen);
    return ao + rdlen;
}

// Reverse (PTR) lookup: parse the in-addr.arpa / ip6.arpa qname, ask the host, emit a PTR RR. Sets *rcode
// (0 = NODATA when unparseable / no name). Returns the new answer offset.
static int dns_answer_ptr(const char *qname, uint8_t *a, int ao, int cap, int *pan) {
    struct sockaddr_storage ss;
    memset(&ss, 0, sizeof ss);
    socklen_t sl = 0;
    if (dns_ci_suffix(qname, "in-addr.arpa")) {
        unsigned d, cc, b, aa;
        if (sscanf(qname, "%u.%u.%u.%u", &d, &cc, &b, &aa) == 4 && d < 256 && cc < 256 && b < 256 && aa < 256) {
            struct sockaddr_in *s = (struct sockaddr_in *)&ss;
            s->sin_family = AF_INET;
            uint8_t *ip = (uint8_t *)&s->sin_addr;
            ip[0] = (uint8_t)aa;
            ip[1] = (uint8_t)b;
            ip[2] = (uint8_t)cc;
            ip[3] = (uint8_t)d; // qname octets are reversed
            sl = sizeof *s;
        }
    } else if (dns_ci_suffix(qname, "ip6.arpa")) {
        uint8_t nib[32];
        int n = 0;
        for (const char *p = qname; *p && n < 33; p++) {
            if (*p == '.') continue;
            int v = dns_hexval(*p);
            if (v < 0) break;
            if (n < 32) nib[n] = (uint8_t)v;
            n++;
        }
        if (n == 32) {
            struct sockaddr_in6 *s = (struct sockaddr_in6 *)&ss;
            s->sin6_family = AF_INET6;
            uint8_t *ip = (uint8_t *)&s->sin6_addr;
            for (int i = 0; i < 16; i++)
                ip[i] = (uint8_t)((nib[31 - 2 * i] << 4) | nib[31 - 2 * i - 1]);
            sl = sizeof *s;
        }
    }
    if (!sl) return ao; // unparseable -> NODATA (rcode already 0)
    char host[NI_MAXHOST];
    if (getnameinfo((struct sockaddr *)&ss, sl, host, sizeof host, NULL, 0, NI_NAMEREQD) != 0) return ao; // NODATA
    uint8_t enc[300];
    int el = dns_enc_name(enc, sizeof enc, host);
    if (el < 0) return ao;
    int nao = dns_put_rr(a, ao, cap, 12 /*PTR*/, enc, el);
    if (nao < 0) return ao;
    (*pan)++;
    return nao;
}

// reach-by-name: resolve a bare container/alias name to a same-network peer's IP from the daemon's
// LIVE per-network table at `<g_netbr>/.names` (one "ip\tname" line per endpoint, rewritten by the daemon
// on every container start -- so a peer that joined AFTER this container launched, and is therefore absent
// from this container's frozen /etc/hosts snapshot, is still resolvable). Consulted BEFORE the macOS host
// resolver so container names stay instant + offline and never leak to external DNS. Gated to user networks
// (the file is only written for those -- Docker withholds embedded-DNS names on the default bridge).
// Returns 1 + fills *ip_be (network byte order) on a case-insensitive name match; 0 otherwise.
static int dns_local_lookup(const char *qname, uint32_t *ip_be) {
    if (!qname || !qname[0]) return 0;
    br_init(); // ensure g_netbr is populated from DD_NETBR (idempotent)
    if (!g_netbr[0]) return 0;
    char path[256];
    snprintf(path, sizeof path, "%s/.names", g_netbr);
    FILE *f = fopen(path, "re");
    if (!f) return 0;
    char line[512];
    int found = 0;
    while (fgets(line, sizeof line, f)) {
        char *tab = strchr(line, '\t');
        if (!tab) continue;
        *tab = 0;
        char *name = tab + 1;
        size_t nl = strlen(name);
        while (nl && (name[nl - 1] == '\n' || name[nl - 1] == '\r' || name[nl - 1] == ' ' || name[nl - 1] == '\t'))
            name[--nl] = 0;
        if (strcasecmp(name, qname) == 0) {
            uint32_t ip = br_parse_ip(line);
            if (ip) {
                if (ip_be) *ip_be = ip;
                found = 1;
                break;
            }
        }
    }
    fclose(f);
    return found;
}

// Build a wire-format response for a wire-format query. Returns the response length, or -1 if the query is
// too malformed to answer at all.
static int dns_build_response(const uint8_t *q, int qlen, uint8_t *out, int cap) {
    if (qlen < 12 || cap < 12) return -1;
    uint16_t id = (uint16_t)((q[0] << 8) | q[1]);
    uint16_t qflags = (uint16_t)((q[2] << 8) | q[3]);
    int opcode = (qflags >> 11) & 0xf;
    uint16_t qd = (uint16_t)((q[4] << 8) | q[5]);
    if (qd < 1) return -1;
    char qname[256];
    int nl = dns_dec_qname(q, qlen, 12, qname, sizeof qname);
    if (nl < 0 || 12 + nl + 4 > qlen) return -1;
    int qtoff = 12 + nl;
    uint16_t qtype = (uint16_t)((q[qtoff] << 8) | q[qtoff + 1]);
    uint16_t qclass = (uint16_t)((q[qtoff + 2] << 8) | q[qtoff + 3]);
    int qsectlen = nl + 4; // the single question we echo back verbatim

    int rcode = 0, ancount = 0;
    uint8_t ans[1600];
    int ao = 0;

    if (opcode != 0 || qclass != 1) {
        rcode = 4;                          // not a standard IN query -> NOTIMP
    } else if (qtype == 1 || qtype == 28) { // A / AAAA
        // Reach-by-name: a same-network peer resolved from the daemon's live table wins over the host
        // resolver (instant, offline, and container names must never escape to external DNS). Local
        // endpoints are IPv4 only: A -> one A RR; AAAA -> NOERROR with no answer (NODATA, name exists but
        // has no v6), which is exactly what Docker's embedded DNS returns for a v4-only service.
        uint32_t local_ip = 0;
        if (dns_local_lookup(qname, &local_ip)) {
            if (qtype == 1) {
                int nao = dns_put_rr(ans, ao, sizeof ans, 1, (uint8_t *)&local_ip, 4);
                if (nao >= 0) {
                    ao = nao;
                    ancount++;
                }
            }
            // rcode stays 0 (NOERROR); AAAA falls through with 0 answers (NODATA). Skip the host resolver.
            goto emit;
        }
        struct addrinfo hints, *res = NULL, *ai;
        memset(&hints, 0, sizeof hints);
        hints.ai_family = AF_UNSPEC; // learn whether the name exists at ALL (so we can tell NXDOMAIN vs NODATA)
        hints.ai_socktype = SOCK_STREAM;
        int grc = getaddrinfo(qname, NULL, &hints, &res);
        if (grc == EAI_NONAME)
            rcode = 3; // NXDOMAIN: the name has no address of any family
        else if (grc != 0)
            rcode = 2; // SERVFAIL: transient resolver failure (EAI_AGAIN/EAI_FAIL/EAI_SYSTEM/...)
        else {
            int want = (qtype == 28) ? AF_INET6 : AF_INET;
            for (ai = res; ai; ai = ai->ai_next) {
                if (ai->ai_family != want) continue;
                if (want == AF_INET) {
                    struct sockaddr_in *s = (struct sockaddr_in *)ai->ai_addr;
                    int nao = dns_put_rr(ans, ao, sizeof ans, 1, (uint8_t *)&s->sin_addr, 4);
                    if (nao < 0) break;
                    ao = nao;
                } else {
                    struct sockaddr_in6 *s = (struct sockaddr_in6 *)ai->ai_addr;
                    int nao = dns_put_rr(ans, ao, sizeof ans, 28, (uint8_t *)&s->sin6_addr, 16);
                    if (nao < 0) break;
                    ao = nao;
                }
                ancount++;
            }
            // ancount==0 here means the name exists but not in the requested family -> NODATA (rcode 0).
        }
        if (res) freeaddrinfo(res);
    } else if (qtype == 12) { // PTR (reverse)
        ao = dns_answer_ptr(qname, ans, ao, (int)sizeof ans, &ancount);
    } else {
        // MX/TXT/SRV/SOA/NS/CAA/HTTPS/SVCB/...: NOERROR + no answer (NODATA) so the client falls back.
        rcode = 0;
    }

emit:; // local-name A/AAAA answer assembled above jumps here, skipping the host resolver
    int need = 12 + qsectlen + ao;
    int tc = 0;
    if (need > cap) { // would overflow (UDP 512) -> truncate: drop answers + set TC so the client retries via TCP
        ao = 0;
        ancount = 0;
        tc = 1;
        need = 12 + qsectlen;
        if (need > cap) return -1;
    }
    out[0] = (uint8_t)(id >> 8);
    out[1] = (uint8_t)id;
    uint16_t rflags = (uint16_t)(0x8000 | (qflags & 0x0100) | 0x0080 | (tc ? 0x0200 : 0) | (rcode & 0xf));
    out[2] = (uint8_t)(rflags >> 8);
    out[3] = (uint8_t)rflags;
    out[4] = 0;
    out[5] = 1; // QDCOUNT
    out[6] = (uint8_t)(ancount >> 8);
    out[7] = (uint8_t)ancount;               // ANCOUNT
    out[8] = out[9] = out[10] = out[11] = 0; // NS/AR counts
    memcpy(out + 12, q + 12, qsectlen);      // echo the question
    memcpy(out + 12 + qsectlen, ans, ao);
    return 12 + qsectlen + ao;
}

// Process one query buffer sent on a DNS socket: build the response and push it into the socketpair peer so
// the guest fd becomes readable. `stream` selects the 2-byte-length-prefixed TCP framing. Returns the byte
// count to report as "sent" (always the whole query -- the guest sees a normal send).
static int64_t dns_send(int fd, const uint8_t *buf, size_t len, int stream) {
    const uint8_t *q = buf;
    size_t qn = len;
    if (stream) {
        if (len < 2) return (int64_t)len; // partial length prefix -> best-effort (resolvers send it whole)
        size_t plen = (size_t)((buf[0] << 8) | buf[1]);
        q = buf + 2;
        qn = len - 2;
        if (qn > plen) qn = plen;
    }
    uint8_t resp[2100];
    int hdr = stream ? 2 : 0;
    int rl = dns_build_response(q, (int)qn, resp + hdr, (int)sizeof resp - hdr);
    if (rl < 0) return (int64_t)len; // unparseable -> swallow (guest retries/times out), never crash
    if (stream) {
        resp[0] = (uint8_t)(rl >> 8);
        resp[1] = (uint8_t)rl;
        rl += 2;
    }
    if (fd >= 0 && fd < DD_NFD && g_dns_sock[fd] && g_dns_peer[fd] >= 0) {
        ssize_t w = write(g_dns_peer[fd], resp, (size_t)rl);
        (void)w;
    }
    return (int64_t)len;
}

// Send-path entry used by sendto/send/sendmsg/sendmmsg (net.c) + write/writev (io.c). If `fd` is already a
// DNS socket, or `dst` targets 127.0.0.11:53 (lazy first-datagram swap for the unconnected sendto path),
// handle it and set *ret; otherwise return 0 so the caller runs the normal socket path.
static int dns_try_send(int fd, const uint8_t *buf, size_t len, const uint8_t *dst, socklen_t dstlen, int64_t *ret) {
    if (!dns_enabled()) return 0;
    int is_dns = (fd >= 0 && fd < DD_NFD && g_dns_sock[fd]);
    if (!is_dns) {
        if (!dns_dest_is(dst, dstlen)) return 0;
        int stream = (fd >= 0 && fd < DD_NFD) ? g_sock_stream[fd] : 0;
        if (dns_swap(fd, stream) < 0) return 0; // couldn't swap -> let the normal path try
    }
    int stream = (fd >= 0 && fd < DD_NFD) ? g_sock_stream[fd] : 0;
    *ret = dns_send(fd, buf, len, stream);
    return 1;
}

// Gather an iovec array into a scratch buffer (shared by the sendmsg/sendmmsg DNS paths).
static size_t dns_gather(const struct iovec *iv, int ivn, uint8_t *tmp, size_t cap) {
    size_t tl = 0;
    for (int i = 0; iv && i < ivn && tl < cap; i++) {
        size_t n = iv[i].iov_len;
        if (tl + n > cap) n = cap - tl;
        memcpy(tmp + tl, iv[i].iov_base, n);
        tl += n;
    }
    return tl;
}

struct loaded {
    uint64_t entry, phdr, base;
    int phent, phnum;
};
