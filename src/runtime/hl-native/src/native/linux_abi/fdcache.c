// hl/linux_abi -- filesystem metadata and path-resolution caches.
#include "fdcache.h"

#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#if !defined(_WIN32)
#include <sys/mman.h>
#endif

// The descriptor -> host-handle side table. Compiled in here so the unity target
// TU and the separately compiled library sources share one instance; see the
// note at the top of fdhandle.c.
#include "fdhandle.c"

#define MCACHE_N 8192
#define RCACHE_N 8192
#define UDCACHE_N 4096
#define UDVCACHE_N 4096
#define DCACHE_N 8192
#define OCACHE_N 8192
#define DC_KEYMAX 320

struct mcent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    char path[192];
    int rc;
    struct stat st;
};

struct rlent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    char path[176];
    int rc;
    char link[200];
    int linklen;
};

struct hl_fdcache_access_entry {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    char path[176];
    int rc;
};

struct rcent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    uint16_t hlen;
    char guest[200];
    char host[256];
};

struct udent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    char dir[200];
};

struct udvent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    uint8_t verdict;
    char dir[200];
};

struct dcent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    uint16_t nmiss;
    uint16_t clen;
    char key[DC_KEYMAX];
    char canon[DC_KEYMAX];
};

struct ocent {
    uint64_t hash;
    uint32_t epoch;
    uint32_t fgen;
    uint16_t hlen;
    char guest[200];
    char host[256];
};

struct hl_fdcache {
    _Atomic uint32_t resolver_epoch_local;
    _Atomic uint32_t *resolver_epoch_ptr;
    uint32_t fork_generation;
    struct mcent metadata[MCACHE_N];
    struct rlent readlink[2048];
    struct hl_fdcache_access_entry access[2048];
    struct rcent resolution[RCACHE_N];
    struct udent upper_negative[UDCACHE_N];
    struct udvent upper_verdict[UDVCACHE_N];
    struct dcent dentry[DCACHE_N];
    struct ocent open_resolution[OCACHE_N];
    _Atomic uint32_t filesystem_generation_local;
    _Atomic uint32_t *filesystem_generation_ptr;
    _Atomic uint32_t filesystem_generation_seen;
    const hl_host_services *filesystem_generation_host;
    hl_host_handle filesystem_generation_mapping;
    hl_fdcache_binding binding;
    hl_host_handle mutex;
};

static struct hl_fdcache g_fdcache = {.resolver_epoch_local = 1,
                                      .resolver_epoch_ptr = &g_fdcache.resolver_epoch_local,
                                      .filesystem_generation_ptr = &g_fdcache.filesystem_generation_local};

/* Compatibility aliases keep the established hot callsites and lock discipline unchanged. */
#define g_res_epoch_ptr g_fdcache.resolver_epoch_ptr
#define g_res_epoch (*g_res_epoch_ptr)
#define g_fs_fgen g_fdcache.fork_generation
#define g_mc g_fdcache.metadata
#define g_rl g_fdcache.readlink
#define g_ac g_fdcache.access
#define g_rc g_fdcache.resolution
#define g_ud g_fdcache.upper_negative
#define g_udv g_fdcache.upper_verdict
#define g_dc g_fdcache.dentry
#define g_oc g_fdcache.open_resolution
#define g_fsgen_ptr g_fdcache.filesystem_generation_ptr
#define g_fsgen_seen g_fdcache.filesystem_generation_seen

static int fdcache_threaded(void) {
    return g_fdcache.binding.threaded && *g_fdcache.binding.threaded;
}

static void fdcache_lock(void) {
    (void)g_fdcache.binding.host->sync->mutex_lock(g_fdcache.binding.host->context, g_fdcache.mutex);
}

static void fdcache_unlock(void) {
    (void)g_fdcache.binding.host->sync->mutex_unlock(g_fdcache.binding.host->context, g_fdcache.mutex);
}

#define CLK                                                                                                            \
    int fdcache_locked = fdcache_threaded();                                                                           \
    if (fdcache_locked) fdcache_lock()
#define CUL                                                                                                            \
    do {                                                                                                               \
        if (fdcache_locked) fdcache_unlock();                                                                          \
    } while (0)
