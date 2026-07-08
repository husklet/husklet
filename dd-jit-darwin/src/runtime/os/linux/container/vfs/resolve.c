// Extracted from ../vfs.c: TOCTOU-free path-jail walk (resolve_at + jail_at)
// Not standalone -- #included by ../vfs.c at the original position (verbatim move, identical
// preprocessed TU). Relies on ../vfs.c's preceding globals/headers; see vfs.c for context.
// Read-only bind-mount enforcement. Returns 1 if the absolute guest path `abs` falls under a volume
// marked read-only (`-v …:ro`); the syscall layer then fails any write-intent op with -EROFS, exactly
// as the Linux kernel does for a write to a read-only mount. Prefix match mirrors jail_pick() so RO
// detection is consistent with which jail a path is routed to. No RO volumes -> always 0 (rw is
// byte-identical: g_vols[].ro is zero-initialized for every legacy/read-write bind).
static int jail_ro(const char *abs) {
    // Test the volume the path actually routes to (longest match), so a read-write inner mount nested in a
    // read-only outer one is correctly writable (and vice-versa) -- the innermost mount governs, as in Linux.
    int i = jail_match(abs);
    if (i >= 0) return g_vols[i].ro; // a bind volume governs its own subtree (rw or ro), even under --read-only
    // Routes to the rootfs/overlay jail: docker --read-only makes it EROFS, except the still-writable
    // pseudo-mounts (/proc /dev /sys /tmp /run) -- exactly as runc leaves those mounted rw over a ro root.
    // A runtime `mount -o remount,ro <subpath>` additionally enforces RO on that subtree (path-based).
    return rootfs_ro_denies(abs) || ro_subpath_denies(abs);
}

// 1 if the absolute guest path falls under ANY bind-mount volume (rw or ro). A volume is its OWN jail
// root, not the overlay rootfs/lowers, so a volume directory must be listed via plain readdir of its
// host fd -- the overlay merged-readdir only knows the image lowers + the upper and would return empty.
// openat uses this to NOT tag a volume dir fd as an overlay dir (else getdents shows an empty mount).
static int jail_is_vol(const char *abs) {
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE);
    for (int i = 0; i < nv; i++)
        if (!g_vols[i].dead && !strncmp(abs, g_vols[i].guest, g_vols[i].glen) &&
            (abs[g_vols[i].glen] == '/' || abs[g_vols[i].glen] == 0))
            return 1;
    return 0;
}

// Convenience: resolve a (dirfd, raw) target to its guest abs path (same as abs_guest) and test RO.
static int jail_ro_at(int dirfd, const char *raw) {
    if (g_nvols == 0 && !g_rootfs_ro && g_nro_subpath == 0)
        return 0; // no RO surface at all -> skip work; behavior identical to before
    char abs[8192];
    abs_guest(dirfd, raw, abs, sizeof abs);
    return jail_ro(abs);
}

// The guest directory that CONTAINS volume `vi`'s mount point: "/x/y" -> "/x", "/data" -> "/". A `..`
// that pops above a volume's own root resolves, per Linux bind-mount semantics, to the parent mount's
// directory at the mount point -- i.e. the dir that holds the volume, which lives in the rootfs/overlay
// jail, not the volume. g_vols[].guest is absolute and has no trailing slash.
static void vol_parent_guest(int vi, char *out, size_t n) {
    const char *g = g_vols[vi].guest;
    const char *sl = strrchr(g, '/');
    size_t plen = (sl && sl != g) ? (size_t)(sl - g) : 0;
    if (plen == 0 || plen >= n) {
        snprintf(out, n, "/");
        return;
    }
    memcpy(out, g, plen);
    out[plen] = 0;
}

// Like jail_pick() but also reports the matched volume index (-1 for the rootfs/overlay jail), so the
// walk can recognize a volume's own root and cross its bind-mount boundary on a `..`. Same first-prefix
// match as jail_pick(); *rel is the path within the chosen jail.
static int jail_pick_idx(const char *abs, const char **rel, int *vi) {
    int i = jail_match(abs);
    if (i >= 0) {
        *rel = abs[g_vols[i].glen] ? abs + g_vols[i].glen : "/";
        *vi = i;
        return g_vols[i].fd;
    }
    *rel = abs;
    *vi = -1;
    return g_root_fd;
}

// Does the path contain a `..` component? The dentry-cache fast path in resolve_at must skip such
// paths: this walk resolves `..` AFTER any preceding symlink (POSIX), while confine()/the dc_ keys
// collapse it lexically -- the two only provably agree on '..'-free paths.
static int path_has_dotdot(const char *p) {
    for (const char *s = p; *s; s++)
        if (s[0] == '.' && s[1] == '.' && (s == p || s[-1] == '/') && (s[2] == '/' || s[2] == 0)) return 1;
    return 0;
}

