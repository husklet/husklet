static int ctty_anchor(void) {
    for (int fd = 0; fd < 3; fd++)
        if (isatty(fd)) return fd;
    return -1;
}

// Is host fd `pfn` the controlling terminal (the same char device as the stdio pty)? True for fd 0/1/2 and
// for any dup of them; used to rename its /proc/self/fd/N link to /dev/pts/0. A guest-opened pty (its own
// /dev/pts/M master/slave) has a DIFFERENT rdev, so it is left alone.
static int fd_is_ctty(int pfn) {
    int a = ctty_anchor();
    if (a < 0 || pfn < 0 || !isatty(pfn)) return 0;
    struct stat sa, sp;
    return fstat(a, &sa) == 0 && fstat(pfn, &sp) == 0 && S_ISCHR(sp.st_mode) && sa.st_rdev == sp.st_rdev;
}

// ---- devpts: a guest-created pty must look like /dev/pts/<N> everywhere  --------------
// Real Linux/devpts numbers pty slaves sequentially from the lowest free index. `docker run -t` takes
// index 0 for the container's controlling terminal, so a guest that then openpty()s gets 1, 2, ...; with
// no controlling terminal the guest may take 0. hl's host pty is a macOS /dev/ttysNNN (or a host
// /dev/pts/M) whose raw name must NEVER leak into the guest -- the slave has to appear as /dev/pts/<N>
// everywhere: open (ahead of the overlay resolver), ptsname(3)/ttyname(3), readlink(/proc/self/
// fd/K), `ls /dev/pts`, and stat as a char device whose dev/ino/rdev match the real slave (glibc/musl
// ttyname compare these;). We map each index N to the host pty MASTER fd -- ptsname(master) resolves
// the host slave device the slave opens -- and stamp the index onto every open master/slave fd so the
// fd->path surface can rewrite it. Keeps the existing master-termios cache (keyed by master fd).
#define DEVPTS_MAX 1024
static int g_pts_master[DEVPTS_MAX];         // pts index N -> (host master fd + 1); 0 = free
static char g_pts_slavename[DEVPTS_MAX][64]; // pts index N -> host slave device path (ptsname of the master),
                                             // cached at pts_alloc. after a (forked) process closes its
                                             // master fd, pts_master_fd(N) can no longer resolve the slave via
                                             // ptsname(master), yet the pty is still alive if ANY other process
                                             // (e.g. the parent) holds the master -- so /dev/pts/N must resolve
                                             // by this cached host path. A host open() of it naturally succeeds
                                             // iff the pty is still alive and fails once it is truly gone.
static int g_fd_ptsn[HL_NFD];                // host fd -> (pts index + 1); 0 = not a pty fd
static uint8_t g_fd_ptsmaster[HL_NFD];       // 1 = this fd is the MASTER end, 0 = a slave

// Materialize/remove the on-disk /dev/pts/<N> node so `ls /dev/pts` reflects the live slaves (devpts
// creates the node when a slave is allocated and drops it when the pty is gone). Backed by an empty upper
// file; its stat()/open()/readlink are intercepted. No-op when the container has no rootfs (bare guest).
static int pts_node_path(int n, char *buf, size_t bn) {
    char directory[4200], leaf[16];
    int length = snprintf(leaf, sizeof leaf, "%d", n);
    if (length < 0 || (size_t)length >= sizeof leaf ||
        path_concat(directory, sizeof directory, g_rootfs_canon, "/dev/pts") != 0)
        return -1;
    return path_join(buf, bn, directory, leaf);
}

static void pts_publish(int n) {
    if (!g_rootfs_canon[0] || n < 0 || n >= DEVPTS_MAX) return;
    char p[4200];
    if (pts_node_path(n, p, sizeof p) != 0) return;
    (void)hl_host_file_create(&g_jit_services, p, 0620);
}

static void pts_unpublish(int n) {
    if (!g_rootfs_canon[0] || n < 0 || n >= DEVPTS_MAX) return;
    char p[4200];
    if (pts_node_path(n, p, sizeof p) != 0) return;
    unlink(p);
}

// Allocate the lowest free pts index for a new host master fd. Index 0 is reserved for the controlling
// terminal whenever the container has one (matching devpts, where the ctty grabbed 0 first).
static int pts_alloc(int masterfd) {
    int start = (ctty_anchor() >= 0) ? 1 : 0;
    for (int n = start; n < DEVPTS_MAX; n++) {
        if (!g_pts_master[n]) {
            g_pts_master[n] = masterfd + 1;
            if (masterfd >= 0 && masterfd < HL_NFD) {
                g_fd_ptsn[masterfd] = n + 1;
                g_fd_ptsmaster[masterfd] = 1;
            }
            // cache the host slave device path now, while the master is open, so /dev/pts/N still
            // resolves after a forked child closes its master (the parent keeps the pty alive).
            g_pts_slavename[n][0] = 0;
            char *sn = ptsname(masterfd);
            if (sn) {
                strncpy(g_pts_slavename[n], sn, sizeof g_pts_slavename[n] - 1);
                g_pts_slavename[n][sizeof g_pts_slavename[n] - 1] = 0;
            }
            return n;
        }
    }
    return -1;
}

