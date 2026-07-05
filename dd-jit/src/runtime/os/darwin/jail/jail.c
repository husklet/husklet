// darwinjail -- run REAL native macOS (arm64) binaries in a container, no VM, no DBT.
//
// Injected via DYLD_INSERT_LIBRARIES; interposes libSystem's path/host/net calls and rewrites them into
// the container (rootfs upper + overlay lowers + bind volumes), plus Seatbelt write-confinement and
// rlimits (cgroup analog). The guest binaries execute natively -- so dynamic linking, the dyld shared
// cache, etc. all "just work" -- they just see a jailed filesystem. Same container model as os/linux.
// (Only plain-arm64, non-SIP binaries are injectable; the userland is a nix/custom arm64 toolchain.)
//
// Config via env: DD_ROOTFS, DD_LOWERS="a,b", DD_VOLUMES="HOST:CONT,…", DD_HOSTNAME, DD_PUBLISH="H:C,…",
//                 DD_MEM_MAX, DD_PIDS_MAX, DD_SANDBOX=1, DD_NET_ISOLATE=1, DD_PID1=1.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <fcntl.h>
#include <dirent.h>
#include <spawn.h>
#include <sys/stat.h>
#include <sys/resource.h>
#include <sys/sysctl.h>
#include <sys/mount.h>
#include <sys/mman.h>
#include <netinet/in.h>
#include <signal.h>
#include <unistd.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <mach-o/loader.h>
#include <mach-o/fat.h>
#include <mach/machine.h>
#include <libkern/OSByteOrder.h>
#include <libkern/OSCacheControl.h>
#include "../../container_parse.h" // strict numeric parsing (the config trust boundary; see LAUNCH.md)
extern int sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
// libcompiler_rt symbol that __builtin___clear_cache lowers to (aliased to dodge the clang builtin,
// whose address can't be taken); we interpose it to flip emulated-RWX pages executable.
extern void dj_clear_cache(void *start, void *end) __asm__("___clear_cache");

static const char *g_rootfs, *g_hostname;
static int g_pid1;
// docker --read-only: writes that route to the rootfs (not a volume / not the writable pseudo-mounts) fail
// EROFS. docker --cpus: online-CPU count the container advertises (0 = unlimited) -- capped in sysconf/sysctl.
static int g_rootfs_ro, g_cpu_max;
// The container-side (guest) current working directory, kept as a canonical absolute container path.
// getcwd() returns THIS (never the leaked host rootfs path), and chdir() resolves its argument against
// it -- so "cd .." ascends to the real parent and clamps at the jail root "/". See dj_canon below.
static char g_cwd[1024] = "/";
static char *g_low[8];
static int g_nlow;

static struct {
    char *host, *cont;
} g_vol[16];

static int g_nvol;

static struct {
    int host, cont;
} g_pub[16];

static int g_npub;

static void split(char *v, void (*f)(char *)) {
    if (!v) return;
    char *s = strdup(v), *t, *sp = 0;
    for (t = strtok_r(s, ",", &sp); t; t = strtok_r(0, ",", &sp))
        f(t);
}

// Collections fail loud on cap overflow (never silently drop) and ports are strictly range-checked.
static void add_low(char *d) {
    if (g_nlow >= 8) {
        fprintf(stderr, "dd: too many DD_LOWERS entries (max 8)\n");
        exit(2);
    }
    g_low[g_nlow++] = strdup(d);
}

static void add_vol(char *hc) {
    char *c = strchr(hc, ':');
    if (!c) {
        fprintf(stderr, "dd: invalid DD_VOLUMES '%s': expected HOST:CONTAINER\n", hc);
        exit(2);
    }
    if (g_nvol >= 16) {
        fprintf(stderr, "dd: too many DD_VOLUMES entries (max 16)\n");
        exit(2);
    }
    *c = 0;
    g_vol[g_nvol].host = strdup(hc);
    g_vol[g_nvol].cont = strdup(c + 1);
    g_nvol++;
}

static void add_pub(char *hc) {
    char *c = strchr(hc, ':');
    if (!c) {
        fprintf(stderr, "dd: invalid DD_PUBLISH '%s': expected HOST:CONTAINER\n", hc);
        exit(2);
    }
    if (g_npub >= 16) {
        fprintf(stderr, "dd: too many DD_PUBLISH entries (max 16)\n");
        exit(2);
    }
    *c = 0;
    g_pub[g_npub].host = (int)dd_parse_port("DD_PUBLISH host port", hc);
    g_pub[g_npub].cont = (int)dd_parse_port("DD_PUBLISH container port", c + 1);
    g_npub++;
}

static const char *jail(const char *p, char *out); // defined below; used by init for DD_CWD
static void dj_canon(const char *base, const char *in, char *out, size_t outsz); // path canonicalizer

