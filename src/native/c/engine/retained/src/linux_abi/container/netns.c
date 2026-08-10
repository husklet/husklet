// hl/linux_abi/container -- termios (Linux<->macOS) + NET-ns private loopback (127/8 -> AF_UNIX).

#include "../host_socket.h" // container DNS: getaddrinfo/getnameinfo via the macOS host resolver (dns_* below)

#include "../shared.h"
#include "../../host/libc_compat.h" // hl_compat_mkdir: the UCRT's mkdir takes no mode
#include "../checkpoint.h"
#include "socket_identity.h"

// Build a pathname AF_UNIX address without ever accepting the silent truncation performed by snprintf.
// Callers must do this before replacing a guest socket so ENAMETOOLONG leaves the original fd untouched.
static int unix_addr_set(struct sockaddr_un *address, const char *path) {
    size_t len = path ? strlen(path) : 0;
    if (!address || !path || len >= sizeof address->sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memset(address, 0, sizeof *address);
    address->sun_family = AF_UNIX;
    memcpy(address->sun_path, path, len + 1);
    return 0;
}

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

// Linux termios baud CODE (Bxxx in c_cflag CBAUD/CIBAUD) <-> numeric bits/s (the macOS speed_t form,
// and what cf{set,get}speed operate on). Standard rates only; custom BOTHER rates are not modeled here.
// Linux CBAUD mask is 0x100f (CBAUDEX 0x1000 | 0x000f); the input speed lives in CIBAUD (that field << 16).
#define TIO_CBAUD 0x100fu
#define TIO_CIBAUD_SHIFT 16
static const uint32_t TIO_BAUD[][2] = {
    {0, 0},           {1, 50},          {2, 75},          {3, 110},         {4, 134},         {5, 150},
    {6, 200},         {7, 300},         {8, 600},         {9, 1200},        {0xa, 1800},      {0xb, 2400},
    {0xc, 4800},      {0xd, 9600},      {0xe, 19200},     {0xf, 38400},     {0x1001, 57600},  {0x1002, 115200},
    {0x1003, 230400}, {0x1004, 460800}, {0x1005, 500000}, {0x1006, 576000}, {0x1007, 921600}, {0x1008, 1000000}};

static uint32_t baud_code_to_num(uint32_t code) {
    for (unsigned i = 0; i < sizeof TIO_BAUD / sizeof TIO_BAUD[0]; i++)
        if (TIO_BAUD[i][0] == code) return TIO_BAUD[i][1];
    return 0;
}

static uint32_t baud_num_to_code(uint32_t num) {
    for (unsigned i = 0; i < sizeof TIO_BAUD / sizeof TIO_BAUD[0]; i++)
        if (TIO_BAUD[i][1] == num) return TIO_BAUD[i][0];
    return 0;
}

// bind()/connect() an AF_UNIX socket at host path `host`. macOS sun_path is only 104 bytes, but a container
// overlay upper socket path ($HOME/.hl/containers/<64-hex>/upper/.../.s.PGSQL.5432) can exceed that -- a
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
        if (fchdir(cwd) != 0) {
            rc = -1;
            e = errno;
        }
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
        if (fchdir(cwd) != 0) {
            rc = -1;
            e = errno;
        }
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
    // Carry the line speed: map the Linux CBAUD (output) / CIBAUD (input) codes to numeric bits/s so the
    // host termios keeps the requested rate. map_bits above ignores the baud field, so without this the
    // speed collapses to B0 (a cfgetispeed/cfgetospeed round-trip then reads 0). An input code of 0 means
    // "same as output" on Linux.
    uint32_t ocode = lc & TIO_CBAUD, icode = (lc >> TIO_CIBAUD_SHIFT) & TIO_CBAUD;
    if (icode == 0) icode = ocode;
    cfsetospeed(M, baud_code_to_num(ocode));
    cfsetispeed(M, baud_code_to_num(icode));
}

static void termios_m2l(const struct termios *M, uint8_t *L) {
    memset(L, 0, 36);
    uint32_t li = map_bits((uint32_t)M->c_iflag, TIO_I, 13, 0), lo = map_bits((uint32_t)M->c_oflag, TIO_O, 5, 0);
    uint32_t lc = map_bits((uint32_t)M->c_cflag, TIO_C, 6, 0), ll = map_bits((uint32_t)M->c_lflag, TIO_L, 9, 0);
    int csz = M->c_cflag & CSIZE;
    lc |= (csz == CS8 ? 0x30 : csz == CS7 ? 0x20 : csz == CS6 ? 0x10 : 0);
    // Encode the host line speed back into the Linux CBAUD (output) / CIBAUD (input) fields.
    uint32_t ocode = baud_num_to_code((uint32_t)cfgetospeed(M)), icode = baud_num_to_code((uint32_t)cfgetispeed(M));
    lc = (lc & ~TIO_CBAUD) | (ocode & TIO_CBAUD);
    lc = (lc & ~(TIO_CBAUD << TIO_CIBAUD_SHIFT)) | ((icode & TIO_CBAUD) << TIO_CIBAUD_SHIFT);
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
#if defined(__linux__) || defined(_WIN32)
    return lf;
#else
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
#endif
}

// macOS MSG_* -> Linux MSG_* (inverse of msgflags_l2m; used for recvmsg msg_flags writeback). The
// returned-flags set differs: notably MSG_CTRUNC is macOS 0x20 / Linux 0x8, MSG_TRUNC macOS 0x10 /
// Linux 0x20, MSG_EOR macOS 0x8 / Linux 0x80. OOB/DONTROUTE map straight through.
static int msgflags_m2l(int mf) {
#if defined(__linux__) || defined(_WIN32)
    return mf;
#else
    // MSG_OOB(0x1)/MSG_DONTROUTE(0x4) identical; MSG_PEEK isn't a returned flag but is harmless
    int lf = mf & (0x1 | 0x2 | 0x4);
    // MSG_TRUNC: macOS 0x10 -> Linux 0x20
    if (mf & 0x10) lf |= 0x20;
    // MSG_CTRUNC: macOS 0x20 -> Linux 0x8
    if (mf & 0x20) lf |= 0x8;
    // MSG_EOR: macOS 0x8 -> Linux 0x80
    if (mf & 0x8) lf |= 0x80;
    return lf;
#endif
}

// Shared ownership metadata for macOS's DGRAM-backed Linux SOCK_SEQPACKET emulation. Definitions live
// ahead of ancillary translation because SCM_RIGHTS send/receive participates in the same lifetime.
#define SEQ_REF_N 4096

struct seq_ref {
    volatile uint32_t used;
    volatile uint32_t refs[2];
    volatile uint32_t pending[2];
};
static struct seq_ref *g_seq_refs;
static uint16_t g_seq_ref[HL_NFD];
static uint8_t g_seq_end[HL_NFD];

// ---- SCM ancillary data: Linux<->macOS cmsg framing translation (SOL_SOCKET/SCM_RIGHTS fd passing).
// hl uses host fds directly as guest fds, so the fd integers in an SCM_RIGHTS payload need no remap --
// only the cmsg framing differs: Linux hdr=16B (8B len @0, int level @8, int type @12), 8-byte align,
// SOL_SOCKET=1; macOS hdr=12B (4B len @0, int level @4, int type @8), 4-byte align, SOL_SOCKET=0xffff.
#define LX_CMSG_ALIGN(n) (((n) + 7u) & ~(size_t)7u) // Linux: 8-byte align
#define LX_CMSGHDR 16u                              // Linux cmsg header: 8(len)+4(level)+4(type)
#define LX_SOL_SOCKET 1
#define HL_CMSG_EVENTFD_MAGIC 0xddefd001u
#define HL_CMSG_SEQ_MAGIC 0xdd5e9001u
#define HL_CMSG_TIMERFD_MAGIC 0xdd71e001u
#define HL_CMSG_OFD_MAGIC 0xdd0fd001u

/* Stable open-file-description identity.  This lives before ancillary translation because SCM_RIGHTS
 * import/export must carry it; dup/close helpers later in the unified syscall translation unit share it. */
static uint64_t g_ofd_id[HL_NFD];
static _Atomic uint32_t g_ofd_next = 1;

static uint64_t ofd_identity_new(void) {
    uint32_t sequence = atomic_fetch_add_explicit(&g_ofd_next, 1u, memory_order_relaxed);
    if (sequence == 0) sequence = atomic_fetch_add_explicit(&g_ofd_next, 1u, memory_order_relaxed);
    return UINT64_C(0x4000000000000000) | ((uint64_t)(uint32_t)getpid() << 32) | sequence;
}

static uint64_t ofd_identity_ensure(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    if (!g_ofd_id[fd]) g_ofd_id[fd] = ofd_identity_new();
    return g_ofd_id[fd];
}

struct hl_cmsg_eventfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t slot;
    uint32_t sema;
    uint32_t nb; // guest EFD_NONBLOCK intent (g_eventfd_gnb) — the host fd is always O_NONBLOCK internally
};

struct hl_cmsg_seq_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t slot;
    uint32_t end;
};

// A timerfd is an engine-emulated object (kqueue-shim host fd + per-fd deadline/interval/clock
// bookkeeping the timerfd read/gettime paths consult, keyed by fd number). Passing one over SCM_RIGHTS
// dups the shared host object into the receiver, but the receiver's fd number carries none of the
// routing state, so its read/poll would miss the timerfd path entirely. Carry that scalar state in a
// hidden trailer marker (mirroring the eventfd/seq trailers) so the received fd behaves like a dup.
struct hl_cmsg_timerfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint32_t first_oneshot;
    int32_t clock;
    int64_t deadline;
    int64_t interval;
    int32_t source_fd;  // sender's fd number (valid only within source_pid)
    int32_t source_pid; // sender engine pid: re-alias the host kqueue only when the receiver shares it
    uint32_t nb;
    uint32_t portable;
    uint32_t restore_shared;
    uint32_t reserved;
    uint64_t object;
    uint64_t shared_state; // restore-only MAP_SHARED address inherited by the reforked process tree
};

struct hl_cmsg_ofd_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint64_t identity;
};

struct hl_cmsg_memfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t seals;
    uint32_t reserved;
};

struct hl_cmsg_pipe_meta {
    uint32_t magic;
    uint32_t ordinal;
    uint64_t identity;
    int32_t size;
    uint32_t reserved;
};

struct hl_cmsg_signalfd_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t source_pid;
    int32_t source_slot;
    uint64_t mask;
};

struct hl_cmsg_kqueue_meta {
    uint32_t magic;
    uint32_t ordinal;
    int32_t source_pid;
    int32_t source_fd;
    uint32_t kind;
    uint32_t nonblock;
    uint64_t object_id;
    uint32_t descriptor_flags;
    int32_t canonical_fd;
    uint32_t hidden_count;
    uint32_t reserved;
    uint64_t image_size;
};

struct hl_cmsg_epoll_watch {
    int32_t descriptor;
    uint32_t events;
    uint32_t armed;
    uint32_t reserved;
    uint64_t data;
};

static int kqueue_scm_export(int fd, struct hl_cmsg_kqueue_meta *metadata);
static int kqueue_scm_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker);
static int epoll_scm_hidden_export(struct hl_cmsg_kqueue_meta *metadata, int *fds, int capacity);
static int epoll_scm_image_remap(const struct hl_cmsg_kqueue_meta *metadata, int marker, const int *fds);

static __thread int g_cmsg_tmpfds[1024];
static __thread uint8_t g_cmsg_tmpfd_borrowed[1024];
static __thread int g_cmsg_ntmpfds;
static __thread uint16_t g_cmsg_seq_slot[253];
static __thread uint8_t g_cmsg_seq_end[253];
static __thread int g_cmsg_nseq;
static __thread uint16_t g_cmsg_event_slot[253];
static __thread int g_cmsg_nevent;
static int bound_attachment_borrow(int guest_fd, int *native_fd);
static void bound_attachment_release(int native_fd);

