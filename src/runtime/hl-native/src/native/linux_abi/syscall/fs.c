// Extracted from service(): Filesystem -- open/openat/stat*/dir/link/perm/xattr/cwd/access, every path
// confined to the rootfs jail (overlay copy-up, /proc/self/exe synth). Returns 1 if nr was handled, 0
// otherwise. Included by service.c AFTER its local helpers (overlay_*/proc_self_exe/synth_str_fd/
// cpu_range_str it calls) and before service() -- same TU scope.
#include "../device.h"
#include "fs/procfd.h"

#if defined(__linux__)
#include <linux/stat.h>
#include <sys/syscall.h>
#endif

static int guest_fill_linux_stat(uint64_t destination, const struct stat *status, const char *host_path,
                                 int descriptor) {
    uint8_t encoded[GUEST_LINUX_STAT_BYTES];
    fill_linux_stat(encoded, status, host_path, descriptor);
    return guest_copy_to(destination, encoded, sizeof encoded) == sizeof encoded ? 0 : -EFAULT;
}

static int dac_snapshot_at(int directory, const char *raw, int nofollow, hl_dac_snapshot *snapshot) {
    char guest[4200], host[4300], path[4200];
    const char *resolved;
    if (g_rootfs) {
        // Resolve against the merged namespace first so DAC observes the VFS walk's exact failure. The
        // pathname compatibility resolver returns only present/absent and therefore collapses ELOOP into
        // ENOENT before open(2) gets a chance to report the original error.
        hl_vfs_cursor_entry entry;
        memset(&entry, 0, sizeof entry);
        int resolution = hl_vfs_cursor_resolve_at(directory, raw, nofollow, &entry);
        hl_vfs_cursor_entry_release(&entry);
        // A typed host authority cannot yet materialize every regular file as a cursor entry; retain the
        // pathname metadata fallback for those compatibility results, while preserving the loop verdict
        // that fallback cannot represent.
        if (resolution == -ELOOP) return resolution;
        abs_guest(directory, raw, guest, sizeof guest);
        if (!overlay_resolve(guest, host, sizeof host, nofollow)) {
            int ancestor_error = overlay_ancestor_error(guest);
            return ancestor_error != 0 ? ancestor_error : -ENOENT;
        }
        resolved = host;
    } else {
        resolved = atpath(directory, raw, path, sizeof path, nofollow);
        if (resolved != NULL && resolved[0] != '/' && ATFD(directory) != AT_FDCWD) {
            char base[4200];
            if (hl_native_fd_path(ATFD(directory), base, sizeof base) != 0 ||
                path_join(host, sizeof host, base, resolved) != 0)
                return -EBADF;
            resolved = host;
        }
    }
    return dac_snapshot_path(resolved, nofollow, snapshot);
}

static int dac_snapshot_parent_at(int directory, const char *raw, hl_dac_snapshot *snapshot) {
    char guest[4200], host[4300], path[4200];
    const char *resolved;
    if (g_rootfs) {
        abs_guest(directory, raw, guest, sizeof guest);
        char *leaf = strrchr(guest, '/');
        if (leaf == guest)
            guest[1] = '\0';
        else if (leaf != NULL)
            *leaf = '\0';
        if (!overlay_resolve(guest, host, sizeof host, 0)) return -ENOENT;
        resolved = host;
    } else {
        resolved = atpath(directory, raw, path, sizeof path, 1);
        if (resolved != NULL && resolved[0] != '/' && ATFD(directory) != AT_FDCWD) {
            char base[4200];
            if (hl_native_fd_path(ATFD(directory), base, sizeof base) != 0 ||
                path_join(host, sizeof host, base, resolved) != 0)
                return -EBADF;
            resolved = host;
        }
        if (resolved != path) {
            if (path_copy(path, sizeof path, resolved) != 0) return -ENAMETOOLONG;
            resolved = path;
        }
        char *leaf = strrchr((char *)resolved, '/');
        if (leaf == resolved)
            leaf[1] = '\0';
        else if (leaf != NULL)
            *leaf = '\0';
        else
            resolved = ".";
    }
    return dac_snapshot_path(resolved, 0, snapshot);
}

static int dac_chmod_at(int directory, const char *raw, int nofollow) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_at(directory, raw, nofollow, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_chmod(&snapshot, &credentials);
}

static int dac_symlink_at(int directory, const char *raw) {
    hl_dac_snapshot snapshot;
    int status = dac_snapshot_at(directory, raw, 1, &snapshot);
    return status != 0 ? status : (snapshot.mode & S_IFMT) == S_IFLNK;
}

static int dac_chmod_fd(int descriptor) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_fd(descriptor, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_chmod(&snapshot, &credentials);
}

static int dac_chown_at(int directory, const char *raw, int nofollow, int64_t uid, int64_t gid) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_at(directory, raw, nofollow, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_chown(&snapshot, &credentials, uid, gid);
}

static int dac_chown_fd(int descriptor, int64_t uid, int64_t gid) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_fd(descriptor, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_chown(&snapshot, &credentials, uid, gid);
}

static int dac_explicit_times_at(int directory, const char *raw, int nofollow) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_at(directory, raw, nofollow, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_explicit_times(&snapshot, &credentials);
}

static int dac_explicit_times_fd(int descriptor) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_fd(descriptor, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_explicit_times(&snapshot, &credentials);
}

static int dac_now_times_at(int directory, const char *raw, int nofollow) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_at(directory, raw, nofollow, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_now_times(&snapshot, &credentials);
}

static int dac_now_times_fd(int descriptor) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_fd(descriptor, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_now_times(&snapshot, &credentials);
}

static int dac_create_at(int directory, const char *raw) {
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_parent_at(directory, raw, &snapshot);
    return status != 0 ? status : -hl_dac_authorize_create(&snapshot, &credentials);
}

static int dac_open_at(int directory, const char *raw, int flags, int path_only) {
    if (path_only) return 0;
    hl_dac_snapshot snapshot;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_at(directory, raw, 0, &snapshot);
    if (status == -ENOENT && (flags & 0x40) != 0) return dac_create_at(directory, raw);
    if (status != 0) return status;
    unsigned requested = (flags & 3) == 0 ? HL_DAC_READ : (flags & 3) == 1 ? HL_DAC_WRITE
                                                                              : HL_DAC_READ | HL_DAC_WRITE;
    if ((flags & 0x200) != 0) requested |= HL_DAC_WRITE;
    return -hl_dac_authorize_access(&snapshot, &credentials, requested);
}

static int dac_sticky_at(int directory, const char *raw) {
    hl_dac_snapshot parent, entry;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    int status = dac_snapshot_parent_at(directory, raw, &parent);
    if (status == 0) status = dac_snapshot_at(directory, raw, 1, &entry);
    return status != 0 ? status : -hl_dac_authorize_sticky(&parent, &entry, &credentials);
}

static int dac_unlink_trailing_slash_at(int directory, const char *raw) {
    size_t length = strlen(raw);
    if (length <= 1 || raw[length - 1] != '/') return 0;
    char trimmed[4200];
    if (path_copy(trimmed, sizeof trimmed, raw) != 0) return -ENAMETOOLONG;
    while (length > 1 && trimmed[length - 1] == '/') trimmed[--length] = 0;
    hl_dac_snapshot entry;
    int status = dac_snapshot_at(directory, trimmed, 1, &entry);
    return status != 0 ? status : (entry.mode & S_IFMT) == S_IFDIR ? 0 : -ENOTDIR;
}

