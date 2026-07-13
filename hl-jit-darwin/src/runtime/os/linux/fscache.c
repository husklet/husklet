// dd/runtime/os/linux -- ELF loader fwd-decls + the FS-metadata cache (stat/access/readlink memoized).

static void load_elf(const char *path, struct loaded *out);
static int elf_interp(const char *path, char *out, size_t n);
static uint64_t build_stack(int argc, char **argv, struct loaded *lm, uint64_t at_base);

// Linux AT_FDCWD(-100) -> macOS AT_FDCWD; real dir-fds pass through unchanged.
#define ATFD(a) (((int)(a) == -100) ? AT_FDCWD : (int)(a))
// ---- S2 path-resolution cache (forward decls; impl after mc_hash, which it reuses) ----
// Memoizes the absolute guest-path -> resolved host-path STRING only (the real syscall still
// runs on the result, so existence/contents are never cached). A global epoch -- bumped by
// service.c on every FS-namespace mutation -- invalidates the whole cache; rc_reset() hard-clears
// it in the fork child so a child never serves the parent's stale mappings. Kill: DD_NOPATHCACHE=1.
static int rc_lookup(const char *g, char *out, size_t n);
static void rc_store(const char *g, const char *host);
static void res_bump(void);
static void rc_reset(void);
// ---- W4D openat resolution cache (forward decls; impl after the S2 rc_* cache it extends) ----
// Extends item-9's rc_* (which memoizes only the read-only atpath() resolver) to the open-heavy half:
// guest-abs-path -> canonical symlink-free host path, so a repeated open collapses the TOCTOU-safe
// per-component jail walk to a single open(host, O_NOFOLLOW). Shares g_res_epoch. Kill: W4_NOOPENCACHE=1.
static int oc_lookup(const char *g, char *out, size_t n);
static void oc_store(const char *g, const char *host);
static void oc_reset(void);

// POSIX shm / named-sem backing path. glibc's shm_open/sem_open create files under /dev/shm; the guest's
// synthesized /dev tmpfs has no real host tmpfs behind it, so /dev/shm/<name> is redirected to a REAL host
// file that MAP_SHARED + fork can share coherently. In CONTAINER mode the backing lives inside the overlay
// upper's own /dev/shm dir (<rootfs_canon>/dev/shm/<name>): the segment is then PER-CONTAINER (no
// cross-container /tmp collision -- two `postgres` containers no longer alias the same DSM segment), it is
// VISIBLE to `ls /dev/shm`/stat/df through the normal overlay machinery (the file physically sits in the
// upper), and it is cleared when the container rootfs is torn down -- matching docker's per-container tmpfs.
// In direct (no-rootfs) mode there is no container to scope to, so fall back to a flat /tmp file. Embedded
// slashes in <name> are flattened so a segment can never escape the shm dir (glibc forbids them anyway).
// Returns buf, or NULL when `guest` is not a "/dev/shm/<name>" path. g_rootfs_canon is defined in vfs.c,
// which is #included ahead of this file in the unity TU.
static const char *shm_backing_path(const char *guest, char *buf, size_t n) {
    if (!guest || guest[0] != '/' || strncmp(guest, "/dev/shm/", 9)) return NULL;
    const char *name = guest + 9;
    int pfx = g_rootfs_canon[0] ? snprintf(buf, n, "%s/dev/shm/", g_rootfs_canon) : snprintf(buf, n, "/tmp/.ddshm-");
    if (pfx < 0 || pfx >= (int)n - 1) return NULL;
    int m = pfx + snprintf(buf + pfx, n - (size_t)pfx, "%s", name);
    if (m > (int)n - 1) m = (int)n - 1;
    for (int i = pfx; i < m; i++)
        if (buf[i] == '/') buf[i] = '_';
    return buf;
}

// Rewrite ABSOLUTE guest paths into the rootfs; relative paths pass through (resolved
// against the dir-fd by the *at syscall, e.g. ls stat-ing entries relative to a dir).
// nofollow=1 leaves the FINAL component unresolved (lstat/AT_SYMLINK_NOFOLLOW unlink), so a
// symlink is stat'd/removed as the link itself rather than its target.
static const char *atpath(int dirfd, const char *raw, char *buf, size_t n, int nofollow) {
    if (!raw) return raw;
    // POSIX shm + named semaphores: glibc backs both with files under /dev/shm (shm_open -> /dev/shm/<name>,
    // sem_open -> /dev/shm/sem.<name>). Route EVERY op (open/link/unlink/stat/rename) at the SAME host
    // backing the open(2) handler uses (case 56, via shm_hostpath -> shm_backing_path), so glibc's
    // multi-step named-sem create (temp file + link to the final name) and sem_unlink all resolve together.
    {
        const char *shp = shm_backing_path(raw, buf, n);
        if (shp) return shp;
    }
    // absolute -> rootfs-relative + confine (final component followed unless nofollow)
    if (raw[0] == '/') {
        // S2: serve the memoized host path (only when a rootfs is configured -- without one the
        // resolvers below return `raw` untouched and leave `buf` garbage, so there's nothing to cache).
        // Follow-path only: the rc_* cache memoizes followed results, so a nofollow lookup must bypass it.
        if (!nofollow && g_rootfs && rc_lookup(raw, buf, n)) return buf;
        if (g_nlower) {
            overlay_resolve(raw, buf, n, nofollow);
            if (!nofollow && g_rootfs) rc_store(raw, buf);
            return buf;
            // overlay: search upper+lowers
        }
        if (g_rootfs) {
            if (nofollow)
                xlate(raw, buf, n);
            else {
                xresolve_exec(raw, buf, n);
                rc_store(raw, buf);
            }
            return buf;
        }
        return nofollow ? xlate(raw, buf, n) : xresolve_exec(raw, buf, n);
    }
    if (!g_rootfs) return raw;
    // relative via a real dir-fd
    if (dirfd >= 0) {
        // untracked dir-fd (dup/inherited/high): FAIL CLOSED
        if (dirfd >= 1024 || !g_fdpath[dirfd][0]) {
            snprintf(buf, n, "%s/.jail-escape-denied", g_rootfs_canon);
            return buf;
        }
        // turn it into a confined absolute path
        const char *gdir = g_fdpath[dirfd];
        if (strncmp(gdir, g_rootfs_canon, g_rootfs_canon_len) == 0)
            // upper -> guest dir
            gdir += g_rootfs_canon_len;
        else
            for (int i = 0; i < g_nlower; i++)
                if (strncmp(gdir, g_lower[i].canon, g_lower[i].clen) == 0) {
                    gdir += g_lower[i].clen;
                    break;
                    // a lower -> guest dir
                }
        char combined[8400];
        snprintf(combined, sizeof combined, "/%s/%s", gdir, raw);
        if (g_nlower) {
            overlay_resolve(combined, buf, n, nofollow);
            return buf;
        }
        // openat then ignores dirfd (path absolute)
        return nofollow ? xlate(combined, buf, n) : xresolve(combined, buf, n);
    }
    {
        char j[8400];
        // AT_FDCWD-relative -> join the guest cwd, then confine
        snprintf(j, sizeof j, "%s/%s", g_cwd, raw);
        if (g_nlower) {
            overlay_resolve(j, buf, n, nofollow);
            return buf;
        }
        return nofollow ? xlate(j, buf, n) : xresolve_exec(j, buf, n);
    }
}