// docker --ulimit: apply DD_ULIMITS="name=soft:hard,..." via native setrlimit. Docker's ulimit NAMEs map to
// the macOS RLIMIT_* set (a subset of Linux -- macOS has no locks/sigpending/msgqueue/nice/rt* resources, so
// those names are silently ignored, as they'd be a no-op anyway). Runs in init(), before the guest starts.
static void dj_apply_ulimits(const char *spec) {
    if (!spec || !spec[0]) return;

    static const struct {
        const char *n;
        int r;
    } t[] = {{"cpu", RLIMIT_CPU},
             {"fsize", RLIMIT_FSIZE},
             {"data", RLIMIT_DATA},
             {"stack", RLIMIT_STACK},
             {"core", RLIMIT_CORE},
             {"rss", RLIMIT_RSS},
             {"as", RLIMIT_RSS},
             {"memlock", RLIMIT_MEMLOCK},
             {"nproc", RLIMIT_NPROC},
             {"nofile", RLIMIT_NOFILE},
             {0, 0}};

    char *s = strdup(spec);
    if (!s) return;
    char *sv = 0;
    for (char *tok = strtok_r(s, ",", &sv); tok; tok = strtok_r(0, ",", &sv)) {
        char *eq = strchr(tok, '=');
        if (!eq) continue;
        *eq = 0;
        int res = -1;
        for (int i = 0; t[i].n; i++)
            if (!strcmp(tok, t[i].n)) {
                res = t[i].r;
                break;
            }
        if (res < 0) continue;
        char *colon = strchr(eq + 1, ':');
        rlim_t soft, hard;
#define DJ_ULV(x)                                                                                                      \
    (!strcmp((x), "unlimited") || !strcmp((x), "-1") ? RLIM_INFINITY                                                   \
                                                     : (rlim_t)dd_parse_u64("DD_ULIMITS", (x), 0, RLIM_INFINITY))
        if (colon) {
            *colon = 0;
            soft = DJ_ULV(eq + 1);
            hard = DJ_ULV(colon + 1);
        } else {
            soft = hard = DJ_ULV(eq + 1);
        }
#undef DJ_ULV
        struct rlimit r = {soft, hard};
        setrlimit(res, &r);
    }
    free(s);
}

// docker --read-only: 1 if a WRITE to container path `cont` must fail EROFS -- i.e. the rootfs is read-only
// and `cont` routes to it (not a bind volume, not a writable pseudo-mount /proc /dev /sys /tmp /run). Mirrors
// the linux engine's rootfs_ro_denies() so the three engines agree. Pure string math on the container path.
static int dj_ro_denies(const char *cont) {
    if (!g_rootfs_ro || !cont || cont[0] != '/') return 0;
    char c[1024];
    dj_canon("/", cont, c, sizeof c);
    for (int i = 0; i < g_nvol; i++) {
        size_t L = strlen(g_vol[i].cont);
        if (!strncmp(c, g_vol[i].cont, L) && (c[L] == '/' || c[L] == 0)) return 0;
    } // a bind volume governs (writable)
    static const char *const w[] = {"/proc", "/dev", "/sys", "/tmp", "/run", 0};
    for (int i = 0; w[i]; i++) {
        size_t L = strlen(w[i]);
        if (!strncmp(c, w[i], L) && (c[L] == '/' || c[L] == 0)) return 0;
    }
    return 1;
}

// A write mode string for fopen/freopen ("w","a","r+","w+","a+",...) requests write access.
static int dj_fopen_writes(const char *m) {
    return m && (strchr(m, 'w') || strchr(m, 'a') || strchr(m, '+'));
}

// Materialize a bind volume's mount point (and every ancestor) as a real, empty directory in the
// writable rootfs, so listing a mount's PARENT shows the mount as a directory entry -- exactly what
// docker does (it mkdir -ps every mount target). Without this a `-v H:/Users/x` left /Users as an
// EMPTY (or absent) rootfs dir, so `cd .. ; ls` from inside the mount showed NOTHING even though the
// mount exists (bug / the darwin analogue of the linux vol_mkmountpoint fix). The
// created dir is only ever a directory ENTRY: an actual open of the mount path still routes to the
// volume host via jail() (the volume prefix-match wins), so this placeholder is never read from.
// Direct libc mkdir here (intra-dylib calls are NOT interposed); best-effort -- an existing path
// (EEXIST) or a read-only rootfs is fine. Runs before DD_SANDBOX so the writes are unrestricted.
static void dj_mkmountpoint(const char *cont) {
    if (!g_rootfs || !cont || cont[0] != '/') return;
    char c[1024];
    dj_canon("/", cont, c, sizeof c); // canonical, root-clamped container path
    char path[1024];
    size_t n = strlen(g_rootfs);
    if (n + 1 >= sizeof path) return; // rootfs too long to append a path onto
    memcpy(path, g_rootfs, n);
    path[n] = 0; // seed with the rootfs prefix
    for (const char *p = c; *p;) {
        while (*p == '/')
            p++;
        if (!*p) break;
        const char *slash = strchr(p, '/');
        size_t len = slash ? (size_t)(slash - p) : strlen(p);
        if (n + 1 + len >= sizeof path) break;
        path[n++] = '/';
        memcpy(path + n, p, len);
        n += len;
        path[n] = 0;
        mkdir(path, 0755); // best-effort per component (EEXIST ok)
        p += len;
    }
}

