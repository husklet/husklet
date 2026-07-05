// Extracted from ../vfs.c: OCI overlay image layers (lower/upper, copy-up, whiteout, merged readdir) + abs_guest
// Not standalone -- #included by ../vfs.c at the original position (verbatim move, identical
// preprocessed TU). Relies on ../vfs.c's preceding globals/headers; see vfs.c for context.
// ---- Overlay (OCI image layers): --rootfs is the writable UPPER; --lower dirs are read-only,
// searched top->down when a path isn't in the upper. Whiteout (.wh.NAME) hides a lower entry;
// copy-up brings a lower file into the upper on write. Off entirely when g_nlower==0.
// Same-TU forward decls into fscache.c (defined later; all files are #included into one unity TU):
//   res_bump          -- the FS-namespace epoch bump; called here whenever a copy-up MUTATES the upper
//                        (mkdir/byte-copy), so the guest->host path caches AND the negative memo below
//                        can never serve a pre-copy-up answer (fchmodat/utimensat/setxattr copy-ups
//                        reach here WITHOUT a bumping syscall in dispatch.c).
//   updirneg_*        -- the "this GUEST DIR does not exist in the upper" negative memo (epoch-gated,
//                        fork/chroot-reset via rc_reset). overlay_lookup consults it so a fresh
//                        container's metadata storm (tar/find/ld.so over a still-empty upper) skips
//                        the per-entry upper realpath climb + whiteout + opaque probes entirely.
static void res_bump(void);
static int updirneg_lookup(const char *dir);
static void updirneg_store(const char *dir);
//   updirverdict_* -- the " merged-view directory VERDICT" memo (0 present / 1 hidden /
//                        2 opaque-cut), epoch-gated, fork/chroot-reset via rc_reset. overlay_dir_verdict
//                        fills it so a lower-only child under a whiteout-removed or opaque-recreated
//                        parent is not surfaced as a stale positive by the per-layer resolve.
static int updirverdict_lookup(const char *dir, int *verdict);
static void updirverdict_store(const char *dir, int verdict);
static const char *xresolve_exec(const char *p, char *buf,
                                 // fwd (defined below; overlay uses it for the upper)
                                 size_t n);

struct olayer {
    char canon[1024];
    size_t clen;
};
static struct olayer g_lower[8];
// [0] = highest-priority lower (searched first)
static int g_nlower = 0;

// register a read-only lower layer (image layer)
static void add_lower(const char *dir) {
    if (g_nlower >= 8 || !dir || !dir[0]) return;
    if (!realpath(dir, g_lower[g_nlower].canon))
        snprintf(g_lower[g_nlower].canon, sizeof g_lower[g_nlower].canon, "%s", dir);
    g_lower[g_nlower].clen = strlen(g_lower[g_nlower].canon);
    g_nlower++;
}

static void wh_hostpath(const char *jcanon, size_t jclen, const char *guest, char *out,
                        // host path of the .wh.NAME marker
                        size_t n) {
    char par[4200];
    snprintf(par, sizeof par, "%s", guest);
    char *sl = strrchr(par, '/');
    char base[256];
    snprintf(base, sizeof base, "%s", sl ? sl + 1 : par);
    if (sl) *sl = 0;
    char gw[4500];
    snprintf(gw, sizeof gw, "%s/.wh.%s", par[0] ? par : "", base);
    confine_in(jcanon, jclen, gw, out, n, 1);
}

static int wh_exists(const char *jcanon, size_t jclen,
                     // is there a whiteout for `guest` in this layer?
                     const char *guest) {
    char host[4300];
    wh_hostpath(jcanon, jclen, guest, host, sizeof host);
    struct stat st;
    return lstat(host, &st) == 0;
}

// One layer's host path for `guest`, following symlinks LAYER-relative (absolute target = layer root,
// like xresolve_exec does for the rootfs). nofollow keeps the final component unresolved. Returns the
// confined host path; the caller lstats it to test existence in this layer.
static void layer_follow(const char *jc, size_t jcl, const char *guest, char *out, size_t n, int nofollow) {
    char cur[4200];
    snprintf(cur, sizeof cur, "%s", guest);
    for (int hop = 0; hop < 40; hop++) {
        char hb[4300];
        // host path, final NOT followed
        confine_in(jc, jcl, cur, hb, sizeof hb, 1);
        struct stat st;
        if (lstat(hb, &st) != 0) {
            confine_in(jc, jcl, cur, out, n, nofollow);
            return;
            // missing -> report (confined)
        }
        if (!S_ISLNK(st.st_mode)) {
            snprintf(out, n, "%s", hb);
            return;
            // real file/dir -> done
        }
        if (nofollow && !strcmp(cur, guest)) {
            snprintf(out, n, "%s", hb);
            return;
            // lstat/readlink: keep the final link
        }
        char tgt[4200];
        ssize_t k = readlink(hb, tgt, sizeof tgt - 1);
        if (k <= 0) {
            snprintf(out, n, "%s", hb);
            return;
        }
        tgt[k] = 0;
        if (tgt[0] == '/')
            // absolute -> layer-relative
            snprintf(cur, sizeof cur, "%s", tgt);
        else {
            char d[4200];
            snprintf(d, sizeof d, "%s", cur);
            char *sl = strrchr(d, '/');
            if (sl) *sl = 0;
            char j[8400];
            snprintf(j, sizeof j, "%s/%s", d, tgt);
            snprintf(cur, sizeof cur, "%s", j);
        }
    }
    confine_in(jc, jcl, cur, out, n, nofollow);
}