// statx(2) creation time. A plain Linux struct stat carries no birth time, so the engine must consult
// a host statx to answer it -- AND to answer HONESTLY: a caller trusts stx_mask before reading
// stx_btime, so STATX_BTIME must be advertised only when the backing filesystem actually reports it
// (tmpfs/ext4/devtmpfs do; procfs/virtiofs do not). Mirroring the host's own mask bit keeps the guest
// byte-identical to a native statx for every filesystem. Returns 1 and fills sec/nsec when the host
// reported a birth time, 0 (with sec/nsec cleared) otherwise; synthetic entries pass fd<0 && path==NULL.
static int hl_statx_host_btime(const char *path, int fd, int nofollow, int64_t *sec, uint32_t *nsec) {
    *sec = 0;
    *nsec = 0;
#if defined(__linux__) && defined(SYS_statx)
    struct statx status;
    int flags = fd >= 0 ? AT_EMPTY_PATH : (nofollow ? AT_SYMLINK_NOFOLLOW : 0);
    const char *name = fd >= 0 ? "" : path;
    int directory = fd >= 0 ? fd : AT_FDCWD;
    if (fd < 0 && path == NULL) return 0;
    memset(&status, 0, sizeof status);
    if (syscall(SYS_statx, directory, name, flags, STATX_BTIME, &status) == 0 && (status.stx_mask & STATX_BTIME) != 0) {
        *sec = (int64_t)status.stx_btime.tv_sec;
        *nsec = (uint32_t)status.stx_btime.tv_nsec;
        return 1;
    }
    return 0;
#elif defined(__APPLE__)
    struct stat s;
    if (fd < 0 && path == NULL) return 0;
    if ((fd >= 0 ? fstat(fd, &s) : (nofollow ? lstat(path, &s) : stat(path, &s))) == 0) {
        *sec = (int64_t)s.st_birthtimespec.tv_sec;
        *nsec = (uint32_t)s.st_birthtimespec.tv_nsec;
        return 1;
    }
    return 0;
#else
    (void)path;
    (void)fd;
    (void)nofollow;
    return 0;
#endif
}

// statx(2) mount id: filled only when the caller requests STATX_MNT_ID (not part of STATX_BASIC_STATS
// or STATX_ALL). Mirror the host's answer so the mask bit + value match a native statx; a synthetic
// entry (fd<0 && path==NULL) has no host mount to report. Returns 1 and fills id when available.
static int hl_statx_host_mnt_id(const char *path, int fd, int nofollow, uint64_t *id) {
    *id = 0;
#if defined(__linux__) && defined(SYS_statx) && defined(STATX_MNT_ID)
    struct statx status;
    int flags = fd >= 0 ? AT_EMPTY_PATH : (nofollow ? AT_SYMLINK_NOFOLLOW : 0);
    const char *name = fd >= 0 ? "" : path;
    int directory = fd >= 0 ? fd : AT_FDCWD;
    if (fd < 0 && path == NULL) return 0;
    memset(&status, 0, sizeof status);
    if (syscall(SYS_statx, directory, name, flags, STATX_MNT_ID, &status) == 0 &&
        (status.stx_mask & STATX_MNT_ID) != 0) {
        *id = (uint64_t)status.stx_mnt_id;
        return 1;
    }
    return 0;
#else
    (void)path;
    (void)fd;
    (void)nofollow;
    return 0;
#endif
}

/* Resolve intent (HL_OPEN_NO_SYMLINKS) carried from an openat2 fall-through into
 * the shared openat handler.  Set on the openat2 (437) path and consumed once on
 * entry to the openat (56) case; a direct openat always observes it cleared. */
static _Thread_local uint32_t g_openat2_resolve_intent;

static int jail_routed_at(int dirfd, const char *path) {
    (void)dirfd;
    if (hl_provider_tree_files_active()) return path != NULL;
    if (g_rootfs) return 1;
    if (!path || path[0] != '/') return 0;
    char normalized[4200];
    confine(path, normalized, sizeof normalized);
    return jail_match(normalized) >= 0;
}

typedef struct bound_handle_slot {
    hl_linux_fd_reservation reservation;
    struct fdvis_reservation fdvis;
    int shadow;
    int active;
} bound_handle_slot;

static int bound_handle_reserve(void *opaque);
static void bound_handle_cancel(bound_handle_slot *slot);
static int64_t bound_adopt_handle(bound_handle_slot *slot, hl_host_handle file, uint32_t flags);
static int bound_handle_dirfd_error(int fd);
static int64_t bound_relocate_lowest(int64_t opened);
static int bound_handle_host_path(hl_host_handle file, char *path, size_t size);
static int bound_handle_chdir(int fd, int *result);

static uint32_t typed_open_flags(uint64_t guest) {
#if G_O_DIRECTORY == 0x4000
    const uint32_t largefile = 0x20000u;
#else
    const uint32_t largefile = 0x8000u;
#endif
    /* O_NOCTTY is meaningful only when opening a terminal. Typed relative regular/directory opens
     * cannot acquire the host controlling terminal, so accept and erase it rather than rejecting a
     * standard glibc/fts directory traversal flag with EINVAL. */
    const uint32_t no_controlling_terminal = 0x100u;
    uint32_t flags =
        (uint32_t)guest & ~(largefile | no_controlling_terminal | (uint32_t)G_O_DIRECTORY | (uint32_t)G_O_NOFOLLOW);
    if (guest & G_O_DIRECTORY) flags |= HL_LINUX_O_DIRECTORY;
    if (guest & G_O_NOFOLLOW) flags |= HL_LINUX_O_NOFOLLOW;
    return flags;
}

static uint32_t typed_host_access(uint64_t guest, int path_only) {
    uint32_t access;
    if (path_only && (guest & G_O_DIRECTORY))
        /* Linux O_PATH directory descriptors remain valid for fchdir and as *at dirfds. macOS
         * path-only/O_EVTONLY handles reject fchdir with EINVAL, so back directories with a readable
         * descriptor and retain O_PATH's I/O rejection in the Linux fd metadata. */
        access = HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY;
    else if (path_only)
        access = HL_HOST_FILE_PATH_ONLY;
    else if ((guest & 3u) == 2u)
        access = HL_HOST_FILE_READ | HL_HOST_FILE_WRITE;
    else if ((guest & 3u) == 1u)
        access = HL_HOST_FILE_WRITE;
    else
        access = HL_HOST_FILE_READ;
    if (guest & 0x400u) access |= HL_HOST_FILE_APPEND;
    if (guest & G_O_DIRECTORY) access |= HL_HOST_FILE_DIRECTORY;
    if (guest & G_O_NOFOLLOW) access |= HL_HOST_FILE_NOFOLLOW;
    return access;
}

static uint32_t typed_host_creation(uint64_t guest) {
    uint32_t creation = 0;
    if (guest & 0x40u) creation |= HL_HOST_FILE_CREATE;
    if (guest & 0x80u) creation |= HL_HOST_FILE_EXCLUSIVE;
    if (guest & 0x200u) creation |= HL_HOST_FILE_TRUNCATE;
    return creation;
}