// ---- FS-metadata cache ----
// Container processes (ld.so, shells, build tools) hammer redundant stat() on
// read-only image layers; the runtime owns the syscall stream, so it can answer
// from cache. Precise invalidation: record fd->path on open, evict that path's
// entry on write/truncate/create. Single-threaded only (no cross-thread races).
//
// POSITIVE entries are evicted precisely (the mutating syscall names the exact path it changed) and
// otherwise survive across unrelated mutations -- the hot path (ld.so probing existing libraries) stays
// warm. NEGATIVE (ENOENT) entries are additionally EPOCH-GATED on g_res_epoch: every name-adding syscall
// (create/rename-into/mkdir/symlink/link, see dispatch.c) bumps the epoch BEFORE dispatch, so a negative
// entry stamped before the mutation instantly misses and the next stat/access re-resolves the now-real
// file. This closes the same-process create->stat coherence gap that precise eviction missed -- e.g. pip
// writes a .pyc to a temp file then renames it into place and immediately stat()s the final name; the
// rename target's earlier ENOENT must not outlive the rename. Cross-process create-after-negative (a child /
// pipe coprocess creates a file the parent probed absent) is covered by g_res_epoch being SHARED across the
// container process-tree (see its definition below) PLUS rc_reset() dropping the inherited caches at fork.
// CROSS-PROCESS coherence (/): the epoch lives in a MAP_SHARED page so a file CREATED by ANOTHER
// process in the same container process-tree bumps the SAME counter every process reads. The headline case is
// apt: its http download method is a fork+exec'd, PERSISTENT pipe coprocess the parent apt never reaps -- so
// there is no fork/wait boundary at which to drop the parent's stale caches. Without a shared epoch, the
// parent that cached a NEGATIVE stat/access/resolve for partial/*_InRelease BEFORE the child downloaded it
// keeps serving ENOENT forever (its own private g_res_epoch never moved), so the file looks "vanished" and
// apt's split rename fails ENOENT. With the counter shared, the child's O_CREAT/rename res_bump() invalidates
// the negative entry in the parent too. The page is created in a constructor BEFORE any guest fork, so every
// fork descendant AND in-process execve (dd keeps its own mappings across the guest execve) share one physical
// counter; a fresh `docker exec` is a NEW dd process = its own tree = its own counter (correct per-container
// isolation). Falls back to a private local counter if the mmap fails (degrades to the old per-process
// behaviour, never crashes). Positive entries are still served epoch-independently, unchanged.
static _Atomic uint32_t g_res_epoch_local = 1;                 // fallback when the shared mapping can't be made
static _Atomic uint32_t *g_res_epoch_ptr = &g_res_epoch_local; // -> a MAP_SHARED page once the ctor runs
#define g_res_epoch (*g_res_epoch_ptr) // 0 is reserved as "never matches" (shared by the rc_/oc_ path caches)