// Is `guestdir` marked OPAQUE in layer (jc,jcl)? An opaque dir hides ALL entries from lower layers (a
// directory that was removed-and-recreated at runtime). Represented on disk as a `.wh..wh..opq` marker
// file inside the dir -- the same wire name real overlayfs uses.
static int dir_is_opaque(const char *jc, size_t jcl, const char *guestdir) {
    char host[4300];
    layer_follow(jc, jcl, guestdir, host, sizeof host, 0);
    char opq[4400];
    snprintf(opq, sizeof opq, "%s/%s", host, ".wh..wh..opq");
    struct stat st;
    return lstat(opq, &st) == 0;
}

// Merged-view visibility VERDICT for the GUEST directory `dir` (memoized per dir; see updirverdict_* in
// fscache.c). Returns 0 = present & lowers contribute; 1 = HIDDEN (dir or an ancestor removed/absent in
// the union -- every entry under it is ENOENT); 2 = OPAQUE-CUT (present, but an opaque marker on it or an
// ancestor hides ALL lower content, so entries come only from the writable upper). overlay_lookup gates
// the lower-layer search on this: the per-layer resolve (layer_follow) resolves a whole path inside ONE
// layer, so without this a lower-only child would still be found through a parent that `rm -r`/rmdir
// whited out (stale-positive stat after remove) or through an opaque recreated parent (stale merged
// readdir/rmdir). Walks root->`dir`: at each ancestor the topmost layer that PROVIDES it (visible)
// or WHITES it out (hidden) decides that level; an opaque upper dir sets a sticky cut that hides lower
// layers for everything beneath it. Recursion is memo-bounded (each ancestor cached, epoch-gated), so a
// metadata storm over a static image pays one climb per directory then O(1). Volume-routed paths never
// reach here (overlay_lookup excludes them -- a bind mount is its own jail, not part of the union).
static int overlay_dir_verdict(const char *dir) {
    if (!g_nlower || !dir || dir[0] != '/' || !dir[1]) return 0; // "/" and non-overlay: always present
    int v;
    if (updirverdict_lookup(dir, &v)) return v;
    // Resolve the parent's verdict first (memoized recursion). A hidden parent hides this dir outright; an
    // opaque-cut parent means this dir may come only from the upper (lowers already hidden above it).
    char par[4200];
    snprintf(par, sizeof par, "%s", dir);
    char *sl = strrchr(par, '/');
    int pver = 0;
    if (sl && sl != par) {
        *sl = 0;
        pver = overlay_dir_verdict(par);
    }
    int verdict;
    if (pver == 1) {
        verdict = 1; // parent hidden -> this dir is hidden too
    } else {
        int opaque_cut = (pver == 2); // an opaque ancestor already cut lower layers
        int provided = 0;             // 1 = a layer provides `dir`; -1 = a layer whites it out
        struct stat st;
        for (int L = -1; L < g_nlower; L++) {
            if (opaque_cut && L >= 0) break; // opaque ancestor/self: lower layers hidden
            const char *jc = L < 0 ? g_rootfs_canon : g_lower[L].canon;
            size_t jcl = L < 0 ? g_rootfs_canon_len : g_lower[L].clen;
            char host[4300];
            layer_follow(jc, jcl, dir, host, sizeof host, 0);
            if (lstat(host, &st) == 0) {
                provided = 1;
                if (L < 0 && dir_is_opaque(jc, jcl, dir)) opaque_cut = 1; // opaque upper dir hides lowers
                break;
            }
            if (wh_exists(jc, jcl, dir)) { // whiteout here, no higher layer provided it -> removed
                provided = -1;
                break;
            }
        }
        verdict = provided <= 0 ? 1 : (opaque_cut ? 2 : 0);
    }
    updirverdict_store(dir, verdict);
    return verdict;
}