__attribute__((constructor)) static void init(void) {
    g_rootfs = getenv("DD_ROOTFS");
    g_hostname = getenv("DD_HOSTNAME");
    g_pid1 = getenv("DD_PID1") != 0;
    split(getenv("DD_LOWERS"), add_low);
    split(getenv("DD_VOLUMES"), add_vol);
    split(getenv("DD_PUBLISH"), add_pub);
    for (int i = 0; i < g_nvol; i++)
        dj_mkmountpoint(g_vol[i].cont); // show mount points in their parent's `ls`
    // initial working directory (docker -w / the cwd ddcli mounts): chdir into the container path.
    const char *cwd = getenv("DD_CWD");
    if (cwd && cwd[0] && g_rootfs) {
        char nc[1024];
        dj_canon("/", cwd, nc, sizeof nc); // canonical container path of the workdir
        char b[1024];
        if (chdir(jail(nc, b)) == 0) strlcpy(g_cwd, nc, sizeof g_cwd); // track it as the guest cwd
    }
    char *mm = getenv("DD_MEM_MAX"), *pm = getenv("DD_PIDS_MAX");
    if (mm) {
        rlim_t v = dd_parse_u64("DD_MEM_MAX", mm, 0, RLIM_INFINITY);
        struct rlimit r = {v, v};
        setrlimit(RLIMIT_AS, &r);
    }
    if (pm) {
        rlim_t v = dd_parse_u64("DD_PIDS_MAX", pm, 0, INT_MAX);
        struct rlimit r = {v, v};
        setrlimit(RLIMIT_NPROC, &r);
    }
    // docker --read-only / --cpus / --ulimit (the darwinjail equivalents of the linux engine's DD_ROOTFS_RO
    // EROFS jail, --cpus reporting cap, and DD_ULIMITS setrlimit set). CPU cap is applied in the sysconf/
    // sysctl interposers below; the rlimits are set natively here so the guest's getrlimit reflects them.
    g_rootfs_ro = getenv("DD_ROOTFS_RO") != 0;
    {
        char *cs = getenv("DD_CPUS");
        if (cs && cs[0]) {
            int v = (int)dd_parse_u64("DD_CPUS", cs, 1, 1024);
            g_cpu_max = v;
        }
    }
    dj_apply_ulimits(getenv("DD_ULIMITS"));
    if (getenv("DD_SANDBOX") && g_rootfs) {
        char prof[4096];
        int n = snprintf(
            prof, sizeof prof,
            "(version 1)(allow default)(deny file-write* (subpath \"/\"))"
            "(allow file-write* (subpath \"%s\"))"
            "(allow file-write* (subpath \"/private/tmp\") (subpath \"/private/var/folders\") (subpath \"/dev\"))",
            g_rootfs);
        for (int i = 0; i < g_nvol && n < 3000; i++)
            n += snprintf(prof + n, sizeof prof - n, "(allow file-write* (subpath \"%s\"))", g_vol[i].host);
        if (getenv("DD_NET_ISOLATE"))
            n += snprintf(
                prof + n, sizeof prof - n,
                "(deny network-outbound (remote ip \"*:*\"))(allow network-outbound (remote ip \"localhost:*\"))");
        // The macOS Seatbelt doesn't nest. On a mac that already confines this process (e.g. Sequoia
        // sandboxes a notarized app's spawned children) sandbox_init fails "Operation not permitted" and
        // libsandbox prints its own "sandbox initialization failed" line to stderr. The container's real
        // jail is the libc path-interposition above, not Seatbelt -- so mute libsandbox's stderr across the
        // call, still apply Seatbelt where it works, and surface only an UNEXPECTED error.
        char *err = 0;
        int sb;
        {
            int sv = dup(2), dn = open("/dev/null", O_WRONLY);
            if (dn >= 0) {
                dup2(dn, 2);
                close(dn);
            }
            sb = sandbox_init(prof, 0, &err);
            if (sv >= 0) {
                dup2(sv, 2);
                close(sv);
            }
        }
        if (sb && (!err || !strstr(err, "Operation not permitted")))
            fprintf(stderr, "[darwinjail] sandbox: %s\n", err ? err : "?");
    }
}