__attribute__((constructor)) static void res_epoch_ctor(void) {
    void *m = mmap(NULL, sizeof(_Atomic uint32_t), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (m != MAP_FAILED) {
        atomic_store((_Atomic uint32_t *)m, 1);
        g_res_epoch_ptr = (_Atomic uint32_t *)m;
    }
}

// process-local fork/chroot GENERATION for every cache in this file (mc/rl/ac + rc/ud/oc).
// Each entry is stamped with g_fs_fgen at store time and only hits while its stamp still matches;
// rc_reset() (the fork-child / chroot hard reset) just bumps the counter -- O(1) -- instead of
// memset'ing ~13MB of arrays inside the fork child's critical path. The counter is ordinary process
// memory (COW-private), so a child's bump never disturbs the parent's warm caches. Reads/writes happen
// under the same CLK lock as the entries themselves.
static uint32_t g_fs_fgen;

#define MCACHE_N 8192

static struct mcent {
    uint64_t hash;
    uint32_t epoch; // stamp at store; a negative (rc<0) entry only hits while it still equals g_res_epoch
    uint32_t fgen;  // fork/chroot generation stamp (see g_fs_fgen)
    char path[192];
    int rc;
    struct stat st;
} g_mc[MCACHE_N];

static uint64_t mc_hash(const char *s) {
    uint64_t h = 1469598103934665603ull;
    for (; *s; s++) {
        h ^= (uint8_t)*s;
        h *= 1099511628211ull;
    }
    return h ? h : 1;
}

static int mc_lookup(const char *p, int *rc, struct stat *out) {
    if (!p || strlen(p) >= 192) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct mcent *e = &g_mc[h & (MCACHE_N - 1)];
    // A negative (ENOENT) entry is only valid within the epoch it was recorded: a later create/rename can
    // turn it positive, and every such mutation bumps g_res_epoch -> miss and re-stat the now-real file.
    if (e->hash == h && e->fgen == g_fs_fgen && (e->rc >= 0 || e->epoch == g_res_epoch) && !strcmp(e->path, p)) {
        *rc = e->rc;
        *out = e->st;
        hit = 1;
    }
    CUL;
    return hit;
}

static void mc_store(const char *p, int rc, const struct stat *s) {
    if (!p || strlen(p) >= 192) return;
    // don't cache mutable volume paths
    if (g_nvols && strncmp(p, g_rootfs_canon, g_rootfs_canon_len)) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct mcent *e = &g_mc[h & (MCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    strcpy(e->path, p);
    e->rc = rc;
    e->st = *s;
    CUL;
}

static void mc_evict(const char *p) {
    if (!p || !p[0]) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct mcent *e = &g_mc[h & (MCACHE_N - 1)];
    if (e->hash == h && !strcmp(e->path, p)) e->hash = 0;
    CUL;
}

// Hardlink coherence: creating or removing a link changes st_nlink on EVERY path that aliases the same
// inode, but the mc cache is keyed by PATH -- so a sibling link's cached stat keeps the pre-mutation
// nlink. link()/unlink() of a multiply-linked inode call this to drop every POSITIVE cached stat for that
// (dev,ino), so the next stat of any alias re-reads the true link count. Rare op (only when nlink>=2), so
// the full-table scan is acceptable; negative entries carry no inode and are left alone.
static void mc_evict_ino(dev_t dev, ino_t ino) {
    if (!ino) return;
    CLK;
    for (int i = 0; i < MCACHE_N; i++) {
        struct mcent *e = &g_mc[i];
        if (e->hash && e->rc == 0 && e->st.st_ino == ino && e->st.st_dev == dev) e->hash = 0;
    }
    CUL;
}

// readlink cache (ld.so resolves symlinks on every library search path)
static struct rlent {
    uint64_t hash;
    uint32_t epoch; // negative (rc<0) entries are epoch-gated; see the mcent rationale above
    uint32_t fgen;  // fork/chroot generation stamp
    char path[176];
    int rc;
    char link[200];
    int linklen;
} g_rl[2048];

static int rl_lookup(const char *p, int *rc, char *out, int bs, int *len) {
    if (!p || strlen(p) >= 176) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    // a negative readlink (ENOENT/EINVAL) only hits within its epoch -- a later symlink/create can make it
    // resolve, and that mutation bumps g_res_epoch.
    if (e->hash == h && e->fgen == g_fs_fgen && (e->rc >= 0 || e->epoch == g_res_epoch) && !strcmp(e->path, p)) {
        *rc = e->rc;
        int n = e->linklen < bs ? e->linklen : bs;
        if (e->rc >= 0) memcpy(out, e->link, n);
        *len = n;
        hit = 1;
    }
    CUL;
    return hit;
}

static void rl_store(const char *p, int rc, const char *link, int len) {
    if (!p || strlen(p) >= 176 || len > 200) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    strcpy(e->path, p);
    e->rc = rc;
    e->linklen = len;
    if (rc >= 0) memcpy(e->link, link, len);
    CUL;
}

static void rl_evict(const char *p) {
    if (!p || !p[0]) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    if (e->hash == h && !strcmp(e->path, p)) e->hash = 0;
    CUL;
}

// access(F_OK) existence cache (ld.so probes every library candidate)
static struct acent {
    uint64_t hash;
    uint32_t epoch; // negative (rc<0) entries are epoch-gated; see the mcent rationale above
    uint32_t fgen;  // fork/chroot generation stamp
    char path[176];
    int rc;
} g_ac[2048];

static int ac_lookup(const char *p, int *rc) {
    if (!p || strlen(p) >= 176) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct acent *e = &g_ac[h & 2047];
    // a negative existence probe only hits within its epoch -- a later create can make the path exist, and
    // that mutation bumps g_res_epoch -> miss and re-probe.
    if (e->hash == h && e->fgen == g_fs_fgen && (e->rc >= 0 || e->epoch == g_res_epoch) && !strcmp(e->path, p)) {
        *rc = e->rc;
        hit = 1;
    }
    CUL;
    return hit;
}

static void ac_store(const char *p, int rc) {
    if (!p || strlen(p) >= 176) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct acent *e = &g_ac[h & 2047];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    strcpy(e->path, p);
    e->rc = rc;
    CUL;
}

static void ac_evict(const char *p) {
    if (!p || !p[0]) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct acent *e = &g_ac[h & 2047];
    if (e->hash == h && !strcmp(e->path, p)) e->hash = 0;
    CUL;
}

// ---- S2 path-resolution cache (impl) ----
// The metadata caches above (mc_/rl_/ac_) memoize the *result* of a syscall, keyed on the resolved
// HOST path -- so the caller must FIRST pay the full atpath()/realpath()+lstat() walk to obtain that
// key. This cache fills that gap: it memoizes the walk itself (guest abs path -> host path string),
// which is a pure function of the FS namespace (dirs + symlinks). The real syscall ALWAYS still runs
// on the returned string, so a stale entry can never fabricate existence or contents -- the only
// failure mode is returning the wrong host *path*, which the epoch guard below prevents:
//   * Whole-cache invalidation by epoch: service.c bumps g_res_epoch on EVERY namespace mutation
//     (mknod/mkdir/unlink/rmdir/symlink/link/rename/(u)mount, open/openat2 w/ O_CREAT). A lookup
//     compares epochs, so every entry stamped before the mutation instantly misses. This covers all
//     stale-entry hazards conservatively (over-invalidates, never under): create-after-negative,
//     delete-after-positive, rename, mkdir/rmdir -- when in doubt we MISS and re-resolve.
//   * Hard reset on fork (rc_reset, called in the clone/clone3 child) so a child never serves a
//     mapping the parent populated before the FS diverged.
#define RCACHE_N 8192

static struct rcent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen; // fork/chroot generation stamp
    uint16_t hlen; // strlen(host), cached so a hit copies via memcpy instead of re-scanning/snprintf
    char guest[200];
    char host[256];
} g_rc[RCACHE_N];

// g_res_epoch is defined up with the FS-metadata cache (the metadata caches' negative-entry gating
// references it too); it is shared by these path-string caches and the metadata caches alike.
// kill switch (read once): DD_NOPATHCACHE=1 -> exact baseline resolution, no memoization. This gates
// the rc_/ud_/udv_/dc_ path caches (and, via oc_enabled below, the oc_ open-resolution cache too).
static int res_enabled(void) {
    static int on = -1;
    if (on < 0) {
        // Gated by EITHER the DD_NOPATHCACHE env var OR a host sentinel file /tmp/dd_nopathcache. The
        // file gate is the reliable one: the engine is spawned with a curated config (via --configfd),
        // NOT the full parent environ, so an ambient DD_NOPATHCACHE never reaches getenv() here (the
        // "mac bridge drops env" seam) -- without the sentinel the kill switch was silently inert.
        // access() runs in engine context against the HOST fs (not the guest jail), so
        // `touch /tmp/dd_nopathcache` mac-side arms it. Cached once; inert (caches ON) when neither is
        // present -> gate-neutral.
        int saved = errno; // access() sets errno=ENOENT when the sentinel is absent -- MUST NOT leak
        const char *e = getenv("DD_NOPATHCACHE");
        int envset = (e && e[0] == '1');
        int fileset = access("/tmp/dd_nopathcache", F_OK) == 0;
        errno = saved;
        on = (envset || fileset) ? 0 : 1;
    }
    return on;
}

// Bump the epoch -> the whole cache misses. Skip 0 (the reserved "never matches" stamp).
// Locked under threads (same model as mc_*) so a bump can't race a concurrent lookup's epoch read.
static void res_bump(void) {
    CLK;
    g_res_epoch++;
    if (!g_res_epoch) g_res_epoch = 1;
    CUL;
}

// ---- overlay upper-parent negative memo ("this guest dir does not exist in the UPPER") ----
// overlay_lookup() (vfs/overlay.c) pays, PER PATH, an upper realpath climb + whiteout lstat + opaque
// probe -- all provably-ENOENT whenever the path's parent dir chain is absent from the (near-empty,
// fresh-container) upper. This memoizes exactly that proof, keyed by the parent GUEST dir, so a
// metadata storm over the read-only image (tar/find/du/ld.so: thousands of lstats, one per file) pays
// the upper probe once per DIRECTORY instead of once per file. Correctness model is identical to rc_*:
//   * epoch-gated: every namespace mutation bumps g_res_epoch (dispatch.c res_bump on create/unlink/
//     rename/mkdir/..., PLUS the copy-up bumps in overlay.c for the non-syscall upper mutations), so an
//     entry can never outlive an upper dir appearing. The epoch is container-SHARED (MAP_SHARED page),
//     so another engine process (docker exec) creating upper dirs invalidates this process's memo too.
//   * fork/chroot hard reset via rc_reset() below.
//   * volume paths are never stored (host-mutable backing; enforced at the overlay_lookup call site).
//   * kill switch: DD_NOPATHCACHE=1 disables it together with the other path caches (res_enabled).
#define UDCACHE_N 4096

static struct udent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen; // fork/chroot generation stamp
    char dir[200];
} g_ud[UDCACHE_N];

static int updirneg_lookup(const char *d) {
    if (!res_enabled() || !d || d[0] != '/' || strlen(d) >= sizeof(((struct udent *)0)->dir)) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(d);
    struct udent *e = &g_ud[h & (UDCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->dir, d)) hit = 1;
    CUL;
    return hit;
}

static void updirneg_store(const char *d) {
    if (!res_enabled() || !d || d[0] != '/' || strlen(d) >= sizeof(((struct udent *)0)->dir)) return;
    CLK;
    uint64_t h = mc_hash(d);
    struct udent *e = &g_ud[h & (UDCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    strcpy(e->dir, d);
    CUL;
}

// ---- overlay merged-view directory VERDICT memo  ----------------------------------------
// overlay_dir_verdict (vfs/overlay.c) answers, per GUEST directory, whether that dir is visible in the
// unioned view and whether lower layers may contribute to its contents:
//   0 = present, lowers contribute (an ordinary union dir);
//   1 = HIDDEN -- the dir (or an ancestor) was removed: a `.wh.` whiteout hides it with no higher layer
//       re-providing it, or an ancestor is simply absent. EVERY entry under it is ENOENT;
//   2 = OPAQUE-CUT -- present, but an opaque marker on it or an ancestor hides all lower content, so its
//       entries come ONLY from the writable upper.
// overlay_lookup consults this BEFORE descending to the lowers. Without it, the per-layer resolve
// (layer_follow walks a whole path inside ONE layer) surfaces a lower-only child through a parent that
// `rm -r`/rmdir whited out -> `stat`/`access` of the child returns a STALE POSITIVE after its directory
// was removed, and a merged readdir/rmdir under an opaque-recreated parent leaks stale lower
// entries. Correctness model is identical to the updirneg memo above: epoch-gated on the
// container-shared g_res_epoch (every unlink/rmdir/rename/mkdir/whiteout/opaque bumps it, so a removal
// instantly invalidates the memo), fork/chroot hard reset via rc_reset(), and DD_NOPATHCACHE=1 disables
// it (overlay_dir_verdict then recomputes every call -- correct, just uncached).
#define UDVCACHE_N 4096

static struct udvent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;   // fork/chroot generation stamp
    uint8_t verdict; // 0 = present (lowers contribute), 1 = hidden, 2 = opaque-cut (upper-only)
    char dir[200];
} g_udv[UDVCACHE_N];

static int updirverdict_lookup(const char *d, int *verdict) {
    if (!res_enabled() || !d || d[0] != '/' || strlen(d) >= sizeof(((struct udvent *)0)->dir)) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(d);
    struct udvent *e = &g_udv[h & (UDVCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->dir, d)) {
        *verdict = e->verdict;
        hit = 1;
    }
    CUL;
    return hit;
}

static void updirverdict_store(const char *d, int verdict) {
    if (!res_enabled() || !d || d[0] != '/' || strlen(d) >= sizeof(((struct udvent *)0)->dir)) return;
    CLK;
    uint64_t h = mc_hash(d);
    struct udvent *e = &g_udv[h & (UDVCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    e->verdict = (uint8_t)verdict;
    strcpy(e->dir, d);
    CUL;
}

// ---- positive dentry/climb cache (dc_*;) -----------------------------------------------------
// The rc_/oc_ caches memoize FULL guest-path -> host-path resolutions, so a metadata storm that touches
// each file ONCE (tar/find/du: readdir + one lstat + one open per file) never hits them -- every file
// still pays confine_in_m's realpath() climb (per LAYER, per probe: entry + whiteout + opaque) and, on
// open, resolve_at's per-component openat walk. This cache memoizes the climb itself, PER DIRECTORY:
//   key   = the exact pre-realpath host string confine_in_m hands to realpath() first (jail canon +
//           normalized rel, final component already peeled in nofollow mode). The jail canon prefix
//           makes the key unique per layer (upper vs each lower) with no extra discriminator.
//   value = (canon, nmiss): realpath() of the deepest EXISTING prefix -- already verified inside the
//           jail by the caller before dc_store -- plus how many trailing components of the key did NOT
//           exist (0 = the whole key resolved). A hit replays the recorded climb outcome exactly:
//           out = canon + (the nmiss popped components, a plain substring of the key) + rem.
// Files in one directory share the key, so a storm pays ONE realpath per (layer, dir) instead of one
// per file; the caller's real lstat/open ALWAYS still runs on the result, so existence and contents
// are never fabricated -- the only possible staleness is a wrong PATH STRING, prevented exactly as in
// rc_/oc_:
//   * EVERY entry (including the nmiss>0 "parent chain missing here" ones) is epoch-gated on the
//     container-shared g_res_epoch: any create/unlink/rename/mkdir/symlink/link/mount -- and every
//     overlay copy-up relocation (res_bump in overlay.c) -- invalidates the WHOLE cache. A symlink
//     flip inside a cached prefix is a symlinkat/unlinkat/renameat -> bumped. Over-invalidate, never
//     under.
//   * fork/chroot drop via rc_reset below: each entry also carries the g_fs_fgen stamp and only
//     hits while it matches, so the O(1) generation bump in the fork child drops every inherited (COW)
//     entry -- same discipline as rc_/oc_/mc_ (COW/re-root hazard).
//   * volume jails are NEVER cached (host-mutable backing): enforced by dc_jail_cacheable, which
//     recognizes only the rootfs upper + the read-only image lowers by pointer identity.
//   * kill switch: DD_NOPATHCACHE=1 (res_enabled), same switch as the other path caches.
// resolve_at (the TOCTOU-safe open walk) additionally CONSUMES entries with canon == key && nmiss == 0:
// such an entry proves every component of the key existed as a real, non-symlink directory (realpath
// returning its input verbatim admits no symlink hop), which is precisely the condition under which
// the lexical fast path and the per-component walk agree -- see the guard comments at that site.
// No dir-fds are cached (path strings only), so there is no fd-exhaustion/LRU concern.
#define DCACHE_N 8192

static struct dcent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;  // fork/chroot generation stamp (rc_reset bumps g_fs_fgen, O(1), instead of memset)
    uint16_t nmiss; // trailing components of key that did NOT exist when resolved (0 = fully exists)
    uint16_t clen;  // strlen(canon)
    char key[DC_KEYMAX];
    char canon[DC_KEYMAX];
} g_dc[DCACHE_N];

static int dc_jail_cacheable(const char *jcanon) {
    if (!res_enabled()) return 0;
    if (jcanon == g_rootfs_canon) return 1; // the writable upper (mutations bump g_res_epoch)
    for (int i = 0; i < g_nlower; i++)
        if (jcanon == g_lower[i].canon) return 1; // read-only image lowers
    return 0; // anything else (bind-mount volumes, unknown roots): host-mutable -> never cache
}

static int dc_lookup(const char *key, char *canon, size_t n, int *nmiss) {
    if (!res_enabled() || !key || !key[0] || strlen(key) >= DC_KEYMAX) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(key);
    struct dcent *e = &g_dc[h & (DCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->key, key)) {
        if (e->clen < n) {
            memcpy(canon, e->canon, (size_t)e->clen + 1);
            *nmiss = e->nmiss;
            hit = 1;
        }
    }
    CUL;
    return hit;
}

static void dc_store(const char *key, const char *canon, int nmiss) {
    if (!res_enabled() || !key || !key[0] || !canon || nmiss < 0 || nmiss > 0xffff) return;
    size_t cl = strlen(canon);
    if (strlen(key) >= DC_KEYMAX || cl >= DC_KEYMAX) return; // over-length: bypass, re-resolved safely
    CLK;
    uint64_t h = mc_hash(key);
    struct dcent *e = &g_dc[h & (DCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch; // stamp the CURRENT epoch; any later namespace mutation invalidates it
    e->fgen = g_fs_fgen;    // fork/chroot generation; a fork child's rc_reset bump drops this entry
    e->nmiss = (uint16_t)nmiss;
    e->clen = (uint16_t)cl;
    strcpy(e->key, key);
    memcpy(e->canon, canon, cl + 1);
    CUL;
}

// fork child: drop every inherited (COW) entry so it cannot outlive a parent-side mutation.
// This covers BOTH the path-string caches (rc_/oc_) and the metadata caches (mc_/rl_/ac_). The
// metadata caches memoize stat/readlink/access RESULTS -- including NEGATIVE (ENOENT) ones -- and a
// negative entry the parent recorded before some OTHER process created the file would otherwise be
// inherited COW and make the child read the now-existing file as missing (build systems hit this hard:
// gmake stats an output as absent, a child compiler creates it, a sibling link/stat then sees the stale
// ENOENT -> "no such file" / "No rule to make target"). The epoch only invalidates this process's own
// mutations, so it can't cover a cross-process create; dropping the inherited entries at the fork point
// makes the child re-resolve against the real FS.
static void rc_reset(void) {
    CLK;
    // O(1) GENERATION BUMP instead of memset'ing all arrays (~13MB: rc+oc ~3.8MB each, mc
    // ~2.9MB, rl/ac/ud/dc) in the fork child's critical path -- those memsets were a fixed ~ms-scale
    // tax on EVERY guest fork. Every entry is stamped with g_fs_fgen at store time and only hits while
    // the stamp still matches, so bumping the (process-local, COW-private) counter atomically drops every
    // inherited entry -- exactly the semantics the memsets had -- while the parent's counter (and its
    // warm caches) are untouched. On the in-practice-unreachable 32-bit wrap (2^32 forks/chroots in one
    // process lineage), fall back to the hard clear so an entry stamped 2^32 generations ago (if it
    // somehow survived unoverwritten) can never alias the new generation.
    if (++g_fs_fgen == 0) {
        memset(g_rc, 0, sizeof g_rc);
        memset(g_mc, 0, sizeof g_mc);
        memset(g_rl, 0, sizeof g_rl);
        memset(g_ac, 0, sizeof g_ac);
        memset(g_ud, 0, sizeof g_ud);   // overlay upper-parent negative memo: same COW/re-root hazard
        memset(g_udv, 0, sizeof g_udv); // overlay dir-verdict memo: same COW/re-root hazard
        memset(g_dc, 0, sizeof g_dc);   // positive dentry/climb cache: same COW/re-root hazard
        oc_reset();                     // W4D: the open-resolution cache too (same COW hazard, under the same lock)
    }
    // NB: do NOT reset g_res_epoch here -- it is a container-wide SHARED counter (see its definition); a
    // fork child / chroot must not rewind the whole tree's epoch. The generation bump above drops every
    // inherited entry of THIS process, which is all this reset needs to do.
    CUL;
}

static int rc_lookup(const char *g, char *out, size_t n) {
    if (!res_enabled() || !g || g[0] != '/' || strlen(g) >= sizeof(((struct rcent *)0)->guest)) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(g);
    struct rcent *e = &g_rc[h & (RCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->guest, g)) {
        if (e->hlen < n) {
            memcpy(out, e->host, (size_t)e->hlen + 1); // hot path: bounded copy incl. NUL, no format parse
        } else {
            memcpy(out, e->host, n - 1);
            out[n - 1] = 0;
        }
        hit = 1;
    }
    CUL;
    return hit;
}

static void rc_store(const char *g, const char *host) {
    if (!res_enabled() || !g || g[0] != '/' || !host) return;
    // over-length paths simply bypass the cache (fixed-size slot) -> re-resolved every time, safely.
    size_t hl = strlen(host);
    if (strlen(g) >= sizeof(((struct rcent *)0)->guest) || hl >= sizeof(((struct rcent *)0)->host)) return;
    CLK;
    uint64_t h = mc_hash(g);
    struct rcent *e = &g_rc[h & (RCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch; // stamp with the CURRENT epoch; a later mutation invalidates it
    e->fgen = g_fs_fgen;
    e->hlen = (uint16_t)hl;
    strcpy(e->guest, g);
    memcpy(e->host, host, hl + 1);
    CUL;
}

// ---- W4D openat resolution cache (impl) ----
// item-9's rc_* cache above memoizes the atpath() resolver used by the read-only metadata syscalls
// (stat/access/readlink/exec). openat takes a DIFFERENT, TOCTOU-safe resolver -- jail_at()/resolve_at()
// walks the path one component at a time on pinned dir-fds (~6 host syscalls) so a guest can't swap a
// component for an out-of-jail symlink between check and use -- and item-9 left THAT uncached. This fills
// the gap for the open-heavy half: it memoizes the WALK as a guest-abs-path -> canonical, symlink-free
// host path obtained via F_GETPATH on a SUCCESSFUL real open. On a HIT the ~6-syscall walk collapses to a
// single open(host, O_NOFOLLOW); the REAL open ALWAYS still runs, so a stale entry can never fabricate
// existence/contents -- the only failure mode is a wrong host *path*, which the SHARED g_res_epoch guard
// prevents: every FS-namespace mutation (service.c res_bump on mknod/mkdir/unlink/rmdir/symlink/link/
// rename/(u)mount + openat O_CREAT) bumps the epoch, so the whole cache misses -- conservative
// over-invalidation, identical threat model to item 9 (the host outside the jail is not in scope; when in
// doubt we MISS and re-walk). oc_store additionally refuses to cache any host path that escaped the rootfs
// (defensive strncmp). The caller EXCLUDES O_CREAT/O_EXCL/O_TRUNC (mutating/creating) and O_DIRECTORY
// (deep-host-path reopen regressed -21%; see optimization-research/w4d-openat.md). Hard reset on fork via
// oc_reset() (from rc_reset). Kill switch (read once): W4_NOOPENCACHE=1 -> the original uncached walk.
#define OCACHE_N 8192

static struct ocent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen; // fork/chroot generation stamp
    uint16_t hlen; // strlen(host), cached so a hit copies via memcpy instead of re-scanning/snprintf
    char guest[200];
    char host[256];
} g_oc[OCACHE_N];

static int oc_enabled(void) {
    static int on = -1;
    if (on < 0) {
        // Same curated-env seam as res_enabled (the engine sees no ambient env under --configfd): honor
        // W4_NOOPENCACHE via env OR the host sentinel /tmp/dd_noopencache, and fold in the master
        // DD_NOPATHCACHE gate (res_enabled) so the master kill switch disables the open-resolution cache
        // alongside the other path caches, matching its documented intent. Gate-neutral: when nothing is
        // armed res_enabled()==1 and off==0 -> on=1 (cache ON), byte-identical to the prior default.
        int saved = errno; // access() sets errno=ENOENT when the sentinel is absent -- MUST NOT leak
        const char *e = getenv("W4_NOOPENCACHE");
        int envset = (e && e[0] == '1');
        int fileset = access("/tmp/dd_noopencache", F_OK) == 0;
        errno = saved;
        on = (envset || fileset || !res_enabled()) ? 0 : 1;
    }
    return on;
}

static int oc_lookup(const char *g, char *out, size_t n) {
    if (!oc_enabled() || !g || g[0] != '/' || strlen(g) >= sizeof(((struct ocent *)0)->guest)) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(g);
    struct ocent *e = &g_oc[h & (OCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->guest, g)) {
        if (e->hlen < n) {
            memcpy(out, e->host, (size_t)e->hlen + 1); // hot path: bounded copy incl. NUL, no format parse
        } else {
            memcpy(out, e->host, n - 1);
            out[n - 1] = 0;
        }
        hit = 1;
    }
    CUL;
    return hit;
}

static void oc_store(const char *g, const char *host) {
    if (!oc_enabled() || !g || g[0] != '/' || !host) return;
    // over-length paths simply bypass the cache (fixed-size slot) -> re-walked every time, safely.
    size_t hl = strlen(host);
    if (strlen(g) >= sizeof(((struct ocent *)0)->guest) || hl >= sizeof(((struct ocent *)0)->host)) return;
    // defensive: never cache a host path that resolved OUTSIDE the rootfs jail (item-9-style confinement).
    if (g_rootfs_canon_len && strncmp(host, g_rootfs_canon, g_rootfs_canon_len)) return;
    CLK;
    uint64_t h = mc_hash(g);
    struct ocent *e = &g_oc[h & (OCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch; // stamp the CURRENT epoch; a later mutation invalidates it
    e->fgen = g_fs_fgen;
    e->hlen = (uint16_t)hl;
    strcpy(e->guest, g);
    memcpy(e->host, host, hl + 1);
    CUL;
}

// fork child: drop every inherited (COW) entry so it cannot outlive a parent-side mutation. Called from
// rc_reset() which already holds the cache lock, so this does NOT re-take it (non-recursive mutex).
static void oc_reset(void) {
    memset(g_oc, 0, sizeof g_oc);
}

// ---- daemon-write coherence: the external-writer generation (docker cp epoch blind spot) ----
// Everything above invalidates on GUEST-initiated mutations: the guest's own syscalls bump g_res_epoch
// (dispatch.c/overlay.c) or evict precisely. But the DAEMON also writes into a LIVE container's fs from
// OUTSIDE any engine -- `docker cp` (PUT /containers/{id}/archive) extracts a tar into the overlay upper
// or a bind/volume source, and an exec/health-probe spawn rewrites /etc/{hosts,resolv.conf,hostname} in
// the upper -- and no guest syscall ever announces those. A cached NEGATIVE would then hide a file
// docker-cp just delivered (guest polls ENOENT forever), and a stale POSITIVE would survive a cp OVER an
// existing file (old size/mtime -- positive mc_/rl_/ac_ entries are NOT epoch-gated, only
// precise-evicted, and the daemon can't call the evictors). Bumping g_res_epoch alone can't fix this:
// (a) its page is MAP_ANON, shared only by THIS engine's fork tree, unreachable from the daemon; and
// (b) it wouldn't invalidate the positives anyway.
//
// Mechanism: the daemon owns a 4-byte generation file, <dd-home>/containers/<cid>/fsgen, created before
// the first engine of the container spawns and handed to EVERY engine of that container (run + exec +
// health probe) as DD_FSGEN_FILE. The daemon atomically increments the mapped u32 AFTER completing any
// external write; each engine process maps the SAME file MAP_SHARED (ctor below; fork children inherit
// the mapping) and polls it once per syscall (dispatch.c service_local, before any handler can consult a
// cache). On a change it drops ALL its caches via rc_reset() -- the same conservative fork-grade full
// flush -- so a daemon write is visible no later than the guest's NEXT syscall, exactly like the
// kernel-coherent dcache on real Linux. Hot-path cost: ONE shared-page atomic load per syscall. Without
// the env (bench/tests/direct `ddjit` runs) the pointer stays on a local counter that never moves, so
// the poll is a single always-equal load and behaviour is byte-identical. Same 32-bit width and atomics
// discipline as g_res_epoch (daemon side increments with release; the poll loads with acquire so the
// flush is ordered after the daemon's completed file writes).
//
// HOT-PATH DISCIPLINE (this runs on EVERY guest syscall -- a metadata-storm workload like `tar` over a
// big tree is millions of syscalls, so even a mispriced check regresses the overlay-metadata cache win):
// the poll is a `static inline` two-word compare with a RELAXED load (plain LDR, not the pricier acquire
// LDAR) and a predicted-not-taken branch; the acquire barrier + rc_reset() live in an out-of-line `cold`
// helper reached only when the generation actually moved (i.e. only right after a real daemon write).
static _Atomic uint32_t g_fsgen_local = 0;             // fallback: "no external writer exists"
static _Atomic uint32_t *g_fsgen_ptr = &g_fsgen_local; // -> the daemon's file page once the ctor runs
static _Atomic uint32_t g_fsgen_seen = 0;              // last generation this process acted on

__attribute__((constructor)) static void fsgen_ctor(void) {
    const char *p = getenv("DD_FSGEN_FILE");
    if (!p || !p[0]) return;
    int fd = open(p, O_RDONLY | O_CLOEXEC); // engine only READS the counter; the daemon writes it
    if (fd < 0) return;
    void *m = mmap(NULL, sizeof(uint32_t), PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    if (m == MAP_FAILED) return; // degrade to the local never-moving counter; never crash
    g_fsgen_ptr = (_Atomic uint32_t *)m;
    // Snapshot the current generation so startup doesn't pay a spurious flush; the caches are empty.
    atomic_store(&g_fsgen_seen, atomic_load(g_fsgen_ptr));
}

// COLD path: the generation moved -> the daemon finished a write into this container's fs since we last
// looked. Re-read with ACQUIRE (pairs with the daemon's release fetch_add, so the flush is ordered after
// its completed file writes), drop ALL caches, and record the acquire-loaded value so a bump that arrives
// DURING the reset is re-noticed next syscall (never lost). Racing guest threads may both flush (harmless).
__attribute__((noinline, cold)) static void fsgen_flush(void) {
    uint32_t g = atomic_load_explicit(g_fsgen_ptr, memory_order_acquire);
    rc_reset(); // drops rc_/oc_/mc_/rl_/ac_/ud_ alike (the fork-grade conservative full flush)
    atomic_store_explicit(&g_fsgen_seen, g, memory_order_relaxed);
}

// The per-syscall poll (called from dispatch.c service_local BEFORE dispatch). Relaxed compare only; a
// stale relaxed read at worst defers the flush to the very next syscall, still within "visible by the
// guest's next syscall". Both operands are one word: the shared page (clean/shared -> L1) and a local.
static inline void fsgen_poll(void) {
    if (__builtin_expect(atomic_load_explicit(g_fsgen_ptr, memory_order_relaxed) !=
                             atomic_load_explicit(&g_fsgen_seen, memory_order_relaxed),
                         0))
        fsgen_flush();
}

static void fd_setpath(int fd, const char *p) {
    if (fd >= 0 && fd < 1024 && p && strlen(p) < 192) strcpy(g_fdpath[fd], p);
}

static void fd_evict(int fd) {
    if (fd >= 0 && fd < 1024 && g_fdpath[fd][0]) mc_evict(g_fdpath[fd]);
}

static void fd_clear(int fd) {
    if (fd >= 0 && fd < 1024) g_fdpath[fd][0] = 0;
}

// A create/rename/link/symlink makes a host path appear -- or changes the identity of an existing
// one. Drop every memoized existence (mc_/ac_) and readlink (rl_) entry for it so a later stat/
// access/readlink -- including a forked child that inherited this COW cache -- re-resolves the real
// file instead of a stale NEGATIVE lookup left over from before the name existed. These metadata
// caches are precise-evict, NOT epoch-gated like the rc_/oc_ path-string caches, so every mutation
// must name the exact path whose existence it changed. Two shapes: a full host path, and the jail
// idiom of a dir-fd + final component (resolved to its host path via F_GETPATH).
static void fc_evict_path(const char *hp) {
    mc_evict(hp);
    ac_evict(hp);
    rl_evict(hp);
}

// macOS errno -> Linux errno. They agree on 1..10 and 12..34 but diverge at 11 (EDEADLK<->EAGAIN)
// and everything >=35 (macOS EAGAIN=35 vs Linux 11, ENOSYS=78 vs 38, ELOOP=62 vs 40, ...). Every
// syscall that returns a host errno is translated at the boundary (QEMU-style). Identity outside.
// macOS ENOATTR(93) and ENODATA(96) both mean "no such attribute" -> Linux ENODATA(61): a wrong map
// here surfaces as ENOLINK ("Link has been severed") from getxattr and breaks cp -a / apt-key(gpgv).
// The tail (>=83) is index-exact to Darwin's bsd/sys/errno.h: EOVERFLOW=84, ECANCELED=89, EIDRM=90,
// ENOMSG=91, EILSEQ=92, ENOATTR=93, EBADMSG=94, EMULTIHOP=95, ENODATA=96, ENOLINK=97, ENOSR=98,
// ENOSTR=99, EPROTO=100, ETIME=101, EOPNOTSUPP=102, ENOTRECOVERABLE=104, EOWNERDEAD=105 — each mapped
// to its Linux number (EOVERFLOW=75, ENOMSG=42 [SysV msgrcv IPC_NOWAIT], ETIME=62, EOPNOTSUPP=95, ...).
static int m2l_errno(int m) {
    static const short T[107] = {0,   1,   2,   3,   4,   5,   6,   7,   8,  9,  10,  35,  12, 13, 14,  15,  16,  17,
                                 18,  19,  20,  21,  22,  23,  24,  25,  26, 27, 28,  29,  30, 31, 32,  33,  34,  11,
                                 115, 114, 88,  89,  90,  91,  92,  93,  94, 95, 96,  97,  98, 99, 100, 101, 102, 103,
                                 104, 105, 106, 107, 108, 109, 110, 111, 40, 36, 112, 113, 39, 22, 87,  122, 116, 66,
                                 22,  22,  22,  22,  22,  37,  38,  22,  22, 22, 22,  22,  75, 22, 22,  22,  22,  125,
                                 43,  42,  84,  61,  74,  72,  61,  67,  63, 60, 71,  62,  95, 22, 131, 130, 22};
    return (m >= 0 && m < 107) ? T[m] : m;
}