static int pts_master_fd(int n) {
    return (n >= 0 && n < DEVPTS_MAX && g_pts_master[n]) ? g_pts_master[n] - 1 : -1;
}

static int pts_index_of_master(int fd) {
    return (fd >= 0 && fd < HL_NFD && g_fd_ptsmaster[fd]) ? g_fd_ptsn[fd] - 1 : -1;
}

static int pts_index_of_fd(int fd) {
    return (fd >= 0 && fd < HL_NFD && g_fd_ptsn[fd]) ? g_fd_ptsn[fd] - 1 : -1;
}

static int pts_fd_is_master(int fd) {
    return fd >= 0 && fd < HL_NFD && g_fd_ptsmaster[fd];
}

// the cached host slave device path for index N (empty string -> NULL). Used to resolve /dev/pts/N
// when this process no longer holds the master fd (a forked child closed it) but the pty is still alive.
static const char *pts_slave_name(int n) {
    return (n >= 0 && n < DEVPTS_MAX && g_pts_slavename[n][0]) ? g_pts_slavename[n] : NULL;
}

// Record a freshly-opened slave fd's pts index and publish its /dev/pts/N node.
static void pts_note_slave(int slavefd, int n) {
    if (slavefd >= 0 && slavefd < HL_NFD) {
        g_fd_ptsn[slavefd] = n + 1;
        g_fd_ptsmaster[slavefd] = 0;
    }
    pts_publish(n);
}

// close(2) / CLOEXEC-sweep teardown: a master frees its index (and its /dev/pts/N node); a slave clears
// only its own entry (other slaves / the master keep the pty alive).
static void pts_on_close(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_fd_ptsn[fd]) return;
    if (g_fd_ptsmaster[fd]) {
        int n = g_fd_ptsn[fd] - 1;
        if (n >= 0 && n < DEVPTS_MAX) g_pts_master[n] = 0;
        pts_unpublish(n);
    }
    g_fd_ptsn[fd] = 0;
    g_fd_ptsmaster[fd] = 0;
}

// Fill *s from the REAL host slave for /dev/pts/N (a guest-created pty), by opening a transient slave via
// the master's host device -- so st_dev/st_ino/st_rdev EXACTLY equal fstat(slavefd), which ttyname(3)
// compares. Returns 1 (char device) on success. N==0 with a ctty is handled by the caller (synth_stat_raw).
static int devpts_slave_stat(int n, struct stat *s) {
    int mfd = pts_master_fd(n);
    const char *sn = (mfd >= 0) ? ptsname(mfd) : NULL;
    if (!sn) sn = pts_slave_name(n); // master closed in this (forked) process; use the cached path
    if (!sn) return 0;
    int t = open(sn, O_RDWR | O_NOCTTY);
    if (t < 0) t = open(sn, O_RDONLY | O_NOCTTY);
    if (t < 0) return 0;
    int ok = fstat(t, s) == 0;
    close(t);
    if (ok) {
        // The backing host devpts instance belongs to the host login user/group.  A private container
        // devpts mount instead creates slaves with uid 0, gid=5 and mode=620 (the options published in
        // mountinfo).  Keep the real device identity used by ttyname(3), but project container ownership.
        s->st_uid = 0;
        s->st_gid = 5;
        s->st_mode = S_IFCHR | 0620;
    }
    return ok && S_ISCHR(s->st_mode);
}

static int dev_node_is_ptmx(const char *gp) {
    return gp && (!strcmp(gp, "/dev/ptmx") || !strcmp(gp, "/dev/pts/ptmx"));
}

static const char *dev_node_hostpath(const char *gp) {
    if (!gp) return NULL;
    return !strcmp(gp, "/dev/null")      ? "/dev/null"
           : !strcmp(gp, "/dev/zero")    ? "/dev/zero"
           : !strcmp(gp, "/dev/full")    ? "/dev/zero" // /dev/full reads return zeros (writes ENOSPC, gated by fd flag)
           : !strcmp(gp, "/dev/random")  ? "/dev/random"
           : !strcmp(gp, "/dev/urandom") ? "/dev/urandom"
           : !strcmp(gp, "/dev/tty")     ? "/dev/tty"
           : !strcmp(gp, "/dev/console") ? "/dev/null" // no host console in the jail -> back it with /dev/null
           : dev_node_is_ptmx(gp) ? "/dev/ptmx" // both Linux spellings name the same devpts multiplexer
                                         : NULL;
}