static int cmsg_tmpfd_track(int fd, int borrowed) {
    if (fd < 0 || g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    g_cmsg_tmpfds[g_cmsg_ntmpfds] = fd;
    g_cmsg_tmpfd_borrowed[g_cmsg_ntmpfds] = (uint8_t)(borrowed != 0);
    g_cmsg_ntmpfds++;
    return 0;
}

static void cmsg_tmpfds_close(void) {
    for (int i = 0; i < g_cmsg_ntmpfds; i++)
        if (g_cmsg_tmpfds[i] >= 0) {
            if (g_cmsg_tmpfd_borrowed[i])
                bound_attachment_release(g_cmsg_tmpfds[i]);
            else
                close(g_cmsg_tmpfds[i]);
        }
    g_cmsg_ntmpfds = 0;
}

static void cmsg_seq_finish(int sent) {
    if (!sent && g_seq_refs) {
        for (int i = 0; i < g_cmsg_nseq; i++) {
            uint32_t slot = g_cmsg_seq_slot[i];
            uint32_t end = g_cmsg_seq_end[i];
            __atomic_sub_fetch(&g_seq_refs[slot].pending[end], 1, __ATOMIC_ACQ_REL);
            __atomic_sub_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
        }
    }
    g_cmsg_nseq = 0;
}

static void cmsg_event_finish(int sent) {
    if (!sent)
        for (int index = 0; index < g_cmsg_nevent; ++index) {
            uint32_t slot = g_cmsg_event_slot[index];
            if (slot < HL_NFD && g_eventfd_refs[slot] > 0) g_eventfd_refs[slot]--;
        }
    g_cmsg_nevent = 0;
}

static int cmsg_level_l2m(int lv) {
    return lv == LX_SOL_SOCKET ? SOL_SOCKET : lv;
}

static int cmsg_level_m2l(int lv) {
    return lv == SOL_SOCKET ? LX_SOL_SOCKET : lv;
}

static int cmsg_eventfd_marker(const struct hl_cmsg_eventfd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-cmsgXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_seq_marker(const struct hl_cmsg_seq_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-seqXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_timerfd_marker(const struct hl_cmsg_timerfd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char tn[] = "/tmp/.hl-tfdXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

// ---- SCM_RIGHTS in-flight retention (macOS only) -------------------------------------------------
// Linux keeps a descriptor passed over an AF_UNIX socket alive for as long as the message carrying it
// sits in the receiving socket, so "sendmsg(fd) then close(fd)" -- the canonical fd-handoff idiom every
// broker/zygote uses -- is safe even if the peer has not called recvmsg yet. XNU does NOT: its unix-rights
// garbage collector (unp_gc) treats a file whose ONLY remaining reference is an in-flight message as
// unreachable and CLOSES it, and it runs asynchronously off any unix-socket close while rights are in
// flight. So the sender's close(2) can race a GC pass that has not yet observed the receiver's
// unp_externalize, and the passed socket is torn down under both peers: the receiver's read() reports EOF
// and the sender's next write() gets EPIPE (default disposition: SIGPIPE kills it). It is a scheduling
// accident, which is why it only shows up under load. Reproduced with a plain C program and no engine in
// the picture: ~1 in 150 runs on an idle machine, ~1 in 60 pinned to the efficiency band.
//
// Fix: keep an engine-private duplicate of every descriptor put in flight, so the file always has a
// reference that is NOT an in-flight message and can therefore never become GC-eligible. Every SCM_RIGHTS
// record already carries one OFD marker file per passed fd, so that marker doubles as the delivery
// receipt: the receiving engine stamps an ack byte past the metadata when it imports the trailer, and the
// sender drops the duplicate on the next sweep. Nothing about the guest-visible message changes.
#if defined(__linux__)
#define cmsg_inflight_sweep() ((void)0)
#define cmsg_inflight_hold(fd, marker) ((void)0)
#define cmsg_inflight_mark() ((void)0)
#define cmsg_inflight_finish(sent) ((void)(sent))
#define cmsg_inflight_ack(marker) ((void)(marker))
#define cmsg_inflight_is_retained(fd) (0)
#else
#define SCM_INFLIGHT_MAX 256
#define SCM_INFLIGHT_ACK_OFFSET ((off_t)sizeof(struct hl_cmsg_ofd_meta))

struct scm_inflight_hold {
    int retained; // engine-private duplicate of the passed descriptor
    int marker;   // engine-private duplicate of that fd's OFD marker file (the delivery receipt)
};
static struct scm_inflight_hold g_scm_hold[SCM_INFLIGHT_MAX];
static int g_scm_hold_n;
static int g_scm_hold_mark;

static void cmsg_inflight_drop(int index) {
    if (g_scm_hold[index].retained >= 0) close(g_scm_hold[index].retained);
    if (g_scm_hold[index].marker >= 0) close(g_scm_hold[index].marker);
    for (int i = index + 1; i < g_scm_hold_n; i++)
        g_scm_hold[i - 1] = g_scm_hold[i]; // keep oldest-first order so the eviction below is FIFO
    g_scm_hold_n--;
    if (g_scm_hold_mark > g_scm_hold_n) g_scm_hold_mark = g_scm_hold_n;
}

// Release every hold whose receipt has been stamped by the receiving engine.
static void cmsg_inflight_sweep(void) {
    for (int i = g_scm_hold_n - 1; i >= 0; i--) {
        uint8_t ack = 0;
        if (pread(g_scm_hold[i].marker, &ack, 1, SCM_INFLIGHT_ACK_OFFSET) == 1 && ack != 0) cmsg_inflight_drop(i);
    }
}

static void cmsg_inflight_hold(int fd, int marker) {
    // Bounded: a peer that never reads the message must not cost the sender descriptors without limit.
    // The oldest hold is the likeliest to have been consumed already, so evict that one.
    if (g_scm_hold_n >= SCM_INFLIGHT_MAX) cmsg_inflight_drop(0);
    int retained = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    if (retained < 0) return; // out of descriptors: degrade to bare XNU behaviour rather than fail the send
    int receipt = fcntl(marker, F_DUPFD_CLOEXEC, 0);
    if (receipt < 0) {
        close(retained);
        return;
    }
    g_scm_hold[g_scm_hold_n].retained = retained;
    g_scm_hold[g_scm_hold_n].marker = receipt;
    g_scm_hold_n++;
}

static void cmsg_inflight_mark(void) {
    g_scm_hold_mark = g_scm_hold_n;
}

// A send that never left the building put nothing in flight, so its holds are released immediately.
static void cmsg_inflight_finish(int sent) {
    if (!sent)
        while (g_scm_hold_n > g_scm_hold_mark)
            cmsg_inflight_drop(g_scm_hold_n - 1);
    g_scm_hold_mark = g_scm_hold_n;
}

// Receiver side: stamp the receipt so the sender can drop its duplicate.
static void cmsg_inflight_ack(int marker) {
    uint8_t ack = 1;
    (void)pwrite(marker, &ack, 1, SCM_INFLIGHT_ACK_OFFSET);
}

// The retained duplicates back the engine's own lifetime guarantee, not the guest's fd table: the
// emulated execve's close-on-exec sweep must leave them alone (Linux keeps an in-flight fd alive across
// the sender's exec too).
static int cmsg_inflight_is_retained(int fd) {
    for (int i = 0; i < g_scm_hold_n; i++)
        if (g_scm_hold[i].retained == fd || g_scm_hold[i].marker == fd) return 1;
    return 0;
}
#endif

static int cmsg_ofd_marker(const struct hl_cmsg_ofd_meta *m) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-ofdXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, m, sizeof *m) != (ssize_t)sizeof *m) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_ofd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_ofd_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != HL_CMSG_OFD_MAGIC)
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1) || !metadata.identity) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) g_ofd_id[fd] = metadata.identity;
        cmsg_inflight_ack(marker); // the fd is installed in this process now: the sender may drop its hold
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_memfd_marker(const struct hl_cmsg_memfd_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-memfd-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_memfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_memfd_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c4d46))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1)) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            g_memfd_is[fd] = 1;
            g_memfd_seal[fd] = metadata.seals;
            memfd_reg_set_fd(fd, metadata.seals);
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_pipe_marker(const struct hl_cmsg_pipe_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-pipe-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_import_pipe_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_pipe_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c5049))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 1) || metadata.identity == 0) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            g_pipe_identity[fd] = metadata.identity;
            g_pipesz[fd] = metadata.size;
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_signalfd_marker(const struct hl_cmsg_signalfd_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-sigfd-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (write(fd, metadata, sizeof *metadata) != (ssize_t)sizeof *metadata) {
        close(fd);
        return -1;
    }
    lseek(fd, 0, SEEK_SET);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int cmsg_kqueue_marker(struct hl_cmsg_kqueue_meta *metadata) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-kqueue-metaXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (typed_inotify_scm_image_export(metadata, fd) != 0 || epoll_scm_image_export(metadata, fd) != 0 ||
        pwrite(fd, metadata, sizeof *metadata, 0) != (ssize_t)sizeof *metadata || lseek(fd, 0, SEEK_SET) < 0 ||
        cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

static int cmsg_kqueue_placeholder(void) {
    if (g_cmsg_ntmpfds >= (int)(sizeof g_cmsg_tmpfds / sizeof g_cmsg_tmpfds[0])) return -1;
    char name[] = "/tmp/.hl-kqueue-fdXXXXXX";
    int fd = mkstemp(name);
    if (fd < 0) return -1;
    unlink(name);
    if (cmsg_tmpfd_track(fd, 0) != 0) {
        close(fd);
        return -1;
    }
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

static int cmsg_import_kqueue_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_kqueue_meta metadata;
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c4b51) || metadata.hidden_count > (uint32_t)(visible - 1))
            break;
        int hidden_base = visible - 1 - (int)metadata.hidden_count;
        if (metadata.ordinal >= (uint32_t)hidden_base) break;
        int fd = fds[metadata.ordinal];
        int imported = -1;
        int adopted = 0;
        for (; adopted < (int)metadata.hidden_count; ++adopted) {
            int private_fd = hl_host_process_fd_private_adopt(fds[hidden_base + adopted]);
            if (private_fd < 0) break;
            fds[hidden_base + adopted] = private_fd;
        }
        if (adopted == (int)metadata.hidden_count &&
            (metadata.kind != 1 || epoll_scm_image_remap(&metadata, marker, fds + hidden_base) == 0) && fd >= 0)
            imported = kqueue_scm_import(fd, &metadata, marker);
        if (imported < 0)
            fprintf(stderr, "[scm-epoll] import failed kind=%u hidden=%u adopted=%d fd=%d errno=%d\n", metadata.kind,
                    metadata.hidden_count, adopted, fd, errno);
        if (imported <= 0) {
            for (int index = 0; index < adopted; ++index) {
                hl_host_process_fd_private_remove(fds[hidden_base + index]);
                close(fds[hidden_base + index]);
            }
            for (uint32_t index = (uint32_t)adopted; index < metadata.hidden_count; ++index)
                close(fds[hidden_base + (int)index]);
        }
        if (imported < 0 && fd >= 0) {
            close(fd);
            fds[metadata.ordinal] = -1;
        }
        close(marker);
        visible = hidden_base;
    }
    return visible;
}

static int cmsg_import_signalfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 3) {
        struct hl_cmsg_signalfd_meta metadata;
        int hidden = fds[visible - 2];
        int marker = fds[visible - 1];
        memset(&metadata, 0, sizeof metadata);
        if (pread(marker, &metadata, sizeof metadata, 0) != (ssize_t)sizeof metadata ||
            metadata.magic != UINT32_C(0x484c5346))
            break;
        if (metadata.ordinal >= (uint32_t)(visible - 2)) break;
        int fd = fds[metadata.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            if (metadata.source_pid == (int32_t)getpid() && metadata.source_slot >= 0 &&
                metadata.source_slot < HL_SFD_MAX && g_sfd[metadata.source_slot].refs > 0) {
                g_sigfd_slot[fd] = (uint8_t)(metadata.source_slot + 1);
                g_sfd[metadata.source_slot].refs++;
                close(hidden);
            } else {
                int slot = sfd_alloc();
                int writer = slot >= 0 ? hl_host_process_fd_private_adopt(hidden) : -1;
                if (slot >= 0 && writer >= 0) {
                    g_sfd[slot].rd = fd;
                    g_sfd[slot].wr = writer;
                    g_sfd[slot].mask = metadata.mask;
                    g_sigfd_slot[fd] = (uint8_t)(slot + 1);
                } else {
                    if (writer >= 0) {
                        hl_host_process_fd_private_remove(writer);
                        close(writer);
                    } else
                        close(hidden);
                    if (slot >= 0) g_sfd[slot].refs = 0;
                }
            }
        } else
            close(hidden);
        close(marker);
        visible -= 2;
    }
    return visible;
}

// Peel trailing timerfd markers (LIFO; appended last by cmsg_l2m so imported first), restoring the
// emulation state onto the received fd so its read/poll/gettime route through the timerfd path.
static int cmsg_import_timerfd_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_timerfd_meta m;
        int marker = fds[visible - 1];
        memset(&m, 0, sizeof m);
        if (pread(marker, &m, sizeof m, 0) != (ssize_t)sizeof m || m.magic != HL_CMSG_TIMERFD_MAGIC) break;
        if (m.ordinal >= (uint32_t)(visible - 1)) break;
        int fd = fds[m.ordinal];
        if (fd >= 0 && fd < HL_NFD) {
            if (m.portable) {
                int timer = kqueue();
                if (timer < 0 || dup2(timer, fd) < 0) {
                    if (timer >= 0) close(timer);
                    close(marker);
                    visible--;
                    continue;
                }
                // Register the alias BEFORE the source descriptor goes away. On macOS a dup'd kqueue IS
                // the same queue and this is a no-op; on a Linux host the queue lives in a side table
                // keyed by descriptor, so without this the queue stayed reachable only through `timer`,
                // which is closed on the next line. The EVFILT_TIMER re-arm below then failed EBADF --
                // and its result is deliberately discarded -- so the received timerfd was never armed:
                // it read its pending expirations correctly but never polled ready (compat scm-timerfd).
                if (timer != fd) hl_native_kqueue_duplicate(timer, fd);
                if (timer != fd) close(timer);
            }
            g_timerfd[fd] = 1;
            g_epoll_family_seen = 1;
            g_tfd_deadline[fd] = m.deadline;
            g_tfd_interval[fd] = m.interval;
            g_tfd_clock[fd] = m.clock;
            g_tfd_nb[fd] = (uint8_t)(m.nb != 0);
            g_tfd_first_oneshot[fd] = (uint8_t)(m.first_oneshot != 0);
            // The timer is armed on the host kqueue registry keyed by fd number. Within the sending
            // engine process the received fd is a live dup of that kqueue, so alias it onto the same
            // queue (exactly as dup() does). Across processes the registry is not shared; the pid guard
            // skips the alias, leaving the received fd inert as before (no cross-process regression).
            if (!m.portable && m.source_pid == (int32_t)getpid()) {
                hl_native_kqueue_duplicate(m.source_fd, fd);
            } else {
                struct timerfd_shared_state *state = NULL;
                if (m.shared_state != 0 && (m.source_pid == (int32_t)getpid() || m.restore_shared != 0))
                    state = (struct timerfd_shared_state *)(uintptr_t)m.shared_state;
                if (state == NULL) {
                    state = mmap(NULL, sizeof *state, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
                    if (state == MAP_FAILED) state = NULL;
                    if (state != NULL) {
                        memset(state, 0, sizeof *state);
                        state->deadline = m.deadline;
                        state->interval = m.interval;
                    }
                }
                if (state == NULL) {
                    close(marker);
                    visible--;
                    continue;
                }
                g_tfd_cslot[fd] = fd + 1;
                g_tfd_object[fd] = m.object ? m.object : ofd_identity_new();
                g_tfd_shared[fd] = state;
                g_tfd_refs[fd]++;
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                timerfd_shared_lock(state);
                int64_t next = state->deadline;
                uint64_t pending = state->pending;
                timerfd_shared_unlock(state);
                g_tfd_deadline[fd] = next;
                g_tfd_interval[fd] = state->interval;
                g_tfd_pending[fd] = pending;
                if (pending != 0 || next > now_ns) {
                    struct kevent event;
                    int64_t delay = pending != 0 ? 1 : next - now_ns;
                    EV_SET(&event, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS, delay, NULL);
                    (void)kevent(fd, &event, 1, NULL, 0, NULL);
                }
            }
        }
        close(marker);
        visible--;
    }
    return visible;
}