// ONE overlay lookup, final component NOT followed: the topmost layer (upper, then lowers top->down)
// that has `guest`. 1 + its host path in `host`; 0 if absent or whiteout-hidden (host = the upper path,
// for ENOENT/O_CREAT). A volume path routes to its bind backing via secure_resolve. This is the single-hop
// primitive; overlay_resolve() drives the cross-layer symlink-follow loop on top of it. Both the upper and
// the lowers are probed nofollow so the FINAL component stays a symlink for the loop to re-resolve.
static int overlay_lookup(const char *guest, char *host, size_t hn) {
    struct stat st;
    char up[4300];
    up[0] = 0; // computed lazily: the memo fast path never needs it unless the entry is absent EVERYWHERE
    // Parent guest dir ("/usr/lib/x.py" -> "/usr/lib"); a root-level entry ("/etc") has no memo key.
    char par[4200];
    snprintf(par, sizeof par, "%s", guest);
    char *psl = strrchr(par, '/');
    int have_par = psl && psl != par;
    if (have_par) *psl = 0;
    // merged-view VERDICT of the parent dir. The per-layer probes below resolve a whole path
    // inside ONE layer, so a lower-only child would still surface through a parent that `rm -r`/rmdir
    // whited out (stale-positive stat after remove) or an opaque recreated parent (stale merged readdir).
    // The verdict climbs the ancestor chain once (memoized): 1 => the parent subtree is removed/absent in
    // the union -> the entry is provably ENOENT; 2 => an ancestor is opaque -> lower layers cannot
    // contribute (skip the lower search below). Rootfs-routed paths only -- a bind-mount volume is its own
    // jail (jail_match), never part of the union, and its mountpoint need not exist in any layer.
    int ovl_verdict = (have_par && jail_match(guest) < 0) ? overlay_dir_verdict(par) : 0;
    if (ovl_verdict == 1) {
        secure_resolve(guest, up, sizeof up, 1);
        snprintf(host, hn, "%s", up);
        return 0; // parent gone -> ENOENT (host = the upper path for O_CREAT/ENOENT)
    }
    // FAST PATH: the negative memo proves `par` does not exist in the UPPER (epoch-valid entry). Then the
    // entry itself, its `.wh.` whiteout, and its parent's `.wh..wh..opq` opaque marker -- all of which
    // would live under that missing upper dir -- cannot exist either, so the three upper probes below are
    // provably ENOENT and we go straight to the lowers. A later mkdir/copy-up in the upper bumps
    // g_res_epoch (dispatch.c res_bump + the copy-up bumps in this file), instantly invalidating the memo.
    if (!(have_par && updirneg_lookup(par))) {
        int upmiss = 0, isvol = 0;
        int injail = secure_resolve_probe(guest, up, sizeof up, 1, &upmiss, &isvol); // upper (or volume)
        if (upmiss == 0) { // parent chain exists in the upper (or a volume): full probe, as before
            if (lstat(up, &st) == 0) {
                snprintf(host, hn, "%s", up);
                return 1;
                // upper shadows lowers
            }
            if (wh_exists(g_rootfs_canon, g_rootfs_canon_len, guest)) {
                snprintf(host, hn, "%s", up);
                return 0;
                // deleted
            }
            // OPAQUE parent: if the guest's parent dir is opaque in the upper (a recreated dir), every lower
            // copy of a child is hidden -- don't descend to the lowers (absent, host = the upper path).
            if (have_par && dir_is_opaque(g_rootfs_canon, g_rootfs_canon_len, par)) {
                snprintf(host, hn, "%s", up);
                return 0;
            }
        } else if (have_par && injail && !isvol) {
            // Parent dir chain missing in the upper -> entry/whiteout/opaque provably absent there. Memoize
            // (rootfs-routed paths only: a volume's backing dir is host-mutable and must never be
            // negative-cached -- same exclusion mc_store applies to volume paths).
            updirneg_store(par);
        }
    }
    // search lowers top->down (skipped when an opaque ancestor cut the lower layers: verdict 2)
    if (ovl_verdict != 2)
        for (int i = 0; i < g_nlower; i++) {
            char lp[4300];
            layer_follow(g_lower[i].canon, g_lower[i].clen, guest, lp, sizeof lp, 1);
            if (lstat(lp, &st) == 0) {
                snprintf(host, hn, "%s", lp);
                return 1;
            }
            if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest)) break; // deleted below this layer
        }
    // absent -> upper path (for ENOENT/O_CREAT); compute it now if the memo fast path skipped it
    if (!up[0]) secure_resolve(guest, up, sizeof up, 1);
    snprintf(host, hn, "%s", up);
    return 0;
}

// Overlay READ resolve: topmost layer that has `guest`, FOLLOWING a final symlink across the WHOLE overlay
// stack. 1 + host on hit; 0 if absent/whiteout-hidden. a symlink CREATED AT RUNTIME in the upper whose
// target lands in a read-only lower (venv's /tmp/ve/bin/python -> /usr/local/bin/python, /etc/hostname,
// /bin/busybox) must resolve to that lower file. The old code followed the link only inside the upper
// (xresolve_exec) and then searched the lowers under the ORIGINAL (link) path, so an upper->lower link
// dead-ended at a nonexistent upper path -> ENOENT for open/exec/stat even though lstat/readlink worked.
// We now re-resolve each symlink target through overlay_lookup (upper THEN lowers + volumes), exactly like
// a fresh path, so the chain crosses layers. nofollow keeps the final component (lstat/readlink/unlink).
// Bounded to 40 hops; a symlink loop terminates as absent (ENOENT), never hangs.
static int overlay_resolve(const char *guest, char *host, size_t hn, int nofollow) {
    char cur[4200];
    snprintf(cur, sizeof cur, "%s", guest);
    for (int hop = 0; hop < 40; hop++) {
        if (!overlay_lookup(cur, host, hn)) return 0; // absent/whiteout -> host is the upper path
        if (nofollow) return 1;                       // want the link itself, not its target
        struct stat st;
        if (lstat(host, &st) != 0 || !S_ISLNK(st.st_mode)) return 1; // real file/dir -> done
        char tgt[4200];
        ssize_t k = readlink(host, tgt, sizeof tgt - 1);
        if (k <= 0) return 1; // unreadable link -> hand back the link's host path
        tgt[k] = 0;
        if (tgt[0] == '/')
            snprintf(cur, sizeof cur, "%s", tgt); // absolute -> overlay-root-relative
        else {                                    // relative -> resolve against the link's own dir
            char d[4200];
            snprintf(d, sizeof d, "%s", cur);
            char *sl = strrchr(d, '/');
            if (sl) *sl = 0;
            char j[8400];
            snprintf(j, sizeof j, "%s/%s", d, tgt);
            snprintf(cur, sizeof cur, "%s", j);
        }
    }
    return 0; // chain too deep -> ENOENT (host holds the last upper path)
}

// Resolve an executable/interpreter path through the FULL overlay (upper THEN lowers), returning the host
// path in `buf`. Drop-in for xresolve_exec at the ELF-loader sites so a program that lives only in a
// read-only --lower (empty upper) is found -- a bare xresolve_exec checks the upper alone and ENOENTs.
// With no lowers (the flat-rootfs pull case) this is identical to xresolve_exec.
static const char *xresolve_overlay(const char *p, char *buf, size_t n) {
    if (!(g_rootfs && p && p[0] == '/')) return p;
    overlay_resolve(p, buf, n, 0);
    return buf;
}