// A terminal-control syscall (tcsetpgrp/tcsetattr) issued by a process that is in a BACKGROUND process
// group raises SIGTTOU on the whole group; with the default disposition that STOPS it. During job-control
// handoff a shell's pipeline child briefly sits in a background group between its setpgid() and the
// parent's tcsetpgrp(), so a foreground command can be SIGTTOU-stopped before it even execs (the
// "[1]+ Stopped  ls | cat" hang -- the engine's in-process children lose this race more readily than a
// real kernel does). POSIX guarantees that when SIGTTOU is blocked the call simply succeeds and NO signal
// is generated -- which is exactly what a correct shell does around these calls (bash's give_terminal_to).
// So block SIGTTOU on the host for the duration of the REAL call: it never fakes the operation (the real
// tcsetpgrp/tcsetattr still runs on the real pty) and is a no-op when the guest already blocked it.
// statfs(2)/fstatfs(2) f_type + geometry fidelity inside a container. A real container's mount tree puts
// the rootfs on OVERLAYFS and the kernel pseudo-filesystems (/proc, /sys, /sys/fs/cgroup, /dev*) on their
// own magic types with the pseudo ones reporting ZERO blocks. hl resolves every guest path into ONE host
// (macOS) directory tree, so a naive host statfs stamps the SAME magic + the SAME real-disk geometry on
// every path -- so `stat -f -c %T /proc` prints the wrong type and `df -h` lists /proc & /sys with a huge
// bogus size (busybox/coreutils df hides a mount only when f_blocks==0, which the pseudo-fs must report).
// Classify by the guest ABSOLUTE path and return the Linux magic; `*zero` marks a pseudo-fs whose block/
// inode counts must be forced to 0 (proc/sysfs/cgroup2). Only used in container (g_rootfs) mode.
static int64_t guest_statfs_magic(const char *g, int *zero) {
    *zero = 0;
    if (!strcmp(g, "/proc") || !strncmp(g, "/proc/", 6)) {
        *zero = 1;
        return 0x9fa0;
    } // PROC_SUPER_MAGIC
    if (!strcmp(g, "/sys/fs/cgroup") || !strncmp(g, "/sys/fs/cgroup/", 15)) {
        *zero = 1;
        return 0x63677270;
    } // CGROUP2
    if (!strcmp(g, "/sys") || !strncmp(g, "/sys/", 5)) {
        *zero = 1;
        return 0x62656572;
    } // SYSFS_MAGIC
    if (!strcmp(g, "/dev/mqueue") || !strncmp(g, "/dev/mqueue/", 12)) return 0x19800202; // MQUEUE_MAGIC
    if (!strcmp(g, "/dev/pts") || !strncmp(g, "/dev/pts/", 9)) return 0x1cd1;            // DEVPTS_SUPER_MAGIC
    // /dev/shm is its OWN tmpfs mount in docker (separate from the /dev tmpfs); classify it explicitly so
    // `stat -f /dev/shm` names tmpfs and `df /dev/shm` shows a real (non-zero) size regardless of any future
    // change to the /dev catch-all below.
    if (!strcmp(g, "/dev/shm") || !strncmp(g, "/dev/shm/", 9)) return 0x01021994; // TMPFS_MAGIC
    if (!strcmp(g, "/dev") || !strncmp(g, "/dev/", 5)) return 0x01021994;         // TMPFS_MAGIC
    return 0x794c7630;                                                            // OVERLAYFS_SUPER_MAGIC (rootfs)
}

static void tty_ctl_block(sigset_t *saved) {
    sigset_t blk;
    sigemptyset(&blk);
    sigaddset(&blk, SIGTTOU);
    sigprocmask(SIG_BLOCK, &blk, saved);
}

static void tty_ctl_restore(const sigset_t *saved) {
    sigprocmask(SIG_SETMASK, saved, NULL);
}

// statx returns device numbers as separate major/minor u32s, whereas struct stat packs them into a
// single st_dev/st_rdev field that the guest decodes with glibc's gnu_dev_major/minor. fill_linux_stat
// copies the host dev value into st_dev/st_rdev VERBATIM, so for statx to report the SAME major:minor a
// caller would compute from fstat/newfstatat, statx must apply those very macros to that same raw value.
// Overlay getdents64 snapshot cache (case 61): the merged cross-layer listing for a directory fd is taken
// once on the first getdents call and consumed across the many small reads libc makes. Keyed by guest
// fd+1 (0 == free). A slot MUST be invalidated on close() -- ovldents_drop, called from case 57 -- so a
// reused fd re-snapshots a fresh directory rather than serving the previous one's leftover tail. Without
// that, a directory read partially then closed poisoned the next directory opened on the same fd, which
// silently truncated postgres initdb's template1->template0/postgres copy (dropping ~1/4 of the catalog,
// e.g. PG_VERSION -> "base/5 is not a valid data directory" on the first client connect).
// nm/ty are heap-allocated by overlay_readdir (it grows them to the real entry count -- no 1024 cap, so
// large directories no longer truncate) and owned until freed (ovldents_free). Indexed DIRECTLY by
// guest fd (the getdents call site guards `fd < HL_NFD`, which is why this table is [HL_NFD] -- the comment
// used to claim [0,1024) and the table used to be [1024], and the call site never agreed); a former 16-slot
// table with slot-0 eviction
// broke deep `find`: a recursive walk keeps one open dir fd per level, so past 16 concurrent overlay dirs
// an ancestor's snapshot was evicted and its next getdents re-snapshotted from pos 0 -> re-descended the
// same subtree forever (loop threshold was exactly depth 16).
typedef struct {
    unsigned references;
    int taken; // 1 = this fd's snapshot is live
    int n, pos;
    char (*nm)[256];
    uint8_t *ty;
} ovldents_snapshot;

static ovldents_snapshot
    *g_ovldents[HL_NFD]; // [HL_NFD], not [1024]: case 61 below guards with `fd < HL_NFD` before indexing this.

static ovldents_snapshot *ovldents_require(int fd) {
    if (fd < 0 || fd >= HL_NFD) return NULL;
    if (g_ovldents[fd] == NULL) {
        g_ovldents[fd] = calloc(1, sizeof *g_ovldents[fd]);
        if (g_ovldents[fd] != NULL) g_ovldents[fd]->references = 1;
    }
    return g_ovldents[fd];
}

static void ovldents_drop(int fd) {
    if (fd < 0 || fd >= HL_NFD || g_ovldents[fd] == NULL) return;
    ovldents_snapshot *snapshot = g_ovldents[fd];
    g_ovldents[fd] = NULL;
    if (--snapshot->references == 0) {
        free(snapshot->nm);
        free(snapshot->ty);
        free(snapshot);
    }
}

static void ovldents_duplicate(int source, int destination) {
    if (source < 0 || source >= HL_NFD || destination < 0 || destination >= HL_NFD || source == destination) return;
    ovldents_snapshot *snapshot = ovldents_require(source);
    if (snapshot == NULL) return;
    ovldents_drop(destination);
    snapshot->references++;
    g_ovldents[destination] = snapshot;
}

// rewinddir/seekdir on an overlay-merged dir: reset the replay cursor. pos<=0 (or out of range) restarts
// from the top; an untaken snapshot is left alone (the next getdents re-snapshots from 0). Forward-declared
// in vfs.c for the lseek handler (io.c), which is compiled into this TU before fs.c.
static void ovldents_rewind(int fd, int pos) {
    if (fd < 0 || fd >= HL_NFD || g_ovldents[fd] == NULL || !g_ovldents[fd]->taken) return;
    ovldents_snapshot *snapshot = g_ovldents[fd];
    snapshot->pos = (pos > 0 && pos <= snapshot->n) ? pos : 0;
}