// Canonicalize container path `in` into `out` as an absolute container path: a relative `in` is resolved
// against `base`, then "." is dropped and ".." ascends one component -- but ".." CLAMPS at "/" (the jail
// root), so no sequence of ".." can ever walk above the rootfs. Pure string math (no host fs access): the
// guest namespace is the container's, not the host's. This both makes "cd .." work and seals the escape
// where e.g. open("/a/../../../etc/passwd") would otherwise let the host fs resolve ".." out of the rootfs.
static void dj_canon(const char *base, const char *in, char *out, size_t outsz) {
    char tmp[2048];
    if (in[0] == '/')
        tmp[0] = 0; // absolute: base is irrelevant
    else
        strlcpy(tmp, (base && base[0]) ? base : "/", sizeof tmp);
    size_t l = strlen(tmp);
    snprintf(tmp + l, sizeof tmp - l, "/%s", in); // join base + "/" + in, then tokenize
    out[0] = '/';
    out[1] = 0;
    size_t olen = 1; // build a clean absolute path in `out`
    char *save = 0;
    for (char *t = strtok_r(tmp, "/", &save); t; t = strtok_r(0, "/", &save)) {
        if (!strcmp(t, ".")) continue;
        if (!strcmp(t, "..")) { // ascend, clamped at root
            while (olen > 1 && out[olen - 1] != '/')
                olen--;           // strip the last component
            if (olen > 1) olen--; // and its leading slash
            out[olen] = 0;
            continue;
        }
        if (olen + 1 + strlen(t) >= outsz) break; // truncate rather than overflow
        if (olen > 1) out[olen++] = '/';
        size_t tl = strlen(t);
        memcpy(out + olen, t, tl);
        olen += tl;
        out[olen] = 0;
    }
}

// container path -> host path: bind volumes, then overlay (upper wins, else a lower, else upper for creates).
static const char *jail(const char *p, char *out) {
    if (!p || p[0] != '/' || !g_rootfs) return p;
    char c[1024];
    dj_canon("/", p, c, sizeof c); // resolve "." / ".." within the jail first
    for (int i = 0; i < g_nvol; i++) {
        size_t L = strlen(g_vol[i].cont);
        if (!strncmp(c, g_vol[i].cont, L) && (c[L] == '/' || c[L] == 0)) {
            snprintf(out, 1024, "%s%s", g_vol[i].host, c + L);
            return out;
        }
    }
    snprintf(out, 1024, "%s%s", g_rootfs, c);
    if (access(out, F_OK) == 0) return out;
    for (int i = 0; i < g_nlow; i++) {
        char t[1024];
        snprintf(t, 1024, "%s%s", g_low[i], c);
        if (access(t, F_OK) == 0) {
            snprintf(out, 1024, "%s", t);
            return out;
        }
    }
    return out;
}

#define JAIL(p)                                                                                                        \
    ({                                                                                                                 \
        static __thread char _b[1024];                                                                                 \
        jail((p), _b);                                                                                                 \
    })

// for two-path syscalls: jail both into distinct thread-local buffers.
static const char *jail2(const char *p, char *out) {
    return jail(p, out);
}

#define JAIL_A(p)                                                                                                      \
    ({                                                                                                                 \
        static __thread char _a[1024];                                                                                 \
        jail2((p), _a);                                                                                                \
    })

#define DJ_WRFLAGS (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC | O_APPEND)

int jail_open(const char *path, int flags, ...) {
    mode_t m = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        m = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    if ((flags & DJ_WRFLAGS) && dj_ro_denies(path)) {
        errno = EROFS;
        return -1;
    } // docker --read-only
    return open(JAIL(path), flags, m);
}

int jail_openat(int fd, const char *path, int flags, ...) {
    mode_t m = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        m = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    if ((flags & DJ_WRFLAGS) && path && path[0] == '/' && dj_ro_denies(path)) {
        errno = EROFS;
        return -1;
    }
    return openat(fd, path && path[0] == '/' ? JAIL(path) : path, flags, m);
}

int jail_stat(const char *p, struct stat *s) {
    return stat(JAIL(p), s);
}

int jail_lstat(const char *p, struct stat *s) {
    return lstat(JAIL(p), s);
}

int jail_fstatat(int fd, const char *p, struct stat *s, int f) {
    return fstatat(fd, p && p[0] == '/' ? JAIL(p) : p, s, f);
}

int jail_access(const char *p, int m) {
    return access(JAIL(p), m);
}