// Overlay CREATE-op pre-flight (mkdirat / mknodat / symlinkat). The host create these syscalls issue is
// confined to the writable UPPER (jail_at), so it cannot see a name a read-only lower still provides, nor a
// non-directory ancestor that lives only in a lower. Without this the create would wrongly SUCCEED (silently
// masking the lower entry with a fresh upper inode) where real overlayfs returns EEXIST, and would report
// ENOENT where a real overlay reports ENOTDIR. Return the negative errno the syscall must fail with, or 0 to
// proceed with the host create:
//   * a missing intermediate path component (merged view)          -> ENOENT;
//   * an intermediate component that is NOT a directory (merged)    -> ENOTDIR;
//   * the final NAME already present -- upper OR a non-whited-out lower, incl. a symlink -> EEXIST.
// MUST be called BEFORE overlay_clear_whiteout so a name currently masked by a `.wh.` whiteout is correctly
// seen as ABSENT (recreatable). Prefix components are resolved following symlinks, exactly like the kernel.
// Rootfs/overlay-jail paths only (a bind-mount volume has its own real backing dir whose host create already
// observes the true state). No-op outside overlay mode (g_nlower==0) -> non-overlay behavior is byte-identical.
static int overlay_create_precheck(const char *guest) {
    if (!g_nlower || !guest || guest[0] != '/') return 0;
    const char *canon;
    size_t clen;
    const char *rel;
    if (jail_pick(guest, &canon, &clen, &rel) != g_root_fd) return 0; // volume jail -> real backing governs
    char g[4200];
    snprintf(g, sizeof g, "%s", guest);
    size_t gl = strlen(g);
    while (gl > 1 && g[gl - 1] == '/')
        g[--gl] = 0; // "mkdir foo/" -> strip trailing slash
    char host[4300];
    struct stat st;
    // 1) prefix: every ancestor directory must exist and be a directory in the merged view.
    for (size_t i = 1; i < gl; i++) {
        if (g[i] != '/') continue;
        char pfx[4200];
        memcpy(pfx, g, i);
        pfx[i] = 0;
        if (!overlay_resolve(pfx, host, sizeof host, 0)) return -ENOENT; // ancestor absent in every layer
        if (lstat(host, &st) != 0) return -ENOENT;
        if (!S_ISDIR(st.st_mode)) return -ENOTDIR; // a file/symlink-to-file used as a directory
    }
    // 2) final name already present (nofollow: a symlink hit is still EEXIST) -> EEXIST.
    if (overlay_lookup(g, host, sizeof host)) return -EEXIST;
    return 0;
}

// Overlay: ensure every PARENT directory of `guest` exists in the writable upper, copying up (mkdir, with
// the lower's mode) each ancestor that currently lives only in a read-only lower layer. A create syscall
// (openat O_CREAT via overlay_copyup, or mkdirat/symlinkat/mknodat/renameat via jail_at) confines to the
// upper, so without this it fails with ENOENT whenever the target's parent dir is still only in the image.
// The FINAL component is never created (that is the syscall's job), and an ancestor present in NO layer is
// left missing so a genuine bad path still fails ENOENT as the kernel would. Overlay mode only (no-op when
// g_nlower==0 -> non-overlay behavior is byte-identical); rootfs-routed paths only (a volume has its own
// real backing dir and must not be mirrored into the upper).
static void overlay_mkparents(const char *guest) {
    if (!g_nlower || !guest || guest[0] != '/') return;
    const char *canon;
    size_t clen;
    const char *rel;
    if (jail_pick(guest, &canon, &clen, &rel) != g_root_fd) return;
    char par[4200];
    snprintf(par, sizeof par, "%s", guest);
    char *sl = strrchr(par, '/');
    // parent is the root dir -> always present
    if (!sl || sl == par) return;
    *sl = 0;
    // Build each ancestor prefix ("/a", "/a/b", ...) and copy it up if missing in the upper. Each level is
    // created before the next, so confine_in always resolves the (now-present) parent.
    char acc[4200];
    size_t al = 0;
    acc[0] = 0;
    int made = 0; // any upper mkdir below must bump the namespace epoch (memo + path-cache coherence)
    for (char *seg = par + 1;;) {
        char *next = strchr(seg, '/');
        if (next) *next = 0;
        int w = snprintf(acc + al, sizeof acc - al, "/%s", seg);
        if (w < 0 || (size_t)w >= sizeof acc - al) return;
        al += (size_t)w;
        char up[4300];
        confine_in(g_rootfs_canon, g_rootfs_canon_len, acc, up, sizeof up, 1);
        struct stat st;
        if (lstat(up, &st) != 0)
            // missing in the upper -> copy it up from the first lower that has it as a directory
            for (int i = 0; i < g_nlower; i++) {
                char lo[4300];
                layer_follow(g_lower[i].canon, g_lower[i].clen, acc, lo, sizeof lo, 0);
                if (lstat(lo, &st) == 0 && S_ISDIR(st.st_mode)) {
                    if (mkdir(up, st.st_mode & 0777) == 0) made = 1;
                    break;
                }
            }
        if (!next) break;
        *next = '/';
        seg = next + 1;
    }
    // Dirs appeared in the upper: invalidate the epoch-gated caches (updirneg memo, rc_/oc_ path strings)
    // so no pre-copy-up "absent in upper" answer survives. Bumped ONCE per call, only when something was
    // actually created -- the common already-materialized case stays bump-free.
    if (made) res_bump();
}