// POSIX shm / named semaphores live under /dev/shm, for which the guest /dev tmpfs has no real host tmpfs;
// glibc backs them with files there (shm_open -> /dev/shm/<name>, sem_open -> a temp /dev/shm/sem.<rnd>
// then link()ed to /dev/shm/sem.<name>). openat (case 56) redirects these to a real host file so the page
// is real and MAP_SHARED across fork; the link/rename/unlink that COMPLETE glibc's create dance must use
// the SAME backing, but the rootfs branches of those handlers resolve via jail_at into the container's
// /dev/shm and would otherwise diverge. Delegates to hl_shm_path: in container mode the
// backing sits inside the overlay upper's /dev/shm (per-container + visible to `ls /dev/shm`), in direct
// mode a flat /tmp file. Returns the host backing path for a /dev/shm/<name> guest path, or NULL otherwise.
static const char *shm_hostpath(const char *guest, char *buf, size_t n) {
    return hl_shm_path(guest, g_vfs_namespace.root_canonical, g_namespace_key, buf, n);
}

// a pty MASTER's termios + winsize are shared line-discipline state that Linux keeps on the master
// itself, so a program (apt/dpkg StartPtyMagic, ncurses, tmux) can get/set them on the master fd without
// ever opening the slave. macOS instead keeps that state in the tty struct, which is DESTROYED the instant
// the LAST slave fd closes -- so servicing a master's TIOCSWINSZ via a transient (open+use+close) slave
// loses the winsize immediately (a later TIOCGWINSZ on the master reads 0x0; verified on the host). We
// therefore CACHE the master's termios/winsize here keyed by the master fd, answer GETs from the cache,
// and on every SET both (a) push it to a transient slave so any *already-open* real slave sees it live and
// (b) stash it so ptm_apply_to_slave() can re-apply it when the guest later opens the real slave
// (/dev/pts/N, N == the master fd via TIOCGPTN). This reproduces exact Linux master semantics WITHOUT
// holding a slave open -- which would defeat the master read()/poll HUP-on-last-slave-close that script /
// tmux depend on to notice the child exited.
static uint8_t g_ptm_tset[HL_NFD], g_ptm_wset[HL_NFD];
static struct termios g_ptm_term[HL_NFD]; // host-form termios last set on the master
static struct winsize g_ptm_win[HL_NFD];  // winsize last set on the master

static void ptm_clear(int fd) {
    if (fd >= 0 && fd < HL_NFD) {
        g_ptm_tset[fd] = 0;
        g_ptm_wset[fd] = 0;
    }
}

// Re-apply a master's cached termios/winsize onto a freshly-opened slave fd (Linux: the slave shares the
// master's line discipline). `ptn` is the pts number, which hl defines to equal the master fd.
static void ptm_apply_to_slave(int ptn, int slavefd) {
    if (ptn < 0 || ptn >= HL_NFD || slavefd < 0) return;
    if (g_ptm_tset[ptn]) tcsetattr(slavefd, TCSANOW, &g_ptm_term[ptn]);
    if (g_ptm_wset[ptn]) ioctl(slavefd, TIOCSWINSZ, &g_ptm_win[ptn]);
}

#if !defined(__linux__)
// Linux keeps a pty's queued input readable on the MASTER after the last slave closes: the reader drains
// what is already there and only then sees EIO. macOS tears the tty's line-discipline queues down the
// instant the last slave fd closes, so the queued bytes vanish and the master reads EOF straight away
// (observed natively: FIONREAD 1 -> 0 across close, read() 0). Move the pending bytes into the master's
// read pushback -- the same buffer tee(2) uses, already served ahead of the host read by read/readv --
// while the tty is still alive, so the master still reads them back in order. Called from fd_reset_emul,
// which runs before the real close(2).
static void pts_master_retain_input(int slave) {
    int index = pts_index_of_fd(slave);
    if (index < 0 || pts_fd_is_master(slave)) return;
    for (int other = 0; other < HL_NFD; ++other)
        if (other != slave && !pts_fd_is_master(other) && pts_index_of_fd(other) == index) return;
    int master = pts_master_fd(index);
    if (master < 0 || master >= HL_NFD || g_fd_pb_len[master]) return;
    uint8_t queued[4096];
    size_t held = 0;
    // poll(), not FIONREAD: on macOS FIONREAD on a MASTER reports the SLAVE's input queue, so it counts
    // bytes the master WROTE and can never read back (verified natively: after write(master,5) with the
    // slave idle, Darwin FIONREAD(master)=5 while poll says nothing readable and a non-blocking read gives
    // EAGAIN; Linux correctly reports 0). Reading on that count blocked forever inside fd_reset_emul.
    // Only a positive POLLIN sanctions a read, so the drain can never block; the cap bounds it anyway.
    for (int round = 0; round < 8 && held < sizeof queued; ++round) {
        struct pollfd ready = {.fd = master, .events = POLLIN};
        if (poll(&ready, 1, 0) <= 0 || !(ready.revents & POLLIN)) break;
        ssize_t got = read(master, queued + held, sizeof queued - held);
        if (got <= 0) break;
        held += (size_t)got;
    }
    if (held) pipe_pushback_set(master, queued, held);
}
#endif