// Populate the container's /dev at start-up. hl flattens the image into one rootfs (no per-container
// devtmpfs) and the OCI unpacker strips every `dev/*` node (unprivileged mknod fails on macOS), so the
// rootfs /dev is empty. Docker mounts a fresh /dev with these standard entries; we materialize the ones
// that don't need a privileged mknod straight in the writable upper so they appear in `ls /dev`, stat,
// and readlink -- while the char devices (null/zero/tty/ptmx/console) keep working through the fs.c
// open()/stat() synth. The big win is the /proc/self/fd symlinks: bash process substitution and postgres
// initdb open /dev/fd/63, and these plus procfd_num() in fs.c make that resolve. Idempotent (EEXIST ok).
static void container_populate_dev(void) {
    if (!g_rootfs_canon[0]) return;
    char base[4200];
    if ((size_t)snprintf(base, sizeof base, "%s/dev", g_rootfs_canon) >= sizeof base) return;
    size_t bl = strlen(base);
    hl_compat_mkdir(base, 0755); // ensure /dev exists (image /dev contents were excluded at unpack)
    // helper: build <rootfs>/dev/<leaf> into a scratch buffer
#define DEVP(leaf) (snprintf(base + bl, sizeof base - bl, "/%s", (leaf)), base)
#define DEVP2(d, leaf) (snprintf(base + bl, sizeof base - bl, "/%s/%s", (d), (leaf)), base)
    // /dev/fd + the std stream aliases: the standard Linux symlinks into /proc/self/fd (which the engine
    // already synthesizes). readlink/ls see the symlink; open("/dev/fd/N") is caught by procfd_num().
    // These are independent namespace entries.  An image is allowed to ship one of them already (and a
    // malformed image can even ship the wrong node type); failure to create that one must not suppress the
    // devpts mount below.  In particular dpkg requires /dev/pts even though it never uses /dev/fd.
    (void)symlink_idempotent("/proc/self/fd", DEVP("fd"));
    (void)symlink_idempotent("/proc/self/fd/0", DEVP("stdin"));
    (void)symlink_idempotent("/proc/self/fd/1", DEVP("stdout"));
    (void)symlink_idempotent("/proc/self/fd/2", DEVP("stderr"));
    // char-device placeholders so they list in /dev; open()/stat() are intercepted by the fs.c synth
    // (dev_node_hostpath), so the empty file is never actually read/written.
    static const char *const chr[] = {"null", "zero", "full", "random", "urandom", "tty", "console"};
    for (size_t i = 0; i < sizeof chr / sizeof *chr; i++) {
        int fd = open(DEVP(chr[i]), O_CREAT | O_WRONLY, 0666);
        if (fd >= 0) close(fd);
    }
    hl_compat_mkdir(DEVP("pts"), 0755); // devpts mount point; /dev/pts/N slaves resolve via ptsname in fs.c
    // devpts publishes a /dev/pts/ptmx multiplexer node (docker mounts it with ptmxmode=0666); `ls /dev/pts`
    // lists it, and open("/dev/pts/ptmx") is intercepted like /dev/ptmx in fs.c.
    {
        int fd = open(DEVP("pts/ptmx"), O_CREAT | O_WRONLY, 0666);
        if (fd >= 0) {
            (void)fchmod(fd, 0666); // host umask must not alter devpts' ptmxmode=0666 contract
            close(fd);
        }
    }
    // Linux exposes /dev/ptmx as the devpts multiplexer.  Use the conventional relative link so path
    // inspection and open through either spelling agree; images that already provide it are left intact.
    (void)symlink_idempotent("pts/ptmx", DEVP("ptmx"));
    // When the container was handed a controlling terminal (docker run -t: the daemon's login_tty made fd
    // 0/1/2 the pty slave), Linux/devpts names it /dev/pts/0. Materialize that entry so `ls /dev/pts` lists
    // it; stat()/open()/readlink of /dev/pts/0 are intercepted (synth_stat_raw + fs.c) and routed to the
    // real controlling tty, so ttyname(3)/`tty`/`ps` resolve it instead of leaking the host pty device name.
    if (isatty(0) || isatty(1) || isatty(2)) {
        int fd = open(DEVP("pts/0"), O_CREAT | O_WRONLY, 0620);
        if (fd >= 0) {
            (void)fchmod(fd, 0620); // devpts mode=620, independent of the engine process's umask
            close(fd);
        }
    }
    hl_compat_mkdir(DEVP("shm"), 01777); // POSIX shm dir (shm_open names get redirected to a host tmp file in fs.c)
    hl_compat_mkdir(DEVP("mqueue"), 01777);
#undef DEVP
#undef DEVP2
}

// materialize /etc/machine-id (32 lowercase hex + newline) so libdbus/systemd/journald/gnome find a
// stable machine identity that AGREES with /proc/sys/kernel/random/boot_id (both derive from the same
// per-container boot bytes). Only written when the image ships no machine-id (missing or empty) -- an
// image/user-provisioned id is left untouched. Written straight into the writable upper (a real file), so
// reads need no interception. /var/lib/dbus/machine-id (the legacy dbus path) is filled the same way when
// its directory exists. Idempotent.
// read a small guest text file (/etc/passwd, /etc/group) through the overlay-aware resolver so an
// image whose /etc lives only in a read-only lower is handled, not just the flat-rootfs upper. Returns the
// byte count read (NUL-terminated in `b`), or 0 if absent/unreadable. Best-effort at container init.
static int read_guest_text(const char *guest, char *b, size_t n) {
    char host[4300];
    const char *hp = xresolve_overlay(guest, host, sizeof host);
    if (!hp) return 0;
    int fd = open(hp, O_RDONLY);
    if (fd < 0) return 0;
    size_t got = 0;
    for (;;) {
        if (got + 1 >= n) break;
        ssize_t r = read(fd, b + got, n - 1 - got);
        if (r <= 0) break;
        got += (size_t)r;
    }
    close(fd);
    b[got] = 0;
    return (int)got;
}