int jail_faccessat(int fd, const char *p, int m, int f) {
    return faccessat(fd, p && p[0] == '/' ? JAIL(p) : p, m, f);
}

ssize_t jail_readlink(const char *p, char *b, size_t n) {
    return readlink(JAIL(p), b, n);
}

ssize_t jail_readlinkat(int fd, const char *p, char *b, size_t n) {
    return readlinkat(fd, p && p[0] == '/' ? JAIL(p) : p, b, n);
}

int jail_unlink(const char *p) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return unlink(JAIL(p));
}

int jail_unlinkat(int fd, const char *p, int f) {
    if (p && p[0] == '/' && dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return unlinkat(fd, p && p[0] == '/' ? JAIL(p) : p, f);
}

int jail_mkdir(const char *p, mode_t m) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return mkdir(JAIL(p), m);
}

int jail_mkdirat(int fd, const char *p, mode_t m) {
    if (p && p[0] == '/' && dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return mkdirat(fd, p && p[0] == '/' ? JAIL(p) : p, m);
}

int jail_rmdir(const char *p) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return rmdir(JAIL(p));
}

// chdir confined to the container: resolve the (relative-or-absolute) target against the guest cwd, with
// ".." clamped at the jail root, then chdir into its host mapping. On success record the new guest cwd so
// getcwd() and subsequent relative chdirs stay consistent. "cd .." at "/" stays at "/" (never escapes).
int jail_chdir(const char *p) {
    if (!p || !g_rootfs) return chdir(p);
    char nc[1024];
    dj_canon(g_cwd, p, nc, sizeof nc);
    char hb[1024];
    int r = chdir(jail(nc, hb));
    if (r == 0) strlcpy(g_cwd, nc, sizeof g_cwd);
    return r;
}

// Report the GUEST cwd (the canonical container path), never the host rootfs path getcwd() would leak.
// Mirrors libc's getcwd contract incl. the NULL-buf / size==0 malloc extension and ERANGE.
char *jail_getcwd(char *buf, size_t size) {
    if (!g_rootfs) return getcwd(buf, size);
    size_t n = strlen(g_cwd);
    if (!buf) {
        size_t a = (size > n + 1) ? size : n + 1;
        buf = malloc(a);
        if (!buf) {
            errno = ENOMEM;
            return 0;
        }
    } else if (size < n + 1) {
        errno = ERANGE;
        return 0;
    }
    memcpy(buf, g_cwd, n + 1);
    return buf;
}

int jail_chmod(const char *p, mode_t m) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return chmod(JAIL(p), m);
}

int jail_chown(const char *p, uid_t u, gid_t g) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return chown(JAIL(p), u, g);
}

int jail_lchown(const char *p, uid_t u, gid_t g) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return lchown(JAIL(p), u, g);
}

int jail_statfs(const char *p, struct statfs *s) {
    return statfs(JAIL(p), s);
}

int jail_utimes(const char *p, const struct timeval t[2]) {
    if (dj_ro_denies(p)) {
        errno = EROFS;
        return -1;
    }
    return utimes(JAIL(p), t);
}

int jail_rename(const char *a, const char *b) {
    if (dj_ro_denies(a) || dj_ro_denies(b)) {
        errno = EROFS;
        return -1;
    }
    char x[1024];
    snprintf(x, sizeof x, "%s", JAIL_A(a));
    return rename(x, JAIL(b));
}

int jail_link(const char *a, const char *b) {
    if (dj_ro_denies(b)) {
        errno = EROFS;
        return -1;
    }
    char x[1024];
    snprintf(x, sizeof x, "%s", JAIL_A(a));
    return link(x, JAIL(b));
}

int jail_symlink(const char *t, const char *l) {
    if (dj_ro_denies(l)) {
        errno = EROFS;
        return -1;
    }
    return symlink(t, JAIL(l));
} // target is stored verbatim

DIR *jail_opendir(const char *p) {
    return opendir(JAIL(p));
}

FILE *jail_fopen(const char *p, const char *m) {
    if (dj_fopen_writes(m) && dj_ro_denies(p)) {
        errno = EROFS;
        return 0;
    }
    char b[1024];
    return fopen(jail(p, b), m);
}

FILE *jail_freopen(const char *p, const char *m, FILE *s) {
    if (dj_fopen_writes(m) && dj_ro_denies(p)) {
        errno = EROFS;
        return 0;
    }
    char b[1024];
    return freopen(jail(p, b), m, s);
}

int jail_gethostname(char *name, size_t len) {
    if (g_hostname) {
        strlcpy(name, g_hostname, len);
        return 0;
    }
    return gethostname(name, len);
}

pid_t jail_getpid(void) {
    return g_pid1 ? 1 : getpid();
}

