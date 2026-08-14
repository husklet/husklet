// hl/linux_abi/container -- termios (Linux<->macOS) + NET-ns private loopback (127/8 -> AF_UNIX).

#include "../../host_socket.h" // container DNS: getaddrinfo/getnameinfo via the macOS host resolver (dns_* below)

#include "../../memory_arena.h"
#include "../../../host/libc_compat.h" // hl_compat_mkdir: the UCRT's mkdir takes no mode
#include "../../checkpoint.h"
#include "../socket_identity.h"

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
#if defined(__linux__)
    int adopted = hl_host_process_fd_private_adopt(pfd);
    if (adopted < 0) {
        int e = -adopted;
        close(pfd);
        errno = e;
        return -1;
    }
    pfd = adopted;
    /* A bind performed after fchdir records only the leaf spelling. recvfrom then cannot reverse-map
     * that peer into its guest pathname. /proc/<pid>/fd keeps the sockaddr short while leaving an
     * absolute spelling that canonicalizes back to the complete overlay path. */
    if (snprintf(un.sun_path, sizeof un.sun_path, "/proc/%ld/fd/%d/%s", (long)getpid(), pfd, sl + 1) >=
        (int)sizeof un.sun_path) {
        close(pfd);
        errno = ENAMETOOLONG;
        return -1;
    }
    int rc = connecting ? connect(fd, (struct sockaddr *)&un, sizeof un) : bind(fd, (struct sockaddr *)&un, sizeof un);
    int e = errno;
    if (rc == 0 && !connecting && fd >= 0 && fd < HL_NFD) {
        if (g_unix_path_anchor[fd] > 0) {
            hl_host_process_fd_private_remove(g_unix_path_anchor[fd] - 1);
            close(g_unix_path_anchor[fd] - 1);
        }
        g_unix_path_anchor[fd] = pfd + 1;
    } else {
        hl_host_process_fd_private_remove(pfd);
        close(pfd);
    }
    errno = e;
    return rc;
#else
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
#endif
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