// Tear down EVERY engine-side emulation-table entry keyed by this fd NUMBER (eventfd peer/counter/sema, timerfd,
// overlay-dir, the socket/loopback/bridge maps, epoll armed-state, flock, pidfd, RAM-scratch memf, and the
// getdents/overlay-dents caches + the path map). Shared by close(2) (case 57) AND the emulated
// close-on-exec sweep (proc.c exec_close_cloexec*). hl's execve reloads the new image IN-PROCESS, so the
// sweep hand-closes each FD_CLOEXEC descriptor -- but it used to close ONLY the real fd, leaving these tables
// stamped. A CLOEXEC eventfd thus left g_eventfd_peer[fd] set after exec; the new program (postgres) opened
// postgresql.conf onto that freed fd number and read() was misrouted to the eventfd emulation -> 0 bytes of
// real content -> `syntax error in file "postgresql.conf" line 1, near token ""` and the server never starts
// (PG16/17 only -- PG15's streaming conf reader tolerated the short read; hence the version gate). Does NOT
// close(fd) itself -- the caller owns the real fd's lifetime. Safe on a non-emulated fd (every branch is
// guarded / idempotent). Mirrors case 57's teardown exactly so close(2) semantics are unchanged.
static void fd_reset_emul(int fd) {
    hl_vfs_fd_cursor_drop(fd);
    if (fd >= 0 && fd < HL_NFD) {
        /* Linux's kqueue compatibility owns private eventfd/timerfd wake descriptors keyed by the
         * kqueue's native identity. Tear those registrations down before the guest closes/reuses the fd. */
#if defined(__linux__)
        hl_native_kqueue_close(fd);
#endif
        if (g_fdvis_private[fd]) {
            hl_host_process_fd_private_remove(fd);
            g_fdvis_private[fd] = 0;
        }
        proc_fdvis_close(fd);
        mq_fd_close(fd);
        g_pipe_identity[fd] = 0;
        if (g_eventfd_peer[fd]) {
            // Refcounted teardown: a dup()'d eventfd shares the peer write end + counter slot, so only close
            // the peer and zero the shared counter when the LAST alias closes -- otherwise closing one
            // duplicate would break the object for the others (fd_carry_virt bumps the slot refcount on dup).
            int eslot = eventfd_counter_slot(fd);
            if (--g_eventfd_refs[eslot] <= 0) {
                hl_host_process_fd_private_remove(g_eventfd_peer[fd] - 1);
                close(g_eventfd_peer[fd] - 1);
                g_eventfd_count[eslot] = 0;
                g_eventfd_refs[eslot] = 0;
            }
            g_eventfd_peer[fd] = 0;
            g_eventfd_cslot[fd] = 0;
            g_eventfd_sema[fd] = 0;
        }
        int timer_slot = -1;
        int timer_last = 1;
        if (g_timerfd[fd]) {
            timer_slot = timerfd_slot(fd);
            if (timer_slot >= 0 && timer_slot < HL_NFD) {
                timer_last = --g_tfd_refs[timer_slot] <= 0;
                if (timer_last) {
                    g_tfd_deadline[timer_slot] = 0;
                    g_tfd_interval[timer_slot] = 0;
                    g_tfd_first_oneshot[timer_slot] = 0;
                    g_tfd_pending[timer_slot] = 0;
                    g_tfd_refs[timer_slot] = 0;
                }
            }
        }
        g_timerfd[fd] = 0;
        if (fd != timer_slot || timer_last) {
            g_tfd_deadline[fd] = 0;
            g_tfd_interval[fd] = 0;
            g_tfd_first_oneshot[fd] = 0;
            g_tfd_pending[fd] = 0;
        }
        g_tfd_clock[fd] = 0;
        g_tfd_cslot[fd] = 0;
        g_tfd_object[fd] = 0;
        g_tfd_nb[fd] = 0;
        g_tfd_shared[fd] = NULL;
        g_memfd_is[fd] = 0;
        g_memfd_seal[fd] = 0;
        g_proc_text_desc[fd][0] = 0;
        g_fdpath[fd][0] = 0;
        g_fdpath_guest[fd] = 0;
        g_proc_text_ro[fd] = 0;
        g_pagemap_fd[fd] = 0;
        g_pipesz[fd] = 0;     // drop this fd's emulated F_SETPIPE_SZ so a reused number reports the default
        g_fd_cport[fd] = 0;   // drop the captured container port so getpeername on a reused fd isn't misrouted
        inotify_fd_reset(fd); // instance/watch teardown -- g_inotify[fd] used to stay stamped (stale routing)
        // signalfd: this fd was one alias of a signalfd OFD. Drop it and, when it was the LAST alias, tear the
        // OFD down (close the engine-private write end; the read end is closed by the caller's close(2)).
        if (g_sigfd_slot[fd]) {
            int sslot = g_sigfd_slot[fd] - 1;
            g_sigfd_slot[fd] = 0;
            if (--g_sfd[sslot].refs <= 0) {
                if (g_sfd[sslot].wr >= 0) close(g_sfd[sslot].wr);
                g_sfd[sslot].wr = g_sfd[sslot].rd = -1;
                g_sfd[sslot].mask = 0;
                g_sfd[sslot].refs = 0;
            }
        }
        if (g_dn_mask[fd]) dnotify_apply(fd, 0, 0); // remove this fd's dnotify (F_NOTIFY) watch before it closes
        g_lease[fd] = 0; // release the F_SETLEASE lease this fd held (POSIX: lease dropped on close)
        g_fsig[fd] = 0;  // drop the fd's F_SETSIG signal so a reused number reports the SIGIO default
        if (g_fd_pushback[fd]) {
            free(g_fd_pushback[fd]);
            g_fd_pushback[fd] = NULL;
            g_fd_pb_len[fd] = 0;
        }
        // g_ovldir/g_opath are now [HL_NFD] like every other table here, so the enclosing `fd < HL_NFD`
        // guard covers them and the historical `fd < 1024` clamp is gone. That clamp was the #215 fix
        // ("beam.smp fork+exec control-flow corruption"): close_range(first, ~0U) — glibc's fd sanitize, which
        // erl_child_setup runs before every port fork — is clamped to fd 65535 and calls fd_reset_emul() for
        // EVERY fd in that range, so an unguarded store past the old [1024] bound went wild into BSS. It fixed
        // the close path only; the OPEN paths below still guarded with `< HL_NFD` against the same [1024]
        // arrays. Resizing the arrays is what makes every guard in the file agree.
        g_ovldir[fd][0] = 0;
        g_opath[fd] = 0;
        g_devfull[fd] = 0;
        g_devseed[fd] = 0;
        g_devtty[fd] = 0;
        unix_bind_clear(fd);
        g_unix_peer[fd][0] = 0;
        g_lo_port[fd] = 0;
        g_lo_v6only[fd] = 0;
        g_sock_stream[fd] = 0;
        tcp_shadow_clear(fd);
        ipopt_shadow_clear(fd);
        g_sock_conn[fd] = 0;
        g_sock_connecting[fd] = 0;
        g_sock_host_backed[fd] = 0;
        g_sock_native_peer[fd] = 0;
        g_sock_fam[fd] = 0;
        g_sock_dgram[fd] = 0;
        udp_ref_drop(fd);
        g_udp_local_port[fd] = g_udp_peer_port[fd] = 0;
        g_udp_local_ip[fd] = g_udp_peer_ip[fd] = 0;
        g_udp_local_interface[fd] = g_udp_peer_interface[fd] = 0;
        g_udp_local_v6[fd] = g_udp_peer_v6[fd] = 0;
        seq_ref_drop(fd);
        g_sock_seqpacket[fd] = 0;
        g_sock_pair_peer[fd] = 0;
        g_sock_object[fd] = 0;
        g_sock_peer_object[fd] = 0;
        g_sock_peer_pid[fd] = 0;
        g_sock_passcred[fd] = 0;
        g_br_port[fd] = 0;
        g_br_ip[fd] = 0;
        g_br_interface[fd] = 0;
        g_tcp_lport[fd] = 0; // drop a reused fd's stale listener so /proc/net/tcp doesn't show a ghost
        g_tcp_listen[fd] = 0;
        g_sock_backlog[fd] = 0;
        if (g_dns_sock[fd] || g_icmp_sock[fd]) { // synthetic network socket: close the engine-held peer
            if (g_dns_peer[fd] >= 0) close(g_dns_peer[fd]);
            g_dns_peer[fd] = -1;
            g_dns_sock[fd] = 0;
            g_icmp_sock[fd] = 0;
        }
        g_icmp_kind[fd] = 0;
        g_icmp_ip[fd] = 0;
        nl_close(fd); // tear down a netlink socket's socketpair peer
        // (eventfd counter/cslot/sema teardown is handled refcounted in the g_eventfd_peer block above so a
        // surviving dup keeps the shared counter; do NOT unconditionally zero the shared slot here.)
        ep_close_rehome(fd); // if this watched fd's OFD survives via a dup, re-home its epoll knote (before reset)
        ep_fd_reset(fd);
        flock_on_close(fd);
        poslk_on_close(fd); // POSIX drops all this process's fcntl record locks when any fd closes
        ptm_clear(fd);      // drop this fd's cached pty-master termios/winsize (see ptm cache below)
#if !defined(__linux__)
        pts_master_retain_input(fd); // last slave: rescue the master's queued input before macOS drops it
#endif
        pts_on_close(fd); // free a master's devpts index (+ /dev/pts/N node) / clear a slave's stamp
    }
    pidfd_forget(fd);
    memf_close(fd);
    dirs_drop(fd);
    ovldents_drop(fd);
    hl_fdcache_fd_clear(fd);
    // The host-handle binding is one more table keyed on this number, and it is the
    // one whose staleness is silent: a handle left filed under a reused descriptor
    // sends the next read or write to the previous object rather than failing. It
    // sheds here with the rest, on close, on dup2's implicit close of the target,
    // on the execve CLOEXEC sweep and on a reopen-by-number. No-op where nothing
    // published, which is every host whose descriptors already name their objects.
    (void)hl_fdhandle_release(fd);
}