// raise() on Darwin is pthread_kill(pthread_self(), sig) -- a THREAD-directed signal. The kqueue
// EVFILT_SIGNAL knote lives on the process klist and only fires for PROCESS-directed signals (the
// psignal path), so a self-raised signal is invisible to a guest's signal kqueue (libdispatch signal
// sources, the kq-signal case). Redirect to a process-directed kill so the knote posts, matching the
// BSD kqueue model. In a single-threaded guest this is equivalent to raise(); getpid() here is the
// real pid (intra-dylib calls are not interposed, as jail_getpid itself relies on).
int jail_raise(int sig) {
    return kill(getpid(), sig);
}

// docker --cpus: cap the online-CPU count native macOS binaries read, so a --cpus-limited darwin container
// self-sizes to its allotment (the darwinjail analog of the linux engine's container_online_cpus()). nproc /
// most libc consumers use sysconf(_SC_NPROCESSORS_*); Go and many runtimes read hw.ncpu / hw.activecpu /
// hw.logicalcpu via sysctl(byname). We cap every path so GOMAXPROCS / thread-pool sizing honour --cpus.
long jail_sysconf(int name) {
    long v = sysconf(name);
    if (g_cpu_max > 0 && (name == _SC_NPROCESSORS_ONLN || name == _SC_NPROCESSORS_CONF) && v > g_cpu_max)
        return g_cpu_max;
    return v;
}

static void dj_cap_cpu_buf(void *oldp, size_t *oldlenp) {
    if (g_cpu_max <= 0 || !oldp || !oldlenp) return; // sysctl returns int OR int64 for hw.* cpu keys
    if (*oldlenp == sizeof(int)) {
        int *p = oldp;
        if (*p > g_cpu_max) *p = g_cpu_max;
    } else if (*oldlenp == sizeof(int64_t)) {
        int64_t *p = oldp;
        if (*p > (int64_t)g_cpu_max) *p = g_cpu_max;
    }
}

static int dj_is_cpu_name(const char *n) {
    static const char *const k[] = {"hw.ncpu",        "hw.activecpu",       "hw.logicalcpu", "hw.logicalcpu_max",
                                    "hw.physicalcpu", "hw.physicalcpu_max", "hw.availcpu",   0};
    for (int i = 0; k[i]; i++)
        if (!strcmp(n, k[i])) return 1;
    return 0;
}

int jail_sysctlbyname(const char *name, void *oldp, size_t *oldlenp, void *newp, size_t newlen) {
    int r = sysctlbyname(name, oldp, oldlenp, newp, newlen);
    if (r == 0 && name && dj_is_cpu_name(name)) dj_cap_cpu_buf(oldp, oldlenp);
    return r;
}

int jail_sysctl(int *mib, u_int namelen, void *oldp, size_t *oldlenp, void *newp, size_t newlen) {
    int r = sysctl(mib, namelen, oldp, oldlenp, newp, newlen);
    if (r == 0 && mib && namelen >= 2 && mib[0] == CTL_HW && (mib[1] == HW_NCPU || mib[1] == HW_AVAILCPU))
        dj_cap_cpu_buf(oldp, oldlenp);
    return r;
}

int jail_bind(int s, const struct sockaddr *a, socklen_t l) {
    if (a && a->sa_family == AF_INET) {
        struct sockaddr_in in = *(struct sockaddr_in *)a;
        int p = ntohs(in.sin_port);
        for (int i = 0; i < g_npub; i++)
            if (g_pub[i].cont == p) {
                in.sin_port = htons(g_pub[i].host);
                return bind(s, (struct sockaddr *)&in, l);
            }
    }
    return bind(s, a, l);
}