// ---- S2 path-resolution cache (forward decls; impl after mc_hash, which it reuses) ----
// Memoizes the absolute guest-path -> resolved host-path STRING only (the real syscall still
// runs on the result, so existence/contents are never cached). A global epoch -- bumped by
// service.c on every FS-namespace mutation -- invalidates the whole cache; hl_fdcache_reset() hard-clears
// it in the fork child so a child never serves the parent's stale mappings.
int hl_fdcache_resolution_lookup(const char *g, char *out, size_t n);
void hl_fdcache_resolution_store(const char *g, const char *host);
void hl_fdcache_resolution_bump(void);
void hl_fdcache_reset(void);
// ---- W4D openat resolution cache (forward decls; impl after the S2 rc_* cache it extends) ----
// Extends item-9's rc_* (which memoizes only the read-only atpath() resolver) to the open-heavy half:
// guest-abs-path -> canonical symlink-free host path, so a repeated open collapses the TOCTOU-safe
// per-component jail walk to a single open(host, O_NOFOLLOW). Shares g_res_epoch. Kill: W4_NOOPENCACHE=1.
int hl_fdcache_open_lookup(const char *g, char *out, size_t n);
void hl_fdcache_open_store(const char *g, const char *host);
static void oc_reset(void);

// ---- FS-metadata cache ----
// Container processes (ld.so, shells, build tools) hammer redundant stat() on
// read-only image layers; the runtime owns the syscall stream, so it can answer
// from cache. Precise invalidation: record fd->path on open, evict that path's
// entry on write/truncate/create. Single-threaded only (no cross-thread races).
//
// Entries are epoch-gated on g_res_epoch. Precise eviction keeps the mutating process coherent without
// waiting for another lookup, while the shared epoch also invalidates positive entries held by sibling
// guest processes after delete/rename. Every namespace syscall bumps the epoch BEFORE dispatch, so an
// entry stamped before the mutation instantly misses and the next stat/access re-resolves the current
// file. This closes same-process and cross-process namespace coherence gaps -- e.g. pip
// writes a .pyc to a temp file then renames it into place and immediately stat()s the final name; the
// rename target's earlier ENOENT must not outlive the rename. Cross-process create-after-negative (a child /
// pipe coprocess creates a file the parent probed absent) is covered by g_res_epoch being SHARED across the
// container process-tree (see its definition below) PLUS hl_fdcache_reset() dropping inherited caches at fork.
// CROSS-PROCESS coherence (/): the epoch lives in a MAP_SHARED page so a file CREATED by ANOTHER
// process in the same container process-tree bumps the SAME counter every process reads. The headline case is
// apt: its http download method is a fork+exec'd, PERSISTENT pipe coprocess the parent apt never reaps -- so
// there is no fork/wait boundary at which to drop the parent's stale caches. Without a shared epoch, the
// parent that cached a NEGATIVE stat/access/resolve for partial/*_InRelease BEFORE the child downloaded it
// keeps serving ENOENT forever (its own private g_res_epoch never moved), so the file looks "vanished" and
// apt's split rename fails ENOENT. With the counter shared, the child's O_CREAT/rename
// hl_fdcache_resolution_bump() invalidates
// the negative entry in the parent too. The page is created in a constructor BEFORE any guest fork, so every
// fork descendant AND in-process execve (hl keeps its own mappings across the guest execve) share one physical
// counter; a fresh `docker exec` is a NEW hl process = its own tree = its own counter (correct per-container
// isolation). Falls back to a private local counter if the mmap fails (degrades to per-process
// behaviour, never crashes).
/* One page that survives fork(2) as ONE physical counter rather than a copy per child. This is the only
 * mapping in this file that the typed memory seam does not create, and the reason is ordering, not spelling:
 * map_anonymous(HL_HOST_MEMORY_SHARED) expresses it exactly, but the counter has to exist before the guest's
 * first fork and the caller below is a load-time constructor that runs before any host is bound. Routing it
 * as-is would hand every host the private-counter fallback and silently drop cross-process coherence on
 * Linux and macOS, which is a worse trade than leaving one call unrouted. Binding a host earlier than the
 * constructor is the fix; until then this stays the single named exception.
 *
 * On a host without fork(2) the sharing has nothing to share WITH -- one process is the whole tree, so a
 * private counter is not a degraded shared one, it is the same object. NULL therefore selects the local
 * counter that the mmap failure path already selects, rather than pretending a page was mapped. */