// build the run user's supplementary group set exactly like runc's additionalGids (see state.c). Find
// the run user (g_uid, default 0=root) in /etc/passwd -> its NAME + primary gid; seed the set with the
// primary gid; then scan /etc/group in file order and append every group whose 4th (member) field lists that
// NAME -- NO dedup, so the set matches runc byte-for-byte (incl. alpine root's duplicate leading 0). Bare
// mode (no rootfs) leaves the set unparsed. Populates the state.c g_groups[]/g_ngroups + g_groups_parsed.
static void container_parse_groups(void) {
    if (!g_rootfs_canon[0]) return; // bare mode: host getgroups fallback, empty status Groups line (as before)
    int run_uid = cuid();
    char uname[64] = "";
    int primary_gid = cgid(); // container's configured primary gid (default 0); == the passwd gid for root
    static char pw[1 << 16];
    if (read_guest_text("/etc/passwd", pw, sizeof pw) > 0) {
        // passwd line: name:passwd:uid:gid:gecos:home:shell -- find the entry whose uid == run_uid.
        for (char *line = strtok(pw, "\n"); line; line = strtok(NULL, "\n")) {
            char *c1 = strchr(line, ':');
            if (!c1) continue;
            char *c2 = strchr(c1 + 1, ':');
            if (!c2) continue;
            char *c3 = strchr(c2 + 1, ':');
            if (!c3) continue;
            *c3 = 0;
            int uid = atoi(c2 + 1); // field 3 (uid)
            if (uid != run_uid) continue;
            *c1 = 0;
            snprintf(uname, sizeof uname, "%s", line); // field 1 (name)
            break;
        }
    }
    if (!uname[0] && run_uid == 0) snprintf(uname, sizeof uname, "root"); // minimal image lacking /etc/passwd
    groups_reset();
    groups_append((gid_t)primary_gid); // additionalGids always begins with the primary gid
    if (!uname[0]) {
        g_groups_parsed = 1;
        return;
    } // no name to match -> primary gid only
    static char gr[1 << 16];
    if (read_guest_text("/etc/group", gr, sizeof gr) > 0) {
        // group line: name:passwd:gid:member,member,... -- append gid iff the member list contains uname.
        for (char *line = strtok(gr, "\n"); line; line = strtok(NULL, "\n")) {
            char *c1 = strchr(line, ':');
            if (!c1) continue;
            char *c2 = strchr(c1 + 1, ':');
            if (!c2) continue;
            char *c3 = strchr(c2 + 1, ':');
            if (!c3) continue;
            int gid = atoi(c2 + 1);       // field 3 (gid)
            const char *members = c3 + 1; // field 4 (comma-separated names), may be empty
            int hit = 0;
            for (const char *m = members; *m && !hit;) {
                const char *e = strchr(m, ',');
                size_t len = e ? (size_t)(e - m) : strlen(m);
                if (len == strlen(uname) && !strncmp(m, uname, len)) hit = 1;
                m = e ? e + 1 : m + len;
            }
            if (hit) groups_append((gid_t)gid);
        }
    }
    g_groups_parsed = 1;
}

static void container_populate_machine_id(void) {
    if (!g_rootfs_canon[0]) return;
    uint8_t b[16];
    boot_id_bytes(b);
    char id[40];
    int idn = 0;
    for (int i = 0; i < 16; i++)
        idn += snprintf(id + idn, sizeof id - (size_t)idn, "%02x", b[i]);
    id[idn++] = '\n';
    static const char *const paths[] = {"/etc/machine-id", "/var/lib/dbus/machine-id", 0};
    for (int i = 0; paths[i]; i++) {
        char p[4200];
        if ((size_t)snprintf(p, sizeof p, "%s%s", g_rootfs_canon, paths[i]) >= sizeof p) continue;
        struct stat s;
        if (stat(p, &s) == 0) {
            if (S_ISREG(s.st_mode) && s.st_size > 0) continue; // a real id already present -> keep it
        } else if (i == 1) {
            continue; // don't create the legacy dbus dir if the image lacks it
        }
        int fd = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (fd >= 0) {
            if (write(fd, id, (size_t)idn) < 0) { /* best-effort */
            }
            close(fd);
        }
    }
}

