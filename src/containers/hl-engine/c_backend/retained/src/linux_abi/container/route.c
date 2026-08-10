#include "route.h"
#include "shm.h"

// POSIX shm / named-sem backing path. glibc's shm_open/sem_open create files under /dev/shm; the guest's
// synthesized /dev tmpfs has no real host tmpfs behind it, so /dev/shm/<name> is redirected to a REAL host
// file that MAP_SHARED + fork can share coherently. In CONTAINER mode the backing lives inside the overlay
// upper's own /dev/shm dir (<rootfs_canon>/dev/shm/<name>): the segment is then PER-CONTAINER (no
// cross-container /tmp collision -- two `postgres` containers no longer alias the same DSM segment), it is
// VISIBLE to `ls /dev/shm`/stat/df through the normal overlay machinery (the file physically sits in the
// upper), and it is cleared when the container rootfs is torn down -- matching docker's per-container tmpfs.
// In direct (no-rootfs) mode use the engine's private namespace identity. The key is minted uniquely for
// every standalone launch and inherited by its fork/exec descendants; an explicitly supplied HL_NETNS lets
// related launches (docker-exec shape) share it.  A former flat /tmp filename made independent engine
// instances alias the same POSIX shm/named-sem object during concurrent matrices. Embedded slashes in
// <name> are flattened so a segment can never escape the shm dir (glibc forbids them anyway).
// Returns buf, or NULL when `guest` is not a "/dev/shm/<name>" path. g_rootfs_canon is defined in vfs.c,
// which is #included ahead of this file in the unity TU.
static int logical_fd_path(int descriptor, char *path, size_t capacity) {
    hl_linux_fd_snapshot snapshot;
    hl_host_result result;
    if (path == NULL || capacity < 2 || g_linux_box == NULL || g_linux_box->host == NULL ||
        g_linux_box->host->file == NULL || g_linux_box->host->file->path == NULL || descriptor < 0 ||
        hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)descriptor, &snapshot) != HL_STATUS_OK)
        return -1;
    result = g_linux_box->host->file->path(g_linux_box->host->context, snapshot.host_handle,
                                           (hl_host_bytes){path, capacity - 1});
    if (result.status != HL_STATUS_OK || result.value >= capacity) return -1;
    path[result.value] = 0;
    return 0;
}

// Rewrite ABSOLUTE guest paths into the rootfs; relative paths pass through (resolved
// against the dir-fd by the *at syscall, e.g. ls stat-ing entries relative to a dir).
// nofollow=1 leaves the FINAL component unresolved (lstat/AT_SYMLINK_NOFOLLOW unlink), so a
// symlink is stat'd/removed as the link itself rather than its target.
static const char *atpath(int dirfd, const char *raw, char *buf, size_t n, int nofollow) {
    resolve_loop_clear(); // reset the per-resolution symlink-loop flag; a followed resolver sets it on ELOOP
    if (!raw) return raw;
    // POSIX shm + named semaphores: glibc backs both with files under /dev/shm (shm_open -> /dev/shm/<name>,
    // sem_open -> /dev/shm/sem.<name>). Route EVERY op (open/link/unlink/stat/rename) at the SAME host
    // backing the open(2) handler uses (case 56, via hl_shm_path), so glibc's
    // multi-step named-sem create (temp file + link to the final name) and sem_unlink all resolve together.
    {
        const char *shp = hl_shm_path(raw, g_vfs_namespace.root_canonical, g_namespace_key, buf, n);
        if (shp) return shp;
    }
    // absolute -> rootfs-relative + confine (final component followed unless nofollow)
    if (raw[0] == '/') {
        // S2: serve the memoized host path (only when a rootfs is configured -- without one the
        // resolvers below return `raw` untouched and leave `buf` garbage, so there's nothing to cache).
        // Follow-path only: the rc_* cache memoizes followed results, so a nofollow lookup must bypass it.
        if (!nofollow && g_rootfs && hl_fdcache_resolution_lookup(raw, buf, n)) return buf;
        if (g_nlower) {
            overlay_resolve(raw, buf, n, nofollow);
            if (!nofollow && g_rootfs) hl_fdcache_resolution_store(raw, buf);
            return buf;
            // overlay: search upper+lowers
        }
        if (g_rootfs) {
            if (nofollow)
                xlate(raw, buf, n);
            else {
                xresolve_exec(raw, buf, n);
                hl_fdcache_resolution_store(raw, buf);
            }
            return buf;
        }
        return nofollow ? xlate(raw, buf, n) : xresolve_exec(raw, buf, n);
    }
    if (!g_rootfs) {
        /* Embedded engines keep a logical guest descriptor table; its number need not be the native
           descriptor number because the host transport owns descriptors too.  Resolve a tracked relative
           directory path through the recorded backing path so every *at syscall remains independent of
           transport descriptor allocation.  Untracked descriptors retain the native direct-mode path. */
        if (dirfd >= 0) {
            char directory[4200];
            const char *base = NULL;
            if (logical_fd_path(dirfd, directory, sizeof directory) == 0)
                base = directory;
            else if (dirfd < 1024 && g_fdpath[dirfd][0])
                base = g_fdpath[dirfd];
            // Only an absolute provider/native path can replace the dirfd.
            // A bare-mode descriptor may carry the relative spelling used to
            // open it (for example "test_dir"). Joining that spelling and
            // then still passing the original dirfd resolves it twice as
            // test_dir/test_dir/child. Leave such names relative so openat(2)
            // performs the authoritative descriptor-relative lookup.
            if (base != NULL && base[0] == '/') {
                if (path_join(buf, n, base, raw) != 0) return NULL;
                return buf;
            }
        }
        return raw;
    }
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
        char rooted[8400];
        if (path_join(rooted, sizeof rooted, "/", gdir) != 0 || path_join(combined, sizeof combined, rooted, raw) != 0)
            return NULL;
        if (g_nlower) {
            overlay_resolve(combined, buf, n, nofollow);
            return buf;
        }
        // openat then ignores dirfd (path absolute)
        if (nofollow)
            xlate(combined, buf, n);
        else
            xresolve(combined, buf, n);
        return buf;
    }
    {
        char j[8400];
        // AT_FDCWD-relative -> join the guest cwd, then confine
        if (path_join(j, sizeof j, g_cwd, raw) != 0) return NULL;
        if (g_nlower) {
            overlay_resolve(j, buf, n, nofollow);
            return buf;
        }
        if (nofollow)
            xlate(j, buf, n);
        else
            xresolve_exec(j, buf, n);
        return buf;
    }
}