static _Atomic uint32_t *fdcache_forkshared_counter(void) {
#if defined(_WIN32)
    return NULL;
#else
    void *m = mmap(NULL, sizeof(_Atomic uint32_t), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    return m == MAP_FAILED ? NULL : (_Atomic uint32_t *)m;
#endif
}

void hl_fdcache_runtime_init(void) {
    if (g_res_epoch_ptr != &g_fdcache.resolver_epoch_local) return;
    _Atomic uint32_t *shared = fdcache_forkshared_counter();
    if (shared != NULL) {
        atomic_store(shared, 1);
        g_res_epoch_ptr = shared;
    }
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void res_epoch_ctor(void) {
    hl_fdcache_runtime_init();
}
#endif

// process-local fork/chroot GENERATION for every cache in this file (mc/rl/ac + rc/ud/oc).
// Each entry is stamped with g_fs_fgen at store time and only hits while its stamp still matches;
// hl_fdcache_reset() (the fork-child / chroot hard reset) just bumps the counter -- O(1) -- instead of
// memset'ing ~13MB of arrays inside the fork child's critical path. The counter is ordinary process
// memory (COW-private), so a child's bump never disturbs the parent's warm caches. Reads/writes happen
// under the same CLK lock as the entries themselves.
static uint64_t mc_hash(const char *s) {
    uint64_t h = 1469598103934665603ull;
    for (; *s; s++) {
        h ^= (uint8_t)*s;
        h *= 1099511628211ull;
    }
    return h ? h : 1;
}

static int fdcache_copy_string(char *destination, size_t capacity, const char *source) {
    size_t length = strlen(source);
    if (length >= capacity) return -1;
    memcpy(destination, source, length + 1u);
    return 0;
}

int hl_fdcache_metadata_lookup(const char *p, int *rc, struct stat *out) {
    if (!p || strlen(p) >= 192) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct mcent *e = &g_mc[h & (MCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->path, p)) {
        *rc = e->rc;
        *out = e->st;
        hit = 1;
    }
    CUL;
    return hit;
}

void hl_fdcache_metadata_store(const char *p, int rc, const struct stat *s) {
    if (!p || strlen(p) >= 192) return;
    // don't cache mutable volume paths
    if (g_fdcache.binding.volume_count && *g_fdcache.binding.volume_count && g_fdcache.binding.root_canonical &&
        g_fdcache.binding.root_canonical_length &&
        strncmp(p, g_fdcache.binding.root_canonical, *g_fdcache.binding.root_canonical_length))
        return;
    CLK;
    uint64_t h = mc_hash(p);
    struct mcent *e = &g_mc[h & (MCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    (void)fdcache_copy_string(e->path, sizeof(e->path), p);
    e->rc = rc;
    e->st = *s;
    CUL;
}

void hl_fdcache_metadata_evict(const char *p) {
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
void hl_fdcache_metadata_evict_inode(dev_t dev, ino_t ino) {
    if (!ino) return;
    CLK;
    for (int i = 0; i < MCACHE_N; i++) {
        struct mcent *e = &g_mc[i];
        if (e->hash && e->rc == 0 && e->st.st_ino == ino && e->st.st_dev == dev) e->hash = 0;
    }
    CUL;
}

// readlink cache (ld.so resolves symlinks on every library search path)
int hl_fdcache_readlink_lookup(const char *p, int *rc, char *out, int bs, int *len) {
    if (!p || strlen(p) >= 176) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->path, p)) {
        *rc = e->rc;
        int n = e->linklen < bs ? e->linklen : bs;
        if (e->rc >= 0) memcpy(out, e->link, (size_t)n);
        *len = n;
        hit = 1;
    }
    CUL;
    return hit;
}

void hl_fdcache_readlink_store(const char *p, int rc, const char *link, int len) {
    if (!p || strlen(p) >= 176 || len > 200) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    (void)fdcache_copy_string(e->path, sizeof(e->path), p);
    e->rc = rc;
    e->linklen = len;
    if (rc >= 0) memcpy(e->link, link, (size_t)len);
    CUL;
}

void hl_fdcache_readlink_evict(const char *p) {
    if (!p || !p[0]) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct rlent *e = &g_rl[h & 2047];
    if (e->hash == h && !strcmp(e->path, p)) e->hash = 0;
    CUL;
}

// access(F_OK) existence cache (ld.so probes every library candidate)
int hl_fdcache_access_lookup(const char *p, int *rc) {
    if (!p || strlen(p) >= 176) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(p);
    struct hl_fdcache_access_entry *e = &g_ac[h & 2047];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->path, p)) {
        *rc = e->rc;
        hit = 1;
    }
    CUL;
    return hit;
}

void hl_fdcache_access_store(const char *p, int rc) {
    if (!p || strlen(p) >= 176) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct hl_fdcache_access_entry *e = &g_ac[h & 2047];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    (void)fdcache_copy_string(e->path, sizeof(e->path), p);
    e->rc = rc;
    CUL;
}

void hl_fdcache_access_evict(const char *p) {
    if (!p || !p[0]) return;
    CLK;
    uint64_t h = mc_hash(p);
    struct hl_fdcache_access_entry *e = &g_ac[h & 2047];
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
//   * Hard reset on fork (hl_fdcache_reset, called in the clone/clone3 child) so a child never serves a
//     mapping the parent populated before the FS diverged.
// g_res_epoch is defined up with the FS-metadata cache (the metadata caches' negative-entry gating
// references it too); it is shared by these path-string caches and the metadata caches alike.
// Bump the epoch -> the whole cache misses. Skip 0 (the reserved "never matches" stamp).
// Locked under threads (same model as mc_*) so a bump can't race a concurrent lookup's epoch read.
void hl_fdcache_resolution_bump(void) {
    CLK;
    g_res_epoch++;
    if (!g_res_epoch) g_res_epoch = 1;
    if (g_fsgen_ptr != &g_fdcache.filesystem_generation_local) {
        uint32_t generation = atomic_fetch_add_explicit(g_fsgen_ptr, 1, memory_order_release) + 1;
        atomic_store_explicit(&g_fsgen_seen, generation, memory_order_relaxed);
    }
    CUL;
}

// ---- overlay upper-parent negative memo ("this guest dir does not exist in the UPPER") ----
// overlay_lookup() (vfs/overlay.c) pays, PER PATH, an upper realpath climb + whiteout lstat + opaque
// probe -- all provably-ENOENT whenever the path's parent dir chain is absent from the (near-empty,
// fresh-container) upper. This memoizes exactly that proof, keyed by the parent GUEST dir, so a
// metadata storm over the read-only image (tar/find/du/ld.so: thousands of lstats, one per file) pays
// the upper probe once per DIRECTORY instead of once per file. Correctness model is identical to rc_*:
//   * epoch-gated: every namespace mutation bumps g_res_epoch (dispatch.c hl_fdcache_resolution_bump on create/unlink/
//     rename/mkdir/..., PLUS the copy-up bumps in overlay.c for the non-syscall upper mutations), so an
//     entry can never outlive an upper dir appearing. The epoch is container-SHARED (MAP_SHARED page),
//     so another engine process (docker exec) creating upper dirs invalidates this process's memo too.
//   * fork/chroot hard reset via hl_fdcache_reset() below.
//   * volume paths are never stored (host-mutable backing; enforced at the overlay_lookup call site).
int hl_fdcache_upper_negative_lookup(const char *d) {
    if (!d || d[0] != '/' || strlen(d) >= sizeof(((struct udent *)0)->dir)) return 0;
    CLK;
    int hit = 0;
    uint64_t h = mc_hash(d);
    struct udent *e = &g_ud[h & (UDCACHE_N - 1)];
    if (e->hash == h && e->fgen == g_fs_fgen && e->epoch == g_res_epoch && !strcmp(e->dir, d)) hit = 1;
    CUL;
    return hit;
}

void hl_fdcache_upper_negative_store(const char *d) {
    if (!d || d[0] != '/' || strlen(d) >= sizeof(((struct udent *)0)->dir)) return;
    CLK;
    uint64_t h = mc_hash(d);
    struct udent *e = &g_ud[h & (UDCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    (void)fdcache_copy_string(e->dir, sizeof(e->dir), d);
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
// instantly invalidates the memo), with a fork/chroot hard reset via hl_fdcache_reset().
int hl_fdcache_upper_verdict_lookup(const char *d, int *verdict) {
    if (!d || d[0] != '/' || strlen(d) >= sizeof(((struct udvent *)0)->dir)) return 0;
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

void hl_fdcache_upper_verdict_store(const char *d, int verdict) {
    if (!d || d[0] != '/' || strlen(d) >= sizeof(((struct udvent *)0)->dir)) return;
    CLK;
    uint64_t h = mc_hash(d);
    struct udvent *e = &g_udv[h & (UDVCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch;
    e->fgen = g_fs_fgen;
    e->verdict = (uint8_t)verdict;
    (void)fdcache_copy_string(e->dir, sizeof(e->dir), d);
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
//           jail by the caller before hl_fdcache_dentry_store -- plus how many trailing components of the key
//           did NOT
//           exist (0 = the whole key resolved). A hit replays the recorded climb outcome exactly:
//           out = canon + (the nmiss popped components, a plain substring of the key) + rem.
// Files in one directory share the key, so a storm pays ONE realpath per (layer, dir) instead of one
// per file; the caller's real lstat/open ALWAYS still runs on the result, so existence and contents
// are never fabricated -- the only possible staleness is a wrong PATH STRING, prevented exactly as in
// rc_/oc_:
//   * EVERY entry (including the nmiss>0 "parent chain missing here" ones) is epoch-gated on the
//     container-shared g_res_epoch: any create/unlink/rename/mkdir/symlink/link/mount -- and every
//     overlay copy-up relocation (hl_fdcache_resolution_bump in overlay.c) -- invalidates the WHOLE cache. A symlink
//     flip inside a cached prefix is a symlinkat/unlinkat/renameat -> bumped. Over-invalidate, never
//     under.
//   * fork/chroot drop via hl_fdcache_reset below: each entry also carries the g_fs_fgen stamp and only
//     hits while it matches, so the O(1) generation bump in the fork child drops every inherited (COW)
//     entry -- same discipline as rc_/oc_/mc_ (COW/re-root hazard).
//   * volume jails are NEVER cached (host-mutable backing): enforced by hl_fdcache_dentry_cacheable, which
//     recognizes only the rootfs upper + the read-only image lowers by pointer identity.
// resolve_at (the TOCTOU-safe open walk) additionally CONSUMES entries with canon == key && nmiss == 0:
// such an entry proves every component of the key existed as a real, non-symlink directory (realpath
// returning its input verbatim admits no symlink hop), which is precisely the condition under which
// the lexical fast path and the per-component walk agree -- see the guard comments at that site.
// No dir-fds are cached (path strings only), so there is no fd-exhaustion/LRU concern.
int hl_fdcache_dentry_cacheable(const char *jcanon) {
    const struct hl_linux_vfs_namespace *vfs = g_fdcache.binding.vfs;
    if (!vfs) return 0;
    if (jcanon == vfs->root_canonical) return 1; // the writable upper (mutations bump g_res_epoch)
    for (int i = 0; i < hl_linux_vfs_lower_count(vfs); i++)
        if (jcanon == vfs->lowers[i].canon) return 1; // read-only image lowers
    return 0; // anything else (bind-mount volumes, unknown roots): host-mutable -> never cache
}

int hl_fdcache_dentry_lookup(const char *key, char *canon, size_t n, int *nmiss) {
    if (!key || !key[0] || strlen(key) >= DC_KEYMAX) return 0;
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

void hl_fdcache_dentry_store(const char *key, const char *canon, int nmiss) {
    if (!key || !key[0] || !canon || nmiss < 0 || nmiss > 0xffff) return;
    size_t cl = strlen(canon);
    if (strlen(key) >= DC_KEYMAX || cl >= DC_KEYMAX) return; // over-length: bypass, re-resolved safely
    CLK;
    uint64_t h = mc_hash(key);
    struct dcent *e = &g_dc[h & (DCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch; // stamp the CURRENT epoch; any later namespace mutation invalidates it
    e->fgen = g_fs_fgen;    // fork/chroot generation; a fork child's hl_fdcache_reset bump drops this entry
    e->nmiss = (uint16_t)nmiss;
    e->clen = (uint16_t)cl;
    (void)fdcache_copy_string(e->key, sizeof(e->key), key);
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
void hl_fdcache_reset(void) {
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

int hl_fdcache_resolution_lookup(const char *g, char *out, size_t n) {
    if (!g || g[0] != '/' || strlen(g) >= sizeof(((struct rcent *)0)->guest)) return 0;
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

void hl_fdcache_resolution_store(const char *g, const char *host) {
    if (!g || g[0] != '/' || !host) return;
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
    (void)fdcache_copy_string(e->guest, sizeof(e->guest), g);
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
// prevents: every FS-namespace mutation (service.c hl_fdcache_resolution_bump on
// mknod/mkdir/unlink/rmdir/symlink/link/
// rename/(u)mount + openat O_CREAT) bumps the epoch, so the whole cache misses -- conservative
// over-invalidation, identical threat model to item 9 (the host outside the jail is not in scope; when in
// doubt we MISS and re-walk). hl_fdcache_open_store additionally refuses to cache any host path that escaped
// the rootfs
// (defensive strncmp). The caller EXCLUDES O_CREAT/O_EXCL/O_TRUNC (mutating/creating) and O_DIRECTORY
// (deep-host-path reopen regressed -21%; see optimization-research/w4d-openat.md). Hard reset on fork via
// oc_reset() (from hl_fdcache_reset). Kill switch (read once): W4_NOOPENCACHE=1 -> the original uncached walk.
static int oc_enabled(void) {
    return 1;
}

int hl_fdcache_open_lookup(const char *g, char *out, size_t n) {
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

void hl_fdcache_open_store(const char *g, const char *host) {
    if (!oc_enabled() || !g || g[0] != '/' || !host) return;
    // over-length paths simply bypass the cache (fixed-size slot) -> re-walked every time, safely.
    size_t hl = strlen(host);
    if (strlen(g) >= sizeof(((struct ocent *)0)->guest) || hl >= sizeof(((struct ocent *)0)->host)) return;
    // defensive: never cache a host path that resolved OUTSIDE the rootfs jail (item-9-style confinement).
    const struct hl_linux_vfs_namespace *vfs = g_fdcache.binding.vfs;
    size_t root_length = vfs ? hl_linux_vfs_root_length(vfs) : 0;
    if (root_length && strncmp(host, vfs->root_canonical, root_length)) return;
    CLK;
    uint64_t h = mc_hash(g);
    struct ocent *e = &g_oc[h & (OCACHE_N - 1)];
    e->hash = h;
    e->epoch = g_res_epoch; // stamp the CURRENT epoch; a later mutation invalidates it
    e->fgen = g_fs_fgen;
    e->hlen = (uint16_t)hl;
    (void)fdcache_copy_string(e->guest, sizeof(e->guest), g);
    memcpy(e->host, host, hl + 1);
    CUL;
}

// fork child: drop every inherited (COW) entry so it cannot outlive a parent-side mutation. Called from
// hl_fdcache_reset() already holds the cache lock, so this does NOT re-take it (non-recursive mutex).
static void oc_reset(void) {
    memset(g_oc, 0, sizeof g_oc);
}

// ---- daemon-write coherence: the external-writer generation (docker cp epoch blind spot) ----
// Everything above invalidates on GUEST-initiated mutations: the guest's own syscalls bump g_res_epoch
// (dispatch.c/overlay.c) or evict precisely. But the DAEMON also writes into a LIVE container's fs from
// OUTSIDE any engine -- `docker cp` (PUT /containers/{id}/archive) extracts a tar into the overlay upper
// or a bind/volume source, and an exec/health-probe spawn rewrites /etc/{hosts,resolv.conf,hostname} in
// the upper -- and no guest syscall ever announces those. A cached result would then hide or misdescribe
// the daemon's completed write. Bumping g_res_epoch cannot fix this because its page is MAP_ANON, shared
// only by THIS engine's fork tree and unreachable from the daemon.
//
// Mechanism: the daemon owns a 4-byte generation file, <hl-home>/containers/<cid>/fsgen, created before
// the first engine of the container spawns and handed to EVERY engine of that container (run + exec +
// health probe) as HL_FSGEN_FILE. The daemon atomically increments the mapped u32 AFTER completing any
// external write, and engines increment it after every guest namespace mutation. Each engine process
// maps the SAME file MAP_SHARED (fork children inherit the mapping) and polls it once per syscall
// (dispatch.c service_local, before any handler can consult a
// cache). On a change it drops ALL its caches via hl_fdcache_reset() -- the same conservative fork-grade full
// flush -- so a daemon write is visible no later than the guest's NEXT syscall, exactly like the
// kernel-coherent dcache on real Linux. Hot-path cost: ONE shared-page atomic load per syscall. Without
// the environment (tests/direct engine runs) the pointer stays on a local counter that never moves, so
// the poll is a single always-equal load and behaviour is byte-identical. Same 32-bit width and atomics
// discipline as g_res_epoch (daemon side increments with release; the poll loads with acquire so the
// flush is ordered after the daemon's completed file writes).
//
// HOT-PATH DISCIPLINE (this runs on EVERY guest syscall -- a metadata-storm workload like `tar` over a
// big tree is millions of syscalls, so even a mispriced check regresses the overlay-metadata cache win):
// the poll is a `static inline` two-word compare with a RELAXED load (plain LDR, not the pricier acquire
// LDAR) and a predicted-not-taken branch; the acquire barrier + hl_fdcache_reset() live out of line
// helper reached only when the generation actually moved (i.e. only right after a real daemon write).
static void fsgen_bind(const hl_host_services *host, const char *path) {
    hl_host_result opened;
    hl_host_result mapped;
    hl_host_result closed;
    hl_host_file_mapping mapping = {HL_HOST_FILE_MAPPING_ABI, sizeof(mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
    const hl_host_services *old_host;
    hl_host_handle old_mapping;

    if (!path || !path[0] || !host || !host->file || !host->memory || !host->file->open_relative ||
        !host->file->close || !host->memory->map_file || !host->memory->release)
        return;
    opened = host->file->open_relative(host->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                                       HL_HOST_FILE_READ | HL_HOST_FILE_WRITE, 0, 0);
    if (opened.status != HL_STATUS_OK || opened.value == HL_HOST_HANDLE_INVALID) return;
    mapped = host->memory->map_file(host->context, opened.value, 0, 0, sizeof(uint32_t),
                                    HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_SHARED, &mapping);
    closed = host->file->close(host->context, opened.value);
    if (mapped.status != HL_STATUS_OK || mapping.handle == HL_HOST_HANDLE_INVALID || mapping.address == 0 ||
        mapping.mapped_size < sizeof(uint32_t) || closed.status != HL_STATUS_OK) {
        if (mapping.handle != HL_HOST_HANDLE_INVALID) (void)host->memory->release(host->context, mapping.handle);
        return;
    }

    old_host = g_fdcache.filesystem_generation_host;
    old_mapping = g_fdcache.filesystem_generation_mapping;
    if (old_host && old_mapping != HL_HOST_HANDLE_INVALID &&
        old_host->memory->release(old_host->context, old_mapping).status != HL_STATUS_OK) {
        (void)host->memory->release(host->context, mapping.handle);
        return;
    }
    g_fdcache.filesystem_generation_host = host;
    g_fdcache.filesystem_generation_mapping = mapping.handle;
    g_fsgen_ptr = (_Atomic uint32_t *)(uintptr_t)mapping.address;
    // Snapshot the current generation so startup doesn't pay a spurious flush; the caches are empty.
    atomic_store(&g_fsgen_seen, atomic_load(g_fsgen_ptr));
}

// COLD path: the generation moved -> the daemon finished a write into this container's fs since we last
// looked. Re-read with ACQUIRE (pairs with the daemon's release fetch_add, so the flush is ordered after
// its completed file writes), drop ALL caches, and record the acquire-loaded value so a bump that arrives
// DURING the reset is re-noticed next syscall (never lost). Racing guest threads may both flush (harmless).
__attribute__((noinline, cold)) static void fsgen_flush(void) {
    uint32_t g = atomic_load_explicit(g_fsgen_ptr, memory_order_acquire);
    hl_fdcache_reset(); // drops every descriptor cache (the fork-grade conservative full flush)
    atomic_store_explicit(&g_fsgen_seen, g, memory_order_relaxed);
}

// The per-syscall poll (called from dispatch.c service_local BEFORE dispatch). Relaxed compare only; a
// stale relaxed read at worst defers the flush to the very next syscall, still within "visible by the
// guest's next syscall". Both operands are one word: the shared page (clean/shared -> L1) and a local.
void hl_fdcache_generation_poll(void) {
    if (__builtin_expect(atomic_load_explicit(g_fsgen_ptr, memory_order_relaxed) !=
                             atomic_load_explicit(&g_fsgen_seen, memory_order_relaxed),
                         0))
        fsgen_flush();
}

void hl_fdcache_fd_setpath(int fd, const char *p) {
    if (fd < 0 || (size_t)fd >= g_fdcache.binding.fd_capacity) return;
    g_fdcache.binding.fd_paths[fd][0] = 0;
    if (p && strlen(p) < HL_FDCACHE_PATH_CAPACITY)
        (void)fdcache_copy_string(g_fdcache.binding.fd_paths[fd], HL_FDCACHE_PATH_CAPACITY, p);
}

void hl_fdcache_fd_evict(int fd) {
    if (fd >= 0 && (size_t)fd < g_fdcache.binding.fd_capacity && g_fdcache.binding.fd_paths[fd][0])
        hl_fdcache_metadata_evict(g_fdcache.binding.fd_paths[fd]);
}

void hl_fdcache_fd_clear(int fd) {
    if (fd >= 0 && (size_t)fd < g_fdcache.binding.fd_capacity) g_fdcache.binding.fd_paths[fd][0] = 0;
}

// A create/rename/link/symlink makes a host path appear -- or changes the identity of an existing
// one. Drop every memoized existence (mc_/ac_) and readlink (rl_) entry for it so a later stat/
// access/readlink -- including a forked child that inherited this COW cache -- re-resolves the real
// file instead of a stale NEGATIVE lookup left over from before the name existed. These metadata
// caches are precise-evict, NOT epoch-gated like the rc_/oc_ path-string caches, so every mutation
// must name the exact path whose existence it changed. Two shapes: a full host path, and the jail
// idiom of a dir-fd + final component (resolved to its host path via F_GETPATH).
void hl_fdcache_evict_path(const char *hp) {
    hl_fdcache_metadata_evict(hp);
    hl_fdcache_access_evict(hp);
    hl_fdcache_readlink_evict(hp);
}

int hl_fdcache_bind(const hl_fdcache_binding *binding) {
    hl_host_result created;
    if (!binding || !binding->host || !binding->host->sync || !binding->host->sync->mutex_create ||
        !binding->host->sync->mutex_lock || !binding->host->sync->mutex_unlock || !binding->fd_paths ||
        !binding->threaded)
        return -1;
    if (g_fdcache.mutex == HL_HOST_HANDLE_INVALID) {
        created = binding->host->sync->mutex_create(binding->host->context);
        if (created.status != HL_STATUS_OK || created.value == HL_HOST_HANDLE_INVALID) return -1;
        g_fdcache.mutex = created.value;
    }
    g_fdcache.binding = *binding;
    fsgen_bind(binding->host, binding->generation_file);
    hl_fdhandle_bind(binding->host);
#if defined(_WIN32)
    /* Adopt the three standard streams into the handle table.
     *
     * Gated on the host for a reason that is about descriptor numbering and not
     * about which host is which. Adopting a stream asks the file group for an
     * independent handle onto it, and where a host handle IS a descriptor that
     * consumes three more descriptor numbers -- which shifts every subsequent
     * guest fd allocation up by three and is observable to any guest that
     * expects a pipe on the by-convention-lowest free number. The table earns
     * nothing there in exchange: the descriptor already names the object, so a
     * handle beside it answers no question the descriptor could not. Where the
     * two namespaces do not coincide it earns everything -- it is the only way
     * the vectored, positional and mapping operations can reach the object at
     * all -- and it costs no guest-visible number, because the handle is not
     * one. */
    (void)hl_fdhandle_adopt_standard_streams();
#endif
    return 0;
}
