// dd/runtime/os/linux/container -- container config state (UTS/cgroup/USER-ns/port-map) + parsers.
#include "../../container_parse.h" // strict numeric parsing (the config trust boundary; see LAUNCH.md)

// ---- container namespace + cgroup state (SentryConfig: ddockerd -> jit) ----
// UTS ns: container hostname (uname/sethostname); "" = host default
static char g_hostname[65] = "";
// cgroup memory.max bytes (0 = unlimited); charged in mmap
static uint64_t g_mem_max = 0;
// cgroup pids.max (0 = unlimited); checked in clone
static int g_pids_max = 0;
// current anon charge (bytes)
static _Atomic uint64_t g_mem_charged = 0;
// live task count (init = 1)
static _Atomic int g_pids_cur = 1;
// PID ns: host pid of the container init -> guest sees it as PID 1
static int g_init_hostpid = 0;
static int container_pid(void) {
    int h = getpid();
    return (g_init_hostpid && h == g_init_hostpid) ? 1 : h;
}
// `docker run --network none`: the daemon sets DD_NET_ISOLATE=1. The container is then LOOPBACK-ONLY — no
// eth0 is presented in the interface model (netlink RTM_GETLINK/GETADDR/GETROUTE dumps + SIOCGIFCONF in
// netns.c, and /proc/net/dev·route + /sys/class/net in vfs.c), matching docker's `none` network (only lo).
// Defined here (state.c is the FIRST container TU include) so both vfs.c and netns.c can consume it. Lazily
// cached. Off (eth0 present) for the default bridge / user networks, so it never affects a normal container.
static int g_net_isolate = -1;
static int net_isolate(void) {
    if (g_net_isolate < 0) g_net_isolate = getenv("DD_NET_ISOLATE") != NULL;
    return g_net_isolate;
}
// ---- container network-interface model (#289) --------------------------------------------------
// dd runs no real network stack, so a container had NO interface introspection at all: /sys/class/net
// and /proc/net/* were absent and AF_NETLINK sockets failed EAFNOSUPPORT, breaking getifaddrs /
// go-sockaddr / netlink (consul, minio, `ip`, ifconfig). To fix that coherently we model exactly two
// interfaces -- lo (127.0.0.1/8, ::1) and eth0 (the container's bridge IP, or a stable synthetic
// 172.17.0.2/16). This ONE model is consumed by the RTNETLINK responder (netns.c) and the procfs /
// sysfs synthesis (vfs.c) so every path agrees. eth0's IPv4 is the bridge IP from DD_IP (set by the
// daemon for a bridged container); with no bridge (--network none/host) it falls back to the synthetic.
// Returns the address as a network-order u32 held in host byte order (a | b<<8 | c<<16 | d<<24), the
// same encoding netns.c's br_parse_ip produces and /proc/net/route prints with %08X.
static uint32_t netif_eth0_ip(void) {
    const char *ip = getenv("DD_IP");
    if (ip && ip[0]) {
        unsigned a = 0, b = 0, cc = 0, d = 0;
        if (sscanf(ip, "%u.%u.%u.%u", &a, &b, &cc, &d) == 4 && a < 256 && b < 256 && cc < 256 && d < 256)
            return (uint32_t)(a | (b << 8) | (cc << 16) | (d << 24));
    }
    return (uint32_t)(172 | (17 << 8) | (0 << 16) | (2 << 24)); // 172.17.0.2 (docker default bridge)
}
static int netif_eth0_prefix(void) { return 16; } // docker default bridge is /16 (cf. br_in_subnet)
// eth0 broadcast = (ip | ~mask); mask = the top prefixlen bits (in network-order-as-host-u32 form).
static uint32_t netif_eth0_bcast(void) {
    int pfx = netif_eth0_prefix();
    uint32_t host_mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u); // low `pfx` bits set (= net bytes)
    return netif_eth0_ip() | ~host_mask;
}
// eth0 network base (ip & mask) and gateway (base | .1, i.e. host-octet 1 -> byte3 in this encoding).
static uint32_t netif_eth0_net(void) {
    int pfx = netif_eth0_prefix();
    uint32_t host_mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u);
    return netif_eth0_ip() & host_mask;
}
static uint32_t netif_eth0_gw(void) { return netif_eth0_net() | 0x01000000u; } // .1 = octet4 = high byte
// eth0 MAC = 02:42:<4 ip bytes> (docker's bridge-container MAC convention). out[6].
static void netif_eth0_mac(uint8_t *out) {
    uint32_t ip = netif_eth0_ip();
    out[0] = 0x02;
    out[1] = 0x42;
    out[2] = (uint8_t)(ip & 0xff);
    out[3] = (uint8_t)((ip >> 8) & 0xff);
    out[4] = (uint8_t)((ip >> 16) & 0xff);
    out[5] = (uint8_t)((ip >> 24) & 0xff);
}