// Linux *at dirfd precondition, shared by the fstatat/statx/link/symlink/rename/unlink/... family.
// For a RELATIVE path with dirfd != AT_FDCWD the kernel resolves the descriptor FIRST: EBADF if it is not an
// open fd, ENOTDIR if it is open but not a directory. hl folds the dirfd into an absolute host path via
// g_fdpath, which silently accepts a bad/regular-file dirfd -- so those errnos were never produced (fstatat
// on a non-dir dirfd wrongly "succeeded", symlinkat/linkat leaked macOS EOPNOTSUPP, statx returned EBADF for
// a non-dir dirfd). hl shares the host descriptor table, so validate against the real fd. Returns 0 (ok) or
// -errno. `raw` is the stable host-local copy imported at the svc_fs boundary; validating it again as a
// guest address is both redundant and wrong because a host pthread stack may overlap guest VM bookkeeping
// ranges. Absolute paths, AT_FDCWD, and the empty path (AT_EMPTY_PATH / the ENOENT case) never consult the
// dirfd. (LTP fstatat01 / statx03 / symlinkat01 / linkat01.)
static int at_dirfd_check(int dirfd, const char *raw) {
    if (!raw || !raw[0] || raw[0] == '/') return 0; // empty or absolute: the dirfd is not walked
    if (dirfd == -100 /*AT_FDCWD*/) return 0;       // cwd-relative
    struct stat ds;
    if (fstat(dirfd, &ds) < 0) return -EBADF;  // not an open descriptor
    if (!S_ISDIR(ds.st_mode)) return -ENOTDIR; // open, but not a directory
    return 0;
}

// ---- guest xattr passthrough (overlay G5) -----------------------------------------------------------
// Real overlayfs exposes a file's xattrs (file caps, SELinux labels, user.* attrs) and copies them up on
// write; hl used to stub set->ignore / get->ENODATA / list->empty, silently dropping them (a correctness
// trap -- setcap "succeeded" but getcap saw nothing). We namespace guest xattrs under `user.hl.guest.` on
// the host backing inode so they round-trip AND survive copy-up, without colliding with the engine's
// `user.hl.owner.*` attrs or host/macOS attrs. The macOS errno is mapped
// to Linux at the dispatch boundary (ENOATTR->ENODATA).
#define HL_GUEST_XATTR_PREFIX "user.hl.guest."

// Host backing path for a path-based xattr op. forwrite copies a lower-only file up first (attr lands on
// the writable upper). Returns 0 (host filled) or -errno.
static int xattr_hostpath(const char *path, int nofollow, int forwrite, char *host, size_t hn) {
    if (!g_rootfs) {
        const char *resolved = nofollow ? xlate(path, host, hn) : xresolve(path, host, hn);
        if (resolved != host) snprintf(host, hn, "%s", resolved ? resolved : "");
        return 0;
    }
    char gp[4200];
    abs_guest(-100 /*AT_FDCWD*/, path, gp, sizeof gp);
    if (g_nlower) {
        if (forwrite) {
            overlay_copyup(gp, host, hn);
            return 0;
        }
        return overlay_resolve(gp, host, hn, nofollow) ? 0 : -ENOENT;
    }
    secure_resolve(gp, host, hn, nofollow);
    return 0;
}

// setxattr with Linux XATTR_CREATE/XATTR_REPLACE semantics. Linux rejects unknown flag bits and the
// mutually-exclusive CREATE|REPLACE combination with EINVAL. Resolve each valid precondition ourselves
// against a host existence probe and hand macOS a plain set (flags=0).
static long guest_xattr_set(const char *host, const char *name, const void *val, size_t sz, uint64_t lflags,
                            int nofollow) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", HL_GUEST_XATTR_PREFIX, name ? name : "");
    int opt = nofollow ? XATTR_NOFOLLOW : 0;
    if ((lflags & ~UINT64_C(3)) != 0 || (lflags & 3) == 3) return -EINVAL;
    if (lflags & 3) { // XATTR_CREATE(1) | XATTR_REPLACE(2)
        int exists = hl_native_getxattr(host, hn, NULL, 0, 0, opt) >= 0;
        if ((lflags & 1) && exists) return -EEXIST;   // XATTR_CREATE on an existing attr
        if ((lflags & 2) && !exists) return -ENOATTR; // XATTR_REPLACE on a missing attr -> ENODATA (m2l)
    }
    if (sz > 65536) return -E2BIG;
    void *local = sz ? malloc(sz) : NULL;
    if (sz && !local) return -ENOMEM;
    if (sz && guest_copy_from(local, (uint64_t)(uintptr_t)val, sz) != (ssize_t)sz) {
        free(local);
        return -EFAULT;
    }
    long result = hl_native_setxattr(host, hn, local, sz, 0, opt) < 0 ? -errno : 0;
    free(local);
    return result;
}

static long guest_xattr_get(const char *host, const char *name, void *val, size_t sz, int opt) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", HL_GUEST_XATTR_PREFIX, name ? name : "");
    /*
     * Linux getxattr(..., NULL, 0) returns the value length.  Darwin accepts
     * that shape but reports zero, so perform a bounded host read for the
     * guest's size probe. Guest xattrs are capped at 64 KiB by set above.
     */
    if (sz == 0) {
        unsigned char probe[65536];
        ssize_t length = hl_native_getxattr(host, hn, probe, sizeof probe, 0, opt);
        return length < 0 ? -errno : length;
    }
    void *local = sz ? malloc(sz) : NULL;
    if (sz && !local) return -ENOMEM;
    ssize_t r = hl_native_getxattr(host, hn, local, sz, 0, opt);
    int saved_error = errno;
    if (r > 0 && guest_copy_to((uint64_t)(uintptr_t)val, local, (size_t)r) != r) {
        free(local);
        return -EFAULT;
    }
    free(local);
    return r < 0 ? -saved_error : r;
}

static long guest_xattr_remove(const char *host, const char *name, int opt) {
    char hn[512];
    snprintf(hn, sizeof hn, "%s%s", HL_GUEST_XATTR_PREFIX, name ? name : "");
    return hl_native_removexattr(host, hn, opt) < 0 ? -errno : 0;
}

// List only guest-visible attrs, prefix stripped, into the guest buffer. sz==0 returns the required size.
static long guest_xattr_list(const char *host, char *out, size_t sz, int opt) {
    char raw[65536];
    char cooked[65536];
    ssize_t n = hl_native_listxattr(host, raw, sizeof raw, opt);
    if (n < 0) return -errno;
    size_t need = 0, pl = strlen(HL_GUEST_XATTR_PREFIX);
    // First pass: size only. The kernel compares the required length to `size` BEFORE any copy_to_user,
    // and faults the WHOLE destination atomically (nothing is copied on EFAULT), so mirror that order:
    // ERANGE before EFAULT, and validate the entire output range up front instead of copying into a
    // straddling/unmapped guest buffer (which memcpy would fault the engine on -- a guest-crashes-engine break).
    for (ssize_t i = 0; i < n;) {
        const char *nm = raw + i;
        size_t l = strlen(nm);
        i += l + 1;
        if (l > pl && !strncmp(nm, HL_GUEST_XATTR_PREFIX, pl)) need += strlen(nm + pl) + 1;
    }
    if (sz == 0) return (long)need;
    if (need > sz) return -ERANGE;
    if (need && guest_accessible_prefix((uint64_t)(uintptr_t)out, need, HL_LOGICAL_VMA_WRITE) != need) return -EFAULT;
    size_t off = 0;
    for (ssize_t i = 0; i < n;) {
        const char *nm = raw + i;
        size_t l = strlen(nm);
        i += l + 1;
        if (l > pl && !strncmp(nm, HL_GUEST_XATTR_PREFIX, pl)) {
            const char *g = nm + pl;
            size_t gl = strlen(g) + 1;
            memcpy(cooked + off, g, gl);
            off += gl;
        }
    }
    if (need && guest_copy_to((uint64_t)(uintptr_t)out, cooked, need) != (ssize_t)need) return -EFAULT;
    return (long)need;
}