// -> macOS struct stat for a synth file
// ---- renameat2(RENAME_WHITEOUT) whiteout markers -------------------------------------------------
// Linux renameat2(...,RENAME_WHITEOUT) renames src->dst AND leaves a whiteout at the source: a character
// device with rdev 0,0 (the same on-disk token overlayfs uses to mask a lower entry). macOS cannot mknod a
// device node rootless, so hl records the source GUEST path here and the stat layer (synth_stat_raw)
// fabricates the S_IFCHR/0,0 whiteout inode for it -- so lstat(src) reports a char device exactly like
// Linux (the finding's observable). The marker is self-cleaning: whiteout_present() re-checks the backing
// file and forgets the entry once a real file exists at the path again (create-over / a later rename onto
// it), so a stale whiteout can never mask a real inode. In overlay mode the caller ALSO drops the `.wh.`
// union marker (overlay_whiteout) so a lower entry the source used to shadow stays hidden.
#define WHITEOUT_N 256
static char g_whiteout[WHITEOUT_N][4200];
static int g_nwhiteout;

static int whiteout_slot(const char *gp) {
    for (int i = 0; i < g_nwhiteout; i++)
        if (!strcmp(g_whiteout[i], gp)) return i;
    return -1;
}

static void whiteout_forget(const char *gp) {
    if (!gp) return;
    int i = whiteout_slot(gp);
    if (i < 0) return;
    if (i != g_nwhiteout - 1) memcpy(g_whiteout[i], g_whiteout[g_nwhiteout - 1], sizeof g_whiteout[0]);
    g_nwhiteout--;
}

static void whiteout_note(const char *gp) {
    if (!gp || !gp[0]) return;
    if (whiteout_slot(gp) >= 0) return;
    if (g_nwhiteout >= WHITEOUT_N) return; // registry full -> best-effort (rare; whiteouts are transient)
    snprintf(g_whiteout[g_nwhiteout], sizeof g_whiteout[0], "%s", gp);
    g_nwhiteout++;
}

// Is `gp` a live whiteout marker (no real backing file)? Self-cleans: if a real inode now occupies the
// path, the whiteout was consumed -> forget it and report "not a whiteout" so the real file wins.
static int whiteout_present(const char *gp) {
    if (!g_nwhiteout || !gp) return 0;
    if (whiteout_slot(gp) < 0) return 0;
    char hb[4300];
    const char *hp = xresolve_overlay(gp, hb, sizeof hb);
    struct stat st;
    if (hp && lstat(hp, &st) == 0) { // a real file reappeared here -> the whiteout is stale
        whiteout_forget(gp);
        return 0;
    }
    return 1;
}

static void synth_stat_set(struct stat *s, mode_t mode, nlink_t links) {
    memset(s, 0, sizeof *s);
    s->st_mode = mode;
    s->st_nlink = links;
}

static int synth_namespace_stat(const char *gp, struct stat *s) {
    char self_path[4200];
    int pid = 0, host = 0;
    const char *leaf = proc_any_leaf(proc_deself(gp, self_path, sizeof self_path), &pid);
    if (!leaf || strncmp(leaf, "ns/", 3) || !leaf[3] ||
        (pid != container_pid() && pid != (int)getpid() && !proc_pid_member(pid, &host)))
        return 0;
    char target[64];
    int length = ns_link_target(leaf + 3, target, sizeof target);
    char *open = length > 0 ? strchr(target, '[') : NULL;
    char *close = open ? strchr(open + 1, ']') : NULL;
    if (!open || !close || close[1] != 0) return 0;
    *close = 0;
    char *end = NULL;
    unsigned long long inode = strtoull(open + 1, &end, 10);
    if (end == open + 1 || *end != 0) return 0;
    synth_stat_set(s, S_IFREG | 0444, 1);
    s->st_ino = (ino_t)inode;
    return 1;
}