// ---- copy-up metadata + opaque + whiteout helpers ------------------------------------------------
// Real overlayfs copy-up preserves the lower inode's mode (INCLUDING setuid/setgid/sticky), its
// atime/mtime, and its xattrs (file caps, security labels). dd used to keep only `st_mode & 0777`, reset
// mtime to now, and drop all xattrs -> `sudo`/`ping`/`passwd` lost setuid and file-caps the moment any
// write touched them, and reproducible builds saw wrong timestamps. These helpers carry the full metadata.

// Copy every xattr from src host file to dst (final component; NOFOLLOW). Carries guest-visible xattrs
// (the user.ddx.* namespace, file caps) AND dd's own owner xattrs (user.dd.uid/gid) across a copy-up.
static void ovl_copy_xattrs(const char *src, const char *dst) {
    char names[16384];
    ssize_t n = listxattr(src, names, sizeof names, XATTR_NOFOLLOW);
    for (ssize_t i = 0; n > 0 && i < n;) {
        const char *nm = names + i;
        i += strlen(nm) + 1;
        char val[65536];
        ssize_t vn = getxattr(src, nm, val, sizeof val, 0, XATTR_NOFOLLOW);
        if (vn >= 0) setxattr(dst, nm, val, (size_t)vn, 0, XATTR_NOFOLLOW);
    }
}

// Apply the lower's mode (incl S_ISUID/S_ISGID/S_ISVTX), atime/mtime and xattrs to a copied-up inode.
static void ovl_copy_meta(const char *src, const char *dst, const struct stat *st) {
    chmod(dst, st->st_mode & 07777);
    struct timespec ts[2] = {st->st_atimespec, st->st_mtimespec};
    utimensat(AT_FDCWD, dst, ts, AT_SYMLINK_NOFOLLOW);
    ovl_copy_xattrs(src, dst);
}

// Recursively remove a host path (file, symlink, or a whole directory subtree). Used to whiteout a
// lower-backed directory: a plain remove() cannot drop an upper dir that still holds child `.wh.` markers
// (ENOTEMPTY), which left the directory wrongly still resolving as present after `rm -rf`.
static void ovl_rm_rf(const char *path) {
    DIR *d = opendir(path);
    if (!d) {
        unlink(path);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        char child[8600];
        snprintf(child, sizeof child, "%s/%s", path, e->d_name);
        struct stat st;
        if (lstat(child, &st) == 0 && S_ISDIR(st.st_mode))
            ovl_rm_rf(child);
        else
            unlink(child);
    }
    closedir(d);
    rmdir(path);
}

// Drop any `.wh.NAME` whiteout shadowing `guest` in the upper (a name is being re-created there).
static void overlay_clear_whiteout(const char *guest) {
    if (!g_nlower) return;
    char wh[4300];
    wh_hostpath(g_rootfs_canon, g_rootfs_canon_len, guest, wh, sizeof wh);
    unlink(wh);
}

// Does a read-only lower provide `guest` as a DIRECTORY (so recreating it in the upper must be opaque to
// keep the lower's stale children hidden)?
static int overlay_lower_has_dir(const char *guest) {
    if (!g_nlower) return 0;
    for (int i = 0; i < g_nlower; i++) {
        char lp[4300];
        struct stat st;
        layer_follow(g_lower[i].canon, g_lower[i].clen, guest, lp, sizeof lp, 0);
        if (lstat(lp, &st) == 0) return S_ISDIR(st.st_mode);
        if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest)) return 0;
    }
    return 0;
}