// mount(2). The historical stub returned 0 unconditionally, so a container entrypoint's `mount --bind`,
// `mount -t tmpfs`, and `mount -o remount,ro` silently did NOTHING -- wrong dir content and, worse, an
// UNENFORCED read-only mount (a silent correctness/security hole). Implement the cases an entrypoint
// actually issues against hl's vfs: bind = a bind-vol alias to the source's host backing; tmpfs/ramfs = a
// fresh empty host scratch dir; remount,ro = enforce RO (whole-rootfs g_rootfs_ro / a bind-vol's ro flag /
// a per-subtree path-based RO list). Pseudo-filesystems hl already synthesizes (proc/sysfs/cgroup/devpts/
// mqueue/...) are a genuine no-op success (they ARE present at their mount point). Anything hl cannot
// materialize returns the honest Linux errno instead of a fake 0. MS_RDONLY=1 REMOUNT=0x20 BIND=0x1000.
static int64_t svc_mount(struct cpu *c, uint64_t a_src, uint64_t a_tgt, uint64_t a_fstype, uint64_t a_flags) {
    (void)c;
    if (!g_rootfs) return 0; // bare (no-jail) mode: nothing to alias into -> keep the legacy success
    char source_text[4200], target_text[4200], filesystem_text[64];
    const char *src = NULL, *tgtraw = NULL, *fstype = NULL;
    if (guest_copy_string(target_text, sizeof target_text, a_tgt) < 0) return -EFAULT;
    tgtraw = target_text;
    if (a_src) {
        int imported = guest_copy_string(source_text, sizeof source_text, a_src);
        if (imported < 0) return imported;
        src = source_text;
    }
    if (a_fstype) {
        int imported = guest_copy_string(filesystem_text, sizeof filesystem_text, a_fstype);
        if (imported < 0) return imported;
        fstype = filesystem_text;
    }
    unsigned long fl = (unsigned long)a_flags;
    if (!tgtraw || guest_bad_ptr((uintptr_t)tgtraw, 1)) return -EFAULT;
    char tgt[4200];
    guest_abspath_at(-100, tgtraw, tgt, sizeof tgt); // guest-absolute, lexically normalized
    if (tgt[0] != '/') return -EINVAL;

    // MS_REMOUNT: change an existing mount's flags. Enforce read-only; ignore other churn.
    if (fl & 0x20) {
        int vi = jail_match(tgt);
        if (fl & 0x1) { // remount,ro
            if (!strcmp(tgt, "/")) {
                g_rootfs_ro = 1;
                return 0;
            }
            if (vi >= 0) {
                g_vols[vi].ro = 1;
                return 0;
            }
            return hl_readonly_table_add(&g_ro_subpaths, tgt) == 0 ? 0 : -ENOMEM;
        }
        // remount,rw (relax where cleanly possible; a path-based RO subtree can't be un-listed race-free).
        if (!strcmp(tgt, "/"))
            g_rootfs_ro = 0;
        else if (vi >= 0)
            g_vols[vi].ro = 0;
        return 0;
    }

    // MS_BIND: alias the target subtree to the source path's host backing.
    if (fl & 0x1000) {
        if (!src || guest_bad_ptr((uintptr_t)src, 1)) return -EFAULT;
        char sabs[4200], shost[4200];
        guest_abspath_at(-100, src, sabs, sizeof sabs);
        if (sabs[0] != '/') return -EINVAL;
        if (!secure_resolve(sabs, shost, sizeof shost, 0)) return -EACCES; // escaped the jail
        struct stat st;
        if (stat(shost, &st) != 0) return -ENOENT; // Linux: bind of a missing source -> ENOENT
        return rt_add_vol(tgt, shost, (fl & 0x1) ? 1 : 0);
    }

    // A named filesystem type.
    if (fstype && guest_bad_ptr((uintptr_t)fstype, 1)) return -EFAULT;
    char ft[64];
    ft[0] = 0;
    if (fstype)
        for (size_t k = 0; k < sizeof ft - 1 && fstype[k]; k++) {
            ft[k] = fstype[k];
            ft[k + 1] = 0;
        }
    // Pseudo-filesystems hl already serves at their canonical mount points -> a real no-op success.
    if (!strcmp(ft, "proc") || !strcmp(ft, "sysfs") || !strcmp(ft, "cgroup") || !strcmp(ft, "cgroup2") ||
        !strcmp(ft, "devpts") || !strcmp(ft, "mqueue") || !strcmp(ft, "devtmpfs") || !strcmp(ft, "debugfs") ||
        !strcmp(ft, "securityfs") || !strcmp(ft, "tracefs") || !strcmp(ft, "configfs") || !strcmp(ft, "bpf") ||
        !strcmp(ft, "fusectl") || !strcmp(ft, "pstore") || !strcmp(ft, "sysctl"))
        return 0;
    // tmpfs / ramfs: back the mount point with a fresh, empty host scratch dir.
    if (!strcmp(ft, "tmpfs") || !strcmp(ft, "ramfs")) {
        char tmpl[] = "/tmp/.hl-tmpfsXXXXXX";
        if (!mkdtemp(tmpl)) return -errno;
        int64_t r = rt_add_vol(tgt, tmpl, (fl & 0x1) ? 1 : 0);
        if (r < 0) rmdir(tmpl);
        return r;
    }
    if (ft[0] == 0) return -EINVAL; // mount without a type, bind, or remount is invalid
    // A real block/overlay/nfs/... filesystem hl cannot materialize -> the honest errno, NOT a fake 0 that
    // would leave the mount point showing the wrong (still-unmounted) content.
    return -ENODEV;
}

static const char *fs_operation_name(uint64_t nr) {
    switch (nr) {
    case 5: return "setxattr";
    case 6: return "lsetxattr";
    case 7: return "fsetxattr";
    case 8: return "getxattr";
    case 9: return "lgetxattr";
    case 10: return "fgetxattr";
    case 11: return "listxattr";
    case 12: return "llistxattr";
    case 13: return "flistxattr";
    case 14: return "removexattr";
    case 15: return "lremovexattr";
    case 16: return "fremovexattr";
    case 17: return "getcwd";
    case 29: return "ioctl";
    case 33: return "mknodat";
    case 34: return "mkdirat";
    case 35: return "unlinkat";
    case 36: return "symlinkat";
    case 37: return "linkat";
    case 38:
    case 276: return "renameat";
    case 39: return "umount2";
    case 40: return "mount";
    case 41: return "pivot_root";
    case 43:
    case 44: return "statfs";
    case 46: return "ftruncate";
    case 47: return "fallocate";
    case 48:
    case 439: return "faccessat";
    case 49: return "chdir";
    case 50: return "fchdir";
    case 52: return "fchmod";
    case 53:
    case 452: return "fchmodat";
    case 54: return "fchownat";
    case 55: return "fchown";
    case 56: return "openat";
    case 57: return "close";
    case 61: return "getdents64";
    case 78: return "readlinkat";
    case 79: return "newfstatat";
    case 80: return "fstat";
    case 81:
    case 267: return "sync";
    case 88: return "utimensat";
    case 166: return "umask";
    case 223: return "fadvise64";
    case 264: return "name_to_handle_at";
    case 291: return "statx";
    case 437: return "openat2";
    default: return NULL;
    }
}