static int cmsg_import_seq_trailer(int *fds, int nfds) {
    int visible = nfds;
    while (visible >= 2) {
        struct hl_cmsg_seq_meta m;
        int marker = fds[visible - 1];
        memset(&m, 0, sizeof m);
        if (pread(marker, &m, sizeof m, 0) != (ssize_t)sizeof m || m.magic != HL_CMSG_SEQ_MAGIC) break;
        if (m.ordinal >= (uint32_t)(visible - 1) || m.slot >= SEQ_REF_N || m.end > 1) break;
        int fd = fds[m.ordinal];
        uint32_t pending = __atomic_load_n(&g_seq_refs[m.slot].pending[m.end], __ATOMIC_ACQUIRE);
        while (pending != 0 && !__atomic_compare_exchange_n(&g_seq_refs[m.slot].pending[m.end], &pending, pending - 1,
                                                            0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {}
        if (pending == 0) __atomic_add_fetch(&g_seq_refs[m.slot].refs[m.end], 1, __ATOMIC_ACQ_REL);
        if (fd >= 0 && fd < HL_NFD) {
            g_seq_ref[fd] = (uint16_t)(m.slot + 1);
            g_seq_end[fd] = (uint8_t)m.end;
        }
        close(marker);
        visible--;
    }
    return visible;
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

static int cmsg_read_eventfd_marker(int fd, struct hl_cmsg_eventfd_meta *m) {
    if (fd < 0 || !m) return 0;
    memset(m, 0, sizeof *m);
    if (pread(fd, m, sizeof *m, 0) != (ssize_t)sizeof *m) return 0;
    return m->magic == HL_CMSG_EVENTFD_MAGIC;
}

static int cmsg_import_eventfd_trailer(int *fds, int nfds) {
    if (!fds || nfds <= 2) return nfds;
    int cap = nfds / 3 + 1;
    int *hidden = calloc((size_t)cap, sizeof(int));
    int *marker_fd = calloc((size_t)cap, sizeof(int));
    struct hl_cmsg_eventfd_meta *metas = calloc((size_t)cap, sizeof(*metas));
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
        struct hl_cmsg_eventfd_meta m;
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
        struct hl_cmsg_eventfd_meta *m = &metas[i];
        int pub = fds[m->ordinal];
        if (pub >= 0 && pub < HL_NFD) {
            g_eventfd_peer[pub] = h + 1;
            g_eventfd_cslot[pub] = (int)m->slot + 1;
            g_eventfd_sema[pub] = (uint8_t)(m->sema != 0);
            eventfd_guest_nb_set(pub, m->nb != 0); // carry the OFD-shared guest blocking intent
            g_eventfd_refs[m->slot]++;
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
// fds leaves higher-level protocols with a successful data message but missing handles.
static ssize_t cmsg_l2m(const uint8_t *g, size_t glen, uint8_t *h, size_t cap, int engine_metadata, int *errp) {
    if (errp) *errp = 0;
    cmsg_tmpfds_close();
    cmsg_seq_finish(0);
    cmsg_event_finish(0);
    cmsg_inflight_finish(0);
    cmsg_inflight_sweep();
    cmsg_inflight_mark();
    size_t go = 0, ho = 0;
    while (go + LX_CMSGHDR <= glen) {
        uint64_t clen = *(const uint64_t *)(g + go); // Linux cmsg_len (8B)
        int lvl = *(const int *)(g + go + 8);
        int typ = *(const int *)(g + go + 12);
        if (clen < LX_CMSGHDR || go + clen > glen) {
            if (errp) *errp = EINVAL;
            return -1;
        }
        size_t dlen = (size_t)clen - LX_CMSGHDR; // payload bytes (e.g. N*4 fds)
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
            combo_cap = nfds * 6; // visible + OFD/seq/timer markers + eventfd sideband pair
            combo = malloc((size_t)combo_cap * sizeof(int));
            if (!combo) {
                if (errp) *errp = ENOMEM;
                return -1;
            }
            struct hl_cmsg_kqueue_meta kqueue_metadata[253];
            memset(kqueue_metadata, 0, sizeof kqueue_metadata);
            for (int i = 0; i < nfds; i++) {
                if (engine_metadata && kqueue_scm_export(fds[i], &kqueue_metadata[i]) > 0) {
                    int placeholder = cmsg_kqueue_placeholder();
                    if (placeholder < 0) {
                        free(combo);
                        if (errp) *errp = EMFILE;
                        return -1;
                    }
                    kqueue_metadata[i].magic = UINT32_C(0x484c4b51);
                    kqueue_metadata[i].ordinal = (uint32_t)i;
                    kqueue_metadata[i].source_pid = (int32_t)getpid();
                    kqueue_metadata[i].source_fd = fds[i];
                    combo[combo_n++] = placeholder;
                    (void)ofd_identity_ensure(fds[i]);
                    continue;
                }
                int native = fds[i];
                int borrowed = bound_attachment_borrow(fds[i], &native);
                if (borrowed < 0 || (borrowed > 0 && cmsg_tmpfd_track(native, 1) != 0)) {
                    if (borrowed > 0) bound_attachment_release(native);
                    free(combo);
                    if (errp) *errp = borrowed < 0 ? -borrowed : EMFILE;
                    return -1;
                }
                combo[combo_n++] = native;
                (void)ofd_identity_ensure(fds[i]);
                if (engine_metadata && fds[i] >= 0 && fds[i] < HL_NFD && g_seq_ref[fds[i]] && g_cmsg_nseq < 253) {
                    uint32_t slot = g_seq_ref[fds[i]] - 1;
                    uint32_t end = g_seq_end[fds[i]];
                    __atomic_add_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
                    __atomic_add_fetch(&g_seq_refs[slot].pending[end], 1, __ATOMIC_ACQ_REL);
                    g_cmsg_seq_slot[g_cmsg_nseq] = (uint16_t)slot;
                    g_cmsg_seq_end[g_cmsg_nseq++] = (uint8_t)end;
                }
            }
            if (!engine_metadata) goto cmsg_visible_only;
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd >= 0 && fd < HL_NFD && g_seq_ref[fd]) {
                    struct hl_cmsg_seq_meta sm = {
                        .magic = HL_CMSG_SEQ_MAGIC,
                        .ordinal = (uint32_t)i,
                        .slot = (uint32_t)(g_seq_ref[fd] - 1),
                        .end = (uint32_t)g_seq_end[fd],
                    };
                    int marker = cmsg_seq_marker(&sm);
                    if (marker < 0) {
                        free(combo);
                        if (errp) *errp = EMSGSIZE;
                        return -1;
                    }
                    combo[combo_n++] = marker;
                }
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd < 0 || fd >= HL_NFD || !g_eventfd_peer[fd]) continue;
                if (combo_n + 2 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                int hidden = g_eventfd_peer[fd] - 1;
                int fl = fcntl(hidden, F_GETFL);
                if (fl >= 0) fcntl(hidden, F_SETFL, fl | O_NONBLOCK);
                fcntl(hidden, F_SETFD, FD_CLOEXEC);
                struct hl_cmsg_eventfd_meta m = {
                    .magic = HL_CMSG_EVENTFD_MAGIC,
                    .ordinal = (uint32_t)i,
                    .slot = (uint32_t)eventfd_counter_slot(fd),
                    .sema = (uint32_t)(g_eventfd_sema[fd] != 0),
                    .nb = (uint32_t)eventfd_guest_nb(fd),
                };
                int event_slot = eventfd_counter_slot(fd);
                if (event_slot < 0 || event_slot >= HL_NFD || g_cmsg_nevent >= 253) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                g_eventfd_refs[event_slot]++;
                g_cmsg_event_slot[g_cmsg_nevent++] = (uint16_t)event_slot;
                int marker = cmsg_eventfd_marker(&m);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = hidden;
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (!memfd_ensure_fd(fd)) continue;
                if (combo_n + 1 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                struct hl_cmsg_memfd_meta metadata = {
                    .magic = UINT32_C(0x484c4d46),
                    .ordinal = (uint32_t)i,
                    .seals = g_memfd_seal[fd],
                };
                int marker = cmsg_memfd_marker(&metadata);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd < 0 || fd >= HL_NFD || g_pipe_identity[fd] == 0) continue;
                if (combo_n + 1 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                struct hl_cmsg_pipe_meta metadata = {
                    .magic = UINT32_C(0x484c5049),
                    .ordinal = (uint32_t)i,
                    .identity = g_pipe_identity[fd],
                    .size = g_pipesz[fd],
                };
                int marker = cmsg_pipe_marker(&metadata);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                struct hl_cmsg_kqueue_meta metadata = kqueue_metadata[i];
                if (metadata.kind == 0) continue;
                int hidden_count = epoll_scm_hidden_export(&metadata, NULL, 0);
                if (hidden_count < 0 || combo_n + hidden_count + 1 > 253) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                if (combo_n + hidden_count + 1 > combo_cap) {
                    int expanded = combo_n + hidden_count + 1;
                    int *replacement = realloc(combo, (size_t)expanded * sizeof *combo);
                    if (replacement == NULL) {
                        free(combo);
                        if (errp) *errp = ENOMEM;
                        return -1;
                    }
                    combo = replacement;
                    combo_cap = expanded;
                }
                if (hidden_count != 0 &&
                    epoll_scm_hidden_export(&metadata, combo + combo_n, hidden_count) != hidden_count) {
                    free(combo);
                    if (errp) *errp = EIO;
                    return -1;
                }
                combo_n += hidden_count;
                int marker = cmsg_kqueue_marker(&metadata);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd < 0 || fd >= HL_NFD || !g_sigfd_slot[fd]) continue;
                int slot = g_sigfd_slot[fd] - 1;
                if (slot < 0 || slot >= HL_SFD_MAX || g_sfd[slot].wr < 0 || combo_n + 2 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                struct hl_cmsg_signalfd_meta metadata = {
                    .magic = UINT32_C(0x484c5346),
                    .ordinal = (uint32_t)i,
                    .source_pid = (int32_t)getpid(),
                    .source_slot = slot,
                    .mask = g_sfd[slot].mask,
                };
                int marker = cmsg_signalfd_marker(&metadata);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = g_sfd[slot].wr;
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                int fd = fds[i];
                if (fd < 0 || fd >= HL_NFD || !g_timerfd[fd]) continue;
                if (combo_n + 1 > combo_cap) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                struct hl_cmsg_timerfd_meta tm = {
                    .magic = HL_CMSG_TIMERFD_MAGIC,
                    .ordinal = (uint32_t)i,
                    .first_oneshot = (uint32_t)(g_tfd_first_oneshot[fd] != 0),
                    .clock = g_tfd_clock[fd],
                    .deadline = g_tfd_deadline[fd],
                    .interval = g_tfd_interval[fd],
                    .source_fd = fd,
                    .source_pid = (int32_t)getpid(),
                    .nb = (uint32_t)(g_tfd_nb[fd] != 0),
                    .portable = 1,
                    .object = g_tfd_object[fd],
                    .shared_state = (uint64_t)(uintptr_t)g_tfd_shared[fd],
                };
                struct hl_cmsg_timerfd_meta placeholder_metadata;
                memset(&placeholder_metadata, 0, sizeof placeholder_metadata);
                int placeholder = cmsg_timerfd_marker(&placeholder_metadata);
                if (placeholder < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[i] = placeholder;
                int marker = cmsg_timerfd_marker(&tm);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = marker;
            }
            for (int i = 0; i < nfds; i++) {
                struct hl_cmsg_ofd_meta metadata = {
                    .magic = HL_CMSG_OFD_MAGIC,
                    .ordinal = (uint32_t)i,
                    .identity = g_ofd_id[fds[i]],
                };
                int marker = cmsg_ofd_marker(&metadata);
                if (marker < 0) {
                    free(combo);
                    if (errp) *errp = EMSGSIZE;
                    return -1;
                }
                combo[combo_n++] = marker;
                cmsg_inflight_hold(fds[i], marker);
            }
        cmsg_visible_only:
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
    cmsg_inflight_sweep(); // cheap no-op unless this process has descriptors of its own still in flight
    if (truncp) *truncp = 0;
    size_t go = off;
    for (struct cmsghdr *c = CMSG_FIRSTHDR((struct msghdr *)mh); c; c = CMSG_NXTHDR((struct msghdr *)mh, c)) {
        if (c->cmsg_len < CMSG_LEN(0)) break;
        size_t dlen = (size_t)c->cmsg_len - CMSG_LEN(0); // payload bytes (macOS hdr=12)
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS && dlen >= sizeof(int)) {
            int nfds = (int)(dlen / sizeof(int));
            int *fds = (int *)CMSG_DATA(c);
            int visible = cmsg_import_ofd_trailer(fds, nfds);
            visible = cmsg_import_signalfd_trailer(fds, visible);
            visible = cmsg_import_kqueue_trailer(fds, visible);
            visible = cmsg_import_pipe_trailer(fds, visible);
            visible = cmsg_import_memfd_trailer(fds, visible);
            visible = cmsg_import_timerfd_trailer(fds, visible);
            visible = cmsg_import_eventfd_trailer(fds, visible);
            visible = cmsg_import_seq_trailer(fds, visible);
            for (int i = 0; i < visible; i++) {
                cmsg_note_recv_sock_fd(fds[i]);
            }
            dlen = (size_t)visible * sizeof(int);
        }
        size_t need = LX_CMSG_ALIGN(LX_CMSGHDR + dlen);
        if (go + LX_CMSGHDR + dlen > cap) {
            if (truncp) *truncp = 1;
            // Linux delivers a partial SCM_RIGHTS record with as many whole fds as fit in the
            // remaining control space and closes the fds that did not fit -- it does not drop the
            // whole record. Match that (and never leak the undelivered host fds).
            if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
                int *fds = (int *)CMSG_DATA(c);
                int total = (int)(dlen / sizeof(int));
                size_t room = (go + LX_CMSGHDR <= cap) ? cap - go - LX_CMSGHDR : 0;
                int keep = (int)(room / sizeof(int));
                if (keep > total) keep = total;
                for (int i = keep; i < total; i++)
                    if (fds[i] >= 0) close(fds[i]);
                if (keep > 0) {
                    size_t kb = (size_t)keep * sizeof(int);
                    *(uint64_t *)(g + go) = (uint64_t)(LX_CMSGHDR + kb);
                    *(int *)(g + go + 8) = cmsg_level_m2l(c->cmsg_level);
                    *(int *)(g + go + 12) = c->cmsg_type;
                    memcpy(g + go + LX_CMSGHDR, CMSG_DATA(c), kb);
                    go += LX_CMSG_ALIGN(LX_CMSGHDR + kb);
                }
            }
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
    *(uint64_t *)(g + off) = (uint64_t)need; // Linux cmsg_len (payload + 16B hdr)
    *(int *)(g + off + 8) = LX_SOL_SOCKET;   // cmsg_level = SOL_SOCKET(1)
    *(int *)(g + off + 12) = 2;              // cmsg_type  = SCM_CREDENTIALS(2)
    *(uint32_t *)(g + off + 16) = (uint32_t)pid;
    *(uint32_t *)(g + off + 20) = (uint32_t)uid;
    *(uint32_t *)(g + off + 24) = (uint32_t)gid;
    return off + LX_CMSG_ALIGN(need);
}

// SOL_SOCKET option name: Linux -> macOS (they differ). -1 = ignore (unsupported here).
/*
 * Windows takes the identity arm with Linux, and that is not an oversight worth
 * re-litigating later: the socket vocabulary this layer calls on that host --
 * src/linux_abi/host_socket.h -- is written in LINUX constants throughout and
 * translates to the neutral host contract itself. Sending it the BSD numbers
 * below would be the classic cross-level aliasing defect rather than a
 * translation: BSD SO_ACCEPTCONN is 0x0002, and 2 is what Linux SO_REUSEADDR
 * already is, so a "translated" SO_REUSEADDR arrives as a request to read a
 * different option -- silently, since setting an option reports nothing.
 * Measured before this arm existed: SO_RCVBUF(8) became 0x1002, which the
 * neutral option table does not name, so a get-after-set reported failure.
 */
static int so_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
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
#endif
}

// IPPROTO_TCP optname Linux -> macOS. CRITICAL: these numbers diverge, and a raw pass-through maps
// Linux TCP_KEEPIDLE(4)/TCP_CORK(3) onto macOS TCP_NOPUSH(4)/TCP_NOOPT(3) — TCP_NOPUSH *corks* the
// socket so a server's reply is never delivered until close (breaks redis & every keepalive-setting
// server). Map the known ones; ignore (-1) unknown rather than pass through and accidentally cork.
static int tcp_opt_l2m(int o) {
#if defined(__linux__) || defined(_WIN32)
    return o;
#else
    switch (o) {
    case 1: return 0x01;  // TCP_NODELAY  (same)
    case 2: return 0x02;  // TCP_MAXSEG   (same)
    case 3: return 0x04;  // Linux TCP_CORK     -> macOS TCP_NOPUSH (deliberate; guest asked to cork)
    case 4: return 0x10;  // Linux TCP_KEEPIDLE -> macOS TCP_KEEPALIVE (seconds)
    case 5: return 0x101; // Linux TCP_KEEPINTVL-> macOS TCP_KEEPINTVL
    case 6: return 0x102; // Linux TCP_KEEPCNT  -> macOS TCP_KEEPCNT
    default: return -1;   // unknown -> ignore (never pass a Linux number straight to macOS IPPROTO_TCP)
    }
#endif
}

// ---- NET namespace: per-container private loopback. A container's explicit 127.0.0.0/8 TCP sockets
// are routed to AF_UNIX sockets under a per-namespace host dir the guest can't name (it's path-jailed),
// so each container's localhost is isolated from the host + other containers. 0.0.0.0/external stay
// host-passthrough (so `-p` publishing still works). Off when g_netns[0]==0.
// host dir for this container's loopback unix sockets ("" = no isolation)
// Both names are generated internally from a fixed prefix plus at most 40 identifier bytes.  Keeping
// the declared bound honest proves every derived AF_UNIX rendezvous name fits sun_path.
static char g_netns[64];
// fd -> the loopback port it's bound/connected to (0 = not a private-lo socket)
static uint16_t g_lo_port[HL_NFD];
// fd -> 1 if this private-lo socket is AF_INET6 (so getsockname/getpeername/accept report a sockaddr_in6
// with ::1 instead of an AF_INET 127.0.0.1). Dual-stack listeners use the common port rendezvous; a
// v6-only wildcard listener uses a separate path so IPv4 may bind the same port. This flag picks the
// address family reported back to the guest.
static uint8_t g_lo_v6[HL_NFD];
// fd -> 1 when an AF_INET6 wildcard bind requested IPV6_V6ONLY. Such a
// listener owns a rendezvous distinct from an IPv4 listener on the same port.
static uint8_t g_lo_v6only[HL_NFD];
// fd -> 1 if created SOCK_STREAM (only those get loopback isolation)
static uint8_t g_sock_stream[HL_NFD];
// A stream that was initially switched to AF_UNIX by a virtual bind may later connect to a genuine
// external INET peer. In that case getsockname still reports the guest's virtual bind address, while
// getpeername must query the rehydrated host INET socket instead of mistaking the local virtual address
// for its peer. Carried across dup and cleared when the descriptor is reused.
static uint8_t g_sock_host_backed[HL_NFD];
// Connected to an exact host-projected AF_UNIX socket. Native peers do not understand engine-private
// SCM_RIGHTS trailer descriptors; sending one would poison the peer's descriptor queue.
static uint8_t g_sock_native_peer[HL_NFD];
// fd -> 1 once a stream connect() SUCCEEDED on it. Linux keeps a connected stream socket in SS_CONNECTED
// (a second connect -> EISCONN) until close, even after the peer sends FIN; macOS drops the peer
// association after FIN so getpeername() there returns ENOTCONN. This sticky flag lets connect(203) report
// EISCONN faithfully (LTP connect01). Cleared on close (fd_reset_emul) and at socket()/accept re-init.
static uint8_t g_sock_conn[HL_NFD];
static uint8_t g_sock_connecting[HL_NFD];
// fd -> a pending asynchronous socket error (Linux errno) to hand back on the next getsockopt(SO_ERROR),
// then clear (Linux delivers SO_ERROR once). A non-blocking stream connect() to a closed private-loopback
// port has no live INET peer to surface ECONNREFUSED through the AF_UNIX switch, so we stash it here and
// report EINPROGRESS, mirroring a real deferred TCP connect failure. Cleared at socket()/accept() re-init.
static int g_so_error[HL_NFD];
// fd -> 1 if the guest set SO_REUSEPORT on this socket. A private-loopback INET socket is backed by an
// AF_UNIX switch socket, and Linux AF_UNIX accepts setsockopt(SO_REUSEPORT) but always reads it back as 0
// (unlike SO_REUSEADDR), so the guest's get-after-set would wrongly report 0. Record the guest's intent
// here and report it on getsockopt. Cleared at socket()/accept() re-init.
static uint8_t g_so_reuseport[HL_NFD];
// fd -> shadowed IPPROTO_TCP integer options. A private-loopback/bridge guest INET stream socket is backed
// on the host by an AF_UNIX switch socket (see lo_swap), which rejects every setsockopt/getsockopt at
// IPPROTO_TCP with ENOPROTOOPT. Linux round-trips these options on a real TCP socket, and applications
// routinely set TCP_NODELAY *after* connect(), so a get-after-set (or a plain set) must not fail across the
// switch. Record the guest's value here and report it back, matching native. Slots: 0 NODELAY, 1 CORK,
// 2 KEEPIDLE, 3 KEEPINTVL, 4 KEEPCNT, 5 QUICKACK, 6 MAXSEG. Cleared at socket()/accept() re-init.
#define TCP_SHADOW_N 7
static int g_tcp_optval[HL_NFD][TCP_SHADOW_N];
static uint8_t g_tcp_optset[HL_NFD][TCP_SHADOW_N];

// Map a Linux IPPROTO_TCP integer optname to a shadow slot, or -1 if it is not a virtualized round-trip
// option (e.g. TCP_INFO, which is a struct handled separately). MAXSEG is get-mostly but Linux lets a guest
// lower the clamp, so it round-trips through a slot too.
static int tcp_shadow_slot(int optname) {
    switch (optname) {
    case 1: return 0;  // TCP_NODELAY
    case 3: return 1;  // TCP_CORK
    case 4: return 2;  // TCP_KEEPIDLE
    case 5: return 3;  // TCP_KEEPINTVL
    case 6: return 4;  // TCP_KEEPCNT
    case 12: return 5; // TCP_QUICKACK
    case 2: return 6;  // TCP_MAXSEG
    default: return -1;
    }
}

// A get on a MAXSEG slot never set by the guest reports a plausible loopback MSS so diagnostic code that
// requires a nonzero segment size keeps working over the switch (the exact value is host-variable on native
// and therefore not a stable fact); every other slot defaults to 0, the Linux default for the booleans.
static int tcp_shadow_default(int slot) {
    return slot == 6 ? 65483 : 0;
}

// Drop any shadowed TCP options for a reused fd number (socket()/accept()/close re-init).
static void tcp_shadow_clear(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    for (int i = 0; i < TCP_SHADOW_N; i++)
        g_tcp_optset[fd][i] = 0;
}

// fd -> shadowed IPPROTO_IP(level 0) / IPPROTO_IPV6(level 41) integer options. Same class as the TCP shadow
// above: once a private-loopback/bridge guest INET socket is bound/connected, its host backing becomes an
// AF_UNIX switch socket (see lo_swap), which rejects every setsockopt/getsockopt at IPPROTO_IP/IPPROTO_IPV6
// with ENOPROTOOPT. Native Linux round-trips these on a real IP socket -- DNS servers set IP_PKTINFO/
// IP_RECVTTL to reply from the right address, QUIC/HTTP3 and dual-stack servers set IP_TOS/IPV6_TCLASS and
// read IPV6_V6ONLY back, and code sets them *after* bind/connect -- so a get-after-set (or plain set) must
// survive the switch. Only options native actually accepts on a connected/bound unicast stream socket are
// shadowed here; options native itself rejects on such a socket (IP_HDRINCL raw-only -> ENOPROTOOPT,
// IP_MULTICAST_HOPS/IPV6_MULTICAST_HOPS on a unicast socket -> ENOPROTOOPT, IP_TRANSPARENT unprivileged ->
// EPERM) are deliberately left OUT so they fall through to the real setsockopt and surface the true errno.
// Slots 0-7 are IPPROTO_IP, 8-13 IPPROTO_IPV6. Cleared at socket()/accept()/close re-init.
#define IPOPT_SHADOW_N 14
static int g_ipopt_val[HL_NFD][IPOPT_SHADOW_N];
static uint8_t g_ipopt_set[HL_NFD][IPOPT_SHADOW_N];

// Map a Linux IPPROTO_IP integer optname to a shadow slot, or -1 if it is not a virtualized round-trip
// option at this level (unknown, struct-valued, or one native rejects on a unicast stream socket).
static int ip_shadow_slot(int optname) {
    switch (optname) {
    case 1: return 0;  // IP_TOS
    case 2: return 1;  // IP_TTL
    case 8: return 2;  // IP_PKTINFO
    case 10: return 3; // IP_MTU_DISCOVER
    case 11: return 4; // IP_RECVERR
    case 12: return 5; // IP_RECVTTL
    case 13: return 6; // IP_RECVTOS
    case 15: return 7; // IP_FREEBIND
    default: return -1;
    }
}

// Map a Linux IPPROTO_IPV6 integer optname to a shadow slot, or -1. IPV6_V6ONLY(26) uses slot 13 but its
// setsockopt is handled specially (native rejects a change after bind with EINVAL), so it is excluded here
// and matched directly on the optname in the setsockopt/getsockopt paths.
static int ip6_shadow_slot(int optname) {
    switch (optname) {
    case 67: return 8;  // IPV6_TCLASS
    case 16: return 9;  // IPV6_UNICAST_HOPS
    case 49: return 10; // IPV6_RECVPKTINFO
    case 51: return 11; // IPV6_RECVHOPLIMIT
    case 66: return 12; // IPV6_RECVTCLASS
    default: return -1;
    }
}

#define IPOPT_V6ONLY_SLOT 13

// A get on a slot the guest never set reports the Linux default so code that reads an option it did not set
// still sees a plausible value over the switch instead of ENOPROTOOPT (only reached when the real host
// getsockopt is also rejected, i.e. the AF_UNIX switch backing). IP_TTL and IPV6_UNICAST_HOPS default to 64;
// the boolean recv-flags and TOS/TCLASS default to 0.
static int ipopt_shadow_default(int slot) {
    switch (slot) {
    case 1: return 64; // IP_TTL
    case 9: return 64; // IPV6_UNICAST_HOPS
    default: return 0;
    }
}

// Drop any shadowed IP/IPV6 options for a reused fd number (socket()/accept()/close re-init).
static void ipopt_shadow_clear(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    for (int i = 0; i < IPOPT_SHADOW_N; i++)
        g_ipopt_set[fd][i] = 0;
}

// fd -> 1 if created AF_INET SOCK_DGRAM (only those get the published-UDP switch redirect, below)
static uint8_t g_sock_dgram[HL_NFD];
static uint16_t g_udp_local_port[HL_NFD], g_udp_peer_port[HL_NFD];
static uint32_t g_udp_local_ip[HL_NFD], g_udp_peer_ip[HL_NFD];
static uint8_t g_udp_local_interface[HL_NFD], g_udp_peer_interface[HL_NFD];
static uint8_t g_udp_local_v6[HL_NFD], g_udp_peer_v6[HL_NFD];

enum { HL_NETIF_MAX = 8 };

#define UDP_REF_N 4096

struct udp_ref {
    volatile uint32_t used, refs;
    char path[200];
    uint8_t alias_count;
    char aliases[HL_NETIF_MAX - 1][200];
};
static struct udp_ref *g_udp_refs;
static uint16_t g_udp_ref[HL_NFD];
// fd -> the socket's guest (Linux) address family, recorded at socket()/accept() so connect(203)/bind(200)
// can validate the guest sockaddr's sa_family against it (EAFNOSUPPORT) without a getsockname() probe --
// which is unreliable after a prior failed connect on the same fd. 0 = untracked (best-effort fallback).
static uint16_t g_sock_fam[HL_NFD];
// fd -> 1 if this DGRAM socket emulates a connection-oriented endpoint that owes its peer an EOF on
// close (a SEQPACKET socketpair or an O_DIRECT "packet" pipe). macOS DGRAM sockets never deliver EOF on
// peer close, so close() injects a zero-length EOF datagram and recv/read coerce ECONNRESET -> 0 for these.
static uint8_t g_sock_seqpacket[HL_NFD];

// macOS backs Linux SOCK_SEQPACKET (and O_DIRECT packet pipes) with AF_UNIX DGRAM. DGRAM preserves
// records but has no connection lifetime: when the last copy of one endpoint closes, its peer receives
// neither EOF nor a wakeup. Keep endpoint ownership in shared memory so fork/dup/close/exec reproduce the
// Linux last-open-file-description rule. The arena is inherited by every guest descendant; dead pairs are
// recycled only after both endpoint reference counts reach zero.
static void seq_ref_arena_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_seq_refs != NULL && g_udp_refs != NULL) return;
    if (g_seq_refs == NULL && hl_linux_shared_create(host, sizeof(struct seq_ref) * SEQ_REF_N, &arena) == HL_STATUS_OK)
        g_seq_refs = (struct seq_ref *)arena;
    arena = NULL;
    if (g_udp_refs == NULL && hl_linux_shared_create(host, sizeof(struct udp_ref) * UDP_REF_N, &arena) == HL_STATUS_OK)
        g_udp_refs = (struct udp_ref *)arena;
}

static int udp_ref_create(int fd, const char *path) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD) return 0;
    for (uint32_t i = 0; i < UDP_REF_N; i++) {
        if (!__sync_bool_compare_and_swap(&g_udp_refs[i].used, 0, 1)) continue;
        snprintf(g_udp_refs[i].path, sizeof g_udp_refs[i].path, "%s", path);
        g_udp_refs[i].alias_count = 0;
        __atomic_store_n(&g_udp_refs[i].refs, 1, __ATOMIC_RELEASE);
        g_udp_ref[fd] = (uint16_t)(i + 1);
        return 0;
    }
    errno = ENFILE;
    return -1;
}

static int udp_ref_add_alias(int fd, const char *path) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD || !g_udp_ref[fd]) {
        errno = EINVAL;
        return -1;
    }
    struct udp_ref *ref = &g_udp_refs[g_udp_ref[fd] - 1];
    if (ref->alias_count >= sizeof ref->aliases / sizeof ref->aliases[0]) {
        errno = ENOSPC;
        return -1;
    }
    snprintf(ref->aliases[ref->alias_count], sizeof ref->aliases[ref->alias_count], "%s", path);
    ref->alias_count++;
    return 0;
}