static int synth_device_stat(const char *gp, struct stat *s) {
    // The controlling terminal, named /dev/pts/0 in the container: fstat the real pty slave so it reports as
    // a character device with the correct rdev. ttyname(3) reads /proc/self/fd/0 -> "/dev/pts/0", then
    // stat()s it and checks S_ISCHR + rdev == fstat(0).rdev; this makes that check pass so `tty` prints
    // /dev/pts/0 instead of "not a tty".
    if (gp && !strcmp(gp, "/dev/pts/0")) {
        int a = ctty_anchor();
        if (a >= 0 && fstat(a, s) == 0) {
            s->st_uid = 0;
            s->st_gid = 5;
            s->st_mode = S_IFCHR | 0620;
            return 1;
        }
        // no ctty: /dev/pts/0 may instead be a guest-allocated slave -> handled by the devpts case below
    }
    // A guest-created pty slave /dev/pts/N (openpty/posix_openpt): fstat the real host slave so it reports
    // as a char device with dev/ino/rdev matching fstat(slavefd) -- what ptsname(3)/ttyname(3) verify.
    if (gp && !strncmp(gp, "/dev/pts/", 9) && gp[9] >= '0' && gp[9] <= '9' && devpts_slave_stat(atoi(gp + 9), s))
        return 1;
    // Pseudo /dev char devices: stat the host node so type/existence agree with open(), then OVERRIDE the
    // rdev + mode with the Linux-canonical values. The host node carries macOS's own major/minor, but Linux
    // fixes these numbers (null 1:3, zero 1:5, full 1:7, random 1:8, urandom 1:9, tty 5:0, console 5:1) and
    // software that checks st_rdev (or `ls -l` which renders "major, minor") must see the Linux encoding.
    const char *dev = dev_node_hostpath(gp);
    if (!dev) return -1;
    if (stat(dev, s) != 0) return 0;

    static const struct {
        const char *p;
        int maj, min;
        unsigned mode;
    } devices[] = {{"/dev/null", 1, 3, 0666},    {"/dev/zero", 1, 5, 0666},
                   {"/dev/full", 1, 7, 0666},    {"/dev/random", 1, 8, 0666},
                   {"/dev/urandom", 1, 9, 0666}, {"/dev/tty", 5, 0, 0666},
                   {"/dev/console", 5, 1, 0600}, {"/dev/ptmx", 5, 2, 0666},
                   {"/dev/pts/ptmx", 5, 2, 0666}, {0, 0, 0, 0}};

    for (int i = 0; devices[i].p; i++)
        if (!strcmp(gp, devices[i].p)) {
            s->st_rdev = (dev_t)(((uint64_t)devices[i].maj << 8) | (unsigned)devices[i].min);
            s->st_mode = S_IFCHR | devices[i].mode;
            s->st_uid = 0;
            s->st_gid = 0;
            break;
        }
    return 1;
}

static int synth_masked_stat(const char *gp, struct stat *s) {
    if (!g_rootfs) return 0;
    // runc MaskedPaths / ReadonlyPaths: these must EXIST (a masked file is an empty regular file; a masked or
    // read-only dir is an empty directory), so stat()/`test -e` see them present -- matching runc, not ENOENT.
    int kind = proc_masked_kind(gp);
    if (kind == 1) {
        synth_stat_set(s, S_IFREG | 0444, 1);
        return 1;
    }
    if (kind == 2 || proc_ro_dir(gp)) {
        synth_stat_set(s, S_IFDIR | 0555, 2);
        return 1;
    }
    if (strcmp(gp, "/proc/sysrq-trigger")) return 0;
    synth_stat_set(s, S_IFREG | 0644, 1);
    return 1;
}

static int synth_sysnet_stat(const char *gp, struct stat *s) {
    // /sys/class/net: the class dir + per-iface dirs are directories; attribute files are regular.
    if (sysnet_hidden(gp)) return 0;
    const char *r = gp + 14;
    // --network none: eth0 (and its statistics/ subdir) does not exist -- direct stat must ENOENT to
    // match the readdir listing, which already omits eth0 under isolation.
    int eth_ok = !net_isolate();
    int isdir = (r[0] == 0 || (r[0] == '/' && r[1] == 0) ||             // /sys/class/net
                 (r[0] == '/' && (!strcmp(r + 1, "lo") ||               // iface dir
                                  (eth_ok && !strcmp(r + 1, "eth0")) || // eth0 iface dir
                                  !strcmp(r + 1, "lo/statistics") ||    // statistics/
                                  (eth_ok && !strcmp(r + 1, "eth0/statistics")))));
    if (isdir) {
        synth_stat_set(s, S_IFDIR | 0555, 2);
        return 1;
    }
    int fd = proc_open(gp);
    if (fd < 0) return 0;
    if (fstat(fd, s) != 0) {
        close(fd);
        return 0;
    }
    close(fd);
    s->st_mode = S_IFREG | 0444;
    s->st_nlink = 1;
    return 1;
}

static int synth_syscpu_kind(const char *gp) {
    // the CPU-topology sysfs tree must stat as PRESENT so tools that stat a path BEFORE opening it
    // (busybox `ls`/glob, `find`, `test -d`, coreutils stat) don't bail ENOENT under the rootfs overlay --
    // those synthetic paths live in no image layer. htop's opendir bypasses stat, but everyone else needs
    // this. Directories: the base /sys/devices/system/cpu and each cpuN in [0, online-count). Regular files:
    // the online/possible/present/offline range files (content served on open via the fs.c cpu synth).
    const char *r = gp + 23;
    if (r[0] == 0 || (r[0] == '/' && r[1] == 0)) return 2;
    if (r[0] != '/') return 0;
    const char *leaf = r + 1;
    if (!strcmp(leaf, "online") || !strcmp(leaf, "possible") || !strcmp(leaf, "present") || !strcmp(leaf, "offline"))
        return 1;
    if (strncmp(leaf, "cpu", 3) || leaf[3] < '0' || leaf[3] > '9') return 0;
    const char *suffix = leaf + 3;
    int cpu = 0;
    for (; *suffix >= '0' && *suffix <= '9'; suffix++)
        cpu = cpu * 10 + (*suffix - '0');
    if (cpu >= container_online_cpus()) return 0;
    if (*suffix == 0 || !strcmp(suffix, "/topology")) return 2;
    if (strncmp(suffix, "/topology/", 10)) return 0;
    char content[96];
    return syscpu_topology_content(gp, content, sizeof content) >= 0;
}