// Mark the upper copy of `guest` opaque (drop a `.wh..wh..opq` marker inside it).
static void overlay_set_opaque(const char *guest) {
    if (!g_nlower) return;
    char up[4300];
    xresolve_exec(guest, up, sizeof up);
    char opq[4400];
    snprintf(opq, sizeof opq, "%s/%s", up, ".wh..wh..opq");
    int fd = open(opq, O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    res_bump(); // an opaque marker appeared: invalidate the dir-verdict memo (and negative caches) so no
                // pre-marker "lowers contribute" verdict survives within this syscall
}

// Copy-up: bring a lower file into the UPPER so it can be modified, then return the upper host path.
// If the file is only in a lower, copy its bytes up; if absent everywhere, return the upper path (create).
static void overlay_copyup(const char *guest, char *host, size_t hn) {
    char up[4300];
    xresolve_exec(guest, up, sizeof up);
    snprintf(host, hn, "%s", up);
    struct stat st;
    // already in the upper (writable)
    if (lstat(up, &st) == 0) return;
    // was deleted -> drop the whiteout, create fresh
    if (wh_exists(g_rootfs_canon, g_rootfs_canon_len, guest)) {
        char wh[4300];
        wh_hostpath(g_rootfs_canon, g_rootfs_canon_len, guest, wh, sizeof wh);
        unlink(wh);
        return;
    }
    char src[4300];
    int have = 0;
    for (int i = 0; i < g_nlower && !have; i++) {
        layer_follow(g_lower[i].canon, g_lower[i].clen, guest, src, sizeof src, 0);
        if (lstat(src, &st) == 0 && S_ISREG(st.st_mode))
            have = 1;
        else if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest))
            break;
    }
    if (!have) {
        // A lower-only DIRECTORY target (e.g. the image's /var/cache/apt/archives/partial) is absent from the
        // upper, so a metadata syscall (chmod/chown/utimensat) that jail_at confines to the upper would ENOENT
        // even though the guest plainly sees the dir. Materialize the directory itself in the upper (the dir
        // analogue of the file copy-up below; overlay_copyup only handled regular files) so the op lands on a
        // real upper inode -- its lower contents still surface through the normal upper+lower readdir merge.
        for (int i = 0; i < g_nlower; i++) {
            char lp[4300];
            struct stat ds;
            layer_follow(g_lower[i].canon, g_lower[i].clen, guest, lp, sizeof lp, 0);
            if (lstat(lp, &ds) == 0) {
                if (S_ISDIR(ds.st_mode)) {
                    overlay_mkparents(guest);
                    if (mkdir(up, ds.st_mode & 0777) == 0) res_bump(); // dir materialized in the upper
                    return;
                }
                break; // lower provides it as a non-dir (symlink/special): leave to the caller's fallback
            }
            if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest)) break; // hidden by a whiteout below here
        }
        // brand-new file: nothing to copy, but its parent dir may be lower-only -> materialize it in the upper
        // so the caller's open(O_CREAT) lands there (otherwise the create ENOENTs on a missing upper parent).
        overlay_mkparents(guest);
        return;
    }
    char dir[4300];
    snprintf(dir, sizeof dir, "%s", up);
    // mkdir -p the upper parent
    char *sl = strrchr(dir, '/');
    if (sl) {
        *sl = 0;
        for (char *q = dir + g_rootfs_canon_len + 1; *q; q++)
            if (*q == '/') {
                *q = 0;
                mkdir(dir, 0755);
                *q = '/';
            }
        mkdir(dir, 0755);
    }
    int in = open(src, O_RDONLY),
        // copy lower -> upper (full mode incl setuid/setgid/sticky; fchmod below is authoritative past umask)
        out = open(up, O_CREAT | O_WRONLY | O_TRUNC, st.st_mode & 0777);
    if (in >= 0 && out >= 0) {
        char b[1 << 16];
        ssize_t r;
        while ((r = read(in, b, sizeof b)) > 0)
            if (write(out, b, r) < 0) break;
    }
    if (in >= 0) close(in);
    if (out >= 0) close(out);
    // Preserve the lower inode's mode (incl S_ISUID/S_ISGID/S_ISVTX), atime/mtime and xattrs -- real
    // overlayfs copy-up semantics (security-critical for setuid/file-cap binaries; correctness for mtime).
    ovl_copy_meta(src, up, &st);
    // The file (and possibly its parent dirs) now lives in the UPPER: its resolved host path relocated
    // lower->upper. Bump the namespace epoch so the guest->host path caches (rc_/oc_) and the updirneg
    // memo can't keep serving the stale LOWER path -- fchmodat/fchownat/utimensat/setxattr copy-ups reach
    // here with NO bumping syscall in dispatch.c (openat write-mode copy-ups bump there as well; a second
    // bump is harmless).
    res_bump();
}

// Recursively copy a lower-only subtree rooted at `guest` into the writable upper, preserving metadata at
// every level. Used by rename(2) of a lower-only directory: real overlayfs (without redirect_dir) returns
// EXDEV and userspace `mv` copies recursively; we do the equivalent copy-up so a plain rename never loses
// the subtree (the old code materialised the lower dir as an EMPTY upper, then moved that -> DATA LOSS).
// A non-directory target falls through to the byte-copy overlay_copyup. Idempotent (skips upper entries).
static void overlay_copyup_tree(const char *guest) {
    if (!g_nlower || !guest || guest[0] != '/') return;
    char lo[4300];
    struct stat lst;
    int have = 0;
    for (int i = 0; i < g_nlower; i++) {
        layer_follow(g_lower[i].canon, g_lower[i].clen, guest, lo, sizeof lo, 0);
        if (lstat(lo, &lst) == 0) {
            have = 1;
            break;
        }
        if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest)) break;
    }
    char up[4300];
    if (!have || !S_ISDIR(lst.st_mode)) { // absent, file, or symlink -> the byte/whiteout copyup path
        overlay_copyup(guest, up, sizeof up);
        return;
    }
    // Directory: materialize it (+ ancestors) in the upper, copy its metadata, then recurse into children.
    overlay_mkparents(guest);
    xresolve_exec(guest, up, sizeof up);
    struct stat ust;
    if (lstat(up, &ust) != 0 && mkdir(up, lst.st_mode & 0777) == 0) res_bump(); // dir appeared in the upper
    ovl_copy_meta(lo, up, &lst);
    DIR *d = opendir(lo);
    if (!d) return;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        if (!strncmp(e->d_name, ".wh.", 4)) continue; // stray marker: skip (its target stays hidden)
        char childg[4600];
        snprintf(childg, sizeof childg, "%s/%s", guest, e->d_name);
        overlay_copyup_tree(childg);
    }
    closedir(d);
}

// Absolute GUEST path for (dirfd, raw) -- combines a dir-fd's guest path (upper or lower) with raw.
static void abs_guest(int dirfd, const char *raw, char *out, size_t n) {
    if (raw && raw[0] == '/') {
        snprintf(out, n, "%s", raw);
        return;
    }
    if (dirfd >= 0 && dirfd < 1024 && g_fdpath[dirfd][0]) {
        const char *gdir = g_fdpath[dirfd];
        if (strncmp(gdir, g_rootfs_canon, g_rootfs_canon_len) == 0)
            gdir += g_rootfs_canon_len;
        else
            for (int i = 0; i < g_nlower; i++)
                if (strncmp(gdir, g_lower[i].canon, g_lower[i].clen) == 0) {
                    gdir += g_lower[i].clen;
                    break;
                }
        snprintf(out, n, "/%s/%s", gdir, raw ? raw : "");
    } else
        // AT_FDCWD-relative -> guest cwd
        snprintf(out, n, "%s/%s", g_cwd, raw ? raw : "");
}