// Follow a synthetic procfd link to its open-file description. Typed handles are authoritative and may
// have no native descriptor at the guest number; native fstat keeps legacy descriptors working.
static int bound_source_is_native(void);

// Synthetic procfd links are resolved from the guest descriptor table, not by walking the rootfs
// symlink. Identify both absolute and dirfd-relative spellings before atpath() follows the final link;
// otherwise /dev/fd -> /proc/self/fd can be mistaken for an on-disk symlink loop before the descriptor
// handler gets a chance to supply Linux's magic-link semantics.
static int procfd_num_at(int dirfd, const char *path) {
    if (!path) return -1;
    int fd = procfd_num(path);
    if (fd >= 0) return fd;
    char guest[4200];
    guest_abspath_at(dirfd, path, guest, sizeof guest);
    return procfd_num(guest);
}

static int procfd_follow_stat(const char *path, struct stat *status) {
    int fd = procfd_num(path);
    if (fd < 0) return 0;
    hl_linux_file_status typed;
    if (g_linux_box != NULL && hl_linux_fstat(g_linux_box, (hl_linux_fd)fd, &typed) == 0) {
        memset(status, 0, sizeof *status);
        status->st_dev = (dev_t)typed.device;
        status->st_ino = (ino_t)typed.object;
        status->st_mode = (mode_t)typed.mode;
        status->st_nlink = (nlink_t)typed.link_count;
        status->st_uid = (uid_t)typed.user;
        status->st_gid = (gid_t)typed.group;
        status->st_rdev = (dev_t)typed.special_device;
        status->st_size = (off_t)typed.size;
        HL_HOST_STAT_SET_BLOCKS(status, typed.blocks_512);
        HL_HOST_STAT_SET_BLKSIZE(status, 4096);
        HL_HOST_STAT_SET_ATIME(status, typed.accessed_ns / UINT64_C(1000000000),
                               typed.accessed_ns % UINT64_C(1000000000));
        HL_HOST_STAT_SET_MTIME(status, typed.modified_ns / UINT64_C(1000000000),
                               typed.modified_ns % UINT64_C(1000000000));
        HL_HOST_STAT_SET_CTIME(status, typed.changed_ns / UINT64_C(1000000000),
                               typed.changed_ns % UINT64_C(1000000000));
        return 1;
    }
    return fstat(fd, status) == 0 ? 1 : -1;
}

#include "fs/attributes.c"
#include "fs/control.c"
#include "fs/namespace.c"
#include "fs/mounts.c"
#include "fs/allocation.c"
#include "fs/access.c"
#include "fs/directory.c"
#include "fs/metadata.c"
#include "fs/extended_status.c"

static int svc_fs(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                  uint64_t a5) {
    /*
     * Path walkers expect ordinary host strings.  Import pathname operands first so a sparse logical VMA
     * works exactly like an identity mapping and an unreadable string returns EFAULT instead of crashing
     * inside strlen/atpath.  Keep this at the family boundary so every overlay/jail branch sees the same
     * stable spelling.
     */
    char imported_path0[4200], imported_path1[4200];
    uint64_t *path_arg0 = NULL, *path_arg1 = NULL;
    switch (nr) {
    case 5:
    case 6:
    case 8:
    case 9:
    case 14:
    case 15:
        path_arg0 = &a0;
        path_arg1 = &a1; /* xattr name */
        break;
    case 7:
    case 10:
    case 16: path_arg0 = &a1; break; /* fd form: xattr name only */
    case 11:
    case 12:
    case 39:
    case 43:
    case 49: path_arg0 = &a0; break;
    case 33:
    case 34:
    case 35:
    case 53:
    case 54:
    case 56:
    case 78:
    case 79:
    case 264:
    case 291:
    case 437:
    case 439:
    case 452:
    case 48: path_arg0 = &a1; break;
    case 88:
        /* utimensat(fd, NULL, times, 0) is futimens and has no pathname to import. */
        if (a1) path_arg0 = &a1;
        break;
    case 36:
        path_arg0 = &a0; /* symlink target */
        path_arg1 = &a2; /* new link pathname */
        break;
    case 37:
    case 38:
    case 276:
        path_arg0 = &a1;
        path_arg1 = &a3;
        break;
    case 41:
        path_arg0 = &a0;
        path_arg1 = &a1;
        break;
    default: break;
    }
    int path_import_status;
    if (path_arg0 && (path_import_status = guest_copy_string(imported_path0, sizeof imported_path0, *path_arg0)) < 0) {
        G_RET(c) = (uint64_t)(int64_t)path_import_status;
        return svc_done_host(c);
    }
    if (path_arg1 && (path_import_status = guest_copy_string(imported_path1, sizeof imported_path1, *path_arg1)) < 0) {
        G_RET(c) = (uint64_t)(int64_t)path_import_status;
        return svc_done_host(c);
    }
    if (path_arg0) *path_arg0 = (uint64_t)(uintptr_t)imported_path0;
    if (path_arg1) *path_arg1 = (uint64_t)(uintptr_t)imported_path1;

    const char *operation = fs_operation_name(nr);
    if (operation != NULL)
        HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "%s nr=%llu a0=%#llx a1=%#llx a2=%#llx", operation, (unsigned long long)nr,
                (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)a2);
    if (!svc_fs_attributes(c, nr, a0, a1, a2, a3, a4, a5) && !svc_fs_control(c, nr, a0, a1, a2, a3, a4, a5) &&
        !svc_fs_namespace(c, nr, a0, a1, a2, a3, a4, a5) && !svc_fs_mounts(c, nr, a0, a1, a2, a3, a4, a5) &&
        !svc_fs_allocation(c, nr, a0, a1, a2, a3, a4, a5) && !svc_fs_access(c, nr, a0, a1, a2, a3, a4, a5) &&
        !svc_fs_directory(c, nr, a0, a1, a2, a3, a4, a5) && !svc_fs_metadata(c, nr, a0, a1, a2, a3, a4, a5) &&
        !svc_fs_extended_status(c, nr, a0, a1, a2, a3, a4, a5))
        return 0;
    if ((nr == 56 || nr == 437) && (int64_t)G_RET(c) >= 0 && G_RET(c) < HL_NFD && a1 != 0) {
        const char *opened_path = (const char *)a1;
        if (!strcmp(opened_path, "/proc") || !strncmp(opened_path, "/proc/", 6) || !strcmp(opened_path, "/dev/fd"))
            snprintf(g_fdpath[(int)G_RET(c)], sizeof g_fdpath[(int)G_RET(c)], "%s", opened_path);
    }
    int handled = svc_done_host(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done_host
    if (nr == 56 || nr == 437)
        HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "%s path=%s flags=%#llx result=%lld", operation != NULL ? operation : "open",
                (const char *)a1, (unsigned long long)a2, (long long)(int64_t)G_RET(c));
    HL_LOGF(&g_jit_log, HL_LOG_TAG_FS, "%s result=%lld", operation != NULL ? operation : "fs",
            (long long)(int64_t)G_RET(c));
    return handled;
}