static int synth_syscpu_stat(const char *gp, struct stat *s) {
    int kind = synth_syscpu_kind(gp);
    if (!kind) return 0;
    synth_stat_set(s, kind == 2 ? (S_IFDIR | 0555) : (S_IFREG | 0444), kind == 2 ? 2 : 1);
    return 1;
}

static int synth_proc_root_stat(const char *gp, struct stat *s) {
    // A bare /proc/self (the magic symlink) or /proc/<pid> directory for an introspectable pid (this
    // process, the container init "1", or our container pid): report the right type so stat()/opendir()
    // succeed and `ps`/`ls /proc` can descend. proc_self_leaf only matches paths WITH a leaf, so handle
    // the no-leaf directory form here.
    if (!strcmp(gp, "/proc/self")) {
        synth_stat_set(s, S_IFLNK | 0777, 1);
        char num[16];
        s->st_size = snprintf(num, sizeof num, "%d", container_pid()); // symlink target = our pid
        return 1;
    }
    {
        const char *q = gp + 6; // tail after "/proc/"
        int isnum = q[0] >= '0' && q[0] <= '9';
        for (const char *t = q; *t && isnum; t++)
            if (*t < '0' || *t > '9') isnum = 0;
        if (isnum) {
            int pid = atoi(q), host;
            // our own pid / the init "1", OR any live PEER container process -> a /proc/<pid> directory,
            // so `ps`/htop can descend into a peer it saw in the /proc listing.
            if (pid == (int)getpid() || pid == container_pid() || pid == 1 || proc_pid_member(pid, &host)) {
                synth_stat_set(s, S_IFDIR | 0555, 8);
                return 1;
            }
        }
    }
    return 0;
}

static int synth_proc_task_stat(const char *gp, struct stat *s) {
    { // /proc/<pid>/task and /proc/<pid>/task/<tid> are directories (htop/`test -e` stat them)
        int pid;
        char dsb[4200];
        const char *lf = proc_any_leaf(proc_deself(gp, dsb, sizeof dsb), &pid); // resolve /proc/self/task/*
        if (lf && pid > 0) {
            int host;
            if (pid == (int)getpid() || pid == container_pid() || pid == 1 || proc_pid_member(pid, &host)) {
                int istaskdir = !strcmp(lf, "task") || !strcmp(lf, "task/"); // guests stat "self/task/"
                int istid = 0;
                if (!istaskdir && !strncmp(lf, "task/", 5) && lf[5]) {
                    istid = 1;
                    for (const char *t = lf + 5; *t; t++)
                        if (*t < '0' || *t > '9') istid = 0; // task/<tid> only (not task/<tid>/<leaf>)
                }
                if (istaskdir || istid) {
                    // For OUR OWN process, reflect the REAL live-thread set: /proc/self/task st_nlink must be
                    // 2 + live-thread-count, and /proc/self/task/<tid> must ENOENT once that thread has
                    // joined/exited. Sandboxes may fstatat-watch /proc/self/task/<tid>
                    // for ENOENT after stopping a helper thread and reads /proc/self/task st_nlink==3 for
                    // single-threadedness; a fixed nlink=3 + a per-tid dir synthesized for ANY number made the
                    // stopped thread never "disappear" -> the process spun until its timeout. A peer
                    // process's threads we cannot enumerate from here, so keep the coarse present/nlink=3 there.
                    int is_self = (pid == (int)getpid() || pid == container_pid());
                    synth_stat_set(s, S_IFDIR | 0555, 0);
                    if (is_self && istaskdir) {
                        s->st_nlink = 2 + thread_live_count();
                        return 1;
                    }
                    if (istid) {
                        int tid = atoi(lf + 5);
                        if (!proc_task_tid_visible(pid, tid))
                            return 0; // not a visible task -> fall through -> ENOENT (the "disappear" signal)
                        if (is_self) {
                            s->st_nlink = 3;
                            return 1;
                        }
                    }
                    s->st_nlink = 3; // peer process (or non-self): coarse present, threads unenumerable here
                    return 1;
                }
            }
        }
    }
    return -1;
}