// TOCTOU-FREE confinement. Resolve `guest` (absolute) one component at a time on PINNED dir-fds,
// never following a symlink out of the jail. Returns a fresh dir-fd to the confined parent (caller
// closes) + the final component in `final`. -1 on escape/error. No check/use gap: each step
// operates on a held fd, symlinks are read+respliced (clamped to root), and the caller's
// openat(pfd, final, O_NOFOLLOW) is atomic -- a concurrent symlink swap cannot redirect it out.
// Fully stack-local (fds[] + buffers) -> thread-safe; g_root_fd is read-only after startup.
// Bind-mount `..`: a `..` that pops above a volume's own root crosses the mount boundary back to the
// dir holding the mount point (in the parent/rootfs jail); we re-resolve that parent dir + the still
// unconsumed tail from scratch (`goto restart`), so routing, symlinks, and any outer mount are handled
// by a fresh confined walk. A `..` at the rootfs root still clamps -> the walk can never escape rootfs.
static int resolve_at(const char *guest, char *final, size_t fn, int nofollow) {
    if (g_root_fd < 0) return -1;
    char gbuf[8192];
    if (g_chroot[0]) // re-root under the guest's chroot, still confined to g_root_fd by the walk below
        chroot_apply(guest, gbuf, sizeof gbuf);
    else
        snprintf(gbuf, sizeof gbuf, "%s", guest);
    int xings = 0; // bounded volume-boundary crossings -- guards against a pathological mount stack
restart:;
    const char *rel;
    int volidx;
    // rootfs/overlay or a volume root
    int root_fd = jail_pick_idx(gbuf, &rel, &volidx);
    if (volidx >= 0 && g_vols[volidx].isfile) {
        // File bind-mount (jail_match matched only the exact mount point): `root_fd` is the host file's
        // PARENT dir. Hand back that parent + the file's basename so the caller's openat opens the bound
        // file itself -- a single-file mount has no interior to walk per-component.
        snprintf(final, fn, "%s", vol_fbase(volidx));
        int d = openat(root_fd, ".", O_RDONLY | O_DIRECTORY);
        return d < 0 ? -errno : d;
    }
    // ---- positive dentry-cache fast path (consume-only; entries produced by confine_in_m) ----------
    // A dc entry whose canon == key with nmiss == 0 proves every component of the key existed as a
    // real, NON-symlink directory when it was realpath'd (realpath returning its input verbatim admits
    // no symlink hop), under an epoch every namespace mutation bumps. On such a chain this walk and the
    // lexical key construction provably agree, so the one-openat-per-component climb collapses to ONE
    // open of the already-canonical parent dir. Exact-semantics guards (any doubt -> the full walk):
    //   * '..' paths skip (lexical vs after-symlink `..` differ -- path_has_dotdot above);
    //   * canon != key skips (a symlink was involved: realpath follows host-side, this walk resplices
    //     jail-side; the two can diverge for absolute link targets);
    //   * in follow mode a final component that IS a symlink skips, so the walk resplices it exactly
    //     as before (nofollow mode wants the link itself -- same contract as the walk's early break);
    //   * volumes (volidx >= 0) are never cached; a failed open falls through to the walk.
    // The caller's openat(pfd, final, ...) still runs -- existence/contents are never fabricated; a
    // stale path is impossible while the epoch matches (see the dc_ model in fscache.c). Kill switch:
    // DD_NOPATHCACHE=1 disables dc_lookup itself.
    if (volidx < 0 && !path_has_dotdot(gbuf)) {
        char dnorm[4200];
        confine(gbuf, dnorm, sizeof dnorm);
        char *dsl = strrchr(dnorm, '/');
        if (dsl && dsl[1] && strlen(dsl + 1) < 255) {
            char fcomp[256];
            snprintf(fcomp, sizeof fcomp, "%s", dsl + 1);
            *dsl = 0; // dnorm = the parent dir ("" when the parent is the jail root itself)
            char dkey[DC_KEYMAX];
            int kl = snprintf(dkey, sizeof dkey, "%s%s", g_rootfs_canon, dnorm);
            char dcanon[DC_KEYMAX];
            int dk;
            if (kl > 0 && (size_t)kl < sizeof dkey && dc_lookup(dkey, dcanon, sizeof dcanon, &dk) && dk == 0 &&
                !strcmp(dcanon, dkey)) {
                int d = open(dcanon, O_RDONLY | O_DIRECTORY);
                if (d >= 0) {
                    if (nofollow) {
                        snprintf(final, fn, "%s", fcomp);
                        return d;
                    }
                    struct stat fst;
                    if (fstatat(d, fcomp, &fst, AT_SYMLINK_NOFOLLOW) != 0 || !S_ISLNK(fst.st_mode)) {
                        snprintf(final, fn, "%s", fcomp);
                        return d;
                    }
                    close(d); // final component is a symlink: the full walk must resplice it
                }
            }
        }
    }
    char rest[8192];
    snprintf(rest, sizeof rest, "%s", rel);
    int fds[260], nf = 0, budget = 40, ret = -EACCES;
    fds[nf++] = openat(root_fd, ".", O_RDONLY | O_DIRECTORY);
    if (fds[0] < 0) return -EACCES;
    final[0] = 0;
    for (;;) {
        char *s = rest;
        while (*s == '/')
            s++;
        if (!*s) break;
        char *e = s;
        while (*e && *e != '/')
            e++;
        int last = (*e == 0), L = (int)(e - s);
        if (L >= 255) {
            ret = -ENAMETOOLONG;
            goto out;
        }
        char comp[256];
        memcpy(comp, s, L);
        comp[L] = 0;
        char tail[8192];
        snprintf(tail, sizeof tail, "%s", e);
        if (!strcmp(comp, ".")) {
            snprintf(rest, sizeof rest, "%s", tail);
            continue;
        }
        if (!strcmp(comp, "..")) {
            if (nf > 1) { // within the current jail -> ordinary parent
                close(fds[--nf]);
                snprintf(rest, sizeof rest, "%s", tail);
                continue;
            }
            if (volidx >= 0 && ++xings <= 64) {
                // at a volume's own root: cross the bind-mount boundary to the dir holding the mount point
                char parent[8192];
                vol_parent_guest(volidx, parent, sizeof parent);
                char next[8192];
                if (parent[1] == 0)
                    snprintf(next, sizeof next, "%s", tail[0] ? tail : "/");
                else
                    snprintf(next, sizeof next, "%s%s", parent, tail);
                for (int i = 0; i < nf; i++)
                    close(fds[i]);
                snprintf(gbuf, sizeof gbuf, "%s", next);
                goto restart;
            }
            // rootfs root (or crossing budget spent) -> clamp; the walk never escapes the rootfs
            snprintf(rest, sizeof rest, "%s", tail);
            continue;
        }
        if (last && nofollow) {
            snprintf(final, fn, "%s", comp);
            break;
        }
        struct stat st;
        if (fstatat(fds[nf - 1], comp, &st, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(st.st_mode)) {
            if (--budget < 0) {
                ret = -ELOOP;
                goto out;
            }
            char lk[4096];
            ssize_t k = readlinkat(fds[nf - 1], comp, lk, sizeof lk - 1);
            if (k < 0) {
                ret = -errno;
                goto out;
            }
            lk[k] = 0;
            if (lk[0] == '/') {
                while (nf > 1)
                    close(fds[--nf]);
                snprintf(rest, sizeof rest, "%s%s", lk, tail);
                // abs -> back to root
            } else
                // tail already carries its leading '/' (or is empty)
                snprintf(rest, sizeof rest, "%s%s", lk, tail);
            continue;
        }
        if (last) {
            snprintf(final, fn, "%s", comp);
            break;
        }
        int d = openat(fds[nf - 1], comp, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
        if (d < 0) {
            ret = -errno;
            goto out;
            // ENOENT/ENOTDIR/ELOOP -> natural
        }
        if (nf >= 260) {
            close(d);
            ret = -ELOOP;
            goto out;
        }
        fds[nf++] = d;
        snprintf(rest, sizeof rest, "%s", tail);
    }
    if (!final[0]) snprintf(final, fn, "%s", ".");
    ret = openat(fds[nf - 1], ".", O_RDONLY | O_DIRECTORY);
    if (ret < 0) ret = -errno;
out:
    for (int i = 0; i < nf; i++)
        close(fds[i]);
    return ret;
}

// Confined (parent-fd, final) for a guest *at path: absolute or tracked-dir-fd-relative; else deny.
static int jail_at(int dirfd, const char *raw, char *final, size_t fn, int nofollow) {
    char abs[8192];
    if (raw[0] == '/')
        snprintf(abs, sizeof abs, "%s", raw);
    else if (dirfd == -100)
        // AT_FDCWD -> guest cwd
        snprintf(abs, sizeof abs, "%s/%s", g_cwd, raw);
    else if (dirfd >= 0 && dirfd < 1024 && g_fdpath[dirfd][0]) {
        const char *gdir = g_fdpath[dirfd];
        if (strncmp(gdir, g_rootfs_canon, g_rootfs_canon_len) == 0) gdir += g_rootfs_canon_len;
        snprintf(abs, sizeof abs, "/%s/%s", gdir, raw);
    } else
        // untracked dir-fd: fail closed
        return -EACCES;
    size_t al = strlen(abs);
    while (al > 1 && abs[al - 1] == '/')
        // strip trailing '/' (mkdir foo/, rmdir foo/)
        abs[--al] = 0;
    // Overlay: jail_at confines to the upper, but the create/rename/metadata syscalls that call it may
    // target a path whose parent dir is still only in a read-only lower (the image). Materialize the upper
    // parent chain (copy-up) so the op lands in the writable layer. No-op outside overlay mode (g_nlower==0).
    overlay_mkparents(abs);
    return resolve_at(abs, final, fn, nofollow);
}
