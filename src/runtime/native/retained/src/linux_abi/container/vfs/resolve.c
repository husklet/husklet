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
    if (name_bind_pick(abs) >= 0) return 1;
    // Routes to the rootfs/overlay jail: docker --read-only makes it EROFS, except the still-writable
    // pseudo-mounts (/proc /dev /sys /tmp /run) -- exactly as runc leaves those mounted rw over a ro root.
    // A runtime `mount -o remount,ro <subpath>` additionally enforces RO on that subtree (path-based).
    return rootfs_ro_denies(abs) || hl_readonly_table_denies(&g_ro_subpaths, abs);
}

// 1 if the absolute guest path falls under ANY bind-mount volume (rw or ro). A volume is its OWN jail
// root, not the overlay rootfs/lowers, so a volume directory must be listed via plain readdir of its
// host fd -- the overlay merged-readdir only knows the image lowers + the upper and would return empty.
// openat uses this to NOT tag a volume dir fd as an overlay dir (else getdents shows an empty mount).
static int jail_is_vol(const char *abs) {
    char normalized[4200];
    confine(abs, normalized, sizeof normalized);
    return jail_match(normalized) >= 0;
}

// Convenience: resolve a (dirfd, raw) target to its guest abs path (same as abs_guest) and test RO.
static int jail_ro_at(int dirfd, const char *raw) {
    if (g_nvols == 0 && g_name_binds_count == 0 && !g_rootfs_ro && hl_readonly_table_empty(&g_ro_subpaths))
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
    char gbuf[8192];
    if (g_chroot[0]) // re-root under the guest's chroot, still confined to g_root_fd by the walk below
        chroot_apply(guest, gbuf, sizeof gbuf);
    else
        snprintf(gbuf, sizeof gbuf, "%s", guest);
    int xings = 0;   // bounded volume-boundary crossings -- guards against a pathological mount stack
    int follows = 0; // total symlinks followed across restarts -- the per-walk `budget` below is reset on
                     // every `goto restart`, so an ABSOLUTE self-referential symlink (loop -> /path/loop)
                     // would restart forever; this persistent cap turns that into ELOOP like Linux (40).
restart:;
    const char *rel;
    int volidx;
    int name_bind = name_bind_pick(gbuf);
    if (name_bind >= 0) {
        snprintf(final, fn, "%s", name_bind_host_leaf(name_bind));
        int descriptor = openat(g_name_binds[name_bind].fd, ".", O_RDONLY | O_DIRECTORY);
        return descriptor < 0 ? -errno : descriptor;
    }
    // rootfs/overlay or a volume root
    int root_fd = jail_pick_idx(gbuf, &rel, &volidx);
    // A bare launch has no rootfs fd, but an absolute path inside a registered bind volume has its own
    // pinned jail fd and is fully resolvable. Reject only after jail selection, not before it.
    int bare_root_fd = -1;
    if (root_fd < 0 && !g_rootfs) {
        bare_root_fd = open("/", O_RDONLY | O_DIRECTORY);
        root_fd = bare_root_fd;
        rel = gbuf;
        volidx = -1;
    }
    if (root_fd < 0) return -1;
    if (volidx >= 0 && g_vols[volidx].isfile) {
        int exact_volume = strcmp(gbuf, g_vols[volidx].guest) == 0;
        if (g_vols[volidx].issymlink && (!nofollow || !exact_volume)) {
            char target[4096], next[8192], parent[8192];
            ssize_t length = readlinkat(root_fd, vol_fbase(volidx), target, sizeof target - 1);
            if (length < 0) return -errno;
            target[length] = 0;
            const char *suffix = gbuf + g_vols[volidx].glen;
            if (++follows > 40) {
                resolve_loop_mark();
                return -ELOOP;
            }
            if (target[0] == '/') {
                if (path_concat(next, sizeof next, target, suffix) != 0) return -ENAMETOOLONG;
            } else {
                vol_parent_guest(volidx, parent, sizeof parent);
                char joined[8192];
                if (path_join(joined, sizeof joined, parent, target) != 0 ||
                    path_concat(next, sizeof next, joined, suffix) != 0)
                    return -ENAMETOOLONG;
            }
            confine(next, gbuf, sizeof gbuf);
            goto restart;
        }
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
            if (kl > 0 && (size_t)kl < sizeof dkey && hl_fdcache_dentry_lookup(dkey, dcanon, sizeof dcanon, &dk) &&
                dk == 0 && !strcmp(dcanon, dkey)) {
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
    if (bare_root_fd >= 0) close(bare_root_fd);
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
                int joined = parent[1] == 0 ? path_copy(next, sizeof next, tail[0] ? tail : "/")
                                            : path_concat(next, sizeof next, parent, tail);
                if (joined != 0) {
                    ret = -ENAMETOOLONG;
                    goto out;
                }
                for (int i = 0; i < nf; i++)
                    close(fds[i]);
                if (path_copy(gbuf, sizeof gbuf, next) != 0) return -ENAMETOOLONG;
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
                resolve_loop_mark();
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
                // Absolute link targets restart namespace routing at the guest root.  In particular, a
                // link inside a bind volume may point outside that mount; bare mode's outer namespace is
                // the host root, while container mode selects the configured rootfs.
                if (++follows > 40) { // Linux caps symlink traversal at 40 (an absolute self-loop else spins)
                    resolve_loop_mark();
                    ret = -ELOOP;
                    goto out;
                }
                if (path_concat(gbuf, sizeof gbuf, lk, tail) != 0) {
                    ret = -ENAMETOOLONG;
                    goto out;
                }
                for (int i = 0; i < nf; i++)
                    close(fds[i]);
                goto restart;
            } else
                // tail already carries its leading '/' (or is empty)
                if (path_concat(rest, sizeof rest, lk, tail) != 0) {
                    ret = -ENAMETOOLONG;
                    goto out;
                }
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
    if (hl_provider_tree_files_active()) return -EACCES;
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

/*
 * Compatibility boundary for incremental descriptor-authority conversion.  The
 * plan contains only guest namespace intent and opaque host-service identity;
 * the legacy parent fd remains an executor-local result and never enters the
 * contract.  Replacing this executor can therefore publish a host handle
 * without changing path classification again.
 */
static int vfs_host_error(hl_status status) {
    switch (status) {
    case HL_STATUS_NOT_FOUND: return -ENOENT;
    case HL_STATUS_PERMISSION_DENIED: return -EACCES;
    case HL_STATUS_ALREADY_EXISTS: return -EEXIST;
    case HL_STATUS_RESOURCE_LIMIT: return -ENFILE;
    case HL_STATUS_PROCESS_LIMIT: return -EMFILE;
    case HL_STATUS_OUT_OF_MEMORY: return -ENOMEM;
    case HL_STATUS_INVALID_ARGUMENT: return -EINVAL;
    case HL_STATUS_NOT_DIRECTORY: return -ENOTDIR;
    case HL_STATUS_IS_DIRECTORY: return -EISDIR;
    case HL_STATUS_SYMLINK_LOOP: return -ELOOP;
    case HL_STATUS_NAME_TOO_LONG: return -ENAMETOOLONG;
    case HL_STATUS_READ_ONLY: return -EROFS;
    default: return -EIO;
    }
}

static int jail_open_plan(int dirfd, const char *raw, uint32_t intent, uint32_t host_access, uint32_t host_creation,
                          uint32_t permissions, int typed, int (*reserve)(void *), void *reserve_opaque,
                          int (*dirfd_error)(int), int *created, char *final, size_t final_size, hl_open_plan *plan) {
    char absolute[8192];
    hl_open_request request;
    int native_parent;
    if (created != NULL) *created = 0;
    if (raw[0] == '/')
        snprintf(absolute, sizeof absolute, "%s", raw);
    else if (dirfd == -100)
        snprintf(absolute, sizeof absolute, "%s/%s", g_cwd, raw);
    else if (dirfd >= 0 && dirfd < 1024 && g_fdpath[dirfd][0]) {
        const char *guest_directory = g_fdpath[dirfd];
        if (strncmp(guest_directory, g_rootfs_canon, g_rootfs_canon_len) == 0) guest_directory += g_rootfs_canon_len;
        snprintf(absolute, sizeof absolute, "/%s/%s", guest_directory, raw);
    } else {
        return dirfd_error != NULL ? dirfd_error(dirfd) : -EBADF;
    }
    request = (hl_open_request){
        absolute, strlen(absolute), HL_HOST_HANDLE_INVALID, intent, g_nlower != 0, jail_ro(absolute), 0};
    if (hl_provider_tree_files_active()) {
        hl_host_result opened;
        hl_host_file_metadata metadata;
        int descriptors[2];
        int reserve_result = reserve != NULL ? reserve(reserve_opaque) : 0;
        uint32_t kind = (intent & HL_OPEN_DIRECTORY) != 0 ? HL_PROVIDER_TREE_DIRECTORY
                        : (intent & (HL_OPEN_PATH_ONLY | HL_OPEN_NOFOLLOW)) == (HL_OPEN_PATH_ONLY | HL_OPEN_NOFOLLOW)
                            ? HL_PROVIDER_TREE_LINK
                            : HL_PROVIDER_TREE_FILE;
        if (reserve_result < 0) return reserve_result;
        if ((intent & HL_OPEN_NOFOLLOW) != 0 && (intent & HL_OPEN_PATH_ONLY) == 0) {
            hl_host_result probe = hl_provider_tree_open_root(
                absolute, strlen(absolute), HL_HOST_FILE_READ | HL_HOST_FILE_PATH_ONLY | HL_HOST_FILE_NOFOLLOW, 0, 0,
                HL_PROVIDER_TREE_LINK);
            hl_host_result inspected;
            unsigned char target;
            if (probe.status != HL_STATUS_OK) return vfs_host_error((hl_status)probe.status);
            inspected = g_host_services->file->readlink(
                g_host_services->context, probe.value, (hl_host_bytes){.data = &target, .size = sizeof(target)});
            (void)g_host_services->file->close(g_host_services->context, probe.value);
            if (inspected.status == HL_STATUS_OK) return -ELOOP;
            if (inspected.status != HL_STATUS_INVALID_ARGUMENT) return vfs_host_error((hl_status)inspected.status);
        }
        opened = hl_provider_tree_open_root(absolute, strlen(absolute), host_access, host_creation, permissions, kind);
        if (opened.status != HL_STATUS_OK) return vfs_host_error((hl_status)opened.status);
        if (g_host_services->file->metadata(g_host_services->context, opened.value, &metadata).status != HL_STATUS_OK) {
            (void)g_host_services->file->close(g_host_services->context, opened.value);
            return -EIO;
        }
        if (pipe(descriptors) != 0) {
            (void)g_host_services->file->close(g_host_services->context, opened.value);
            return -errno;
        }
        close(descriptors[1]);
        plan->directory = HL_HOST_HANDLE_INVALID;
        plan->target = opened.value;
        plan->target_type = metadata.type;
        plan->path_size = 0;
        plan->path[0] = 0;
        if (created != NULL) *created = (host_creation & HL_HOST_FILE_CREATE) != 0;
        return descriptors[0];
    }
    {
        const hl_provider_node *service = hl_provider_namespace_launch_resolve(absolute, strlen(absolute));
        if (service != NULL) {
            hl_host_result opened;
            int placeholder;
            int reserve_result;
            if ((intent & (HL_OPEN_DIRECTORY | HL_OPEN_PATH_ONLY)) != 0) return -EINVAL;
            reserve_result = reserve != NULL ? reserve(reserve_opaque) : 0;
            if (reserve_result < 0) return reserve_result;
            opened = hl_provider_files_open_service(service->service, host_access | HL_HOST_FILE_NONBLOCK);
            if (opened.status != HL_STATUS_OK) return vfs_host_error((hl_status)opened.status);
            placeholder = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
            if (placeholder < 0) {
                (void)g_host_services->file->close(g_host_services->context, opened.value);
                return -errno;
            }
            plan->directory = HL_HOST_HANDLE_INVALID;
            plan->target = opened.value;
            plan->target_type = HL_HOST_FILE_TYPE_REGULAR;
            plan->path_size = 0;
            plan->path[0] = 0;
            if (created != NULL) *created = 0;
            return placeholder;
        }
    }
    if (hl_open_plan_build(&request, plan) != HL_STATUS_OK) return -EINVAL;
    /* Complete namespace/read-only/overlay validation before open_beneath can
       create or truncate the host object. */
    native_parent = jail_at(dirfd, raw, final, final_size, (intent & HL_OPEN_NOFOLLOW) != 0);
    if (native_parent < 0) return native_parent;
    /* The native descriptor walk above resolves `..` after symlinks and can cross from a bind root
       into its parent namespace. The host-service resolver is deliberately rooted in one selected
       jail, so feeding it the same `..` path would clamp at that jail and overwrite the correct
       native result (for example `/data/..` would remain inside the `/data` bind). */
    if (plan->kind == HL_OPEN_HOST_PATH && !path_has_dotdot(absolute) && g_host_services && g_host_services->file &&
        g_host_services->file->resolve_beneath) {
        char rooted[8192];
        const char *relative;
        hl_host_handle route_root = g_root_handle;
        hl_host_file_resolution resolved;
        uint32_t policy = (intent & HL_OPEN_NOFOLLOW) ? HL_HOST_RESOLVE_NOFOLLOW_FINAL : 0;
        if (intent & HL_OPEN_NO_SYMLINKS) policy |= HL_HOST_RESOLVE_NO_SYMLINKS;
        if (intent & HL_OPEN_CREATE) policy |= HL_HOST_RESOLVE_ALLOW_MISSING;
        if (g_chroot[0])
            chroot_apply(absolute, rooted, sizeof rooted);
        else
            snprintf(rooted, sizeof rooted, "%s", absolute);
        int volume = jail_match(rooted);
        if (volume >= 0 && !g_vols[volume].issymlink) {
            route_root = g_vols[volume].handle;
            relative = g_vols[volume].isfile ? vol_fbase(volume) : rooted + g_vols[volume].glen;
        } else if (volume < 0) {
            relative = rooted;
        } else {
            route_root = HL_HOST_HANDLE_INVALID;
            relative = rooted;
        }
        while (*relative == '/')
            relative++;
        if (!*relative) relative = ".";
        if (route_root != HL_HOST_HANDLE_INVALID &&
            g_host_services->file
                    ->resolve_beneath(g_host_services->context, route_root, relative, strlen(relative), policy,
                                      &resolved)
                    .status == HL_STATUS_OK) {
            plan->directory = resolved.parent;
            plan->target = resolved.target;
            plan->target_type = resolved.target_type;
            plan->path_size = resolved.final_size;
            memcpy(plan->path, resolved.final, resolved.final_size + 1);
            if (typed && g_host_services->file->open_beneath != NULL &&
                (resolved.target_type == HL_HOST_FILE_TYPE_REGULAR ||
                 (resolved.target_type == HL_HOST_FILE_TYPE_DIRECTORY && (intent & HL_OPEN_DIRECTORY) != 0) ||
                 (resolved.target == HL_HOST_HANDLE_INVALID && (intent & HL_OPEN_CREATE) != 0))) {
                int opened_created = 0;
                uint32_t open_policy = policy & ~(uint32_t)HL_HOST_RESOLVE_ALLOW_MISSING;
                hl_host_result opened;
                int reserve_result = reserve != NULL ? reserve(reserve_opaque) : 0;
                if (reserve_result < 0) {
                    if (resolved.target != HL_HOST_HANDLE_INVALID)
                        (void)g_host_services->file->close(g_host_services->context, resolved.target);
                    (void)g_host_services->file->close(g_host_services->context, resolved.parent);
                    close(native_parent);
                    return reserve_result;
                }
                if ((host_creation & HL_HOST_FILE_CREATE) != 0) {
                    opened = g_host_services->file->open_beneath(g_host_services->context, route_root, relative,
                                                                 strlen(relative), host_access | HL_HOST_FILE_NONBLOCK,
                                                                 host_creation | HL_HOST_FILE_EXCLUSIVE, permissions,
                                                                 open_policy);
                    if (opened.status == HL_STATUS_OK)
                        opened_created = 1;
                    else if (opened.status == HL_STATUS_ALREADY_EXISTS && (host_creation & HL_HOST_FILE_EXCLUSIVE) == 0)
                        opened = g_host_services->file->open_beneath(
                            g_host_services->context, route_root, relative, strlen(relative),
                            host_access | HL_HOST_FILE_NONBLOCK, host_creation, permissions, open_policy);
                } else {
                    opened = g_host_services->file->open_beneath(g_host_services->context, route_root, relative,
                                                                 strlen(relative), host_access | HL_HOST_FILE_NONBLOCK,
                                                                 host_creation, permissions, open_policy);
                }
                if (resolved.target != HL_HOST_HANDLE_INVALID)
                    (void)g_host_services->file->close(g_host_services->context, resolved.target);
                (void)g_host_services->file->close(g_host_services->context, resolved.parent);
                plan->directory = HL_HOST_HANDLE_INVALID;
                plan->target = opened.status == HL_STATUS_OK ? opened.value : HL_HOST_HANDLE_INVALID;
                if (opened.status != HL_STATUS_OK) {
                    close(native_parent);
                    return vfs_host_error((hl_status)opened.status);
                }
                if (created != NULL) *created = opened_created;
                /* A successful exclusive create produces a regular file even
                 * when a later metadata probe is unavailable. Do not fall
                 * back to a second native O_EXCL open and turn this success
                 * into a synthetic EEXIST. */
                if (opened_created) plan->target_type = HL_HOST_FILE_TYPE_REGULAR;
                hl_host_file_metadata metadata;
                if (g_host_services->file->metadata(g_host_services->context, plan->target, &metadata).status ==
                    HL_STATUS_OK)
                    plan->target_type = metadata.type;
            }
        }
    }
    return native_parent;
}