static int synth_proc_self_fd_stat(const char *gp, struct stat *s) {
    // /proc/<pid>/fd is a directory and /proc/<pid>/fd/N is a symlink -- answer these directly so stat()
    // sees the right type WITHOUT proc_open() materializing a temp dir as a stat side effect.
    const char *leaf = proc_self_leaf(gp);
    if (leaf) {
        if (!strcmp(leaf, "fd") || !strcmp(leaf, "fd/")) {
            synth_stat_set(s, S_IFDIR | 0555, 2);
            return 1;
        }
        if (!strncmp(leaf, "fdinfo/", 7) && leaf[7]) { // /proc/self/fdinfo/<N> -> a regular file (if fd open)
            int isnum = 1;
            for (const char *t = leaf + 7; *t; t++)
                if (*t < '0' || *t > '9') isnum = 0;
            if (isnum) {
                int fn = atoi(leaf + 7);
                hl_linux_fd_snapshot typed;
                int typed_live = g_linux_box != NULL &&
                                 hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fn, &typed) == HL_STATUS_OK;
                if (eventfd_hidden_peer_fd(fn) || (!typed_live && fcntl(fn, F_GETFD) < 0)) return 0;
                synth_stat_set(s, S_IFREG | 0444, 1);
                return 1;
            }
        }
        if (!strncmp(leaf, "fd/", 3) && leaf[3]) {
            int isnum = 1;
            for (const char *t = leaf + 3; *t; t++)
                if (*t < '0' || *t > '9') isnum = 0;
            if (isnum) {
                int pfd = atoi(leaf + 3);
                if (eventfd_hidden_peer_fd(pfd)) return 0;
                // Typed provider/embedding descriptors need not occupy the same native fd number. The guest
                // descriptor table is authoritative; F_GETFD remains the compatibility path for legacy fds.
                hl_linux_fd_snapshot typed;
                int typed_live = g_linux_box != NULL &&
                                 hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)pfd, &typed) == HL_STATUS_OK;
                if (!typed_live && fcntl(pfd, F_GETFD) < 0) return 0;
                synth_stat_set(s, S_IFLNK | 0777, 1);
                s->st_size = 64; // Linux reports a fixed 64 for /proc/<pid>/fd/N links
                return 1;
            }
        }
    }
    return -1;
}

static int synth_proc_peer_fd_stat(const char *gp, struct stat *s) {
    // Peer /proc/<pid>/fd (a directory) and /proc/<pid>/fd/<N> (a symlink to the peer fd's target) -- answer
    // stat directly (a live peer fd from its host descriptor snapshot) so lstat/stat see the right type WITHOUT
    // proc_open() materializing a temp dir as a side effect. proc_self_leaf matched only our own pid above.
    {
        int peer = -1, hp = 0;
        const char *aleaf = proc_any_leaf(gp, &peer);
        if (aleaf && proc_pid_member(peer, &hp)) {
            if (!strcmp(aleaf, "fd")) {
                synth_stat_set(s, S_IFDIR | 0555, 2);
                return 1;
            }
            if (!strncmp(aleaf, "fd/", 3) && aleaf[3]) {
                int isnum = 1;
                for (const char *t = aleaf + 3; *t; t++)
                    if (*t < '0' || *t > '9') isnum = 0;
                if (isnum) {
                    if (!proc_fd_pid_open_one(hp, atoi(aleaf + 3))) return 0; // closed/absent -> ENOENT
                    synth_stat_set(s, S_IFLNK | 0777, 1);
                    s->st_size = 64;
                    return 1;
                }
            }
        }
    }
    return -1;
}

static int synth_proc_file_stat(const char *gp, struct stat *s) {
    int fd = proc_open(gp);
    // -2 (not synth) or mkstemp fail
    if (fd < 0) return 0;
    if (fstat(fd, s) != 0) {
        close(fd);
        return 0;
    }
    close(fd);
    // /proc/self/comm is 0644 on Linux (writing it renames the task; see the write handler in io.c).
    int writable_proc = gp && (strstr(gp, "/oom_score_adj") || strstr(gp, "/oom_adj") || strstr(gp, "/self/comm"));
    s->st_mode = S_IFREG | (writable_proc ? 0644 : 0444);
    // present as a readable regular file
    s->st_nlink = 1;
    return 1;
}

static int synth_stat_raw(const char *gp, struct stat *s) {
    if (!gp) return 0;
    if (whiteout_present(gp)) {
        synth_stat_set(s, S_IFCHR, 1);
        s->st_rdev = 0;
        return 1;
    }
    if (synth_misc_dir_is(gp)) {
        synth_stat_set(s, S_IFDIR | 0555, 2);
        return 1;
    }
    if (synth_namespace_stat(gp, s)) return 1;
    if (!strncmp(gp, "/dev/", 5)) {
        int result = synth_device_stat(gp, s);
        if (result >= 0) return result;
    }
    if (synth_masked_stat(gp, s)) return 1;
    if (!strncmp(gp, "/sys/class/net", 14)) return synth_sysnet_stat(gp, s);
    if (!strncmp(gp, "/sys/devices/system/cpu", 23)) return synth_syscpu_stat(gp, s);
    if (strncmp(gp, "/proc/", 6) && strncmp(gp, "/sys/fs/cgroup/", 15)) return 0;
    if (synth_proc_root_stat(gp, s)) return 1;
    int result = synth_proc_task_stat(gp, s);
    if (result >= 0) return result;
    result = synth_proc_self_fd_stat(gp, s);
    if (result >= 0) return result;
    result = synth_proc_peer_fd_stat(gp, s);
    if (result >= 0) return result;
    return synth_proc_file_stat(gp, s);
}

// (synth_stat wrapper removed: dead — all callers use synth_stat_raw directly)

#include "../route.c"