// Map a canonical HOST directory path back to its GUEST path. The guest cwd is a guest-visible path, but
// chdir/fchdir only have the host path the dir actually resolved to -- which, under the overlay, may sit in
// the writable upper (g_rootfs_canon), in any read-only lower (the image), or in a bind-mount volume. Strip
// whichever jail prefix backs it so the guest cwd is tracked correctly regardless of layer (the bare
// "strip g_rootfs_canon" form silently left g_cwd stale for a dir that lives only in a lower). Boundary
// check ('/' or end) avoids a prefix collision between sibling layer roots. Unknown -> "/" (fail safe).
static void guest_from_host_raw(const char *host, char *out, size_t n) {
    if (g_rootfs && !strncmp(host, g_rootfs_canon, g_rootfs_canon_len) &&
        (host[g_rootfs_canon_len] == '/' || host[g_rootfs_canon_len] == 0)) {
        const char *g = host + g_rootfs_canon_len;
        snprintf(out, n, "%s", g[0] ? g : "/");
        return;
    }
    for (int i = 0; i < g_nlower; i++)
        if (!strncmp(host, g_lower[i].canon, g_lower[i].clen) &&
            (host[g_lower[i].clen] == '/' || host[g_lower[i].clen] == 0)) {
            const char *g = host + g_lower[i].clen;
            snprintf(out, n, "%s", g[0] ? g : "/");
            return;
        }
    for (int i = 0; i < g_nvols; i++)
        if (!strncmp(host, g_vols[i].hcanon, g_vols[i].hlen) &&
            (host[g_vols[i].hlen] == '/' || host[g_vols[i].hlen] == 0)) {
            snprintf(out, n, "%s%s", g_vols[i].guest, host + g_vols[i].hlen);
            return;
        }
    snprintf(out, n, "/");
}

// Map a canonical HOST dir to the GUEST path, then fold it into the active chroot frame: under a chroot,
// chdir/fchdir resolve to a host dir whose rootfs-relative guest path includes the chroot prefix, but the
// guest must see g_cwd in its OWN root, so strip the prefix. No-op (byte-identical) with no chroot.
static void guest_from_host(const char *host, char *out, size_t n) {
    guest_from_host_raw(host, out, n);
    if (g_chroot[0]) chroot_strip(out, n);
}

// Append name/type to a growable (realloc-doubling) parallel array pair. Returns -1 on OOM (leaving the
// arrays valid, just not grown) so the caller emits what it already has rather than overrunning.
static int ovl_push(char (**names)[256], uint8_t **types, int *cap, int n, const char *nm, uint8_t ty) {
    if (n == *cap) {
        int nc = *cap ? *cap * 2 : 64;
        char (*n2)[256] = realloc(*names, (size_t)nc * 256);
        uint8_t *t2 = realloc(*types, (size_t)nc);
        if (n2) *names = n2;
        if (t2) *types = t2;
        if (!n2 || !t2) return -1;
        *cap = nc;
    }
    snprintf((*names)[n], 256, "%s", nm);
    (*types)[n] = ty;
    return 0;
}

// Append to the growable dedup list. Returns -1 on OOM.
static int ovl_seen(char (**seen)[256], int *scap, int ns, const char *nm) {
    if (ns == *scap) {
        int nc = *scap ? *scap * 2 : 64;
        char (*s2)[256] = realloc(*seen, (size_t)nc * 256);
        if (!s2) return -1;
        *seen = s2;
        *scap = nc;
    }
    snprintf((*seen)[ns], 256, "%s", nm);
    return 0;
}