static void udp_ref_dup(int dst, int src) {
    if (!g_udp_refs || src < 0 || src >= HL_NFD || dst < 0 || dst >= HL_NFD || !g_udp_ref[src]) return;
    uint32_t slot = g_udp_ref[src] - 1;
    __atomic_add_fetch(&g_udp_refs[slot].refs, 1, __ATOMIC_ACQ_REL);
    g_udp_ref[dst] = g_udp_ref[src];
}

static void udp_ref_drop(int fd) {
    if (!g_udp_refs || fd < 0 || fd >= HL_NFD || !g_udp_ref[fd]) return;
    uint32_t slot = g_udp_ref[fd] - 1;
    g_udp_ref[fd] = 0;
    if (__atomic_sub_fetch(&g_udp_refs[slot].refs, 1, __ATOMIC_ACQ_REL) == 0) {
        unlink(g_udp_refs[slot].path);
        for (uint8_t i = 0; i < g_udp_refs[slot].alias_count; i++) {
            unlink(g_udp_refs[slot].aliases[i]);
            g_udp_refs[slot].aliases[i][0] = 0;
        }
        g_udp_refs[slot].alias_count = 0;
        g_udp_refs[slot].path[0] = 0;
        __atomic_store_n(&g_udp_refs[slot].used, 0, __ATOMIC_RELEASE);
    }
}

// exit_group(2) tears the whole process down with a raw _exit() that never runs fd_reset_emul, so any
// rendezvous inode still owned by an un-closed descriptor (e.g. a fork()ed client child that inherits the
// server's listening fd and _exit()s without closing it) would keep its refcount pinned above zero and
// orphan the AF_UNIX inode on the fs. Drop this process's remaining udp/bridge refs at exit so the LAST
// reference across the whole process tree unlinks the inode -- mirroring the kernel freeing an INET port
// when its last owning process dies. Per-fd and idempotent: fds already closed carry no ref.
static void udp_ref_process_exit(void) {
    if (!g_udp_refs) return;
    for (int fd = 0; fd < HL_NFD; fd++)
        if (g_udp_ref[fd]) udp_ref_drop(fd);
}

static int seq_ref_pair(int first, int second) {
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD || g_seq_refs == NULL) return -1;
    for (uint32_t i = 0; i < SEQ_REF_N; i++) {
        if (!__sync_bool_compare_and_swap(&g_seq_refs[i].used, 0, 1)) continue;
        __atomic_store_n(&g_seq_refs[i].refs[0], 1, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].refs[1], 1, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].pending[0], 0, __ATOMIC_RELAXED);
        __atomic_store_n(&g_seq_refs[i].pending[1], 0, __ATOMIC_RELAXED);
        g_seq_ref[first] = (uint16_t)(i + 1);
        g_seq_end[first] = 0;
        g_seq_ref[second] = (uint16_t)(i + 1);
        g_seq_end[second] = 1;
        return 0;
    }
    errno = ENFILE;
    return -1;
}

static void seq_ref_dup(int dst, int src) {
    if (!g_seq_refs || src < 0 || src >= HL_NFD || dst < 0 || dst >= HL_NFD || !g_seq_ref[src]) return;
    uint32_t slot = g_seq_ref[src] - 1;
    uint32_t end = g_seq_end[src];
    __atomic_add_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL);
    g_seq_ref[dst] = g_seq_ref[src];
    g_seq_end[dst] = (uint8_t)end;
}

static void seq_ref_drop(int fd) {
    if (!g_seq_refs || fd < 0 || fd >= HL_NFD || !g_seq_ref[fd]) return;
    uint32_t slot = g_seq_ref[fd] - 1;
    uint32_t end = g_seq_end[fd];
    g_seq_ref[fd] = 0;
    g_seq_end[fd] = 0;
    if (__atomic_sub_fetch(&g_seq_refs[slot].refs[end], 1, __ATOMIC_ACQ_REL) == 0) (void)send(fd, "", 0, MSG_DONTWAIT);
    if (__atomic_load_n(&g_seq_refs[slot].refs[0], __ATOMIC_ACQUIRE) == 0 &&
        __atomic_load_n(&g_seq_refs[slot].refs[1], __ATOMIC_ACQUIRE) == 0)
        __atomic_store_n(&g_seq_refs[slot].used, 0, __ATOMIC_RELEASE);
}

// Reserve the references the child will inherit before fork. A failed fork rolls them back; a successful
// fork consumes the reservation, requiring no allocation or lock in the post-fork child.
static void seq_ref_fork_prepare(void) {
    if (g_seq_refs == NULL) return;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (!g_seq_ref[fd]) continue;
        uint32_t slot = g_seq_ref[fd] - 1;
        __atomic_add_fetch(&g_seq_refs[slot].refs[g_seq_end[fd]], 1, __ATOMIC_ACQ_REL);
    }
    if (g_udp_refs)
        for (int fd = 0; fd < HL_NFD; fd++)
            if (g_udp_ref[fd]) __atomic_add_fetch(&g_udp_refs[g_udp_ref[fd] - 1].refs, 1, __ATOMIC_ACQ_REL);
}