// The jail dylib is plain arm64; dyld refuses to insert it into an arm64e process (system binaries
// under /usr/bin, /bin, … are arm64e) and aborts the child with "incompatible architecture". Such
// binaries can't be path-jailed via DYLD interposition anyway, so for an arm64e target we strip
// DYLD_INSERT_LIBRARIES from its environment and let it run un-jailed instead of crashing.
static int is_arm64e_image(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    uint8_t hdr[4096];
    ssize_t n = read(fd, hdr, sizeof hdr);
    close(fd);
    if (n < (ssize_t)sizeof(uint32_t)) return 0;
    uint32_t magic = *(uint32_t *)hdr;
    if (magic == MH_MAGIC_64 || magic == MH_MAGIC) { // thin Mach-O (host byte order)
        struct mach_header_64 *mh = (void *)hdr;
        return mh->cputype == CPU_TYPE_ARM64 && (mh->cpusubtype & ~CPU_SUBTYPE_MASK) == CPU_SUBTYPE_ARM64E;
    }
    if (magic == FAT_MAGIC || magic == FAT_CIGAM || magic == FAT_MAGIC_64 || magic == FAT_CIGAM_64) {
        int is64 = (magic == FAT_MAGIC_64 || magic == FAT_CIGAM_64); // fat: arch list is big-endian
        uint32_t nfat = OSSwapBigToHostInt32(((struct fat_header *)hdr)->nfat_arch);
        uint8_t *p = hdr + sizeof(struct fat_header), *end = hdr + n;
        for (uint32_t i = 0; i < nfat; i++) {
            cpu_type_t ct;
            cpu_subtype_t cs;
            size_t sz = is64 ? sizeof(struct fat_arch_64) : sizeof(struct fat_arch);
            if (p + sz > end) break;
            if (is64) {
                struct fat_arch_64 *fa = (void *)p;
                ct = OSSwapBigToHostInt32(fa->cputype);
                cs = OSSwapBigToHostInt32(fa->cpusubtype);
            } else {
                struct fat_arch *fa = (void *)p;
                ct = OSSwapBigToHostInt32(fa->cputype);
                cs = OSSwapBigToHostInt32(fa->cpusubtype);
            }
            // dyld prefers an arm64e slice on Apple Silicon, where our arm64-only dylib won't match.
            if (ct == CPU_TYPE_ARM64 && (cs & ~CPU_SUBTYPE_MASK) == CPU_SUBTYPE_ARM64E) return 1;
            p += sz;
        }
    }
    return 0;
}

static char **env_drop_dyld_insert(char *const env[]) { // malloc'd copy minus DYLD_INSERT_LIBRARIES
    size_t n = 0;
    while (env && env[n])
        n++;
    char **out = malloc((n + 1) * sizeof *out);
    if (!out) return (char **)env;
    size_t j = 0;
    for (size_t i = 0; i < n; i++)
        if (strncmp(env[i], "DYLD_INSERT_LIBRARIES=", 22)) out[j++] = env[i];
    out[j] = 0;
    return out;
}

// Return a malloc'd env copy with DD_CWD set to the LIVE guest cwd. The daemon injects DD_CWD once (the
// docker -w) at container start, but a shell then cd's around IN-PROCESS -- so an exec'd child that
// inherited the stale start value would re-init() into the wrong directory and getcwd() would report it
// (the shell's builtin pwd hides this; real tools calling getcwd -- git/make/coreutils -- do not).
// Carrying the current g_cwd forward on every exec keeps getcwd() consistent across exec. Mirrors
// env_drop_dyld_insert's copy-on-exec pattern; the child seeds g_cwd from this DD_CWD in init().
static char **env_set_cwd(char *const env[]) {
    size_t n = 0;
    while (env && env[n])
        n++;
    char **out = malloc((n + 2) * sizeof *out); // room for the (re)added DD_CWD + NULL
    if (!out) return (char **)env;
    static __thread char kv[sizeof g_cwd + 8]; // "DD_CWD=" + path + NUL
    snprintf(kv, sizeof kv, "DD_CWD=%s", g_cwd);
    size_t j = 0;
    for (size_t i = 0; i < n; i++)
        if (strncmp(env[i], "DD_CWD=", 7)) out[j++] = env[i]; // drop any stale DD_CWD
    out[j++] = kv;
    out[j] = 0; // then append the live one
    return out;
}

// exec: jail the program path so a container PATH / a container-local binary resolves into the rootfs.
// The child inherits DYLD_INSERT_LIBRARIES (env), so the jail re-arms in the new process -- except for
// an arm64e target, where the insert is dropped (see is_arm64e_image). A jailed child also carries the
// live DD_CWD (env_set_cwd) so getcwd() stays anchored to the current dir across exec.
int jail_execve(const char *p, char *const a[], char *const e[]) {
    const char *jp = JAIL(p);
    if (is_arm64e_image(jp)) return execve(jp, a, env_drop_dyld_insert(e)); // un-jailed child: DD_CWD unused
    char **ne = env_set_cwd(e);
    int r = execve(jp, a, ne);
    free(ne);
    return r; // free reached only if exec fails
}

int jail_posix_spawn(pid_t *pid, const char *p, const posix_spawn_file_actions_t *fa, const posix_spawnattr_t *at,
                     char *const a[], char *const e[]) {
    const char *jp = JAIL(p);
    if (is_arm64e_image(jp)) {
        char **ne = env_drop_dyld_insert(e);
        int r = posix_spawn(pid, jp, fa, at, a, ne);
        free(ne);
        return r;
    }
    char **ne = env_set_cwd(e);
    int r = posix_spawn(pid, jp, fa, at, a, ne);
    free(ne);
    return r;
}