// Overlay whiteout for a delete: remove the upper copy (if any) and drop a .wh.NAME marker in the upper.
// Merged readdir across layers (upper first, then lowers). Higher layer wins; a .wh.NAME hides NAME
// in all lower layers; .wh.* markers are not emitted. Allocates the merged name/type arrays sized to the
// actual directory (no fixed cap -- a dir with >1024 merged entries enumerates fully;) and hands
// them back via *names_out/*types_out for the caller to free(); returns the entry count (0 leaves the
// out-pointers NULL). The internal `seen` dedup list grows with the directory too.
static int overlay_readdir(const char *gdir, char (**names_out)[256], uint8_t **types_out) {
    char (*names)[256] = NULL, (*seen)[256] = NULL;
    uint8_t *types = NULL;
    int cap = 0, nout = 0, scap = 0, ns = 0;
    // POSIX: readdir must return "." (self) and ".." (parent) as real entries. The per-layer scan below
    // skips "."/".." from every layer (a higher layer's "." must not shadow a lower's contents), so
    // synthesize them first as DT_DIR (recorded in seen[] so no stray layer/volume entry doubles them).
    // Without these, GNU find infinite-loops on deep trees -- it relies on "."/".." to walk.
    if (ovl_seen(&seen, &scap, ns, ".") >= 0) {
        ns++;
        if (ovl_push(&names, &types, &cap, nout, ".", DT_DIR) >= 0) nout++;
    }
    if (ovl_seen(&seen, &scap, ns, "..") >= 0) {
        ns++;
        if (ovl_push(&names, &types, &cap, nout, "..", DT_DIR) >= 0) nout++;
    }
    // L=-1 is the upper (rootfs)
    for (int L = -1; L < g_nlower; L++) {
        const char *jc = L < 0 ? g_rootfs_canon : g_lower[L].canon;
        size_t jcl = L < 0 ? g_rootfs_canon_len : g_lower[L].clen;
        char host[4300];
        layer_follow(jc, jcl, gdir, host, sizeof host, 0);
        DIR *d = opendir(host);
        if (!d) continue;
        struct dirent *e;
        while ((e = readdir(d))) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            int wh = !strncmp(e->d_name, ".wh.", 4);
            const char *name = wh ? e->d_name + 4 : e->d_name;
            int dup = 0;
            for (int i = 0; i < ns; i++)
                if (!strcmp(seen[i], name)) {
                    dup = 1;
                    break;
                }
            if (dup) continue;
            // higher layer already decided this name -- record it (whiteouts included, so they keep
            // hiding the lower-layer copy) in the growable dedup list
            if (ovl_seen(&seen, &scap, ns, name) < 0) break;
            ns++;
            if (!wh) {
                if (ovl_push(&names, &types, &cap, nout, name, e->d_type) < 0) break;
                nout++;
                // whiteout -> hide, don't emit
            }
        }
        closedir(d);
        // OPAQUE: if this layer marks `gdir` opaque (`.wh..wh..opq`), every LOWER layer is hidden -- stop
        // merging. A dir removed-and-recreated (or an image's opaque layer) thus never re-exposes stale
        // lower children through the readdir merge.
        if (dir_is_opaque(jc, jcl, gdir)) break;
    }
    // Bind-mount mount points: a volume is its own jail (in no layer), so a NESTED mount's parent dirs are
    // invisible to the layer scan above -- and the empty placeholder we create in the writable upper can be
    // served STALE by the host FS cache when the rootfs dir is held open for the container's lifetime.
    // Synthesize each volume's immediate child under `gdir` straight from the volume table so a parent
    // listing always shows the mount entry, exactly as Docker (which mkdir -p's every mount target). A
    // directory mount's child (and any intermediate dir of a nested mount) shows as a directory; a
    // single-file mount's leaf shows as a regular file. Deduped against the real layer entries. (A volume
    // dir fd lists via plain readdir, never here -- see the jail_is_vol() guard at the openat site -- so a
    // mount is never asked to enumerate itself.)
    size_t glen = strlen(gdir);
    int at_root = glen == 1 && gdir[0] == '/';
    for (int i = 0; i < g_nvols; i++) {
        const char *rest;
        if (at_root)
            rest = g_vols[i].guest + 1; // "/data" -> "data", "/x/y" -> "x/y"
        else if (!strncmp(g_vols[i].guest, gdir, glen) && g_vols[i].guest[glen] == '/')
            rest = g_vols[i].guest + glen + 1;
        else
            continue; // not under gdir
        if (!*rest) continue;
        char child[256];
        size_t k = 0;
        while (rest[k] && rest[k] != '/' && k < sizeof child - 1) {
            child[k] = rest[k];
            k++;
        }
        child[k] = 0;
        // The file mount's own leaf (rest fully consumed, no further '/') lists as a regular file; an
        // intermediate dir of a nested mount, or a directory mount's child, lists as a directory.
        uint8_t cty = (g_vols[i].isfile && rest[k] == 0) ? DT_REG : DT_DIR;
        int dup = 0;
        for (int j = 0; j < ns; j++)
            if (!strcmp(seen[j], child)) {
                dup = 1;
                break;
            }
        if (dup) continue;
        if (ovl_seen(&seen, &scap, ns, child) < 0) break;
        ns++;
        if (ovl_push(&names, &types, &cap, nout, child, cty) < 0) break;
        nout++;
    }
    // /proc has no host backing (macOS has no /proc), so the layer scan of the empty mountpoint lists no
    // processes and `ps` shows nothing. Synthesize the introspectable pid directories -- the container
    // init ("1"), this process's own pid -- plus the "self" symlink, so a process listing finds the
    // running task(s). Each is deduped against the real entries (the proc files are served by proc_open).
    if (!strcmp(gdir, "/proc")) {
        char ent[3][16];
        uint8_t ety[3];
        int ne = 0;
        snprintf(ent[ne], sizeof ent[ne], "1");
        ety[ne++] = DT_DIR;
        int cp = container_pid();
        if (cp != 1) {
            snprintf(ent[ne], sizeof ent[ne], "%d", cp);
            ety[ne++] = DT_DIR;
        }
        snprintf(ent[ne], sizeof ent[ne], "self");
        ety[ne++] = DT_LNK;
        for (int i = 0; i < ne; i++) {
            int dup = 0;
            for (int j = 0; j < ns; j++)
                if (!strcmp(seen[j], ent[i])) {
                    dup = 1;
                    break;
                }
            if (dup) continue;
            if (ovl_seen(&seen, &scap, ns, ent[i]) < 0) break;
            ns++;
            if (ovl_push(&names, &types, &cap, nout, ent[i], ety[i]) < 0) break;
            nout++;
        }
    }
    free(seen);
    *names_out = names;
    *types_out = types;
    return nout;
}

static void overlay_whiteout(const char *guest) {
    char up[4300];
    xresolve_exec(guest, up, sizeof up);
    // drop any upper copy (file, or a whole dir subtree -- a plain remove() cannot unlink a dir that still
    // holds child `.wh.` markers, which left the path wrongly resolving as present after `rm -rf`).
    ovl_rm_rf(up);
    char wh[4300];
    wh_hostpath(g_rootfs_canon, g_rootfs_canon_len, guest, wh, sizeof wh);
    char dir[4300];
    snprintf(dir, sizeof dir, "%s", wh);
    // mkdir -p parent
    char *s2 = strrchr(dir, '/');
    if (s2) {
        *s2 = 0;
        for (char *q = dir + g_rootfs_canon_len + 1; *q; q++)
            if (*q == '/') {
                *q = 0;
                mkdir(dir, 0755);
                *q = '/';
            }
        mkdir(dir, 0755);
    }
    int fd = open(wh, O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    res_bump(); // a whiteout appeared (and the upper subtree was dropped): invalidate the dir-verdict memo
                // and negative caches so no pre-removal "present" verdict/stat survives this syscall
}