static void seq_ref_fork_cancel(void) {
    if (!g_seq_refs) return;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (!g_seq_ref[fd]) continue;
        uint32_t slot = g_seq_ref[fd] - 1;
        __atomic_sub_fetch(&g_seq_refs[slot].refs[g_seq_end[fd]], 1, __ATOMIC_ACQ_REL);
    }
    if (g_udp_refs)
        for (int fd = 0; fd < HL_NFD; fd++)
            if (g_udp_ref[fd]) __atomic_sub_fetch(&g_udp_refs[g_udp_ref[fd] - 1].refs, 1, __ATOMIC_ACQ_REL);
}

// fd -> (its socketpair/O_DIRECT-pipe PARTNER fd + 1); 0 = no known partner. Recorded for both ends at
// socketpair(SEQPACKET)/pipe2(O_DIRECT) so close() can tell a genuine last-local close (inject the synthetic
// EOF) from a parent dropping the child's fork-inherited peer end while it still holds its OWN end (must NOT
// inject: Linux delivers no EOF while the child still references that end, and our zero-length datagram would
// otherwise land in our own retained end's queue and be misread as a premature EOF -- exactly what broke
// a multi-process SEQPACKET handshake). Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_pair_peer[HL_NFD];
static uint64_t g_sock_object[HL_NFD];
static uint64_t g_sock_peer_object[HL_NFD];
static _Atomic uint32_t g_sock_object_next = 1;

static uint64_t sock_object_new(void) {
    uint32_t sequence = atomic_fetch_add_explicit(&g_sock_object_next, 1u, memory_order_relaxed);
    if (sequence == 0) sequence = atomic_fetch_add_explicit(&g_sock_object_next, 1u, memory_order_relaxed);
    return ((uint64_t)(uint32_t)getpid() << 32) | sequence;
}

static void sock_pair_identity_assign(int first, int second) {
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD) return;
    uint64_t first_object = sock_object_new();
    uint64_t second_object = sock_object_new();
    g_sock_object[first] = first_object;
    g_sock_peer_object[first] = second_object;
    g_sock_object[second] = second_object;
    g_sock_peer_object[second] = first_object;
}

/* Private loopback/bridge streams are AF_UNIX transports hidden behind guest INET sockets. Encode their
 * reciprocal object identities in the client's AF_UNIX bind name, so accept can recover them with
 * getpeername() without placing engine-private bytes in the guest stream. */
static int sock_internal_connect_prepare(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_sock_object[fd]) {
        errno = EINVAL;
        return -1;
    }
    // A failed AF_UNIX connect poisons its socket and the retry path replaces it with lo_swap(). Re-bind
    // every replacement, while retaining the same logical peer object across attempts.
    uint64_t peer = g_sock_peer_object[fd] ? g_sock_peer_object[fd] : sock_object_new();
    char path[HL_SOCKET_IDENTITY_PATH_SIZE];
    if (hl_socket_identity_format(path, sizeof path, g_sock_object[fd], peer) != 0) {
        errno = EINVAL;
        return -1;
    }
    struct sockaddr_un address;
    if (unix_addr_set(&address, path) < 0) return -1;
    unlink(path);
    if (bind(fd, (struct sockaddr *)&address, sizeof address) != 0) return -1;
    unlink(path);
    g_sock_peer_object[fd] = peer;
    return 0;
}

static int sock_internal_accept_identify(int fd) {
    struct sockaddr_un peer = {0};
    socklen_t length = sizeof peer;
    if (getpeername(fd, (struct sockaddr *)&peer, &length) != 0) return -1;
    peer.sun_path[sizeof peer.sun_path - 1] = '\0';
    if (peer.sun_family != AF_UNIX ||
        strncmp(peer.sun_path, HL_SOCKET_IDENTITY_PREFIX, sizeof(HL_SOCKET_IDENTITY_PREFIX) - 1) != 0)
        return 0;
    uint64_t client = 0, server = 0;
    if (hl_socket_identity_parse(peer.sun_path, &client, &server) != 0) {
        errno = EPROTO;
        return -1;
    }
    g_sock_object[fd] = server;
    g_sock_peer_object[fd] = client;
    return 1;
}

// fd -> a DISTINCT synthetic peer pid stamped on both ends at socketpair() creation (0 = none). macOS
// captures LOCAL_PEERPID at socketpair-creation time and reports the CREATOR's pid on BOTH ends, never
// updating it on fork -- so the process that CREATED the pair (typically container init, guest
// pid 1) reads its OWN pid as the peer credential for every forked child, which the
// SCM_CREDENTIALS/SO_PEERCRED self-fallback then collapsed to container_pid() == guest 1 for ALL of them.
// IPC uses that peer pid as a node identity, so every child collided on node "1" and the
// node-merge never finalized (transport up, but OnChannelConnected never fired -> the child's IO thread
// blocked in recvmsg until its connection watchdog killed it).
// Stamping each end with a unique id (>= 1<<30, above Linux PID_MAX ~4M so it never aliases a real guest
// pid the protocol also tracks) gives every child a distinct, non-self node identity whenever LOCAL_PEERPID
// degenerates to self. Carried on dup (fd_carry_sock); reset on close (fd_reset_emul).
static int g_sock_peer_pid[HL_NFD];

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
    return fd >= 0 && fd < HL_NFD && g_sock_seqpacket[fd];
}

// fd -> 1 if the guest enabled SO_PASSCRED on this AF_UNIX socket. macOS has no SO_PASSCRED/SCM_CREDENTIALS,
// so we record the request and synthesize the peer-credentials ancillary record on each recvmsg (below).
// Credential-aware IPC sets SO_PASSCRED and requires an SCM_CREDENTIALS cmsg on the bootstrap message
// -- without it the receiver logs "missing credentials" and aborts. Carried on dup, reset on close.
static uint8_t g_sock_passcred[HL_NFD];