int jail_posix_spawnp(pid_t *pid, const char *p, const posix_spawn_file_actions_t *fa, const posix_spawnattr_t *at,
                      char *const a[], char *const e[]) {
    if (p && p[0] == '/' && is_arm64e_image(p)) {
        char **ne = env_drop_dyld_insert(e);
        int r = posix_spawnp(pid, p, fa, at, a, ne);
        free(ne);
        return r;
    }
    char **ne = env_set_cwd(e);
    int r = posix_spawnp(pid, p, fa, at, a, ne);
    free(ne);
    return r;
} // p may be a name; PATH via interposed access()

// macOS forbids a page that is simultaneously writable and executable (W^X), so a guest's plain
// mmap(PROT_WRITE|PROT_EXEC) with no MAP_JIT -- the pattern guest JIT runtimes (JVM/V8/LuaJIT) use --
// returns EPERM. Emulate RWX with the MAP_JIT mechanism: add MAP_JIT under the hood and leave the
// region writable for this thread so the guest's code-write succeeds. The guest's mandatory icache
// flush before executing the new code (interposed below) flips the region to executable -- the natural
// W^X transition point, so write-then-execute works without the guest knowing about MAP_JIT.
void *jail_mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off) {
    if ((prot & PROT_EXEC) && (flags & MAP_ANON) && !(flags & MAP_JIT)) {
        void *p = mmap(addr, len, prot, flags | MAP_JIT, fd, off);
        if (p != MAP_FAILED) pthread_jit_write_protect_np(0); // start writable so the guest can fill it
        return p;
    }
    return mmap(addr, len, prot, flags, fd, off);
}

// The W^X transition for the emulated-RWX MAP_JIT regions above: a guest flushes the icache after
// writing code and before executing it, so flip those regions back to executable, then flush as asked.
// Both libcompiler_rt's __clear_cache (what __builtin___clear_cache lowers to) and a direct
// sys_icache_invalidate land here; the toggle is harmless for guests that manage MAP_JIT themselves.
void jail_sys_icache_invalidate(void *start, size_t len) {
    pthread_jit_write_protect_np(1);
    sys_icache_invalidate(start, len);
}

void jail_clear_cache(void *start, void *end) {
    pthread_jit_write_protect_np(1);
    sys_icache_invalidate(start, (char *)end - (char *)start);
}

#define INTERPOSE(repl, orig)                                                                                          \
    __attribute__((used)) static struct {                                                                              \
        const void *r;                                                                                                 \
        const void *o;                                                                                                 \
    } _ip_##orig __attribute__((section("__DATA,__interpose"))) = {(const void *)repl, (const void *)orig};
// hand-aligned macro-invocation table -- chained INTERPOSE() without semicolons is not clang-format-stable
// clang-format off
INTERPOSE(jail_open, open)         INTERPOSE(jail_openat, openat)      INTERPOSE(jail_stat, stat)
INTERPOSE(jail_lstat, lstat)       INTERPOSE(jail_fstatat, fstatat)    INTERPOSE(jail_access, access)
INTERPOSE(jail_faccessat, faccessat) INTERPOSE(jail_readlink, readlink) INTERPOSE(jail_readlinkat, readlinkat)
INTERPOSE(jail_unlink, unlink)     INTERPOSE(jail_unlinkat, unlinkat)  INTERPOSE(jail_mkdir, mkdir)
INTERPOSE(jail_mkdirat, mkdirat)   INTERPOSE(jail_rmdir, rmdir)        INTERPOSE(jail_chdir, chdir)
INTERPOSE(jail_getcwd, getcwd)
INTERPOSE(jail_chmod, chmod)       INTERPOSE(jail_chown, chown)        INTERPOSE(jail_lchown, lchown)
INTERPOSE(jail_statfs, statfs)     INTERPOSE(jail_utimes, utimes)      INTERPOSE(jail_rename, rename)
INTERPOSE(jail_link, link)         INTERPOSE(jail_symlink, symlink)    INTERPOSE(jail_opendir, opendir)
INTERPOSE(jail_fopen, fopen)       INTERPOSE(jail_freopen, freopen)
INTERPOSE(jail_gethostname, gethostname) INTERPOSE(jail_bind, bind)    INTERPOSE(jail_getpid, getpid)
INTERPOSE(jail_raise, raise)
INTERPOSE(jail_sysconf, sysconf)   INTERPOSE(jail_sysctlbyname, sysctlbyname) INTERPOSE(jail_sysctl, sysctl)
INTERPOSE(jail_execve, execve)     INTERPOSE(jail_posix_spawn, posix_spawn) INTERPOSE(jail_posix_spawnp, posix_spawnp)
INTERPOSE(jail_mmap, mmap)         INTERPOSE(jail_sys_icache_invalidate, sys_icache_invalidate)
INTERPOSE(jail_clear_cache, dj_clear_cache)
    // clang-format on