static int g_uid = -1,
           // USER ns: container uid/gid (-1 = passthrough host id; container defaults to 0=root)
           g_gid = -1;
static int cuid(void) { return g_uid >= 0 ? g_uid : (int)getuid(); }
static int cgid(void) { return g_gid >= 0 ? g_gid : (int)getgid(); }
// ---- BUG #181: guest-set ownership persistence (chown(2)/fchownat on overlay-upper files) ----
// Rootless: a guest chown can't change the host file's REAL owner, and #156 reports host-owned files
// as the container uid/gid -- so a guest-set owner was silently lost (chown returned 0 but a re-stat
// still showed the #156 default). Persist the guest-set (uid,gid) as host xattrs on the overlay
// backing file; fill_linux_stat prefers them over the cuid/cgid default. A guest id of -1 means
// "don't change" (POSIX chown) -> leave that xattr untouched so the other id / the default survives.
// xattrs live on the real APFS upper file, so they persist across a re-stat AND across processes.
#include <sys/xattr.h>
#define DD_XATTR_UID "user.dd.uid"
#define DD_XATTR_GID "user.dd.gid"
static void chown_xattr_set_path(const char *hostpath, int uid, int gid, int nofollow) {
    int opt = nofollow ? XATTR_NOFOLLOW : 0;
    if (uid >= 0) {
        uint32_t v = (uint32_t)uid;
        setxattr(hostpath, DD_XATTR_UID, &v, sizeof v, 0, opt);
    }
    if (gid >= 0) {
        uint32_t v = (uint32_t)gid;
        setxattr(hostpath, DD_XATTR_GID, &v, sizeof v, 0, opt);
    }
}
static void chown_xattr_set_fd(int fd, int uid, int gid) {
    if (uid >= 0) {
        uint32_t v = (uint32_t)uid;
        fsetxattr(fd, DD_XATTR_UID, &v, sizeof v, 0, 0);
    }
    if (gid >= 0) {
        uint32_t v = (uint32_t)gid;
        fsetxattr(fd, DD_XATTR_GID, &v, sizeof v, 0, 0);
    }
}
// Read back the guest-set ids (fd preferred when fd>=0, else hostpath). Each out is the set id or -1
// (no xattr -> keep the #156 cuid/cgid default). Returns 1 if either id was guest-set.
static int chown_xattr_get(const char *hostpath, int fd, int *uid, int *gid) {
    *uid = -1;
    *gid = -1;
    uint32_t v;
    if (fd >= 0) {
        if (fgetxattr(fd, DD_XATTR_UID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) *uid = (int)v;
        if (fgetxattr(fd, DD_XATTR_GID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) *gid = (int)v;
    } else if (hostpath) {
        if (getxattr(hostpath, DD_XATTR_UID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) *uid = (int)v;
        if (getxattr(hostpath, DD_XATTR_GID, &v, sizeof v, 0, 0) == (ssize_t)sizeof v) *gid = (int)v;
    }
    return (*uid >= 0 || *gid >= 0);
}
// ---- runtime credential overlay (USER ns) -- defined here (BEFORE fs.c AND proc.c in the unity TU) --
// cuid()/cgid() give the container's CONFIGURED identity (default 0=root); a privileged guest may drop
// to an unprivileged id at runtime (apt forks /usr/lib/apt/methods/http, switching to `_apt`; gosu
// switches postgres to uid 70) and then VERIFIES the drop took -- and that it can NOT regain root. We
// track real/effective/saved uid+gid and honour the Linux permission model (a euid==0 task is
// privileged; otherwise a new id must already be one of its three) so both the drop AND the
// regain-must-fail check behave as on Linux. The base is cuid()/cgid() (fork inherits the copy, exec
// re-seeds from the container default). The set*id syscall HANDLERS live in proc.c and mutate these.
static int g_cred_init = 0;
static int g_ruid, g_euid, g_suid; // real / effective / saved-set uid
static int g_rgid, g_egid, g_sgid; // real / effective / saved-set gid
// #257: model the CAP_SETUID/CAP_SETGID capability that actually governs set*id -- not just euid==0. Real
// Linux clears a task's EFFECTIVE caps when euid transitions 0->nonzero, and clears the PERMITTED set too
// once every uid (r/e/s) is nonzero UNLESS PR_SET_KEEPCAPS is armed; a later capset() can then re-raise
// effective from permitted. setpriv relies on exactly this: it sets KEEPCAPS, does setresuid(1000,...),
// capset()s to re-raise, then setresgid(1000,...) -- which our euid==0-only gate wrongly rejected (EPERM).
// apt/gosu drop WITHOUT keepcaps, so permitted->0 and they correctly can never regain root (#242/#255).
static int g_keepcaps = 0;                  // PR_SET_KEEPCAPS armed (caps survive the all-nonzero uid drop)
static int g_cap_setid_perm, g_cap_setid_eff; // permitted / effective CAP_SETUID+CAP_SETGID (move together)
static void cred_init(void) {
    if (g_cred_init) return;
    g_ruid = g_euid = g_suid = cuid();
    g_rgid = g_egid = g_sgid = cgid();
    // A container starts as root (uid 0 by default) with full caps; a non-root container default holds none.
    g_cap_setid_perm = g_cap_setid_eff = (g_euid == 0);
    g_cred_init = 1;
}
// Recompute the CAP_SETID state after a uid change, per the kernel's credential rules (call from every
// set*uid handler AFTER it mutates g_ruid/euid/suid). effective is cleared the moment euid != 0; permitted
// is cleared once all three uids are nonzero unless KEEPCAPS is armed; root (euid 0) holds both.
static void cred_uid_changed(void) {
    if (g_euid == 0) {
        g_cap_setid_perm = g_cap_setid_eff = 1;
        return;
    }
    g_cap_setid_eff = 0; // euid left 0 -> effective caps dropped (a capset can re-raise from permitted)
    if (g_ruid != 0 && g_suid != 0 && !g_keepcaps) g_cap_setid_perm = 0; // all-nonzero, no keepcaps -> gone
}
// #257: execve of an ordinary (non-setuid, no-file-cap) binary recomputes the capability state: a non-root
// task loses all caps, root keeps them, and PR_SET_KEEPCAPS is cleared -- so a program that dropped uid and
// then exec'd cannot silently retain CAP_SETUID/SETGID. The uid/gid values THEMSELVES persist across exec
// (the engine reloads the image in-process), exactly as the kernel carries credentials over an execve.
static void cred_after_exec(void) {
    cred_init();
    g_keepcaps = 0;
    g_cap_setid_perm = g_cap_setid_eff = (g_euid == 0);
}
static int cred_euid(void) {
    cred_init();
    return g_euid;
}
static int cred_egid(void) {
    cred_init();
    return g_egid;
}
// A task may set an id it already holds (real/effective/saved) or ANY id while it holds effective
// CAP_SETUID/CAP_SETGID (which root does, and which KEEPCAPS+capset preserves across a uid drop). -1 means
// "leave unchanged".
static int uid_permitted(int id) {
    return id == -1 || g_cap_setid_eff || id == g_ruid || id == g_euid || id == g_suid;
}
static int gid_permitted(int id) {
    return id == -1 || g_cap_setid_eff || id == g_rgid || id == g_egid || id == g_sgid;
}
// ---- BUG #255: new-file ownership stamp (runtime setuid/setgid drop) ----------------------------
// A guest that drops privilege at runtime (setuid/setresuid/setfsuid -> gosu's postgres) and then
// CREATES a file/dir must have the new inode owned by its CURRENT effective fsuid/fsgid, NOT the
// cuid/cgid container default that fill_linux_stat applies to host-owned files. #181 tracked only
// EXPLICIT chown(2); a plain create left no xattr, so a new file re-appeared as the container id (0),
// which broke initdb ("data directory has wrong ownership"). fsuid/fsgid follow the overlay's
// euid/egid unless setfsuid/setfsgid override them (g_fs*_ovr >= 0); any subsequent set*id resets the
// override (POSIX: fsuid tracks euid). We persist the intended owner as the SAME dd.uid/gid xattr the
// chown path uses, so a later stat reports it. The create sites in fs.c call the helpers below.
static int g_fsuid_ovr = -1, g_fsgid_ovr = -1; // -1 = follow euid/egid
static int newfile_uid(void) { return g_fsuid_ovr >= 0 ? g_fsuid_ovr : cred_euid(); }
static int newfile_gid(void) { return g_fsgid_ovr >= 0 ? g_fsgid_ovr : cred_egid(); }
// True only when a runtime cred drop makes the new-file owner differ from the cuid/cgid default -- the
// create paths gate their pre-existence probe + stamp on this so the common (no-drop) case is free.
static int newfile_stamp_wanted(void) { return newfile_uid() != cuid() || newfile_gid() != cgid(); }
// Stamp a freshly-created inode's owner, but only the id(s) that differ from the default (so a
// root-created file stays xattr-free). fd form for openat(O_CREAT); path form for mkdir/mknod.
static void newfile_stamp_fd(int fd) {
    int u = newfile_uid(), g = newfile_gid();
    chown_xattr_set_fd(fd, u != cuid() ? u : -1, g != cgid() ? g : -1);
}
static void newfile_stamp_path(const char *hostpath, int nofollow) {
    int u = newfile_uid(), g = newfile_gid();
    chown_xattr_set_path(hostpath, u != cuid() ? u : -1, g != cgid() ? g : -1, nofollow);
}
// ---- NET ns Phase 1: port-map (docker run -p H:C). bind(:C) actually binds the host port :H;
// getsockname reports :C back so the guest sees the port it asked for. {cport->hport} table.
static struct {
    uint16_t cport, hport;
} g_portmap[32];
static int g_nportmap = 0;
// fd -> the container port it bound (for getsockname)
static uint16_t g_fd_cport[1024];
static uint16_t pm_host(uint16_t c) {
    for (int i = 0; i < g_nportmap; i++)
        if (g_portmap[i].cport == c) return g_portmap[i].hport;
    return c;
}
// "H:C,H:C,..." (docker -p order: host:container). Ports are strictly validated (1..65535);
// a bad field or more than the cap of entries is an error, not a silent drop.
static void parse_publish(const char *s) {
    while (s && *s) {
        if (g_nportmap >= 32) {
            fprintf(stderr, "dd: too many DD_PUBLISH entries (max 32)\n");
            exit(2);
        }
        const char *colon = strchr(s, ':');
        const char *comma = strchr(s, ',');
        if (!colon || (comma && colon > comma)) {
            fprintf(stderr, "dd: invalid DD_PUBLISH '%s': expected HOST:CONTAINER\n", s);
            exit(2);
        }
        unsigned h = dd_parse_port_field("DD_PUBLISH host port", s, colon);
        unsigned cc = dd_parse_port_field("DD_PUBLISH container port", colon + 1, comma);
        g_portmap[g_nportmap].cport = (uint16_t)cc;
        g_portmap[g_nportmap].hport = (uint16_t)h;
        g_nportmap++;
        if (!comma) break;
        s = comma + 1;
    }
}
// "128M"/"2G"/"512K"/"1048576" -> bytes (docker-style suffixes). Strict: empty/non-numeric/an
// unknown suffix is an error (atoi/strtoull would have silently yielded 0 = unlimited).
static uint64_t parse_size(const char *s) {
    if (!s || !*s) return 0;
    errno = 0;
    char *e = NULL;
    uint64_t v = strtoull(s, &e, 10);
    if (errno != 0 || e == s) {
        fprintf(stderr, "dd: invalid size '%s': not a number\n", s);
        exit(2);
    }
    switch (*e) {
    case '\0': return v;
    case 'k':
    case 'K': return v << 10;
    case 'm':
    case 'M': return v << 20;
    case 'g':
    case 'G': return v << 30;
    default:
        fprintf(stderr, "dd: invalid size '%s': bad suffix\n", s);
        exit(2);
    }
}

// guest PC -> (host = prologue entry for a fresh dispatcher entry,
//             body = post-prologue entry for a CHAINED jump with regs already live)
#define MAP_N 65536