static void cmsg_note_recv_sock_fd(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
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
static uint16_t g_tcp_lport[HL_NFD];
static uint32_t g_tcp_laddr[HL_NFD];     // fd -> raw __be32 v4 bind addr (0.0.0.0 -> 0), printed %08X kernel-style
static uint8_t g_tcp_l6[HL_NFD];         // fd -> 1 if the bind was AF_INET6 (row goes in /proc/net/tcp6)
static uint8_t g_tcp_laddr6[HL_NFD][16]; // fd -> 16-byte v6 bind addr
static uint8_t g_tcp_listen[HL_NFD];     // fd -> 1 once listen(2) succeeded (row is emitted only then)
static int g_sock_backlog[HL_NFD];

static int lo_on(void) {
#if defined(_WIN32)
    /*
     * The private loopback namespace is off on this host, and the reason is
     * structural rather than a missing feature.
     *
     * lo_swap() implements it by MINTING A FRESH AF_UNIX SOCKET AND dup2()ING IT
     * OVER THE GUEST'S DESCRIPTOR, so that an INET socket the guest still holds
     * is quietly backed by a local one. That requires two things this host does
     * not have. First, a descriptor number here is the C library's, and the
     * socket behind it lives in a side table that a C library dup2() does not
     * move -- the swap would leave the guest's number pointing at the ORIGINAL
     * socket while the new one is bound, which is exactly the failure measured
     * before this guard existed: bind() answering EAFNOSUPPORT because an INET
     * socket was handed a local address. Second, the rendezvous is a filesystem
     * path under the guest's own /tmp, and a local socket here is bound with a
     * host path, so the name the switch agrees on does not exist to bind to.
     *
     * With it off, a guest's 127.0.0.1 reaches the real host loopback. What is
     * lost is ISOLATION between containers on one machine -- two guests would
     * see each other's localhost -- not any socket behaviour, which is why the
     * guard is here and not at the call sites: every one of them is correct
     * unconditionally once the answer is "no namespace".
     */
    return 0;
#else
    return g_netns[0] != 0;
#endif
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

// A v6-only wildcard listener and an IPv4 wildcard listener may own the same
// numeric port at once. Keep the v6-only rendezvous distinct; dual-stack IPv6
// listeners retain the historical path so IPv4 and IPv6 clients share it.
static void lo_tcp_path(uint16_t port, int v6only, char *out, size_t n) {
    if (v6only)
        snprintf(out, n, "%s/p6-%u", g_netns, (unsigned)port);
    else
        lo_path(port, out, n);
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
// SOL_SOCKET options a guest may set on its INET socket BEFORE the private-loopback swap replaces it with a
// fresh AF_UNIX socket. Each is generic to SOL_SOCKET (valid + readable on AF_UNIX), so carrying them over
// preserves both the option's effect (a receive timeout still fires, so a blocked recv wakes with EAGAIN
// instead of hanging) and its get-after-set readback (SO_REUSEADDR/SO_REUSEPORT report 1, not the fresh
// socket's 0). Options the guest sets AFTER the swap already land on the AF_UNIX fd directly.
static const int lo_carry_opts[] = {SO_REUSEADDR, SO_REUSEPORT, SO_RCVTIMEO,  SO_SNDTIMEO,
                                    SO_KEEPALIVE, SO_BROADCAST, SO_OOBINLINE, SO_LINGER};

static int stream_swap(int fd, int family) {
    int fl = fcntl(fd, F_GETFL), df = fcntl(fd, F_GETFD);
    // Snapshot the carried SOL_SOCKET options from the old (INET) fd before dup2 replaces it.
    unsigned char ov[sizeof lo_carry_opts / sizeof lo_carry_opts[0]][64];
    socklen_t ol[sizeof lo_carry_opts / sizeof lo_carry_opts[0]];
    for (unsigned i = 0; i < sizeof lo_carry_opts / sizeof lo_carry_opts[0]; i++) {
        ol[i] = sizeof ov[i];
        if (getsockopt(fd, SOL_SOCKET, lo_carry_opts[i], ov[i], &ol[i]) < 0) ol[i] = 0;
    }
    int u = socket(family, SOCK_STREAM, 0);
    if (u < 0) return -1;
    (void)hl_native_set_no_sigpipe(u);
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
    // Re-apply the carried options to the fresh AF_UNIX socket (best-effort: a value the AF_UNIX socket
    // rejects is simply skipped, exactly as it would be ignored on the INET original).
    for (unsigned i = 0; i < sizeof lo_carry_opts / sizeof lo_carry_opts[0]; i++)
        if (ol[i]) setsockopt(fd, SOL_SOCKET, lo_carry_opts[i], ov[i], ol[i]);
    return 0;
}

static int lo_swap(int fd) {
    return stream_swap(fd, AF_UNIX);
}

// Restore the real INET transport after bind(0.0.0.0, ...) selected the virtual switch but connect()
// targets an external address. The guest family is retained separately in g_sock_fam even while the host
// descriptor is AF_UNIX, so IPv4 and IPv6 are reconstructed without inspecting the substituted socket.
static int inet_stream_swap(int fd) {
    if (fd < 0 || fd >= HL_NFD) {
        errno = EBADF;
        return -1;
    }
    int family;
    if (g_sock_fam[fd] == AF_INET)
        family = AF_INET;
    else if (g_sock_fam[fd] == LX_AF_INET6_FAM)
        family = AF_INET6;
    else {
        errno = EAFNOSUPPORT;
        return -1;
    }
    return stream_swap(fd, family);
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
// network (same /16 as our own HL_IP, and not 127/8) is routed to an AF_UNIX socket at
//   /tmp/.hl-bridge-<HL_NETBR>/<ip>:<port>
// The listening container listens on /tmp/.hl-bridge-<netid>/<ownip>:<port>;
// a peer connect(<ownip>:<port>) dials the same path. Because every container on the host is a JIT
// process under the same user, the two AF_UNIX endpoints rendezvous with no bridge / TUN / root. The dir
// is keyed by <netid> (mode 0700, the guest is path-jailed) so other networks never share sockets. The
// 127/8 loopback path (g_netns / lo_*) is untouched and stays per-container; only non-127 in-subnet
// AF_INET is bridged. Off when g_netbr[0]==0 || g_myip==0.
struct br_interface {
    char path[64];
    uint32_t ip;
    uint8_t prefix;
};
static struct br_interface g_netif[HL_NETIF_MAX];
static uint8_t g_netif_count;
static uint16_t g_br_port[HL_NFD];     // fd -> virtual port of a bridge socket (0 = not a bridge socket)
static uint32_t g_br_ip[HL_NFD];       // fd -> virtual IP (network order) reported via getsockname/getpeername
static uint8_t g_br_interface[HL_NFD]; // fd -> interface index + 1
static int g_br_init;
static uint8_t g_icmp_kind[HL_NFD]; // 1=dgram ping socket, 2=raw ping socket
static uint8_t g_icmp_sock[HL_NFD]; // fd has been replaced by a reply socketpair
static uint32_t g_icmp_ip[HL_NFD];  // connected/last echo destination

// Carry the per-fd socket-emulation metadata (SOCK_STREAM-ness, loopback/bridge port + ip) from `src`
// to `dst` when an fd is duplicated/moved (dup/dup3/fcntl F_DUPFD). Without this, a guest that creates a
// TCP socket then relocates it to another fd number (e.g. busybox's xmove_fd -> a fixed low fd) loses the
// `g_sock_stream` flag that gates the loopback + per-network bridge bind/connect redirection, so its
// AF_INET traffic silently falls through to host passthrough and never rendezvous with a peer container.
static void fd_carry_sock(int dst, int src) {
    if (dst < 0 || dst >= HL_NFD || src < 0 || src >= HL_NFD) return;
    g_sock_stream[dst] = g_sock_stream[src];
    g_sock_dgram[dst] = g_sock_dgram[src];
    udp_ref_dup(dst, src);
    g_udp_local_port[dst] = g_udp_local_port[src];
    g_udp_peer_port[dst] = g_udp_peer_port[src];
    g_udp_local_ip[dst] = g_udp_local_ip[src];
    g_udp_peer_ip[dst] = g_udp_peer_ip[src];
    g_udp_local_interface[dst] = g_udp_local_interface[src];
    g_udp_peer_interface[dst] = g_udp_peer_interface[src];
    g_udp_local_v6[dst] = g_udp_local_v6[src];
    g_udp_peer_v6[dst] = g_udp_peer_v6[src];
    g_sock_seqpacket[dst] = g_sock_seqpacket[src];
    seq_ref_dup(dst, src);
    g_sock_pair_peer[dst] = g_sock_pair_peer[src]; // dup aliases the same end -> same partner
    g_sock_object[dst] = g_sock_object[src];
    g_sock_peer_object[dst] = g_sock_peer_object[src];
    g_sock_peer_pid[dst] = g_sock_peer_pid[src]; // ... and the same synthetic peer node identity
    g_sock_passcred[dst] = g_sock_passcred[src];
    g_sock_conn[dst] = g_sock_conn[src];
    g_sock_connecting[dst] = g_sock_connecting[src];
    g_sock_host_backed[dst] = g_sock_host_backed[src];
    g_sock_native_peer[dst] = g_sock_native_peer[src];
    g_so_error[dst] = g_so_error[src];
    g_so_reuseport[dst] = g_so_reuseport[src];
    memcpy(g_tcp_optval[dst], g_tcp_optval[src], sizeof g_tcp_optval[dst]);
    memcpy(g_tcp_optset[dst], g_tcp_optset[src], sizeof g_tcp_optset[dst]);
    memcpy(g_ipopt_val[dst], g_ipopt_val[src], sizeof g_ipopt_val[dst]);
    memcpy(g_ipopt_set[dst], g_ipopt_set[src], sizeof g_ipopt_set[dst]);
    g_sock_fam[dst] = g_sock_fam[src];
    g_lo_port[dst] = g_lo_port[src];
    g_lo_v6[dst] = g_lo_v6[src];
    g_lo_v6only[dst] = g_lo_v6only[src];
    g_br_port[dst] = g_br_port[src];
    g_br_ip[dst] = g_br_ip[src];
    g_br_interface[dst] = g_br_interface[src];
    g_tcp_lport[dst] = g_tcp_lport[src];
    g_tcp_laddr[dst] = g_tcp_laddr[src];
    g_tcp_l6[dst] = g_tcp_l6[src];
    g_tcp_listen[dst] = g_tcp_listen[src];
    g_sock_backlog[dst] = g_sock_backlog[src];
    memcpy(g_tcp_laddr6[dst], g_tcp_laddr6[src], 16);
    g_icmp_kind[dst] = g_icmp_kind[src];
    g_icmp_ip[dst] = g_icmp_ip[src];
}

// ---- listening-TCP introspection (ss/netstat -l): a socket the guest bind()+listen()s MUST appear in
// /proc/net/tcp[6] with state 0A (TCP_LISTEN). hl translates the guest's AF_INET(6) bind onto a host
// AF_UNIX switch (or a real host bind in passthrough), so the synthesized /proc/net/tcp table -- which
// runs no real IP stack -- has to remember the guest-requested (addr,port) itself. bind(200) records it;
// listen(201) arms g_tcp_listen; the vfs synth walks these to emit the LISTEN rows. Cleared on
// close/socket-reinit (fd_reset_emul) so a reused fd never reports a stale listener.
// Note: g_tcp_lport/laddr/l6/listen/laddr6 are declared up top alongside the other per-fd socket arrays.
static void netns_tcp_bind_note(int fd, uint16_t port_host, int v6, uint32_t addr4_be, const uint8_t *addr6) {
    if (fd < 0 || fd >= HL_NFD) return;
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
    if (fd >= 0 && fd < HL_NFD && g_tcp_lport[fd]) g_tcp_listen[fd] = 1;
}

// Update just the recorded local port after the initial bind_note. bind(:0) on the loopback/bridge switch
// records port 0 at bind time (the guest-requested port), then allocates a real ephemeral port -- without
// this write-back the /proc/net/tcp[6] row would carry :0000 AND be suppressed entirely (listen_note gates
// on a nonzero port). The address/family recorded by the earlier bind_note stay correct.
static void netns_tcp_port_note(int fd, uint16_t port_host) {
    if (fd >= 0 && fd < HL_NFD) g_tcp_lport[fd] = port_host;
}

// Emit the LISTEN rows for the v4 (v6==0) or v6 (v6==1) table into `out` (<=cap). Returns bytes written.
// Row layout mirrors the kernel's tcp4_seq/tcp6_seq: sl, local_address:port, rem 0, st 0A, queues 0,
// uid 0, a synthetic-but-stable inode, refcount 1. Values a real ss/netstat parses positionally.
static int netns_tcp_emit(char *out, size_t cap, int v6) {
    int off = 0, sl = 0;
    for (int fd = 0; fd < HL_NFD && off < (int)cap - 256; fd++) {
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
// the bridge needs no edit to the per-target startup code: HL_NETBR=<netid>, HL_IP=<dotted-quad>.
static void br_init(void) {
    if (g_br_init) return;
    g_br_init = 1;
    const char *interfaces = hl_option_get("HL_NETIFS");
    if (interfaces && interfaces[0]) {
        const char *line = interfaces;
        while (*line && g_netif_count < HL_NETIF_MAX) {
            const char *end = strchr(line, '\n');
            const char *equal = strchr(line, '=');
            size_t bridge_size;
            char ip[20];
            size_t ip_size;
            char *slash;
            char *prefix_end;
            unsigned long prefix;
            if (!end) end = line + strlen(line);
            if (!equal || equal >= end) break;
            bridge_size = (size_t)(equal - line);
            ip_size = (size_t)(end - equal - 1);
            if (bridge_size == 0 || bridge_size > 40 || ip_size == 0 || ip_size >= sizeof ip) break;
            snprintf(g_netif[g_netif_count].path, sizeof g_netif[g_netif_count].path, "/tmp/.hl-bridge-%.*s",
                     (int)bridge_size, line);
            memcpy(ip, equal + 1, ip_size);
            ip[ip_size] = 0;
            slash = strchr(ip, '/');
            if (!slash) break;
            *slash++ = 0;
            errno = 0;
            prefix = strtoul(slash, &prefix_end, 10);
            if (errno || prefix_end == slash || *prefix_end || prefix > 32) break;
            g_netif[g_netif_count].ip = br_parse_ip(ip);
            if (!g_netif[g_netif_count].ip) break;
            g_netif[g_netif_count].prefix = (uint8_t)prefix;
            hl_compat_mkdir(g_netif[g_netif_count].path, 0700);
            g_netif_count++;
            line = *end ? end + 1 : end;
        }
    } else {
        const char *nbr = hl_option_get("HL_NETBR");
        const char *dip = hl_option_get("HL_IP");
        if (nbr && nbr[0] && dip && dip[0]) {
            snprintf(g_netif[0].path, sizeof g_netif[0].path, "/tmp/.hl-bridge-%.40s", nbr);
            g_netif[0].ip = br_parse_ip(dip);
            if (g_netif[0].ip) {
                g_netif[0].prefix = 16;
                hl_compat_mkdir(g_netif[0].path, 0700);
                g_netif_count = 1;
            }
        }
    }
}

static int br_on(void) {
    if (!g_br_init) br_init();
    return g_netif_count != 0;
}

static int br_for_ip(uint32_t ip_be) {
    for (uint8_t i = 0; i < g_netif_count; i++) {
        uint8_t prefix = g_netif[i].prefix;
        uint32_t mask = prefix ? UINT32_MAX << (32u - prefix) : 0;
        if ((ntohl(ip_be) & mask) == (ntohl(g_netif[i].ip) & mask)) return (int)i;
    }
    return -1;
}

// connect(dest): bridge if AF_INET, non-127, in our subnet
static int br_connect_interface(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return -1;
    return br_for_ip(*(uint32_t *)(sa + 4));
}

// bind(addr): bridge if AF_INET, non-127, and 0.0.0.0 (ANY) / our own IP / in-subnet
static int br_bind_interface(const uint8_t *sa, socklen_t l) {
    if (!sa || l < 8 || *(uint16_t *)sa != AF_INET || sa[4] == 127) return -1;
    uint32_t ip = *(uint32_t *)(sa + 4);
    if (ip == 0) return g_netif_count ? 0 : -1;
    for (uint8_t i = 0; i < g_netif_count; i++)
        if (ip == g_netif[i].ip) return (int)i;
    return br_for_ip(ip);
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
static int br_path(int interface, uint32_t ip_be, uint16_t port, char *out, size_t n) {
    if (out == NULL || n == 0 || interface < 0 || interface >= g_netif_count) {
        if (out != NULL && n != 0) out[0] = 0;
        errno = EINVAL;
        return -1;
    }
    const uint8_t *b = (const uint8_t *)&ip_be;
    int size = snprintf(out, n, "%s/%u.%u.%u.%u:%u", g_netif[interface].path, b[0], b[1], b[2], b[3], (unsigned)port);
    if (size < 0 || (size_t)size >= n) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

static int br_v6only_path(int interface, uint32_t ip_be, uint16_t port, char *out, size_t n) {
    if (br_path(interface, ip_be, port, out, n) != 0) return -1;
    size_t length = strlen(out);
    int size = snprintf(out + length, n - length, ".v6only");
    if (size < 0 || (size_t)size >= n - length) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

// A wildcard listener belongs to every attached interface, not just eth0. The switch transport is one
// AF_UNIX listening socket, so expose that same inode at each interface's rendezvous name with symlinks.
// connect(2) follows the symlink to the bound socket. The aliases are owned by the socket's shared
// udp_ref and therefore disappear only when the final dup/fork reference closes.
static int br_alias_wildcard_listener(int fd, int primary, uint16_t port, int v6only) {
    char target[200];
    int target_status = v6only ? br_v6only_path(primary, g_netif[primary].ip, port, target, sizeof target)
                               : br_path(primary, g_netif[primary].ip, port, target, sizeof target);
    if (target_status != 0) return -1;
    for (uint8_t interface = 0; interface < g_netif_count; interface++) {
        if ((int)interface == primary) continue;
        char alias[200];
        int alias_status = v6only ? br_v6only_path(interface, g_netif[interface].ip, port, alias, sizeof alias)
                                  : br_path(interface, g_netif[interface].ip, port, alias, sizeof alias);
        if (alias_status != 0) return -1;
        unlink(alias);
        if (symlink(target, alias) < 0) return -1;
        if (udp_ref_add_alias(fd, alias) < 0) {
            int saved = errno;
            unlink(alias);
            errno = saved;
            return -1;
        }
    }
    return 0;
}

// bind(:0) on the bridge -> a free, round-trippable ephemeral port keyed by OUR ip (cf. lo_alloc_ephemeral)
static int br_alloc_ephemeral(int interface, uint16_t *port) {
    static uint16_t next;
    if (port == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (next < 1024) next = (uint16_t)(20000 + (getpid() & 0x3fff));
    for (int tries = 0; tries < 45000; tries++) {
        uint16_t cand = next++;
        if (next < 1024) next = 1024;
        if (cand < 1024) continue;
        char path[200];
        if (br_path(interface, g_netif[interface].ip, cand, path, sizeof path) != 0) return -1;
        if (access(path, F_OK) != 0) {
            *port = cand;
            return 0;
        }
    }
    errno = EADDRINUSE;
    return -1;
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
// (Uses the container's explicit port-map state from state.c.) TCP only for now;
// published UDP is a follow-up (the guest's UDP path isn't switch-redirected today).

// Is `cport` a published container port? (pm_host() returns cport on a miss, so it can't answer this.)
static int pm_published(uint16_t cport) {
    return hl_linux_ports_contains(&g_ports, cport);
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
    uint32_t address;
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
    sin.sin_addr.s_addr = fl.address;
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
    size_t path_len = strlen(path);
    if (path_len >= sizeof un.sun_path) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(un.sun_path, path, path_len + 1);
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
// (HL_PUBLISH_DAEMON=1), the engine must NOT open its own in-process host AF_INET listener — that listener
// lived in whichever guest process called listen(), so a prefork / re-listening server tore it down on
// every rebind and two guest processes raced EADDRINUSE. The daemon's listener (hl-daemon/containers/
// ports.rs) outlives every guest process and dials this container's switch inode per connection instead.
// The guest-side bind/listen->switch redirect + getsockname->cport reporting below are UNCHANGED (the
// daemon relies on them). Cached (env is fixed for the process).
static int g_hostfwd_daemon = -1;

static int hostfwd_by_daemon(void) {
    if (g_hostfwd_daemon < 0) g_hostfwd_daemon = (hl_option_get("HL_PUBLISH_DAEMON") != NULL);
    return g_hostfwd_daemon;
}

// Called from listen(): if `fd` is a published switch-backed listening socket, start its host forwarder.
static void fwd_maybe_start(int fd) {
    if (hostfwd_by_daemon()) return; // daemon owns the TCP host listener -> don't race it
    if (fd < 0 || fd >= HL_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        if (br_path((int)g_br_interface[fd] - 1, g_br_ip[fd], cport, upath, sizeof upath) != 0) return;
    } else if (g_lo_port[fd]) {
        cport = g_lo_port[fd];
        lo_tcp_path(cport, g_lo_v6only[fd], upath, sizeof upath);
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
    fl->address = pm_address(cport);
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

// Route every private-network IPv4 datagram through the same AF_UNIX rendezvous namespace as streams.
// The pathname is also the sender identity returned by recvfrom/recvmsg, so replies need no side table.
static int udp_switch_bind(int fd, int interface, uint32_t ip, uint16_t port) {
    char path[200];
    if (!port && interface >= 0) {
        if (br_alloc_ephemeral(interface, &port) != 0) return -1;
    } else if (!port) {
        port = lo_alloc_ephemeral();
    }
    if (!port) {
        errno = EADDRINUSE;
        return -1;
    }
    if (interface >= 0) {
        if (br_path(interface, ip, port, path, sizeof path) != 0) return -1;
    } else
        lo_path(port, path, sizeof path);
    struct sockaddr_un un;
    if (unix_addr_set(&un, path) < 0) return -1;
    if (!g_udp_local_port[fd] && udp_swap(fd) < 0) return -1;
    unlink(path);
    if (bind(fd, (struct sockaddr *)&un, sizeof un) < 0) return -1;
    if (udp_ref_create(fd, path) < 0) {
        int saved = errno;
        unlink(path);
        errno = saved;
        return -1;
    }
    g_udp_local_port[fd] = port;
    g_udp_local_ip[fd] = ip;
    g_udp_local_interface[fd] = (uint8_t)(interface + 1);
    return 0;
}

static int udp_switch_ensure_source(int fd, int interface) {
    if (g_udp_local_port[fd]) return 0;
    return udp_switch_bind(fd, interface, interface >= 0 ? g_netif[interface].ip : 0, 0);
}

static int udp_switch_destination(const uint8_t *sa, socklen_t len, int *interface, uint32_t *ip, uint16_t *port,
                                  char *path, size_t capacity) {
    if (lo_on() && lo6_is(sa, len)) {
        *ip = 0;
        *port = ntohs(*(const uint16_t *)(sa + 2));
        *interface = -1;
        lo_path(*port, path, capacity);
        return 1;
    }
    if (!sa || len < 8 || *(const uint16_t *)sa != AF_INET) return 0;
    *ip = *(const uint32_t *)(sa + 4);
    *port = ntohs(*(const uint16_t *)(sa + 2));
    if (lo_on() && sa[4] == 127) {
        *interface = -1;
        lo_path(*port, path, capacity);
        return 1;
    }
    *interface = br_on() ? br_for_ip(*ip) : -1;
    if (*interface >= 0) {
        if (br_path(*interface, *ip, *port, path, capacity) != 0) return -1;
        return 1;
    }
    return 0;
}

// Materialize the rendezvous address for a logically-connected switch-backed UDP socket. UDP connect
// records a default peer but deliberately leaves the AF_UNIX transport unconnected: unlike AF_UNIX,
// Linux UDP connect succeeds when no process is listening and reports refusal on later I/O.
static int udp_switch_peer_path(int fd, char *path, size_t capacity) {
    if (fd < 0 || fd >= HL_NFD || !g_udp_peer_port[fd]) return 0;
    int interface = (int)g_udp_peer_interface[fd] - 1;
    if (interface >= 0) {
        if (br_path(interface, g_udp_peer_ip[fd], g_udp_peer_port[fd], path, capacity) != 0) return -1;
    } else
        lo_path(g_udp_peer_port[fd], path, capacity);
    return 1;
}

// write(2)/writev(2) are valid send operations on a connected datagram socket. The private UDP
// transport deliberately keeps its AF_UNIX backing unconnected so Linux connect() can succeed before
// a peer binds; route descriptor writes to the recorded logical peer explicitly instead of writing the
// unconnected host socket and leaking EDESTADDRREQ to applications such as BusyBox nc.
static int udp_switch_write(int fd, const struct iovec *iov, int iov_count, int64_t *result) {
    char path[200];
    if (fd < 0 || fd >= HL_NFD || !g_sock_dgram[fd]) return 0;
    int route = udp_switch_peer_path(fd, path, sizeof path);
    if (route < 0) {
        *result = -errno;
        return 1;
    }
    if (route == 0) return 0;
    int interface = (int)g_udp_peer_interface[fd] - 1;
    if (udp_switch_ensure_source(fd, interface) < 0) {
        *result = -errno;
        return 1;
    }
    struct sockaddr_un address;
    if (unix_addr_set(&address, path) < 0) {
        *result = -errno;
        return 1;
    }
    struct msghdr message;
    memset(&message, 0, sizeof message);
    message.msg_name = &address;
    message.msg_namelen = sizeof address;
    message.msg_iov = (struct iovec *)iov;
    message.msg_iovlen = iov_count;
    ssize_t sent = sendmsg(fd, &message, 0);
    *result = sent < 0 ? -errno : sent;
    return 1;
}

static int udp_switch_source(const struct sockaddr_storage *source, socklen_t length, uint8_t *guest,
                             socklen_t *guest_length) {
    if (!source || source->ss_family != AF_UNIX || length < offsetof(struct sockaddr_un, sun_path) + 2) return 0;
    const char *path = ((const struct sockaddr_un *)source)->sun_path;
    unsigned port;
    if (g_netns[0] && !strncmp(path, g_netns, strlen(g_netns)) && sscanf(path + strlen(g_netns), "/p%u", &port) == 1) {
        fill_inet_lo(guest, guest_length, (uint16_t)port);
        return 1;
    }
    for (int i = 0; i < g_netif_count; i++) {
        size_t prefix = strlen(g_netif[i].path);
        unsigned a, b, c, d;
        if (!strncmp(path, g_netif[i].path, prefix) &&
            sscanf(path + prefix, "/%u.%u.%u.%u:%u", &a, &b, &c, &d, &port) == 5 && a < 256 && b < 256 && c < 256 &&
            d < 256 && port < 65536) {
            uint8_t bytes[4] = {(uint8_t)a, (uint8_t)b, (uint8_t)c, (uint8_t)d};
            uint32_t address;
            memcpy(&address, bytes, sizeof address);
            fill_inet_br(guest, guest_length, address, (uint16_t)port);
            return 1;
        }
    }
    return 0;
}

#define UDP_FWD_MAXPEERS 64

struct udp_peer {
    struct sockaddr_storage caddr; // host client addr (macOS layout, as recvfrom delivered it)
    socklen_t calen;
    int gs;          // guest-facing AF_UNIX/SOCK_DGRAM socket (bound to its own path,
                     // connected to the guest switch socket) -- this client's identity
    unsigned pathid; // the pseq used to build this peer's bound socket path (pdir/<pathid>); kept so the
                     // on-disk inode can be unlink'd when the peer is evicted or the forwarder tears down
    int used;
};

struct udp_fwd {
    uint16_t hport;
    uint32_t address;
    char upath[200]; // guest switch datagram socket path (br_/lo_path, as the guest bound)
    char pdir[80];   // dir holding this forwarder's synthetic per-client socket paths
    int hs;          // host AF_INET/SOCK_DGRAM socket on the published address and port
    struct udp_peer peers[UDP_FWD_MAXPEERS];
    int npeers;
    unsigned pseq; // monotonic id for unique synthetic peer paths (+ ring eviction)
};

// Remove a peer's bound AF_UNIX socket inode (pdir/<pathid>) from the host filesystem. Without this every
// distinct host UDP client leaves a socket file behind forever -> unbounded on-disk inode growth over a
// UDP-heavy container's life (only 64 peers are ever live, but eviction used to close the fd and orphan the
// file). Safe to call for any recorded peer; a missing file just makes unlink() a no-op.
static void udp_peer_unlink(const struct udp_fwd *f, unsigned pathid) {
    char path[sizeof f->pdir + 24];
    snprintf(path, sizeof path, "%s/%u", f->pdir, pathid);
    unlink(path);
}

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
        if (f->peers[slot].used) {
            close(f->peers[slot].gs);
            udp_peer_unlink(f, f->peers[slot].pathid); // drop the evicted client's on-disk socket inode
        }
        f->peers[slot].used = 0;
    }
    // On an eviction failure the slot is already cleared (used=0) above and stays reusable in place; we must
    // NOT decrement npeers here. !appended means the table was full (npeers==UDP_FWD_MAXPEERS), so lowering it
    // would drop the LAST slot out of the poll + teardown-close loops -> that peer's fd + inode would leak and
    // its forwarding would silently stop. Append failures never bumped npeers (done only on success below), so
    // they need no adjustment either. (Supersedes the earlier F5 npeers-- which caused exactly that hide/leak.)
    int gs = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (gs < 0) return -1;
    unsigned pathid = f->pseq++;
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s/%u", f->pdir, pathid);
    unlink(un.sun_path);
    if (bind(gs, (struct sockaddr *)&un, sizeof un) < 0) {
        close(gs);
        return -1;
    }
    struct sockaddr_un gu;
    memset(&gu, 0, sizeof gu);
    gu.sun_family = AF_UNIX;
    size_t upath_len = strlen(f->upath);
    if (upath_len >= sizeof gu.sun_path) {
        close(gs);
        unlink(un.sun_path);
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(gu.sun_path, f->upath, upath_len + 1);
    if (connect(gs, (struct sockaddr *)&gu, sizeof gu) < 0) {
        close(gs);
        unlink(un.sun_path);
        return -1;
    }
    if (appended) f->npeers++;
    f->peers[slot].used = 1;
    f->peers[slot].gs = gs;
    f->peers[slot].pathid = pathid;
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
    sin.sin_addr.s_addr = f->address;
    if (bind(hs, (struct sockaddr *)&sin, sizeof sin) < 0) { // host port busy -> no forwarding
        close(hs);
        fwd_unmark(g_udp_fwd_started, &g_nudpfwd, f->hport); // transient busy: allow a later bind() to retry (F7)
        free(f);
        return NULL;
    }
    f->hs = hs;
    snprintf(f->pdir, sizeof f->pdir, "/tmp/.hl-udp.%d.%u", (int)getpid(), (unsigned)f->hport);
    hl_compat_mkdir(f->pdir, 0700);
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
        if (f->peers[i].used) {
            close(f->peers[i].gs);
            udp_peer_unlink(f, f->peers[i].pathid); // release each live peer's socket inode on teardown
        }
    close(hs);
    rmdir(f->pdir); // all peer inodes are gone -> drop the now-empty per-forwarder dir
    free(f);
    return NULL;
}

// Called from the UDP bind hook once a published switch-backed datagram socket is bound: start its host
// forwarder. Mirrors fwd_maybe_start() but triggers at bind (UDP has no listen) and keys off g_*_port,
// which the bind hook just set on this fd.
static void udp_fwd_maybe_start(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    uint16_t cport = 0;
    char upath[200];
    if (g_br_port[fd]) {
        cport = g_br_port[fd];
        if (br_path((int)g_br_interface[fd] - 1, g_br_ip[fd], cport, upath, sizeof upath) != 0) return;
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
    f->address = pm_address(cport);
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
    if (fd < 0 || fd >= HL_NFD || !g_sock_dgram[fd]) return 0;
    uint16_t cport;
    char up[200];
    uint32_t myip = 0;
    int interface = br_bind_interface(sa, l);
    if (br_on() && interface >= 0) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0; // only explicit, published ports get switched
        myip = g_netif[interface].ip;
        if (br_path(interface, myip, cport, up, sizeof up) != 0) {
            *out = -errno;
            return 1;
        }
    } else if (lo_on() && lo_is(sa, l)) {
        cport = ntohs(*(const uint16_t *)(sa + 2));
        if (cport == 0 || !pm_published(cport)) return 0;
        lo_path(cport, up, sizeof up);
    } else {
        return 0;
    }
    size_t up_len = strlen(up);
    if (up_len >= sizeof(((struct sockaddr_un *)0)->sun_path)) {
        *out = -ENAMETOOLONG;
        return 1;
    }
    if (udp_swap(fd) < 0) {
        *out = -errno;
        return 1;
    }
    unlink(up);
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    memcpy(un.sun_path, up, up_len + 1);
    int r = bind(fd, (struct sockaddr *)&un, sizeof un);
    if (r == 0) {
        if (myip) {
            g_br_port[fd] = cport;
            g_br_ip[fd] = myip;
            g_br_interface[fd] = (uint8_t)(interface + 1);
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

static int saxl_on(void) {
    return 1;
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
#if defined(__APPLE__)
        m->sin_len = sizeof *m;
#endif
        m->sin_family = AF_INET;
        memcpy(&m->sin_port, g + 2, 2); // port (BE), same offset
        memcpy(&m->sin_addr, g + 4, 4); // addr (BE), same offset
        return (socklen_t)sizeof *m;    // 16
    }
    if (fam == LX_AF_INET6 && glen >= 24) {
        struct sockaddr_in6 *m = (struct sockaddr_in6 *)out;
        memset(m, 0, sizeof *m);
#if defined(__APPLE__)
        m->sin6_len = sizeof *m;
#endif
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

// ---- Per-workspace VPN egress redirect ---------------------------------------------------------------
// When HL_EGRESS_SOCKS="host:port" is armed, the guest's genuine external TCP connect()s are funneled
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
        const char *e = hl_option_get("HL_EGRESS_SOCKS");
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

// #261 — IPv4-only container network: hl models eth0 exactly like Docker's default bridge does — one IPv4
// address, NO global IPv6 address, and an empty IPv6 routing table (see the RTM_GETADDR/RTM_GETROUTE dumps
// in nl_emit_dump: eth0 gets only an AF_INET addr, and the only v6 addr is ::1 on lo). So a genuine external
// (global-unicast) IPv6 destination has NO ROUTE and, on a real container kernel, connect()/sendto() to it
// fails *immediately* with ENETUNREACH. hl must reproduce that instead of forwarding the dial to the
// underlying host's v6 stack: on a mac whose v6 path to the destination is black-holed (orbstack NAT is
// v4-only), that forward hangs the guest for the full ~2-minute connect timeout, and a happy-eyeballs client
// (apt, curl, glibc) that tried the AAAA first never falls back — the exact #261 stall. Failing fast is what
// lets it fall back to IPv4 in milliseconds, so `apt-get update` Just Works without Acquire::ForceIPv4.
// The AAAA record itself is still returned by the embedded resolver (dns_build_response), matching Docker's
// embedded DNS which also serves AAAA on a v4-only network — the guest learns the v6 addr, tries it, and is
// bounced instantly. Loopback (::1 -> private lo), link-local, unspecified, and the bridge/DNS classes are
// all peeled off before the direct-connect site, so only true external v6 reaches this predicate.
// When HL_EGRESS_SOCKS is armed the v6 destination is
// tunneled through the proxy instead (egress_should_redirect handles AF_INET6), so this is consulted only on
// the direct path, after that redirect has had its chance.
static int v6_no_route(const struct sockaddr *m) {
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
        if (w > 0) {
            off += (size_t)w;
            continue;
        }
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
        if (r > 0) {
            off += (size_t)r;
            continue;
        }
        if (r < 0 && errno == EINTR) continue;
        if (r == 0) errno = ECONNRESET; // proxy closed mid-handshake
        return -1;
    }
    return 0;
}

// Perform a SOCKS5 CONNECT to the macOS destination `m` over the guest socket `fd`, dialing the proxy in
// HL_EGRESS_SOCKS. Returns 0 (fd now relayed to the real dest through the tunnel) or -1/errno mirroring
// connect(). The guest fd is put in blocking mode for the short handshake and its O_NONBLOCK is restored
// after (a non-blocking guest that we return 0 to simply sees connect() complete immediately — legal).
static int egress_connect(int fd, const struct sockaddr *m, socklen_t mlen) {
    (void)mlen;
    const char *hp = egress_socks();
    if (!hp) {
        errno = EINVAL;
        return -1;
    }
    // Parse the proxy "host:port" (host is an IPv4 loopback literal, e.g. 127.30.0.1).
    const char *colon = strrchr(hp, ':');
    if (!colon || colon == hp) {
        errno = EINVAL;
        return -1;
    }
    char host[64];
    size_t hl = (size_t)(colon - hp);
    if (hl >= sizeof host) {
        errno = EINVAL;
        return -1;
    }
    memcpy(host, hp, hl);
    host[hl] = 0;
    int port = atoi(colon + 1);
    if (port <= 0 || port > 65535) {
        errno = EINVAL;
        return -1;
    }
    struct sockaddr_in px;
    memset(&px, 0, sizeof px);
#if defined(__APPLE__)
    px.sin_len = sizeof px;
#endif
    px.sin_family = AF_INET;
    px.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &px.sin_addr) != 1) {
        errno = EINVAL;
        return -1;
    }

    int fl = fcntl(fd, F_GETFL);
    int was_nb = (fl >= 0 && (fl & O_NONBLOCK));
    if (was_nb) fcntl(fd, F_SETFL, fl & ~O_NONBLOCK);

    int rc = -1, e = 0;
    do {
        int cr;
        do {
            cr = connect(fd, (struct sockaddr *)&px, sizeof px);
        } while (cr < 0 && errno == EINTR);
        if (cr != 0) {
            e = errno;
            break;
        } // proxy down -> report as the connect error
        // SOCKS5 greeting: VER=5, NMETHODS=1, METHOD=0 (no auth).
        uint8_t greet[3] = {0x05, 0x01, 0x00};
        if (egress_io_write(fd, greet, 3) != 0) {
            e = errno;
            break;
        }
        uint8_t sel[2];
        if (egress_io_read(fd, sel, 2) != 0) {
            e = errno;
            break;
        }
        if (sel[0] != 0x05 || sel[1] != 0x00) {
            e = ECONNREFUSED;
            break;
        } // no acceptable auth method
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
        if (egress_io_write(fd, req, (size_t)n) != 0) {
            e = errno;
            break;
        }
        // Reply: VER, REP, RSV, ATYP, bound-addr, bound-port. Read the 4-byte header, then drain the bound
        // address (length per ATYP) and 2-byte port so the fd is left clean at the start of the relayed stream.
        uint8_t rep[4];
        if (egress_io_read(fd, rep, 4) != 0) {
            e = errno;
            break;
        }
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
            if (egress_io_read(fd, &l, 1) != 0) {
                e = errno;
                break;
            }
            skip = l;
        }
        if (skip < 0) {
            e = ECONNREFUSED;
            break;
        } // unknown ATYP
        uint8_t junk[256];
        if (skip > 0 && egress_io_read(fd, junk, (size_t)skip) != 0) {
            e = errno;
            break;
        }
        if (egress_io_read(fd, junk, 2) != 0) {
            e = errno;
            break;
        } // bound port
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
    // Abstract-namespace name (leading NUL), including a kernel autobind address: an opaque binary blob,
    // not a filesystem path -- echo it to the guest verbatim (no volume/path translation, no NUL scan).
    if ((size_t)mlen > off && u->sun_path[0] == 0) {
        size_t alen = (size_t)mlen - off;
        uint8_t t[2 + sizeof u->sun_path];
        if (alen > sizeof u->sun_path) alen = sizeof u->sun_path;
        *(uint16_t *)t = AF_UNIX;
        memcpy(t + 2, u->sun_path, alen);
        int llen = (int)(2 + alen);
        if (g && gcap) memcpy(g, t, (size_t)gcap < (size_t)llen ? gcap : (size_t)llen);
        return llen;
    }
    size_t hplen = (size_t)mlen > off ? (size_t)mlen - off : 0; // path bytes the host reported (no NUL guarantee)
    char hpath[256];
    size_t i = 0;
    for (; i < hplen && i + 1 < sizeof hpath && u->sun_path[i]; i++)
        hpath[i] = u->sun_path[i];
    hpath[i] = 0;
    char canonical[4200];
    const char *backing = hpath;
    /* Kernels preserve the pathname spelling used at bind time, including symlinked ancestors. Volume
     * roots are canonical, so normalize an existing socket pathname before the prefix lookup; otherwise
     * a peer address created through /tmp -> /private/tmp escapes reverse mapping and cannot be echoed. */
    if (hpath[0] == '/' && canonicalize_path(hpath, canonical, sizeof canonical) == 0) backing = canonical;
    char gpath[256];
    int guest_backing = hpath[0] == '/' && g_rootfs != NULL;
    int matched_volume = -1;
    /* unix_sock_at binds from the socket's parent directory when the complete host path exceeds sun_path.
     * The kernel then reports only the leaf name to recvfrom. Recover an exact volume-root peer before
     * translating it; this is the common /tmp datagram shape and remains unambiguous across mounted roots. */
    if (hpath[0] != 0 && hpath[0] != '/')
        for (int volume = 0; volume < g_nvols; ++volume) {
            struct stat status;
            if (g_vols[volume].dead || g_vols[volume].isfile ||
                snprintf(canonical, sizeof canonical, "%s/%s", g_vols[volume].hcanon, hpath) >= (int)sizeof canonical)
                continue;
            if (lstat(canonical, &status) != 0 || !S_ISSOCK(status.st_mode)) continue;
            backing = canonical;
            guest_backing = 1;
            matched_volume = volume;
            break;
        }
    for (int volume = 0; volume < g_nvols; ++volume)
        if (!g_vols[volume].dead && !strncmp(backing, g_vols[volume].hcanon, g_vols[volume].hlen) &&
            (backing[g_vols[volume].hlen] == '/' || backing[g_vols[volume].hlen] == 0) &&
            (matched_volume < 0 || g_vols[volume].hlen > g_vols[matched_volume].hlen)) {
            guest_backing = 1;
            matched_volume = volume;
        }
    if (matched_volume >= 0) {
        if (path_concat(gpath, sizeof gpath, g_vols[matched_volume].guest, backing + g_vols[matched_volume].hlen) != 0)
            gpath[0] = 0;
    } else if (guest_backing) {
        if (guest_from_host(backing, gpath, sizeof gpath) <= 0) gpath[0] = 0;
    } else {
        snprintf(gpath, sizeof gpath, "%s", hpath); // unnamed/autobind (empty) or non-jail: pass through
    }
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
// abstract name to a real filesystem socket under a per-namespace dir keyed by HL_NETNS (same key as
// ipc_ns_key), so two guests in one container rendezvous and different containers stay isolated. The
// guest socket is already a real host AF_UNIX socket (case 198), so only the ADDRESS is rewritten.
static char g_absdir[200];
static int g_abs_init;

static void abs_init(void) {
    if (g_abs_init) return;
    g_abs_init = 1;
    const char *ns = hl_option_get("HL_NETNS"); // same key used by ipc_ns_key (service.c)
    snprintf(g_absdir, sizeof g_absdir, "/tmp/.hl-abstract-%.40s", (ns && ns[0]) ? ns : "default");
    hl_compat_mkdir(g_absdir, 0700); // EEXIST fine; peers share it (0700, guest is path-jailed)
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
// failed with "Address family not supported". hl models exactly two interfaces (lo + eth0; see
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
static int g_nl_peer[HL_NFD];

static int nl_is(int fd) {
    return fd >= 0 && fd < HL_NFD && g_nl_peer[fd];
}

// close a netlink fd's peer (called from fd_reset_emul on the guest close). Idempotent.
static void nl_close(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_nl_peer[fd]) {
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
    if (g < 0 || g >= HL_NFD) { // untracked fd range -> can't route sends; refuse cleanly
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

// One RTM_NEWLINK for a modelled interface slot (0=lo, 1=eth0). Shared by the dump and the
// single-interface (non-dump) query paths so both stay byte-identical.
static void nl_link_slot(uint8_t *b, size_t *o, uint32_t seq, int slot) {
    uint8_t zero6[6] = {0}, ff6[6] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff}, mac[6];
    if (slot == 0)
        nl_link(b, o, seq, "lo", 1, 772 /*ARPHRD_LOOPBACK*/, 0x10049u /*UP|LOOP|RUN|LOWER_UP*/, zero6, 65536, zero6);
    else {
        netif_eth0_mac(mac);
        nl_link(b, o, seq, "eth0", 2, 1 /*ARPHRD_ETHER*/, 0x11043u /*UP|BCAST|RUN|MCAST|LOWER_UP*/, mac, 1500, ff6);
    }
}

// Resolve a non-dump RTM_GETLINK target (ifi_index and/or IFLA_IFNAME) to a modelled slot, or -1 if
// no such interface exists (eth0 is absent under --network none). Mirrors the dump's lo(+eth0) set.
static int nl_link_slot_for(int32_t idx, const char *name) {
    if (idx == 1 || (name && strcmp(name, "lo") == 0)) return 0;
    if (!net_isolate() && (idx == 2 || (name && strcmp(name, "eth0") == 0))) return 1;
    return -1;
}

// Build + queue (one datagram to `peer`) the dump for request `type` with echoed `seq`.
static void nl_emit_dump(int peer, uint16_t type, uint32_t seq) {
    uint8_t out[4096];
    size_t o = 0;
    if (type == NL_RTM_GETLINK) {
        nl_link_slot(out, &o, seq, 0); // lo
        // --network none: loopback-only, so eth0 is absent from the link dump (`ip link` sees just lo).
        if (!net_isolate()) nl_link_slot(out, &o, seq, 1); // eth0
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
    *(uint32_t *)(out + 0) = 36;                               // nlmsg_len = hdr(16) + err(4) + echoed hdr(16)
    *(uint16_t *)(out + 4) = 2;                                // NLMSG_ERROR
    *(uint16_t *)(out + 6) = 0;                                // nlmsg_flags
    *(uint32_t *)(out + 8) = *(const uint32_t *)(req_hdr + 8); // echo the request's seq
    *(uint32_t *)(out + 12) = (uint32_t)getpid();              // nlmsg_pid == our port id (matches the dumps)
    *(int32_t *)(out + 16) = (int32_t)err;                     // nlmsgerr.error (negative errno; 0 == ACK)
    memcpy(out + 20, req_hdr, 16);                             // nlmsgerr.msg = echoed request header
    ssize_t w = send(peer, out, sizeof out, 0);
    (void)w;
}

// NLM_F_DUMP = NLM_F_ROOT(0x100)|NLM_F_MATCH(0x200); the kernel routes a request to the dump handler
// when either bit is set, otherwise to the single-object (.doit) handler.
#define NL_NLM_F_DUMP 0x300

// A non-dump RTM_GETLINK targets one interface by ifi_index or IFLA_IFNAME. Emit its single
// RTM_NEWLINK (no NLM_F_MULTI, no trailing NLMSG_DONE), or NLMSG_ERROR -ENODEV if it does not exist --
// matching the kernel's rtnl_getlink, where the old code wrongly replied with the whole link dump.
static void nl_getlink_one(int peer, const uint8_t *req, uint32_t nlen, uint32_t seq) {
    int32_t idx = 0;
    const char *name = NULL;
    char nbuf[64];
    if (nlen >= 16 + 16) idx = *(const int32_t *)(req + 16 + 4); // ifinfomsg.ifi_index
    // Optional IFLA_IFNAME attribute after the ifinfomsg.
    for (uint32_t ao = 16 + 16; ao + 4 <= nlen;) {
        uint16_t rlen = *(const uint16_t *)(req + ao), rtype = *(const uint16_t *)(req + ao + 2);
        if (rlen < 4 || ao + rlen > nlen) break;
        if (rtype == 3 /*IFLA_IFNAME*/) {
            uint16_t dl = rlen - 4;
            if (dl >= sizeof nbuf) dl = sizeof nbuf - 1;
            memcpy(nbuf, req + ao + 4, dl);
            nbuf[dl] = 0;
            name = nbuf;
        }
        ao += (rlen + 3u) & ~3u;
    }
    int slot = nl_link_slot_for(idx, name);
    if (slot < 0) {
        nl_error(peer, req, -ENODEV);
        return;
    }
    uint8_t out[1024];
    size_t o = 0;
    nl_link_slot(out, &o, seq, slot);
    *(uint16_t *)(out + 6) = 0; // clear NLM_F_MULTI: a single .doit reply carries no NLMSG_DONE
    ssize_t w = send(peer, out, o, 0);
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
        uint16_t nflags = *(const uint16_t *)(buf + off + 6);
        uint32_t nseq = *(const uint32_t *)(buf + off + 8);
        // RTM message groups run base+0=NEW, +1=DEL, +2=GET, +3=SET. A GET (type%4==2) is a read the dump
        // responder answers; a NEW/DEL/SET (type%4!=2) is a MODIFICATION hl has no writable netlink stack to
        // apply. Reply to modifications with a real NLMSG_ERROR (-EPERM) instead of the old phantom empty
        // NLMSG_DONE, so `ip addr/route add`/SETLINK fail loudly rather than silently succeeding unchanged.
        if (ntype >= 16 && (ntype % 4) != 2)
            nl_error(peer, buf + off, -EPERM);
        else if (ntype == NL_RTM_GETLINK && !(nflags & NL_NLM_F_DUMP))
            // `ip link show dev X`: a single-interface query, not a dump -> one reply or -ENODEV.
            nl_getlink_one(peer, buf + off, nlen < len - off ? nlen : (uint32_t)(len - off), nseq);
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
// The container's /etc/resolv.conf (provisioned by the daemon) points at 127.0.0.11 -- hl's embedded
// nameserver, the same address Docker uses. glibc/musl in the guest then send DNS as UDP (default) or TCP
// (fallback) to 127.0.0.11:53. We intercept those sends here, parse the query, resolve it via the macOS
// host resolver (getaddrinfo / getnameinfo -- which honor the host's system DNS, INCLUDING a corporate
// VPN's split-DNS, synthesize a wire-format DNS response, and make
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
#define HL_DNS_NS 0x0b00007fu      // 127.0.0.11, network byte order (bytes 7f 00 00 0b == LE u32 0x0b00007f)
static uint8_t g_dns_sock[HL_NFD]; // fd -> 1 if this fd is an intercepted, socketpair-backed DNS socket
static int g_dns_peer[HL_NFD];     // fd -> engine-held socketpair end we write synthesized responses into

static void synthetic_reply_send(int fd, const void *data, size_t size, int stream) {
    int flags = MSG_DONTWAIT;
#if defined(__linux__)
    flags |= MSG_NOSIGNAL;
#endif
    ssize_t sent;
    do
        sent = send(g_dns_peer[fd], data, size, flags);
    while (sent < 0 && errno == EINTR);
    if (stream && sent != (ssize_t)size) {
        close(g_dns_peer[fd]);
        g_dns_peer[fd] = -1;
    }
}

// DNS interception is off under HL_NET_ISOLATE: the isolated network has no resolver, so
// :53 to 127.0.0.11 is left to fall through to the (dead) host loopback and name resolution fails, matching.
static int g_dns_off = -1;

static int dns_enabled(void) {
    if (g_dns_off < 0) g_dns_off = (hl_option_get("HL_NET_ISOLATE") != NULL);
    return !g_dns_off;
}

// A Linux sockaddr_in destined for the embedded nameserver 127.0.0.11:53 (family value 2 == macOS AF_INET).
static int dns_dest_is(const uint8_t *sa, socklen_t l) {
    return sa && l >= 8 && *(const uint16_t *)sa == AF_INET && *(const uint16_t *)(sa + 2) == htons(53) &&
           *(const uint32_t *)(sa + 4) == HL_DNS_NS;
}

// Report the nameserver's address (127.0.0.11:53) back to the guest as the packet source / peer.
static void dns_fill_ns(uint8_t *sa, socklen_t *l) {
    if (!sa) return;
    *(uint16_t *)(sa + 0) = AF_INET;
    *(uint16_t *)(sa + 2) = htons(53);
    *(uint32_t *)(sa + 4) = HL_DNS_NS;
    memset(sa + 8, 0, 8);
    if (l) *l = 16;
}

// Swap the guest's AF_INET DNS socket for one end of an AF_UNIX socketpair (keeping the fd number + flags);
// stash the other end so send handlers can push synthesized responses into it. Idempotent per fd.
static int dns_swap(int fd, int stream) {
    if (fd < 0 || fd >= HL_NFD) return -1;
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
    (void)hl_native_set_no_sigpipe(sv[1]);
    g_dns_peer[fd] = sv[1];
    g_dns_sock[fd] = 1;
    return 0;
}

static uint16_t icmp_checksum(const void *data, size_t size) {
    const uint8_t *bytes = data;
    uint32_t sum = 0;
    while (size > 1) {
        sum += (uint32_t)((bytes[0] << 8) | bytes[1]);
        bytes += 2;
        size -= 2;
    }
    if (size) sum += (uint32_t)bytes[0] << 8;
    while (sum >> 16)
        sum = (sum & 0xffffu) + (sum >> 16);
    return htons((uint16_t)~sum);
}

static int icmp_swap(int fd) {
    int fl, df, sv[2];
    if (fd < 0 || fd >= HL_NFD) return -1;
    if (g_icmp_sock[fd]) return 0;
    fl = fcntl(fd, F_GETFL);
    df = fcntl(fd, F_GETFD);
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) < 0) return -1;
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
    fcntl(sv[1], F_SETFD, FD_CLOEXEC);
    (void)hl_native_set_no_sigpipe(sv[1]);
    g_dns_peer[fd] = sv[1];
    g_icmp_sock[fd] = 1;
    return 0;
}

static int icmp_try_send(int fd, const uint8_t *input, size_t size, const uint8_t *destination,
                         socklen_t destination_size, int64_t *result) {
    uint8_t reply[2048];
    uint8_t *icmp = reply;
    size_t reply_size = size;
    uint32_t peer;
    if (fd < 0 || fd >= HL_NFD || !g_icmp_kind[fd] || input == NULL || size < 8 || size > 2000) return 0;
    if (destination && destination_size >= 8 && *(const uint16_t *)destination == AF_INET)
        peer = *(const uint32_t *)(destination + 4);
    else
        peer = g_icmp_ip[fd];
    if (!peer) {
        *result = -EDESTADDRREQ;
        return 1;
    }
    // Loopback ping (127/8) is a purely local echo: the kernel reflects the request without touching a wire,
    // so synthesize the reply ourselves regardless of a configured bridge. This is the container-healthcheck
    // `ping 127.0.0.1` / `ping localhost` case. Off-loopback still requires a bridge route.
    int loopback = (peer & 0xffu) == 127u;
    if (!loopback && (!br_on() || br_for_ip(peer) < 0)) {
        *result = -ENETUNREACH;
        return 1;
    }
    if (icmp_swap(fd) < 0) return 0;
    g_icmp_ip[fd] = peer;
    if (g_icmp_kind[fd] == 2) {
        int bidx = br_for_ip(peer);
        memset(reply, 0, 20);
        reply[0] = 0x45;
        *(uint16_t *)(reply + 2) = htons((uint16_t)(20 + size));
        reply[8] = 64;
        reply[9] = 1;
        *(uint32_t *)(reply + 12) = peer;
        *(uint32_t *)(reply + 16) = bidx >= 0 ? g_netif[bidx].ip : peer;
        *(uint16_t *)(reply + 10) = icmp_checksum(reply, 20);
        icmp = reply + 20;
        reply_size += 20;
    }
    memcpy(icmp, input, size);
    if (icmp[0] == 8) icmp[0] = 0;
    icmp[2] = icmp[3] = 0;
    *(uint16_t *)(icmp + 2) = icmp_checksum(icmp, size);
    synthetic_reply_send(fd, reply, reply_size, 0);
    *result = (int64_t)size;
    return 1;
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
    br_init();
    for (uint8_t interface = 0; interface < g_netif_count; interface++) {
        char path[256];
        int path_size = snprintf(path, sizeof path, "%s/.names", g_netif[interface].path);
        if (path_size < 0 || (size_t)path_size >= sizeof path) continue;
        FILE *f = fopen(path, "re");
        if (!f) continue;
        char line[512];
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
                    fclose(f);
                    return 1;
                }
            }
        }
        fclose(f);
    }
    return 0;
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
    if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd] && g_dns_peer[fd] >= 0) {
        synthetic_reply_send(fd, resp, (size_t)rl, stream);
    }
    return (int64_t)len;
}

// Send-path entry used by sendto/send/sendmsg/sendmmsg (net.c) + write/writev (io.c). If `fd` is already a
// DNS socket, or `dst` targets 127.0.0.11:53 (lazy first-datagram swap for the unconnected sendto path),
// handle it and set *ret; otherwise return 0 so the caller runs the normal socket path.
static int dns_try_send(int fd, const uint8_t *buf, size_t len, const uint8_t *dst, socklen_t dstlen, int64_t *ret) {
    if (!dns_enabled()) return 0;
    int is_dns = (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]);
    if (!is_dns) {
        if (!dns_dest_is(dst, dstlen)) return 0;
        int stream = (fd >= 0 && fd < HL_NFD) ? g_sock_stream[fd] : 0;
        if (dns_swap(fd, stream) < 0) return 0; // couldn't swap -> let the normal path try
    }
    int stream = (fd >= 0 && fd < HL_NFD) ? g_sock_stream[fd] : 0;
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
