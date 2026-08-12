// hl/linux_abi/container -- the container VFS: TOCTOU-free path jail, overlay image layers
// (lower/upper + copy-up + whiteout + merged readdir), and /proc + /sys synthesis.

#include "../open_plan.h"
#include "../page.h" // hl_linux_host_page_size
#include "../shared.h"
#include "../../host/libc_compat.h" // hl_compat_mkdir: the UCRT's mkdir takes no mode
#include "../../host/file.h"
#include "../../host/resolve.h"
#include "../../core/provider/files.h"
#include "../../core/provider/namespace.h"
#include "../../core/provider/tree_files.h"
#if defined(__linux__)
#include <sys/prctl.h>     // host PR_SET_NAME: mirror the guest comm onto this host task so a PEER's
                           // /proc/<pid>/{stat,status,comm} read (hl_host_process_read) reports the guest
                           // program name, not the engine binary "hl-engine-linux".
#include <sys/sysmacros.h> // glibc keeps major()/minor() here, not in <sys/types.h>: the dev field of a
                           // file-backed /proc/<pid>/maps row.
#endif

// Set when a followed path resolution exceeds the symlink-traversal limit (Linux caps at 40 -> ELOOP). The
// jail resolvers return a host-path string with no errno channel, so a self-referential / cyclic symlink
// would otherwise degrade into a host stat of a mis-followed path (ENOENT) instead of ELOOP. atpath() clears
// this at entry; the path syscalls consult resolve_loop_detected() after resolving and surface -ELOOP.
static _Thread_local int g_symloop_hit;

static void resolve_loop_mark(void) {
    g_symloop_hit = 1;
}

static void resolve_loop_clear(void) {
    g_symloop_hit = 0;
}

static int resolve_loop_detected(void) {
    return g_symloop_hit;
}

static int path_copy(char *out, size_t capacity, const char *value) {
    size_t length;
    if (!out || capacity == 0 || !value) {
        errno = EINVAL;
        return -1;
    }
    length = strlen(value);
    if (length >= capacity) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(out, value, length + 1);
    return 0;
}

static int path_concat(char *out, size_t capacity, const char *first, const char *second) {
    size_t a = first ? strlen(first) : 0, b = second ? strlen(second) : 0;
    if (!out || capacity == 0 || !first || !second) {
        errno = EINVAL;
        return -1;
    }
    if (a >= capacity || b >= capacity - a) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(out, first, a);
    memcpy(out + a, second, b + 1);
    return 0;
}

static int path_join(char *out, size_t capacity, const char *directory, const char *leaf) {
    size_t d = directory ? strlen(directory) : 0, l = leaf ? strlen(leaf) : 0;
    int slash = d != 0 && directory[d - 1] != '/';
    if (!out || capacity == 0 || !directory || !leaf) {
        errno = EINVAL;
        return -1;
    }
    if (d >= capacity || (size_t)slash >= capacity - d || l >= capacity - d - (size_t)slash) {
        out[0] = 0;
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(out, directory, d);
    if (slash) out[d++] = '/';
    memcpy(out + d, leaf, l + 1);
    return 0;
}

static int symlink_idempotent(const char *target, const char *path) {
    if (symlink(target, path) == 0 || errno == EEXIST) return 0;
    return -1;
}

// ---- rootfs path rewriting (ported from mac_elf.c) ----
static const char *g_rootfs = NULL;

// Linux CLONE_FS shares cwd and root between processes. Keep the ordinary
// process-local context inline, then promote it to MAP_SHARED only when a
// caller requests that contract. A later fork without CLONE_FS detaches in
// the child, preserving normal copy-on-write filesystem state.
struct guest_fs_context {
    char cwd[4200];
    char root[4200];
};
static struct guest_fs_context g_fs_local = {.cwd = "/"};
static struct guest_fs_context *g_fs = &g_fs_local;
#define g_cwd (g_fs->cwd)
#define g_chroot (g_fs->root)

static int guest_fs_share(void) {
    if (g_fs != &g_fs_local) return 0;
    struct guest_fs_context *shared = mmap(NULL, sizeof *shared, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (shared == MAP_FAILED) return -errno;
    memcpy(shared, &g_fs_local, sizeof *shared);
    g_fs = shared;
    return 0;
}

static void guest_fs_after_fork(int shared) {
    if (shared || g_fs == &g_fs_local) return;
    memcpy(&g_fs_local, g_fs, sizeof g_fs_local);
    g_fs = &g_fs_local;
}

static uint8_t g_auxv_data[1024];
// serialized auxv for /proc/self/auxv
static int g_auxv_len;
// Guest main-thread stack bounds, published by build_stack. Used to synthesize a [stack] line in
// /proc/self/maps so glibc's pthread_getattr_np(pthread_self()) finds the main stack (it scans the
// maps for the line containing %rsp). Without it that call returns ENOENT, which derails Rust std's
// startup (stack-overflow guard init) and cascades into wrong behavior later. 0 => not published yet.
uint64_t g_stack_lo, g_stack_hi;

// Sandbox: normalize a guest absolute path -- drop '.', collapse '//', and clamp '..' at the
// ROOT so a translated path can never escape the rootfs ("/../../etc" -> "/etc"). Without this,
// the guest reads host files by traversing above $rootfs. Result always starts with '/'.
static void confine(const char *p, char *out, size_t n) {
    const char *comp[512];
    int clen[512], nc = 0;
    for (const char *s = p; *s;) {
        while (*s == '/')
            s++;
        if (!*s) break;
        const char *st = s;
        while (*s && *s != '/')
            s++;
        int L = (int)(s - st);
        // "."  -> skip
        if (L == 1 && st[0] == '.') continue;
        if (L == 2 && st[0] == '.' && st[1] == '.') {
            if (nc > 0) nc--;
            continue;
            // ".." -> pop, never past root
        }
        if (nc < 512) {
            comp[nc] = st;
            clen[nc] = L;
            nc++;
        }
    }
    size_t o = 0;
    for (int i = 0; i < nc; i++) {
        if (o + 1 < n) out[o++] = '/';
        for (int j = 0; j < clen[i] && o + 1 < n; j++)
            out[o++] = comp[i][j];
    }
    // empty -> "/"
    if (o == 0 && n > 1) out[o++] = '/';
    out[o < n ? o : n - 1] = 0;
}

// Guest chroot(2) prefix: a rootfs-relative guest path ("" = none). chroot(2) re-roots the guest WITHIN
// the existing rootfs jail -- its target is resolved through the jail first (so it can never name a host
// path) and recorded here; every absolute guest path is then walked under this prefix yet STILL confined
// to g_root_fd, so a guest can never reach the host fs (a `..` still clamps at the rootfs root). Inherited
// across fork and preserved across execve, exactly as on Linux.
// Re-root an absolute guest path under the active chroot: clamp its `..` (after chroot the guest's own
// root IS the chroot dir) and prepend the prefix. The result is still a rootfs-absolute guest path, which
// the resolvers below confine to g_root_fd as usual. Callers invoke this only while a chroot is active.
static void chroot_apply(const char *guest, char *out, size_t n) {
    char norm[4200];
    confine(guest ? guest : "/", norm, sizeof norm);
    int rc;
    if (!g_chroot[0])
        rc = path_copy(out, n, norm);
    else if (norm[1] == 0)
        rc = path_copy(out, n, g_chroot); // the chroot root itself
    else
        rc = path_concat(out, n, g_chroot, norm);
    if (rc != 0 && n) out[0] = 0;
}

// Strip the active chroot prefix from a rootfs-relative guest path, yielding the chroot-relative view the
// guest sees (used to keep g_cwd in the guest's own frame after chdir under a chroot). No-op with no
// chroot, or for a path that lies outside the chroot subtree (clamped to "/" -- the guest cannot be there).
static void chroot_strip(char *guest, size_t n) {
    if (!g_chroot[0] || !guest || guest[0] != '/') return;
    size_t cl = strlen(g_chroot);
    if (strncmp(guest, g_chroot, cl) == 0 && (guest[cl] == '/' || guest[cl] == 0)) {
        char tmp[4200];
        snprintf(tmp, sizeof tmp, "%s", guest[cl] ? guest + cl : "/");
        snprintf(guest, n, "%s", tmp);
    } else {
        snprintf(guest, n, "/");
    }
}

#include "namespace.h"

/*
 * realpath(3) requires a PATH_MAX-sized destination when the caller supplies
 * storage.  Several namespace records deliberately use smaller bounded path
 * fields, so passing those fields directly is undefined and is rejected by
 * fortified libc builds.  Canonicalize into libc-owned storage, then perform
 * an explicit capacity check before publishing the result.
 */
static int canonicalize_path(const char *path, char *destination, size_t capacity) {
    char *canonical;
    size_t size;
    if (path == NULL || destination == NULL || capacity == 0) {
        errno = EINVAL;
        return -1;
    }
    canonical = realpath(path, NULL);
    if (canonical == NULL) return -1;
    size = strlen(canonical) + 1;
    if (size > capacity) {
        free(canonical);
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(destination, canonical, size);
    free(canonical);
    return 0;
}

// Preserve the final symlink inode while canonicalizing its parent directory.
// Namespace projections need the guest to observe and follow the link itself;
// ordinary bind sources continue to bind their canonical target.
static int canonicalize_link_path(const char *path, char *destination, size_t capacity) {
    char copy[4200], parent[4200];
    if (path_copy(copy, sizeof copy, path) != 0) return -1;
    char *slash = strrchr(copy, '/');
    if (slash == NULL || slash[1] == 0) {
        errno = EINVAL;
        return -1;
    }
    const char *name = slash + 1;
    if (slash == copy)
        copy[1] = 0;
    else
        *slash = 0;
    if (canonicalize_path(copy, parent, sizeof parent) != 0) return -1;
    if (path_join(destination, capacity, parent, name) != 0) {
        errno = ENAMETOOLONG;
        return -1;
    }
    struct stat status;
    if (lstat(destination, &status) != 0 || !S_ISLNK(status.st_mode)) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

// realpath(g_rootfs) -- the true rootfs boundary
static char g_rootfs_canon[4200];
static size_t g_rootfs_canon_len;
// fd -> host path it was opened with (dir-fd confinement + cache)
static char g_fdpath[HL_NFD][192];
// overlay: dir-fd -> its GUEST path (for merged getdents); "" = not an overlay dir
//
// Sized [HL_NFD], like every other fd-indexed table in this file. It was [1024] until an audit found the
// two spellings live side by side: the close path clamps to `fd < 1024` (the #215 fix -- close_range() from
// glibc's fd sanitize walks to 65535 and an unguarded store went wild into BSS), while the OPEN paths in
// syscall/fs.c guard with `< HL_NFD` and so wrote past the end for any fd in [1024, HL_NFD). Both spellings
// cannot be right. HL_NFD is the correct one because it is the bound the rest of the fd tables use and the
// bound the guest's descriptor numbers actually observe; clamping the writers to 1024 instead would silently
// stop tagging overlay directories and O_PATH descriptors above 1024, turning a memory bug into a behaviour
// bug. BSS cost matches g_fdpath[HL_NFD][192], which is already this size and demand-zero.
static char g_ovldir[HL_NFD][192];
// O_PATH: fd opened with Linux O_PATH -- it names a file (fstat / *at dirfd / fchdir) but is NOT open for
// I/O, so read/write/pread/pwrite/readv/writev through it must fail EBADF (macOS has no O_PATH; we open a
// normal read fd for the metadata ops and gate the I/O family on this flag). 1 = O_PATH.
static uint8_t g_opath[HL_NFD];
// Synthesized /proc text files are backed by mkstemp(), so the host fd is O_RDWR even though Linux exposes
// procfs regular files as read-only for file-status queries. 1 = force F_GETFL access mode to O_RDONLY.
static uint8_t g_proc_text_ro[HL_NFD];
// Linux exposes oom_score_adj as writable per-process state inherited by fork
// and preserved across exec. Keep the guest value independent of the host's OOM
// policy; allowing a guest to mutate the host process would violate isolation.
static int g_proc_oom_score_adj;
// /dev/full: reads return zeros (backed by /dev/zero) but every WRITE fails ENOSPC. macOS has no
// /dev/full, so we flag the fd here and gate the write family in svc_io. 1 = /dev/full.
static uint8_t g_devfull[HL_NFD];
// /dev/urandom + /dev/random accept WRITEs on Linux as entropy-pool seeding (returning the byte count);
// macOS rejects them with EPERM. 1 = this fd is such a device, so svc_io swallows its writes as a no-op
// success -- entropy-seeding probes (libgcrypt, some init scripts) then behave as on Linux.
static uint8_t g_devseed[HL_NFD];
// /dev/tty (and the console we back with /dev/null): a controlling terminal NEVER reports EOF because it
// has no input -- a nonblocking read with nothing pending returns EAGAIN, and a blocking read waits. But hl
// may back /dev/tty with a host device (or /dev/null for /dev/console) that returns 0 (EOF) when empty, so
// readline/TUI/event-loop code treats "no input" as terminal closure and tears down. 1 = this fd carries
// tty read semantics: a 0-byte (EOF) read on a NONBLOCKING such fd is reported as EAGAIN instead (svc_io).
static uint8_t g_devtty[HL_NFD];
// Guest-visible bound AF_UNIX socket names, for /proc/net/unix enumeration (`ss -x`, socket-inventory
// tools). Recorded on a successful AF_UNIX bind (net.c); a pathname keeps its guest path, an abstract name
// is stored as "@name". Empty slot = not a bound unix socket. Process-local (one net-namespace per engine).
static char g_unix_bind[HL_NFD][108];

static void unix_bind_note(int fd, const char *guestname) {
    if (fd >= 0 && fd < HL_NFD && guestname) snprintf(g_unix_bind[fd], sizeof g_unix_bind[fd], "%s", guestname);
}

static void unix_bind_clear(int fd) {
    if (fd >= 0 && fd < HL_NFD) g_unix_bind[fd][0] = 0;
}

// Guest-visible peer name of a connected AF_UNIX socket, recorded on connect so getpeername can echo the
// guest abstract/pathname address instead of the engine's backing host fs path. Same "@name" convention as
// g_unix_bind. Empty slot = no recorded peer name.
static char g_unix_peer[HL_NFD][108];

static void unix_peer_note(int fd, const char *guestname) {
    if (fd >= 0 && fd < HL_NFD && guestname) snprintf(g_unix_peer[fd], sizeof g_unix_peer[fd], "%s", guestname);
}

// Fill a guest sockaddr_un from a recorded "@name" (abstract) or pathname guest name. Returns the Linux
// addrlen, or -1 if the name slot is empty. Abstract: family + NUL + name (no trailing NUL). Pathname:
// family + path + trailing NUL.
static int unix_name_fill(const char *name, uint8_t *g, socklen_t gcap, socklen_t *glen) {
    if (!name || !name[0]) return -1;
    uint8_t t[2 + 108];
    memset(t, 0, sizeof t);
    *(uint16_t *)t = AF_UNIX;
    int llen;
    if (name[0] == '@') {
        size_t nl = strlen(name + 1);
        if (nl > sizeof t - 3) nl = sizeof t - 3;
        memcpy(t + 3, name + 1, nl); // t[2] stays NUL (abstract), name follows
        llen = (int)(2 + 1 + nl);
    } else {
        size_t nl = strlen(name);
        if (nl > sizeof t - 3) nl = sizeof t - 3;
        memcpy(t + 2, name, nl);
        llen = (int)(2 + nl + 1); // include the trailing NUL
    }
    if (g && gcap) memcpy(g, t, (size_t)gcap < (size_t)llen ? gcap : (size_t)llen);
    if (glen) *glen = (socklen_t)llen;
    return llen;
}

// Overlay merged-getdents snapshot cursor reset (rewinddir/seekdir on an overlay dir). Defined in fs.c
// where g_ovldents lives, but the lseek handler (io.c) is included before fs.c, so forward-declare it.
static void ovldents_rewind(int fd, int pos);
// eventfd(read-end) -> pipe write-end + 1 (0 = not an eventfd)
static int g_eventfd_peer[HL_NFD];
// eventfd accumulating counter: write() adds, read() returns + resets (the pipe is only readiness).
// _xproc-eventfd-lockf_: the counter array lives in a MAP_SHARED anonymous region so a child created by
// hl's real host fork() updates the SAME physical counters the parent reads -- the readiness pipe is
// already fork-shared (inherited fds), but the accumulating count must be too, or the parent reads 0
// while the child's write()s land in its COW-private copy. Created ONCE at startup (constructor, before
// any guest fork) so every forked worker inherits the same physical array. All g_eventfd_count[fd]
// indexing (io.c, the eventfd2 creation in service.c) is unchanged.
static uint64_t *g_eventfd_count;
static uint8_t *g_eventfd_nb_shared;
// eventfd public fd -> counter slot + 1. Normally the slot is the fd number, but an eventfd imported via
// SCM_RIGHTS may land on a different fd number while still needing to update the sender's shared counter.
static int g_eventfd_cslot[HL_NFD];

static void eventfd_count_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_eventfd_count) return;
    // One slot per POSSIBLE fd number: eventfd_counter_slot() indexes this by the fd number (or a
    // SCM_RIGHTS-imported eventfd's sender-fd slot), and large workloads open far more than 1024 fds — a 1024-slot
    // array is a cross-process out-of-bounds write for any eventfd whose fd number exceeds it (silent
    // counter corruption / heap clobber past the mapped page). Size it to the whole fd space.
    size_t sz = sizeof(uint64_t) * HL_NFD + sizeof(uint8_t) * HL_NFD;
    if (hl_linux_shared_create(host, sz, &arena) != HL_STATUS_OK) abort();
    g_eventfd_count = (uint64_t *)arena;
    g_eventfd_nb_shared = (uint8_t *)(g_eventfd_count + HL_NFD);
}

// Guest-requested O_NONBLOCK for an eventfd. The backing pipe's read end is kept PERMANENTLY O_NONBLOCK at
// the host level so hl's internal counter/pipe drains never toggle the fd's flags — an eventfd is shared
// across processes (fork / SCM_RIGHTS) as one open file description, so a transient host O_NONBLOCK flip in
// one process's drain is observed by a concurrent reader in ANOTHER process (g_eventfd_lock is process-
// private and cannot serialize it), which then wrongly takes the nonblocking path and returns a spurious
// EAGAIN on a BLOCKING eventfd used for a cross-process command-buffer wakeup. The
// guest's REAL blocking/non-blocking intent lives here instead; the read path consults it and blocks via
// poll() when the guest asked to block. Propagated on dup + SCM_RIGHTS import alongside the peer/slot.
static uint8_t g_eventfd_gnb[HL_NFD];

static int eventfd_guest_nb(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    int slot = g_eventfd_cslot[fd] > 0 ? g_eventfd_cslot[fd] - 1 : fd;
    return g_eventfd_nb_shared ? g_eventfd_nb_shared[slot] : g_eventfd_gnb[fd];
}

static void eventfd_guest_nb_set(int fd, int nonblock) {
    if (fd < 0 || fd >= HL_NFD) return;
    int slot = g_eventfd_cslot[fd] > 0 ? g_eventfd_cslot[fd] - 1 : fd;
    g_eventfd_gnb[fd] = (uint8_t)(nonblock != 0);
    if (g_eventfd_nb_shared) g_eventfd_nb_shared[slot] = (uint8_t)(nonblock != 0);
}

// _eventfd-atomicity_: an eventfd is emulated as {accumulating counter, readiness pipe}. write() does
// `count += add; drain-pipe; write-one-byte` and read() does `v = count; count = 0; drain-pipe; if
// count>0 re-signal` -- a PAIR of mutations (counter + pipe) that MUST move together. With no lock, two
// threads (work-scheduling writers versus an event-loop reader) interleave and strand the invariant
// "pipe-readable IFF count>0": a byte left in the pipe with count==0 makes a level-triggered epoll_wait
// report the fd endlessly ready while read() returns EAGAIN (the pump busy-spins), and an edge-triggered
// watcher that saw no fresh edge never wakes at all (the "lost wakeup" park). Either way the event-loop
// thread stops making progress. Serialize every counter+pipe mutation for a given eventfd under this lock
// so the pair is atomic; the epoll/kqueue side then only ever observes a consistent pipe state. Process-
// private (in-process multi-threading is the case that matters -- the counter's own MAP_SHARED cross-fork
// sharing stays best-effort, unchanged); re-init in the fork child so an inherited-locked copy can't wedge.
static pthread_mutex_t g_eventfd_lock = PTHREAD_MUTEX_INITIALIZER;

static void eventfd_after_fork(void) {
    pthread_mutex_init(&g_eventfd_lock, NULL);
}

static uint8_t g_eventfd_sema[HL_NFD]; // EFD_SEMAPHORE: read() returns 1 and decrements by 1, not the whole counter
// Alias refcount per counter-slot: a dup() of an eventfd creates a second guest fd that shares the SAME
// eventfd object (peer write end + counter slot). Keyed by eventfd_counter_slot(); the creator sets it to 1
// and each dup increments it. fd_reset_emul only closes the shared peer / zeroes the shared counter when the
// LAST alias closes, so closing one duplicate never tears the object out from under the others. A non-dup'd
// eventfd keeps refs==1, so its close path is byte-identical to before.
static int g_eventfd_refs[HL_NFD];

static int eventfd_counter_slot(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_eventfd_cslot[fd] > 0) return g_eventfd_cslot[fd] - 1;
    return fd;
}

static int eventfd_hidden_peer_fd(int fd) {
    if (fd < 0) return 0;
    for (int i = 0; i < HL_NFD; i++)
        if (g_eventfd_peer[i] == fd + 1) return 1;
    return 0;
}

// /proc/<pid>/pagemap emulation: the file is VA-indexed (8 bytes per page, addressed by lseek to
// vaddr/pagesize*8), so it can't be materialized as static text. We back it with a real empty seekable fd
// (lseek to any offset works natively) and synthesize the 8-byte entries in the read path (io.c). This
// marks which fds are pagemap backings; cleared on close (fd_reset_emul).
static uint8_t g_pagemap_fd[HL_NFD];

// ===================== cross-process guest task-state table =====================
// Linux's /proc/<pid>/stat field 3 is the task run state (R/S/D/T/Z). hl used to synthesize it from the
// macOS process status (proc_bsdinfo.pbi_status): but that BSD p_stat only ever reports SRUN/SSTOP/SZOMB
// for the whole PROCESS -- it has NO way to express "every thread is asleep in a blocking syscall". A
// guest parked in pause()/ppoll()/wait4() therefore showed 'R' (running) where real Linux shows 'S'
// (interruptible sleep). LTP pause01/pause02 poll a CHILD's /proc/<pid>/stat waiting for that 'S' and
// timed out. Since the reader is a DIFFERENT process (parent reads child), the guest's own idea of its
// run state must be PUBLISHED where any peer can see it: a MAP_SHARED table created pre-fork (like the
// eventfd counters / futex buckets above), keyed by HOST pid (== guest pid for every non-init task; init
// maps gp==1 -> g_init_hostpid, which is init's own getpid()). Each guest stamps 'S' before it parks in a
// host blocking wait inside service() and 'R' when it wakes / on every other syscall; the /proc synthesis
// overrides the (coarse) pbi_status with this authoritative value. Inert & O(1): a thread-cached slot
// pointer + one relaxed atomic store per blocking wait; zombie/stopped stay pbi-authoritative (see below).
struct ts_slot {
    _Atomic int pid;          // host pid owning this slot (0 = free)
    _Atomic unsigned char st; // Linux state char: 'R' 'S' 'D' 'T' 'Z'
};

#define TS_N 4096 // power of two; open-addressed by host pid
static struct ts_slot *g_ts_tab;

/* Cross-process view of typed logical descriptors. The host fd table contains reservation shadows at
 * guest-visible numbers, so peer /proc/<pid>/fd resolves the logical fd's stable OFD identity to a persistent
 * peer descriptor before asking the host process service for vnode/socket information. Entries live in
 * pre-fork shared memory and are generation stamped so readers never combine old identity with fd reuse. */
#ifndef FDVIS_N
#define FDVIS_N 131072
#endif

struct fdvis_slot {
    uint64_t key; /* host pid in high 32 bits; guest fd + 1 in low 32 bits; 0 = free */
    uint64_t generation;
    uint64_t owner_start_ns;
    uint32_t kind;
    uint32_t reserved;
    uint64_t device;
    uint64_t object;
};
static struct fdvis_slot *g_fdvis;

struct fdvis_control {
    _Atomic uint64_t owner;
    uint64_t generation;
};
static struct fdvis_control *g_fdvis_control;
static int g_fdvis_fork_parent;
static uint64_t g_fdvis_fork_parent_start;
static uint64_t g_pipe_identity[HL_NFD];
// Guest-visible F_SETPIPE_SZ/F_GETPIPE_SZ state is also needed by early SCM_RIGHTS marshalling.
static int g_pipesz[HL_NFD];
static uint8_t g_fdvis_private[HL_NFD];
static _Atomic uint64_t g_pipe_identity_next = 1;
static void proc_fdvis_cleanup(void);
static void proc_fdvis_close(int guest_fd);
static int proc_fdvis_publish_native_fd(int guest_fd);

struct fdvis_fork_entry {
    unsigned slot;
    int guest_fd;
    uint32_t kind;
    uint64_t device;
    uint64_t object;
};

struct fdvis_fork_plan {
    struct fdvis_fork_entry *entries;
    size_t count;
};

static uint64_t fdvis_key(int pid, int fd) {
    return pid > 0 && fd >= 0 ? ((uint64_t)(uint32_t)pid << 32) | ((uint32_t)fd + 1u) : 0;
}

static uint64_t fdvis_process_token(int pid) {
    hl_host_process_info info;
    return pid > 0 && hl_host_process_read(pid, &info) ? info.start_time_ns : 0;
}

static uint64_t fdvis_identity(int pid, uint64_t start_ns) {
    uint32_t fingerprint = (uint32_t)start_ns ^ (uint32_t)(start_ns >> 32);
    return ((uint64_t)(uint32_t)pid << 32) | fingerprint;
}

static void fdvis_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_fdvis != NULL) return;
    size_t bytes = sizeof(struct fdvis_slot) * FDVIS_N + sizeof(*g_fdvis_control);
    if (hl_linux_shared_create(host, bytes, &arena) != HL_STATUS_OK) return;
    g_fdvis = arena;
    g_fdvis_control = (void *)((unsigned char *)arena + sizeof(struct fdvis_slot) * FDVIS_N);
    (void)atexit(proc_fdvis_cleanup);
    // Enumerate this process's open descriptors ONCE and publish the non-engine-private ones. Each
    // hl_host_process_fds() call is a full /proc/self/fd getdents scan whose kernel cost is O(highest open
    // fd); the engine keeps its internal descriptors at the high private floor (65536+), so a scan is ~1.2ms.
    // The prior count-then-fill idiom paid that scan TWICE. A generous on-stack buffer captures every real
    // descriptor in a single scan (an engine launch has a handful of inherited fds, far below the floor);
    // only a pathological overflow falls back to the exact two-pass path, so behavior is unchanged.
    hl_host_process_fd inline_entries[128];
    size_t count = 0;
    if (hl_host_process_fds(getpid(), inline_entries, sizeof inline_entries / sizeof *inline_entries, &count)) {
        hl_host_process_fd *entries = inline_entries;
        hl_host_process_fd *heap = NULL;
        if (count > sizeof inline_entries / sizeof *inline_entries) {
            // Rare: more open descriptors than the inline buffer. Re-scan into an exact heap buffer.
            heap = calloc(count, sizeof(*heap));
            if (heap && hl_host_process_fds(getpid(), heap, count, &count))
                entries = heap;
            else
                count = sizeof inline_entries / sizeof *inline_entries; // publish what the inline scan captured
        }
        for (size_t index = 0; index < count; ++index)
            if ((entries[index].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) == 0)
                (void)proc_fdvis_publish_native_fd(entries[index].descriptor);
        free(heap);
    }
}

static struct fdvis_slot *fdvis_find(uint64_t key, uint64_t owner_start_ns, int claim) {
    if (!g_fdvis || key == 0) return NULL;
    unsigned start = (unsigned)((key ^ (key >> 32)) * UINT64_C(2654435761)) & (FDVIS_N - 1);
    for (unsigned probe = 0; probe < FDVIS_N; ++probe) {
        struct fdvis_slot *slot = &g_fdvis[(start + probe) & (FDVIS_N - 1)];
        uint64_t present = slot->key;
        if (present == key) {
            if (slot->owner_start_ns == owner_start_ns) return slot;
            if (!claim) return NULL;
            memset(slot, 0, sizeof *slot);
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
        if (claim && present == 0) {
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
    }
    return NULL;
}

static void fdvis_lock(void) {
    int me = (int)getpid();
    uint64_t mine = fdvis_identity(me, fdvis_process_token(me));
    for (unsigned spin = 0;; ++spin) {
        uint64_t expected = 0;
        if (atomic_compare_exchange_weak_explicit(&g_fdvis_control->owner, &expected, mine, memory_order_acquire,
                                                  memory_order_relaxed))
            return;
        if ((spin & 1023u) == 1023u) {
            uint64_t owner = atomic_load_explicit(&g_fdvis_control->owner, memory_order_relaxed);
            int owner_pid = (int)(uint32_t)(owner >> 32);
            uint64_t live_start = fdvis_process_token(owner_pid);
            if (owner != 0 && (live_start == 0 || fdvis_identity(owner_pid, live_start) != owner) &&
                atomic_compare_exchange_strong_explicit(&g_fdvis_control->owner, &owner, mine, memory_order_acquire,
                                                        memory_order_relaxed))
                return;
            sched_yield();
        }
    }
}

static void fdvis_unlock(void) {
    atomic_store_explicit(&g_fdvis_control->owner, 0, memory_order_release);
}

static void fdvis_sweep_stale_locked(void) {
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        int owner = (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        uint64_t live_start = fdvis_process_token(owner);
        if (live_start == 0 || live_start != slot->owner_start_ns) memset(slot, 0, sizeof *slot);
    }
}

struct fdvis_reservation {
    unsigned slot;
    int active;
    int new_slot;
};

static int proc_fdvis_reserve(struct fdvis_reservation *reservation) {
    if (!reservation) return -EINVAL;
    memset(reservation, 0, sizeof *reservation);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    for (unsigned pass = 0; pass < 2; ++pass) {
        for (unsigned index = 0; index < FDVIS_N; ++index) {
            if (g_fdvis[index].key != 0) continue;
            g_fdvis[index].key = UINT64_MAX;
            reservation->slot = index;
            reservation->active = 1;
            reservation->new_slot = 1;
            fdvis_unlock();
            return 0;
        }
        fdvis_sweep_stale_locked();
    }
    fdvis_unlock();
    return -ENOSPC;
}

static int proc_fdvis_reserve_at(int guest_fd, struct fdvis_reservation *reservation) {
    int pid = (int)getpid();
    uint64_t owner_start = fdvis_process_token(pid);
    if (!reservation) return -EINVAL;
    memset(reservation, 0, sizeof *reservation);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    struct fdvis_slot *present = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (present) {
        reservation->slot = (unsigned)(present - g_fdvis);
        reservation->active = 1;
        fdvis_unlock();
        return 0;
    }
    for (unsigned pass = 0; pass < 2; ++pass) {
        for (unsigned index = 0; index < FDVIS_N; ++index) {
            if (g_fdvis[index].key != 0) continue;
            g_fdvis[index].key = UINT64_MAX;
            reservation->slot = index;
            reservation->active = 1;
            reservation->new_slot = 1;
            fdvis_unlock();
            return 0;
        }
        fdvis_sweep_stale_locked();
    }
    fdvis_unlock();
    return -ENOSPC;
}

static void proc_fdvis_reservation_cancel(struct fdvis_reservation *reservation) {
    if (!reservation || !reservation->active) return;
    fdvis_lock();
    struct fdvis_slot *slot = &g_fdvis[reservation->slot];
    if (reservation->new_slot && slot->key == UINT64_MAX) memset(slot, 0, sizeof *slot);
    fdvis_unlock();
    reservation->active = 0;
}

static void proc_fdvis_reservation_publish(struct fdvis_reservation *reservation, int guest_fd, uint32_t kind,
                                           uint64_t device, uint64_t object) {
    int pid = (int)getpid();
    uint64_t owner_start = fdvis_process_token(pid);
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    struct fdvis_slot *reserved = &g_fdvis[reservation->slot];
    if (slot) {
        if (reserved != slot && reserved->key == UINT64_MAX) memset(reserved, 0, sizeof *reserved);
    } else {
        slot = reserved;
    }
    slot->device = device;
    slot->object = object;
    slot->kind = kind;
    slot->owner_start_ns = owner_start;
    slot->generation = ++g_fdvis_control->generation;
    slot->key = fdvis_key(pid, guest_fd);
    fdvis_unlock();
    reservation->active = 0;
}

static int proc_fdvis_publish(int guest_fd, uint32_t kind, uint64_t device, uint64_t object) {
    int pid = (int)getpid();
    uint64_t owner_start = fdvis_process_token(pid);
    if (guest_fd < 0 || guest_fd >= HL_NFD) return -EBADF;
    if (!g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 1);
    if (!slot) {
        fdvis_sweep_stale_locked();
        slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 1);
    }
    if (!slot) {
        fdvis_unlock();
        return -ENOSPC;
    }
    uint64_t generation = ++g_fdvis_control->generation;
    slot->device = device;
    slot->object = object;
    slot->kind = kind;
    slot->generation = generation;
    fdvis_unlock();
    return 0;
}

static int proc_fdvis_publish_native_fd(int guest_fd) {
    hl_host_process_fd detail;
    size_t ignored = 0;
    if (guest_fd < 0 || !hl_host_process_fd_read(getpid(), guest_fd, &detail, NULL, 0, &ignored)) return -EBADF;
    return proc_fdvis_publish(guest_fd, detail.kind, detail.stable_device, detail.stable_object);
}

static int proc_fdvis_publish_pipe_pair(int first, int second) {
    uint64_t sequence = atomic_fetch_add_explicit(&g_pipe_identity_next, 1, memory_order_relaxed);
    uint64_t identity = fdvis_identity((int)getpid(), fdvis_process_token((int)getpid())) ^ sequence;
    if (identity == 0) identity = sequence ? sequence : 1;
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD) return -EINVAL;
    if (proc_fdvis_publish(first, HL_HOST_FD_PIPE, 1, identity) != 0) return -ENOSPC;
    if (proc_fdvis_publish(second, HL_HOST_FD_PIPE, 1, identity) != 0) {
        proc_fdvis_close(first);
        return -ENOSPC;
    }
    g_pipe_identity[first] = identity;
    g_pipe_identity[second] = identity;
    return 0;
}

static void proc_fdvis_close(int guest_fd) {
    if (!g_fdvis_control) return;
    fdvis_lock();
    int pid = (int)getpid();
    uint64_t owner_start = fdvis_process_token(pid);
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (slot) memset(slot, 0, sizeof *slot);
    fdvis_unlock();
}

static int proc_fdvis_lookup(int pid, int guest_fd, uint32_t *kind, uint64_t *device, uint64_t *object) {
    if (!g_fdvis_control) return 0;
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), fdvis_process_token(pid), 0);
    if (slot) {
        if (kind) *kind = slot->kind;
        if (device) *device = slot->device;
        if (object) *object = slot->object;
    }
    fdvis_unlock();
    return slot != NULL;
}

struct fdvis_view {
    int guest_fd;
    uint32_t kind;
    uint64_t device;
    uint64_t object;
};

static size_t proc_fdvis_list(int pid, struct fdvis_view *views, size_t capacity) {
    uint64_t owner_start = fdvis_process_token(pid);
    size_t count = 0;
    if (!g_fdvis || !g_fdvis_control || owner_start == 0) return 0;
    fdvis_lock();
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        if ((int)(uint32_t)(slot->key >> 32) != pid || slot->owner_start_ns != owner_start) continue;
        int guest_fd = (int)(uint32_t)slot->key - 1;
        if (guest_fd < 0 || guest_fd >= HL_NFD) continue;
        if (count < capacity) {
            views[count].guest_fd = guest_fd;
            views[count].kind = slot->kind;
            views[count].device = slot->device;
            views[count].object = slot->object;
        }
        ++count;
    }
    fdvis_unlock();
    return count;
}

static int proc_fdvis_fork_prepare(struct fdvis_fork_plan *plan) {
    size_t count = 0;
    size_t capacity = 0;
    size_t reserved = 0;
    struct fdvis_fork_entry *entries = NULL;
    g_fdvis_fork_parent = (int)getpid();
    g_fdvis_fork_parent_start = fdvis_process_token(g_fdvis_fork_parent);
    memset(plan, 0, sizeof *plan);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;

    fdvis_lock();
    /* One pass fuses the stale sweep with the parent-descriptor collect. A parent-owned live slot is
     * never stale (its owner is us and owner_start_ns matches), so the "collect" and "sweep" categories
     * are disjoint: the result is byte-identical to sweeping the whole table first and then counting +
     * copying the parent's slots. Collecting the fd identity here also folds the old separate fill pass
     * away, since the reserve pass below never touches occupied parent slots. */
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        int owner = (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        if (owner == g_fdvis_fork_parent && slot->owner_start_ns == g_fdvis_fork_parent_start) {
            if (count == capacity) {
                size_t next = capacity ? capacity * 2 : 16;
                struct fdvis_fork_entry *grown = realloc(entries, next * sizeof *grown);
                if (grown == NULL) {
                    fdvis_unlock();
                    free(entries);
                    return -ENOMEM;
                }
                entries = grown;
                capacity = next;
            }
            entries[count].guest_fd = (int)(uint32_t)slot->key - 1;
            entries[count].kind = slot->kind;
            entries[count].device = slot->device;
            entries[count].object = slot->object;
            ++count;
            continue;
        }
        uint64_t live_start = fdvis_process_token(owner);
        if (live_start == 0 || live_start != slot->owner_start_ns) memset(slot, 0, sizeof *slot);
    }
    for (unsigned index = 0; index < FDVIS_N && reserved < count; ++index) {
        if (g_fdvis[index].key != 0) continue;
        g_fdvis[index].key = UINT64_MAX;
        entries[reserved++].slot = index;
    }
    if (reserved != count) {
        for (size_t index = 0; index < reserved; ++index)
            memset(&g_fdvis[entries[index].slot], 0, sizeof *g_fdvis);
        fdvis_unlock();
        free(entries);
        return -ENOSPC;
    }
    fdvis_unlock();
    plan->entries = entries;
    plan->count = count;
    return 0;
}

static void proc_fdvis_fork_cancel(struct fdvis_fork_plan *plan) {
    if (!plan->entries) return;
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        struct fdvis_slot *slot = &g_fdvis[plan->entries[index].slot];
        if (slot->key == UINT64_MAX) memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

static void proc_fdvis_after_fork(struct fdvis_fork_plan *plan, int child, int in_child) {
    uint64_t child_start = fdvis_process_token(child);
    if (!g_fdvis || !g_fdvis_control || child <= 0) return;
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *copy = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (copy->key != UINT64_MAX && copy->key != key) continue;
        copy->device = entry->device;
        copy->object = entry->object;
        copy->kind = entry->kind;
        copy->owner_start_ns = child_start;
        copy->generation = ++g_fdvis_control->generation;
        copy->key = key;
    }
    if (in_child) {
        g_fdvis_fork_parent = child;
        g_fdvis_fork_parent_start = child_start;
    }
    fdvis_unlock();
}

static void proc_fdvis_cleanup(void) {
    int owner = (int)getpid();
    uint64_t owner_start = fdvis_process_token(owner);
    if (!g_fdvis || !g_fdvis_control) return;
    fdvis_lock();
    for (unsigned index = 0; index < FDVIS_N; ++index)
        if ((int)(uint32_t)(g_fdvis[index].key >> 32) == owner && g_fdvis[index].owner_start_ns == owner_start)
            memset(&g_fdvis[index], 0, sizeof g_fdvis[index]);
    fdvis_unlock();
}

static void ts_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_ts_tab) return;
    size_t sz = sizeof(struct ts_slot) * TS_N;
    if (hl_linux_shared_create(host, sz, &arena) == HL_STATUS_OK) g_ts_tab = (struct ts_slot *)arena;
}

// Find (or, when claim, atomically allocate) the slot for host pid `pid`. Open addressing with linear
// probe; a freshly claimed slot defaults to 'R' (running), overwriting any stale value a recycled pid left.
static struct ts_slot *ts_slot_for(int pid, int claim) {
    if (!g_ts_tab || pid <= 0) return NULL;
    unsigned h = ((unsigned)pid * 2654435761u) & (TS_N - 1);
    for (unsigned i = 0; i < TS_N; i++) {
        struct ts_slot *s = &g_ts_tab[(h + i) & (TS_N - 1)];
        int p = atomic_load_explicit(&s->pid, memory_order_acquire);
        if (p == pid) return s;
        if (claim && p == 0) {
            int expect = 0;
            if (atomic_compare_exchange_strong(&s->pid, &expect, pid)) {
                atomic_store_explicit(&s->st, 'R', memory_order_release);
                return s;
            }
            if (atomic_load_explicit(&s->pid, memory_order_acquire) == pid) return s; // raced to same pid
        }
    }
    return NULL; // table full: caller falls back to pbi_status
}
// This thread's/process's own slot, cached. Once resolved, the cached slot is reused with NO per-call
// getpid(): current glibc (>=2.25) no longer caches getpid(), so calling it on every syscall issued a
// real host getpid() -- ~88ns of pure overhead on the gettid-loop fast path. Fork safety instead rides on
// ts_after_fork(), which drops this cache in the child; every host fork() that goes on to run guest
// syscalls (guest fork/vfork/clone in proc.c, the fork-server runner, checkpoint restore) is a glibc
// fork(), so registering ts_after_fork() as a pthread_atfork child handler resets the cache on ALL of
// them -- the child then re-derives its own pid once, exactly as the old per-call getpid() did lazily.
static _Thread_local struct ts_slot *ts_self;
static _Thread_local int ts_self_pid;
static void ts_after_fork(void); // pthread_atfork child handler: drops the inherited slot cache
static pthread_once_t ts_atfork_once = PTHREAD_ONCE_INIT;

static void ts_atfork_install(void) {
    (void)pthread_atfork(NULL, NULL, ts_after_fork);
}

static struct ts_slot *ts_mine(void) {
    if (__builtin_expect(ts_self != NULL, 1)) return ts_self;
    (void)pthread_once(&ts_atfork_once, ts_atfork_install);
    int pid = (int)getpid();
    ts_self = ts_slot_for(pid, 1);
    ts_self_pid = pid;
    return ts_self;
}

static inline void ts_set_self(unsigned char st) {
    struct ts_slot *s = ts_mine();
    if (s) atomic_store_explicit(&s->st, st, memory_order_release);
}

// Bracket a host blocking wait: 'S' (interruptible sleep) on entry, 'R' (running) on wake. Errno-safe --
// getpid() + an atomic store never clobber the caller's errno on the wait's return path.
static inline void ts_wait_enter(void) {
    ts_set_self('S');
}

static inline void ts_wait_leave(void) {
    ts_set_self('R');
}

static inline void ts_running(void) {
    ts_set_self('R');
} // every non-blocking syscall = we were running

// Reader side: the published state char for host pid `host`, or 0 if this task has no published slot.
static int ts_lookup(int host) {
    struct ts_slot *s = ts_slot_for(host, 0);
    return s ? (int)atomic_load_explicit(&s->st, memory_order_acquire) : 0;
}

// A guest fork child re-claims its own slot lazily (getpid mismatch), but drop the inherited cache eagerly
// so its very first published state is its OWN, not a stale pointer into the parent's slot.
static void ts_after_fork(void) {
    ts_self = NULL;
    ts_self_pid = 0;
}

// ===================== in-memory temp-file backing (sqlite sorter/index spill) =====================
// A genuinely-PRIVATE scratch file is served from a host RAM buffer instead of issuing pread/pwrite to
// a host temp file. SQLite's sorter/index spill ("etilqs_*") opens O_RDWR|O_CREAT|O_EXCL under the temp
// dir and unlink()s it IMMEDIATELY while still open (delete-on-close), and glibc/rustix also use
// O_TMPFILE. Once a regular file has been unlinked while open with link count 0 it has NO name and CANNOT
// be reached by any other path -> it is private scratch, exactly equivalent to an anonymous memfd, so it
// is safe to back with RAM (this is the same anonymity O_TMPFILE has from birth).
//
// PLUMBING: the guest fd stays a REAL host fd (a created-then-unlinked regular file), so the fd NUMBER,
// poll/select/epoll readiness, fcntl, and fork inheritance all behave exactly like a normal file. The RAM
// buffer is a transparent write-back cache: read/write/pread/pwrite/lseek/ftruncate/fstat/fsync on the fd
// hit RAM (memcpy), turning the per-block host I/O syscalls into memory copies. On ANY operation that
// could let another observer see the bytes through the real fd -- dup/sendfile/splice/copy_file_range,
// mmap, an SCM_RIGHTS send, a /proc/self/fd reopen, fork, or execve -- we first "materialize" (flush the
// RAM buffer back into the real fd, restore its size+offset) and drop the cache, after which the fd is an
// ordinary host file and behaves identically to the unoptimized path. This materialize-on-escape rule is
// the bit-exact safety argument: backing a file changes only WHERE the bytes live, never any observable
// byte/size/seek/stat result.
//
// KILL SWITCH: NOTMPFS=1 disables all backing (pure host-file behaviour). BOUND: a file larger than
// MEMF_CAP, or once the process-wide RAM total would exceed MEMF_TOTAL_CAP, is materialized and spills to
// the real host file (host I/O resumes) -- RAM use is bounded, never unbounded.
#define MEMF_CAP (256ull * 1024 * 1024)        // per-file RAM cap; beyond this, spill to the host file
#define MEMF_TOTAL_CAP (1024ull * 1024 * 1024) // process-wide RAM cap for all backed files

struct memf {
    uint8_t *buf;
    size_t size; // logical file size (bytes)
    size_t cap;  // allocated bytes of buf
    off_t pos;   // current file offset (for read/write/lseek SEEK_CUR)
};
static struct memf *g_memf[HL_NFD];
static _Atomic uint64_t g_memf_total; // sum of logical sizes of all backed files

static int memf_disabled(void) {
    return 0;
}

static inline struct memf *memf_get(int fd) {
    return (fd >= 0 && fd < HL_NFD) ? g_memf[fd] : NULL;
}

// grow buf to >= need bytes, zero-filling the new tail (so a sparse write reads back as zeros).
static int memf_reserve(struct memf *m, size_t need) {
    if (need <= m->cap) return 0;
    size_t nc = m->cap ? m->cap : 65536;
    while (nc < need)
        nc = nc < (16u << 20) ? nc << 1 : nc + (16u << 20); // double, then +16MiB chunks
    uint8_t *nb = realloc(m->buf, nc);
    if (!nb) return -1;
    memset(nb + m->cap, 0, nc - m->cap);
    m->buf = nb;
    m->cap = nc;
    return 0;
}

// Attach a RAM cache to real host fd `fd`, slurping `init` bytes already present in the fd. Returns 1 if
// backed, 0 if left as a plain host fd (kill switch / over cap / OOM). The fd becomes anonymous.
static int memf_attach(int fd, off_t init, off_t pos) {
    if (memf_disabled() || fd < 0 || fd >= HL_NFD || g_memf[fd]) return 0;
    if (init < 0 || (uint64_t)init > MEMF_CAP) return 0;
    if (atomic_load(&g_memf_total) + (uint64_t)init > MEMF_TOTAL_CAP) return 0;
    struct memf *m = calloc(1, sizeof *m);
    if (!m) return 0;
    if (init > 0) {
        if (memf_reserve(m, (size_t)init)) {
            free(m);
            return 0;
        }
        off_t got = 0;
        for (off_t o = 0; o < init;) { // slurp existing bytes from the real fd into RAM
            ssize_t r = pread(fd, m->buf + o, (size_t)(init - o), o);
            if (r <= 0) break;
            o += r;
            got = o;
        }
        if (got != init) { // unreadable fd / short read: zero-filling the tail would read back as zeros and
            free(m->buf);  // a later memf_materialize would pwrite those zeros over real on-disk bytes (data
            free(m);       // loss). Abort the adoption and fall back to the plain host fd (F1).
            g_memf[fd] = NULL;
            return 0;
        }
        m->size = (size_t)init;
    }
    m->pos = pos < 0 ? 0 : pos;
    g_memf[fd] = m;
    atomic_fetch_add(&g_memf_total, (uint64_t)m->size);
    g_fdpath[fd][0] = 0; // anonymous: no tracked host path
    return 1;
}

// Flush the RAM buffer back into the real fd (size + offset restored) and drop the cache: the fd reverts
// to a plain host file behaving exactly as if it had never been backed.
static void memf_materialize(int fd) {
    struct memf *m = memf_get(fd);
    if (!m) return;
    g_memf[fd] = NULL;
    for (size_t o = 0; o < m->size;) {
        ssize_t w = pwrite(fd, m->buf + o, m->size - o, (off_t)o);
        if (w <= 0) break;
        o += (size_t)w;
    }
    if (ftruncate(fd, (off_t)m->size) < 0) {}
    lseek(fd, m->pos, SEEK_SET);
    atomic_fetch_sub(&g_memf_total, (uint64_t)m->size);
    free(m->buf);
    free(m);
}

static void memf_materialize_all(void) {
    for (int fd = 0; fd < HL_NFD; fd++)
        if (g_memf[fd]) memf_materialize(fd);
}

static void memf_close(int fd) { // fd is being closed: just discard the RAM buffer
    struct memf *m = memf_get(fd);
    if (!m) return;
    g_memf[fd] = NULL;
    atomic_fetch_sub(&g_memf_total, (uint64_t)m->size);
    free(m->buf);
    free(m);
}

// I/O served from RAM. pread/pwrite are positional; read/write advance m->pos.
static ssize_t memf_pread(struct memf *m, void *buf, size_t n, off_t off) {
    if (off < 0) return -EINVAL;
    size_t avail = (size_t)off < m->size ? m->size - (size_t)off : 0;
    size_t k = n < avail ? n : avail;
    if (k) memcpy(buf, m->buf + off, k);
    return (ssize_t)k;
}

static ssize_t memf_pwrite(struct memf *m, const void *buf, size_t n, off_t off) {
    if (off < 0) return -EINVAL;
    size_t end = (size_t)off + n;
    if (memf_reserve(m, end)) return -ENOMEM;
    memcpy(m->buf + off, buf, n);
    if (end > m->size) {
        atomic_fetch_add(&g_memf_total, end - m->size);
        m->size = end;
    }
    return (ssize_t)n;
}

static ssize_t memf_read_pos(struct memf *m, void *buf, size_t n) {
    ssize_t k = memf_pread(m, buf, n, m->pos);
    if (k > 0) m->pos += k;
    return k;
}

static ssize_t memf_write_pos(struct memf *m, const void *buf, size_t n) {
    ssize_t k = memf_pwrite(m, buf, n, m->pos);
    if (k > 0) m->pos += k;
    return k;
}

static ssize_t memf_preadv(struct memf *m, const struct iovec *iov, int cnt, off_t off, int advance) {
    off_t p = advance ? m->pos : off;
    ssize_t tot = 0;
    for (int i = 0; i < cnt; i++) {
        ssize_t k = memf_pread(m, iov[i].iov_base, iov[i].iov_len, p);
        if (k < 0) return tot ? tot : k;
        tot += k;
        p += k;
        if ((size_t)k < iov[i].iov_len) break; // short read -> EOF
    }
    if (advance) m->pos = p;
    return tot;
}

static ssize_t memf_pwritev(struct memf *m, const struct iovec *iov, int cnt, off_t off, int advance) {
    off_t p = advance ? m->pos : off;
    ssize_t tot = 0;
    for (int i = 0; i < cnt; i++) {
        ssize_t k = memf_pwrite(m, iov[i].iov_base, iov[i].iov_len, p);
        if (k < 0) return tot ? tot : k;
        tot += k;
        p += k;
    }
    if (advance) m->pos = p;
    return tot;
}

// lseek on RAM. Returns the new offset, -1 for EINVAL, or -2 to mean "unsupported whence -> materialize".
static off_t memf_lseek(struct memf *m, off_t off, int whence) {
    off_t np;
    if (whence == 0)
        np = off; // SEEK_SET
    else if (whence == 1)
        np = m->pos + off; // SEEK_CUR
    else if (whence == 2)
        np = (off_t)m->size + off; // SEEK_END
    else
        return -2; // SEEK_DATA/SEEK_HOLE: let the host fd handle it
    if (np < 0) return -1;
    m->pos = np;
    return np;
}

static int memf_fstat(int fd, struct stat *s) { // real-file metadata, RAM size/blocks
    if (fstat(fd, s) != 0) return -1;
    struct memf *m = g_memf[fd];
    s->st_size = (off_t)m->size;
    HL_HOST_STAT_SET_BLOCKS(s, (m->size + 511) / 512);
    return 0;
}

// Returns 1 if writing up to byte `end` stays within the caps; otherwise materializes fd (spills to the
// host file) and returns 0 so the caller falls through to the real host write.
static int memf_room_or_spill(int fd, off_t end) {
    struct memf *m = g_memf[fd];
    if (end < 0 || (uint64_t)end <= m->size) return 1;
    uint64_t grow = (uint64_t)end - m->size;
    if ((uint64_t)end > MEMF_CAP || atomic_load(&g_memf_total) + grow > MEMF_TOTAL_CAP) {
        memf_materialize(fd);
        return 0;
    }
    return 1;
}

// After the guest unlinked a temp file (dev/ino captured before the unlink), adopt it as RAM-backed iff
// EXACTLY ONE open fd now holds the last (zero) link to that regular file -- i.e. it is now anonymous and
// privately owned by this one description. More than one matching fd (a dup) shares an offset we don't
// model, so we leave those as a plain host file.
// Defined later in the same unity TU (syscall/binding.c): true when the bound fd source is the raw native
// host table rather than the typed box, in which case fdvis is not authoritative for open regular files.
static int bound_source_is_native(void);

// Probe a single candidate fd for the just-unlinked (dev,ino); update *found (>=0), return 1 if a second
// distinct match was seen (a duped description -> caller must bail without adopting).
static int memf_adopt_probe(int fd, uint64_t dev, uint64_t ino, int *found) {
    struct stat s;
    if (fd < 0 || fd >= HL_NFD || g_memf[fd]) return 0;
    if (fstat(fd, &s) != 0) return 0;
    if ((uint64_t)s.st_dev != dev || (uint64_t)s.st_ino != ino) return 0;
    if (*found >= 0) return 1; // duped: shared description -> don't risk it
    *found = fd;
    return 0;
}

static void memf_try_adopt(uint64_t dev, uint64_t ino) {
    if (memf_disabled() || !ino) return;
    int found = -1;
    // The adoptable fd is a still-open regular file the guest just unlinked. Rather than fstat all
    // HL_NFD (65536) descriptors -- a multi-millisecond storm on a WAL/journal-heavy server that unlinks
    // open temp files -- probe only the process's PUBLISHED fd set (fdvis). In bound-source mode EVERY guest
    // open is published to fdvis with its identity, so that set is authoritative for open regular files; use
    // it to bound the probe. Fall back to the full scan when the published set is unavailable (native source,
    // fdvis off) so behavior is otherwise byte-for-byte unchanged.
    //
    // A generous on-stack buffer captures the published set in a SINGLE table walk: proc_fdvis_list already
    // returns the true total (including any entries past the buffer), so the old count-then-fill idiom walked
    // the whole FDVIS_N (131072-slot, multi-MB) table TWICE on every unlink of an open temp file (WAL/journal
    // churn). Only a pathological overflow (>256 live published fds) re-scans into an exact heap buffer; a
    // heap-alloc failure there falls back to the exhaustive fstat scan, so the candidate set is never smaller
    // than before -- adoption behavior stays identical, and the duped-description bail is order-independent.
    if (!bound_source_is_native()) {
        struct fdvis_view inl[256];
        struct fdvis_view *views = inl, *heap = NULL;
        size_t cap = sizeof inl / sizeof *inl;
        size_t got = proc_fdvis_list((int)getpid(), inl, cap);
        if (got > cap) {
            heap = malloc(got * sizeof *heap);
            if (heap == NULL) {
                for (int fd = 0; fd < HL_NFD; fd++)
                    if (memf_adopt_probe(fd, dev, ino, &found)) return;
                goto adopt_decide;
            }
            size_t refill = proc_fdvis_list((int)getpid(), heap, got);
            views = heap;
            if (refill < got) got = refill;
        }
        for (size_t i = 0; i < got; i++)
            if (memf_adopt_probe(views[i].guest_fd, dev, ino, &found)) {
                free(heap);
                return;
            }
        free(heap);
    } else {
        for (int fd = 0; fd < HL_NFD; fd++)
            if (memf_adopt_probe(fd, dev, ino, &found)) return;
    }
adopt_decide:
    if (found < 0) return;
    struct stat s;
    if (fstat(found, &s) != 0 || !S_ISREG(s.st_mode) || s.st_nlink != 0) return;
    int fl = fcntl(found, F_GETFL); // only adopt an O_RDWR fd: a RAM cache serves both reads and writes, so
    if (fl < 0 || (fl & O_ACCMODE) != O_RDWR) return;         // adopting an O_RDONLY/O_WRONLY scratch fd would accept
    memf_attach(found, s.st_size, lseek(found, 0, SEEK_CUR)); // I/O the kernel would reject with EBADF (F2).
}

// A non-PIE ET_EXEC is linked at a fixed low vaddr but __PAGEZERO forbids mapping there, so load_elf biases
// it high. Its un-relocated absolute refs still point at the low link range; when the guest takes an
// absolute jump there, the dispatcher redirects pc into the biased image (pc += bias) instead of faulting
// on the unmapped low address. [lo,hi) is the un-biased link span of the current main image (0 if PIE).
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;
// Sticky (never-cleared, COW-inherited across fork) flag: set the first time THIS process lineage creates or
// receives an epoll / timerfd / inotify instance. kqueue_rebuild_after_fork -- an O(HL_NFD) scan + several
// full-array memsets run in every fork child -- is a guaranteed no-op when this is unset (a watched/armed fd
// can only exist behind an instance, whose creation sets this), so the child can skip the whole sweep. It is
// only an UPPER BOUND: once set it stays set even after all such fds close, so the sweep then runs unchanged.
static uint8_t g_epoll_family_seen;
// fd is a timerfd (a kqueue with an EVFILT_TIMER) -> read() drains it
static uint8_t g_timerfd[HL_NFD];
// fd is an inotify (a kqueue with EVFILT_VNODE watches) -> read() drains it
static uint8_t g_inotify[HL_NFD];
// per inotify instance: IN_NONBLOCK was requested. macOS kqueue fds don't survive fork, so the child's
// rebuilt kqueue must re-apply O_NONBLOCK (else a blocking read on the inherited instance can hang).
static uint8_t g_inotify_nb[HL_NFD];
// inotify-on-a-directory emulation: kqueue says "the dir changed" but not which entry, so we keep the
// watched dir's path + a snapshot of its names and diff on read() to synthesize IN_CREATE/IN_DELETE+name.
static char g_inotify_wpath[HL_NFD][512];
static char *g_inotify_snap[HL_NFD]; // newline-joined entry names of the last snapshot (malloc'd)
// inotify: which inotify-instance fd owns each watch fd (wd) -> read(instance) drains that wd's move queue.
static int g_inotify_owner[HL_NFD];
static uint32_t g_inotify_mask[HL_NFD];
static uint32_t g_inotify_pending[HL_NFD];
static uint8_t g_inotify_isdir[HL_NFD];
static uint64_t g_inotify_object[HL_NFD];
static uint32_t g_inotify_object_next;
static uint8_t *g_inotify_raw[HL_NFD];
static size_t g_inotify_raw_len[HL_NFD];
static size_t g_inotify_raw_pos[HL_NFD];

static void inotify_object_assign(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_inotify[fd] || g_inotify_object[fd]) return;
    uint32_t serial = ++g_inotify_object_next;
    if (!serial) serial = ++g_inotify_object_next;
    g_inotify_object[fd] = ((uint64_t)(uint32_t)getpid() << 32) | serial;
}

// timerfd remaining-time tracking (lsys-timerfd-gettime): absolute CLOCK_MONOTONIC deadline (ns) of the
// next expiry + the interval (ns). timerfd_settime records them so timerfd_gettime reports it_value/interval.
static int64_t g_tfd_deadline[HL_NFD];
static int64_t g_tfd_interval[HL_NFD];
// timerfd aliases share pending expirations and checkpoint identity through one canonical slot.
static int g_tfd_cslot[HL_NFD];
static uint64_t g_tfd_pending[HL_NFD];
static int g_tfd_refs[HL_NFD];
static uint64_t g_tfd_object[HL_NFD];
static uint8_t g_tfd_nb[HL_NFD];

struct timerfd_shared_state {
    volatile int lock;
    int64_t deadline;
    int64_t interval;
    uint64_t pending;
};
static struct timerfd_shared_state *g_tfd_shared[HL_NFD];

static void timerfd_shared_lock(struct timerfd_shared_state *state) {
    while (__sync_lock_test_and_set(&state->lock, 1))
        sched_yield();
}

static void timerfd_shared_unlock(struct timerfd_shared_state *state) {
    __sync_lock_release(&state->lock);
}

static uint32_t g_tfd_object_next;

static int timerfd_slot(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_tfd_cslot[fd] > 0) return g_tfd_cslot[fd] - 1;
    return fd;
}

static void timerfd_object_assign(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_timerfd[fd] || g_tfd_object[fd]) return;
    uint32_t serial = ++g_tfd_object_next;
    if (!serial) serial = ++g_tfd_object_next;
    g_tfd_object[fd] = ((uint64_t)(uint32_t)getpid() << 32) | serial;
    g_tfd_cslot[fd] = fd + 1;
    g_tfd_refs[fd] = 1;
}

// A periodic timerfd whose FIRST expiry (it_value) differs from its interval (it_interval) can't be
// expressed in a single kqueue EVFILT_TIMER (which fires first only after its period). So we arm a
// ONE-SHOT at the first delay and set this flag; on the first read() drain the timer is re-armed as a
// recurring periodic at g_tfd_interval. 1 = currently armed one-shot for the distinct first deadline.
static uint8_t g_tfd_first_oneshot[HL_NFD];
// The clockid the timerfd was created with (Linux CLOCK_REALTIME=0/MONOTONIC=1/BOOTTIME=7/REALTIME_ALARM=8/
// ...). A TFD_TIMER_ABSTIME deadline is expressed in THIS clock, so timerfd_settime must convert against it.
static int g_tfd_clock[HL_NFD];
// memfd sealing (lsys-memfd-seal): g_memfd_is[fd]=1 marks an anonymous memfd; g_memfd_seal[fd] carries the
// F_SEAL_* bitmask (F_SEAL_SEAL=1,SHRINK=2,GROW=4,WRITE=8,FUTURE_WRITE=16). A non-ALLOW_SEALING memfd starts
// already F_SEAL_SEAL'd, so further F_ADD_SEALS fail EPERM exactly as on Linux.
static uint8_t g_memfd_is[HL_NFD];
static int g_memfd_seal[HL_NFD];

#define MEMFD_REG_MAX 4096

struct memfd_reg_ent {
    uint64_t dev, ino;
    int seals;
};

struct memfd_reg {
    volatile int lock;
    int n;
    struct memfd_reg_ent e[MEMFD_REG_MAX];
};
static struct memfd_reg *g_memfd_reg;

static struct memfd_reg *memfd_reg(void) {
    void *arena = NULL;
    if (g_memfd_reg) return g_memfd_reg;
    if (hl_linux_shared_create(effective_host_services(), sizeof(struct memfd_reg), &arena) != HL_STATUS_OK)
        return NULL;
    g_memfd_reg = (struct memfd_reg *)arena;
    return g_memfd_reg;
}

static void memfd_reg_lock(struct memfd_reg *r) {
    while (__sync_lock_test_and_set(&r->lock, 1)) {}
}

static void memfd_reg_unlock(struct memfd_reg *r) {
    __sync_lock_release(&r->lock);
}

static int memfd_fd_id(int fd, uint64_t *dev, uint64_t *ino) {
    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0) return 0;
    *dev = (uint64_t)st.st_dev;
    *ino = (uint64_t)st.st_ino;
    return *ino != 0;
}

static void memfd_reg_set_id(uint64_t dev, uint64_t ino, int seals) {
    struct memfd_reg *r = memfd_reg();
    if (!r || !ino) return;
    memfd_reg_lock(r);
    for (int i = 0; i < r->n; i++) {
        if (r->e[i].dev == dev && r->e[i].ino == ino) {
            r->e[i].seals = seals;
            memfd_reg_unlock(r);
            return;
        }
    }
    if (r->n < MEMFD_REG_MAX) {
        int i = r->n++;
        r->e[i].dev = dev;
        r->e[i].ino = ino;
        r->e[i].seals = seals;
    }
    memfd_reg_unlock(r);
}

static void memfd_reg_set_fd(int fd, int seals) {
    uint64_t dev = 0, ino = 0;
    if (memfd_fd_id(fd, &dev, &ino)) memfd_reg_set_id(dev, ino, seals);
}

static int memfd_reg_get_fd(int fd, int *seals) {
    uint64_t dev = 0, ino = 0;
    if (!memfd_fd_id(fd, &dev, &ino)) return 0;
    struct memfd_reg *r = memfd_reg();
    if (!r) return 0;
    int found = 0, val = 0;
    memfd_reg_lock(r);
    for (int i = 0; i < r->n; i++) {
        if (r->e[i].dev == dev && r->e[i].ino == ino) {
            found = 1;
            val = r->e[i].seals;
            break;
        }
    }
    memfd_reg_unlock(r);
    if (!found) return 0;
    if (seals) *seals = val;
    return 1;
}

static int memfd_ensure_fd(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    if (g_memfd_is[fd]) return 1;
    // Not flagged as a memfd on this fd yet. The only way an unflagged fd can still BE a memfd is if it was
    // received (SCM_RIGHTS) or inherited (fork/exec) after being created+sealed elsewhere -- and every such
    // memfd is recorded (dev/ino) in the fork-shared registry by memfd_create / F_ADD_SEALS. So if the
    // registry is empty (or not yet mapped in this process), no fd anywhere can match the dev/ino lookup
    // below: skip the per-write fstat(2) probe entirely. This is a pure fast path -- memfd_reg_get_fd would
    // return "not found" (0) in exactly these states -- and it makes every write(2)/pwrite/writev/ftruncate
    // to a NON-memfd fd (pipe/socket/file) one host fstat cheaper. A sealed memfd that is SCM-passed must
    // have been created+sealed before the fork that shares it, so the receiver sees a NON-empty registry and
    // still runs the full lookup below -- F_SEAL_WRITE stays enforced (see ipc_scm_memfd_seal).
    if (g_memfd_reg == NULL || g_memfd_reg->n == 0) return 0;
    int seals = 0;
    if (!memfd_reg_get_fd(fd, &seals)) return 0;
    g_memfd_is[fd] = 1;
    g_memfd_seal[fd] = seals;
    return 1;
}

static int memfd_seals_fd(int fd) {
    if (!memfd_ensure_fd(fd)) return 0;
    return (fd >= 0 && fd < HL_NFD) ? g_memfd_seal[fd] : 0;
}

// pipe read-pushback (tee(2)): tee() consumes bytes from the source pipe to copy them, then re-queues them
// here so the next read()/readv() on that fd re-serves them -> tee leaves the source pipe intact.
static uint8_t *g_fd_pushback[HL_NFD];
static size_t g_fd_pb_len[HL_NFD];
// pinned O_DIRECTORY fd to the rootfs (set at startup)
static int g_root_fd = -1;
/* Opaque twin used by the host-service resolver; legacy VFS paths retain g_root_fd until converted. */
static hl_host_handle g_root_handle = HL_HOST_HANDLE_INVALID;

/*
 * Pin the namespace root through the host-service ABI.  In container mode this
 * is the configured rootfs; a bare guest still needs an opaque handle for "/"
 * so absolute HOST_PATH opens do not fall back to the legacy native-fd lane.
 * Reinitialization (checkpoint/exec) replaces rather than leaks the old pin.
 */
static int root_handle_bind(const char *path) {
    const hl_host_file_services *file;
    const hl_host_posix_attachment_services *attachment;
    hl_host_result root;
    hl_host_result canonical;
    hl_host_result borrowed = {0};
    char canonical_path[sizeof g_rootfs_canon];

    if (g_host_services == NULL || g_host_services->file == NULL || path == NULL || path[0] == '\0') return -1;
    file = g_host_services->file;
    // The native twin is an OPTIONAL POSIX adapter (HL_HOST_CAP_POSIX_ATTACHMENT): a host with no native
    // descriptor to hand out leaves g_root_fd unbound rather than failing the pin, because the pin is a GATE
    // and not a functional dependency for a bare guest -- jail_routed_at() is identically 0 without a rootfs
    // and without a volume, so the confined walk (the only consumer of the native root) is never entered, and
    // the guest ELF already loads through the typed lane (image.c). Every remaining g_root_fd reader is
    // -1-safe: jail_pick()/jail_pick_idx() hand it back for equality tests (dispatch.c, overlay.c) or into
    // resolve_at's explicit `root_fd < 0 && !g_rootfs` fallback, engine_fd_reloc() ignores a slot that does
    // not equal the target fd, and engine_fd_vacate_range()/exec_fd_is_engine() filter on >= 0. A rootfs DOES
    // need the walk; root_native_require() below refuses it there.
    attachment = g_host_services->posix_attachment;
    root = file->open_relative(g_host_services->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                               HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY | HL_HOST_FILE_PATH_ONLY, 0, 0);
    if (root.status != HL_STATUS_OK) return -1;
    canonical = file->path(g_host_services->context, root.value,
                           (hl_host_bytes){(unsigned char *)canonical_path, sizeof(canonical_path) - 1});
    if (canonical.status != HL_STATUS_OK || canonical.value >= sizeof(canonical_path)) goto fail_root;
    canonical_path[canonical.value] = '\0';
    if (attachment != NULL) {
        borrowed = attachment->borrow_file_at_least(g_host_services->context, root.value, 1u << 20);
        if (borrowed.status != HL_STATUS_OK)
            borrowed = attachment->borrow_file_at_least(g_host_services->context, root.value, 64);
        if (borrowed.status != HL_STATUS_OK || borrowed.value > INT_MAX) goto fail_root;
        if (g_root_fd >= 0 &&
            attachment->release(g_host_services->context, (uint64_t)(unsigned)g_root_fd).status != HL_STATUS_OK)
            goto fail_borrowed;
    }
    if (g_root_handle != HL_HOST_HANDLE_INVALID &&
        file->close(g_host_services->context, g_root_handle).status != HL_STATUS_OK)
        goto fail_borrowed;
    g_root_handle = root.value;
    if (attachment != NULL) g_root_fd = (int)borrowed.value;
    if (g_rootfs != NULL) {
        memcpy(g_rootfs_canon, canonical_path, (size_t)canonical.value + 1);
        g_rootfs_canon_len = (size_t)canonical.value;
    }
    return 0;

fail_borrowed:
    if (attachment != NULL) (void)attachment->release(g_host_services->context, borrowed.value);
fail_root:
    (void)file->close(g_host_services->context, root.value);
    return -1;
}

static int root_native_require(void) {
    return g_root_fd >= 0 ? 0 : -1;
}

// Engine-private host fds (the rootfs dir-fd + each bind-mount volume dir-fd) share the guest's descriptor
// table in hl's in-process model. Opened at startup, right after stdio, they otherwise squat the LOW numbers
// Linux would leave free for the guest: g_root_fd lands on fd 3, shifting every guest fd allocation up by one
// AND becoming visible to the guest at a number a native run has free. s6-linux-init reads its
// notification pipe on the by-convention-lowest fd 3, which under hl was g_root_fd -- a DIRECTORY -> the
// read returns EISDIR ("unable to read from fd 3: Is a directory") and stage 1 aborts. Hoist each startup
// engine fd above a high floor so the guest's low fd space is exactly as on Linux (only 0/1/2 taken). Mirrors
// engine_fd_reloc's F_DUPFD floor (io.c) but relocates unconditionally, not just off a collision. Lazily
// created engine fds (the timer kqueue, the signalfd self-pipe) are made after the guest is running and take
// whatever is free then, so they never squat a fd the just-started guest relies on.
static int engine_fd_hoist(int fd) {
    if (fd < 3) return fd;                   // stdio (or a failed open) -> nothing to move
    int hi = fcntl(fd, F_DUPFD, 1 << 20);    // high floor; F_DUPFD returns the lowest free fd >= floor
    if (hi < 0) hi = fcntl(fd, F_DUPFD, 64); // floor beyond the guest's active low fds under a small RLIMIT
    if (hi < 0) return fd;                   // relocation failed -> keep the original (still functional)
    close(fd);
    return hi;
}

// Bind-mount volumes: a guest path prefix -> a host directory, each its own confined jail root.
struct vol {
    char guest[256];
    size_t glen;
    char hcanon[1024];
    size_t hlen;
    int fd;
    hl_host_handle handle; /* opaque twin of fd; rooted at the directory jail (or file parent) */
    int ro;                // 1 = read-only bind (`-v …:ro`): write-intent syscalls under `guest` fail EROFS
    int isfile;            // 1 = single-file bind (`-v host/f:/ctr/f`): `fd` is the host file's PARENT dir, `hcanon`
                           // is the file itself, and `guest` matches ONLY its exact path (a file has no children).
    int issymlink;         // projected link: resolve its target in the guest namespace
    int dead;              // 1 = detached by a runtime umount2(2): skipped by jail_match/jail_is_vol so the mount
                           // point reverts to the underlying rootfs/overlay content (the slot is never compacted --
                           // append-only keeps concurrent path resolves race-free).
};

#define HL_VOLUME_MAX 256

static struct vol g_vols[HL_VOLUME_MAX];
static int g_nvols;

#define HL_NAME_BIND_MAX 32
#define HL_NAME_BIND_ALIAS_MAX 8

struct name_bind {
    char names[HL_NAME_BIND_ALIAS_MAX][256];
    int names_count;
    char hcanon[1024];
    int fd;
};

static struct name_bind g_name_binds[HL_NAME_BIND_MAX];
static int g_name_binds_count;
static _Thread_local int g_name_bind_probe;

static int name_bind_pick(const char *guest);

static const char *name_bind_host_leaf(int index) {
    const char *slash = strrchr(g_name_binds[index].hcanon, '/');
    return slash ? slash + 1 : g_name_binds[index].hcanon;
}

static int add_name_bind(char *record) {
    if (!record || g_name_binds_count >= HL_NAME_BIND_MAX) return -1;
    char *field = strchr(record, '\t');
    if (!field) return -1;
    *field++ = 0;
    struct stat status;
    struct name_bind *bind = &g_name_binds[g_name_binds_count];
    memset(bind, 0, sizeof *bind);
    if (canonicalize_path(record, bind->hcanon, sizeof bind->hcanon) != 0 || stat(bind->hcanon, &status) != 0 ||
        !S_ISREG(status.st_mode))
        return -1;
    while (field && *field) {
        if (bind->names_count >= HL_NAME_BIND_ALIAS_MAX) return -1;
        char *next = strchr(field, '\t');
        if (next) *next++ = 0;
        if (!field[0] || !strcmp(field, ".") || !strcmp(field, "..") || strchr(field, '/') ||
            strlen(field) >= sizeof bind->names[0])
            return -1;
        for (int index = 0; index < bind->names_count; index++)
            if (!strcmp(field, bind->names[index])) return -1;
        snprintf(bind->names[bind->names_count++], sizeof bind->names[0], "%s", field);
        field = next;
    }
    if (bind->names_count == 0) return -1;
    char parent[1024];
    snprintf(parent, sizeof parent, "%s", bind->hcanon);
    char *slash = strrchr(parent, '/');
    if (!slash) return -1;
    if (slash == parent)
        parent[1] = 0;
    else
        *slash = 0;
    bind->fd = open(parent, O_RDONLY | O_DIRECTORY);
    if (bind->fd < 0) return -1;
    bind->fd = engine_fd_hoist(bind->fd);
    g_name_binds_count++;
    return 0;
}

static int name_binds_parse(const char *spec) {
    if (!spec || !spec[0]) return 0;
    char *copy = strdup(spec);
    if (!copy) return -1;
    char *save = NULL;
    for (char *record = strtok_r(copy, "\n", &save); record; record = strtok_r(NULL, "\n", &save))
        if (add_name_bind(record) != 0) {
            free(copy);
            return -1;
        }
    free(copy);
    return 0;
}

static void vol_handle_bind(struct vol *volume, const char *directory) {
    hl_host_result opened;
    if (volume == NULL) return;
    volume->handle = HL_HOST_HANDLE_INVALID;
    if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->open_relative == NULL ||
        directory == NULL)
        return;
    opened =
        g_host_services->file->open_relative(g_host_services->context, HL_HOST_HANDLE_CWD, directory, strlen(directory),
                                             HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY | HL_HOST_FILE_PATH_ONLY, 0, 0);
    if (opened.status == HL_STATUS_OK) volume->handle = opened.value;
}

// Materialize a volume's mount point (and every ancestor) as empty dirs in the writable rootfs/upper, the
// way Docker mkdir -p's each mount target inside the container rootfs. Without it a NESTED mount leaves its
// parent absent: `-v H:/x/y` makes `/x/y` resolve to the host dir, but `ls /x` ENOENTs because `/x` exists
// in no layer. Creating /x (and /x/y) in the upper lets the merged readdir list `/x` -> `y`; the mount
// itself still wins in jail_pick(), so `/x/y` shows the host files, not the empty placeholder. The rootfs
// is the per-container overlay upper (daemon) or the plain rootfs (manual) -- both writable & private.
// No-op until the rootfs is known (the bridge supplies HL_VOLUMES after container_init resolves g_rootfs_canon).
// A file mount's leaf is created as an empty placeholder FILE (not a dir) so a parent `ls` shows it as a
// file, exactly as Docker materializes a single-file bind target inside the rootfs.
static void vol_mkmountpoint(const char *guest, int isfile) {
    if (!g_rootfs_canon[0] || !guest || guest[0] != '/') return;
    char mp[4300];
    if ((size_t)snprintf(mp, sizeof mp, "%s%s", g_rootfs_canon, guest) >= sizeof mp) return;
    for (char *s = mp + g_rootfs_canon_len + 1; *s; s++)
        if (*s == '/') {
            *s = 0;
            hl_compat_mkdir(mp, 0755);
            *s = '/';
        }
    if (isfile) {
        int fd = open(mp, O_CREAT | O_RDONLY, 0644);
        if (fd >= 0) close(fd);
    } else
        hl_compat_mkdir(mp, 0755);
}

static int volume_hex(unsigned char value) {
    if (value >= '0' && value <= '9') return value - '0';
    if (value >= 'a' && value <= 'f') return value - 'a' + 10;
    if (value >= 'A' && value <= 'F') return value - 'A' + 10;
    return -1;
}

static int volume_unescape(char *value) {
    char *read = value;
    char *write = value;
    while (*read) {
        if (*read == '%') {
            int high = volume_hex((unsigned char)read[1]);
            int low = volume_hex((unsigned char)read[2]);
            if (high < 0 || low < 0 || (high == 0 && low == 0)) return -1;
            *write++ = (char)((high << 4) | low);
            read += 3;
        } else {
            *write++ = *read++;
        }
    }
    *write = 0;
    return 0;
}

static void add_vol(const char *spec) { // "[v2:][ro:]guestpath:hostdir" -> a confined bind-mount volume
    if (g_nvols >= HL_VOLUME_MAX) return;
    int escaped = 0;
    if (!strncmp(spec, "v2:", 3)) {
        escaped = 1;
        spec += 3;
    }
    // Optional read-only marker. A guest path always begins with '/', so a leading "ro:"/"rw:" token is
    // unambiguous; absent (the legacy `guest:host` form) it defaults to read-write -> byte-identical.
    int ro = 0;
    int preserve_link = 0;
    if (!strncmp(spec, "link:", 5)) {
        ro = 1;
        preserve_link = 1;
        spec += 5;
    } else if (!strncmp(spec, "ro:", 3)) {
        ro = 1;
        spec += 3;
    } else if (!strncmp(spec, "rw:", 3)) {
        spec += 3;
    }
    char tmp[4096];
    if (path_copy(tmp, sizeof tmp, spec) != 0) return;
    char *col = strchr(tmp, ':');
    if (!col || tmp[0] != '/') return;
    *col = 0;
    if (escaped && (volume_unescape(tmp) != 0 || volume_unescape(col + 1) != 0)) return;
    struct vol *v = &g_vols[g_nvols];
    v->ro = ro;
    if (path_copy(v->guest, sizeof v->guest, tmp) != 0) return;
    v->glen = strlen(v->guest);
    while (v->glen > 1 && v->guest[v->glen - 1] == '/')
        v->guest[--v->glen] = 0;
    if ((preserve_link ? canonicalize_link_path(col + 1, v->hcanon, sizeof v->hcanon)
                       : canonicalize_path(col + 1, v->hcanon, sizeof v->hcanon)) != 0)
        return;
    v->hlen = strlen(v->hcanon);
    struct stat hst;
    if ((preserve_link ? lstat(v->hcanon, &hst) : stat(v->hcanon, &hst)) == 0 && !S_ISDIR(hst.st_mode)) {
        // Single-file bind (regular file, but ALSO a socket / fifo / device): openat's jail base must be a
        // directory, so pin the source's PARENT dir as `fd` and route the exact mount point straight to
        // `hcanon`. Dropping the O_DIRECTORY requirement here is what lets a non-dir source register at all
        // (it ENOTDIRs otherwise -> the mount was silently lost). Matching on !S_ISDIR (not just S_ISREG)
        // is what makes a bind-mounted Unix socket — e.g. the docker daemon socket — resolve so the guest's
        // connect() dials the real host socket instead of ENOENT.
        v->isfile = 1;
        v->issymlink = preserve_link;
        char par[1024];
        snprintf(par, sizeof par, "%s", v->hcanon);
        char *sl = strrchr(par, '/');
        if (!sl) return;
        if (sl == par)
            par[1] = 0; // file directly under "/" -> parent is "/"
        else
            *sl = 0;
        if ((v->fd = open(par, O_RDONLY | O_DIRECTORY)) < 0) return;
        vol_handle_bind(v, par);
    } else if ((v->fd = open(v->hcanon, O_RDONLY | O_DIRECTORY)) < 0)
        return;
    else
        vol_handle_bind(v, v->hcanon);
    v->fd = engine_fd_hoist(v->fd); // keep this engine dir-fd out of the guest's low fd range
    g_nvols++;
    vol_mkmountpoint(v->guest, v->isfile);
}

// Runtime bind/tmpfs volume registration for mount(2): like add_vol but takes an already-resolved host
// backing (a real dir or a single file) + a guest target directly -- no "spec" string, so a guest path
// containing ':' can never be misparsed. g_nvols is published LAST (release), so a concurrent path
// resolver sees either the old count or a fully-populated entry (never a half-written one). The mount
// point (+ ancestors) is materialized in the writable upper so a parent `ls` shows it. 0 or -errno.
static int rt_add_vol(const char *guest, const char *hostsrc, int ro) {
    if (!guest || guest[0] != '/' || !hostsrc) return -EINVAL;
    if (g_nvols >= HL_VOLUME_MAX) return -ENOMEM;
    struct vol *v = &g_vols[g_nvols];
    memset(v, 0, sizeof *v);
    v->ro = ro ? 1 : 0;
    snprintf(v->guest, sizeof v->guest, "%s", guest);
    v->glen = strlen(v->guest);
    while (v->glen > 1 && v->guest[v->glen - 1] == '/')
        v->guest[--v->glen] = 0;
    if (canonicalize_path(hostsrc, v->hcanon, sizeof v->hcanon) != 0) {
        int e = errno;
        return e ? -e : -ENOENT;
    }
    v->hlen = strlen(v->hcanon);
    struct stat hst;
    if (stat(v->hcanon, &hst) == 0 && !S_ISDIR(hst.st_mode)) {
        // Single-file (or socket/fifo/device) bind: pin the source's PARENT dir as the jail base and route
        // the exact mount point straight to `hcanon` (see add_vol for the full rationale).
        v->isfile = 1;
        char par[1024];
        snprintf(par, sizeof par, "%s", v->hcanon);
        char *sl = strrchr(par, '/');
        if (!sl) return -EINVAL;
        if (sl == par)
            par[1] = 0;
        else
            *sl = 0;
        if ((v->fd = open(par, O_RDONLY | O_DIRECTORY)) < 0) return -errno;
        vol_handle_bind(v, par);
    } else if ((v->fd = open(v->hcanon, O_RDONLY | O_DIRECTORY)) < 0)
        return -errno;
    else
        vol_handle_bind(v, v->hcanon);
    v->fd = engine_fd_hoist(v->fd);
    vol_mkmountpoint(v->guest, v->isfile);
    __atomic_store_n(&g_nvols, g_nvols + 1, __ATOMIC_RELEASE); // publish the complete entry LAST
    return 0;
}

// Detach the bind/tmpfs volume mounted at EXACTLY `guest` (runtime umount2). Marks the slot dead (never
// compacted -> race-free) so the path reverts to the underlying rootfs/overlay. 0 if one was detached,
// -EINVAL if no volume is mounted there (Linux umount of a non-mount-point).
static int rt_del_vol(const char *guest) {
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE), hit = -EINVAL;
    for (int i = 0; i < nv; i++)
        if (!g_vols[i].dead && !strcmp(g_vols[i].guest, guest)) {
            g_vols[i].dead = 1;
            if (g_vols[i].handle != HL_HOST_HANDLE_INVALID && g_host_services && g_host_services->file &&
                g_host_services->file->close) {
                (void)g_host_services->file->close(g_host_services->context, g_vols[i].handle);
                g_vols[i].handle = HL_HOST_HANDLE_INVALID;
            }
            hit = 0;
        }
    return hit;
}

// Longest matching bind-mount volume for an absolute guest path (the DEEPEST mount wins, exactly as the
// kernel routes a path to the innermost mount), or -1 for the rootfs/overlay jail. Longest-prefix so a
// nested volume (`-v H1:/x/y -v H2:/x/y/z`) routes /x/y/z to the inner mount regardless of registration
// order; for non-nested volumes (no guest is a prefix of another) it is identical to a first-match scan.
static int jail_match(const char *abs) {
    int best = -1;
    size_t blen = 0;
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE);
    for (int i = 0; i < nv; i++) {
        if (g_vols[i].dead) continue; // runtime-umounted: no longer routes here
        char b = abs[g_vols[i].glen];
        // A projected symlink owns suffixes through its guest target; ordinary
        // single-file binds still match only their exact mount point.
        int hit = g_vols[i].isfile && !g_vols[i].issymlink ? (b == 0) : (b == '/' || b == 0);
        if (g_vols[i].glen > blen && hit && !strncmp(abs, g_vols[i].guest, g_vols[i].glen)) {
            best = i;
            blen = g_vols[i].glen;
        }
    }
    return best;
}

// Whether `abs` is an exact single-socket bind mount supplied by the host. Connections to these endpoints
// leave the engine process (Wayland, Docker, provider services, ...), so their SCM_RIGHTS records must contain
// only the descriptors requested by the public protocol. Engine-private descriptor metadata trailers are
// meaningful only when another engine endpoint receives and removes them.
static int jail_is_projected_socket(const char *abs) {
    int index = jail_match(abs);
    if (index < 0 || !g_vols[index].isfile || strcmp(abs, g_vols[index].guest) != 0) return 0;
    struct stat status;
    return stat(g_vols[index].hcanon, &status) == 0 && S_ISSOCK(status.st_mode);
}

// Basename of a file bind-mount's host source: the leaf to openat under the pinned parent-dir `fd`.
static const char *vol_fbase(int vi) {
    const char *sl = strrchr(g_vols[vi].hcanon, '/');
    return sl ? sl + 1 : g_vols[vi].hcanon;
}

// Pick the jail (rootfs or a volume) for an absolute guest path; *rel = the path within that jail.
static int jail_pick(const char *abs, const char **canon, size_t *clen, const char **rel) {
    int i = jail_match(abs);
    if (i >= 0) {
        if (canon) {
            *canon = g_vols[i].hcanon;
            *clen = g_vols[i].hlen;
        }
        *rel = abs[g_vols[i].glen] ? abs + g_vols[i].glen : "/";
        return g_vols[i].fd;
    }
    if (canon) {
        *canon = g_rootfs_canon;
        *clen = g_rootfs_canon_len;
    }
    *rel = abs;
    return g_root_fd;
}

// SECURE path resolution. confine() handles '..' lexically, but symlinks resolve in the kernel
// BELOW that layer (a mid-path symlink to '/' walks straight out), so lexical clamping is NOT a
// boundary. This realpath()s the deepest existing prefix (following ALL symlinks) and verifies
// the canonical result is inside g_rootfs_canon; anything that escapes is redirected to a
// guaranteed-nonexistent in-jail path (-> ENOENT). `nofollow` keeps the final component
// unresolved (for readlink/lstat). Returns 1 if inside the jail, 0 if an escape was blocked.
// ---- positive dentry/climb cache (dc_*; impl in fscache.c next to the rc_/oc_/updirneg caches) ----
// Memoizes confine_in_m's realpath climb per DIRECTORY: key = the exact pre-realpath host string
// (jail canon + normalized rel, final component peeled in nofollow mode); value = (canonical deepest
// EXISTING prefix, #trailing components missing). Epoch-gated on the container-shared g_res_epoch,
// hard-reset on fork/chroot (hl_fdcache_reset), and volumes are never cached. See the full
// correctness model at the impl. DC_KEYMAX bounds the fixed-size slots (longer paths bypass, safely).
#define DC_KEYMAX 320

// Core: confine `rel` within an explicit jail root (jcanon). Generalized from secure_resolve so the
// overlay can resolve the SAME guest path inside each layer's root, reusing the realpath boundary.
// `missing` (optional): the number of trailing DIRECTORY components that did NOT exist under the jail
// root (the climb-loop pops below). 0 => the parent chain fully exists. The overlay uses this to prove
// "this entry cannot exist in the upper" (and no whiteout/opaque marker can either) without extra probes.
static int confine_in_m(const char *jcanon, size_t jclen, const char *rel, char *out, size_t n, int nofollow,
                        int *missing) {
    if (missing) *missing = 0;
    char norm[4200];
    confine(rel, norm, sizeof norm);
    char h[8400];
    snprintf(h, sizeof h, "%s%s", jcanon, norm);
    char rem[4400] = "";
    // peel the final component, resolve its dir
    if (nofollow) {
        char *sl = strrchr(h, '/');
        if (sl && (size_t)(sl - h) >= jclen) {
            snprintf(rem, sizeof rem, "/%s", sl + 1);
            *sl = 0;
        }
        if (!h[0]) snprintf(h, sizeof h, "/");
    }
    // Dentry-cache fast path: `h` is exactly the string the climb below would hand to realpath() first,
    // so an epoch-valid entry replays the recorded outcome verbatim -- out = canon + the nmiss trailing
    // components the climb popped (a plain suffix of the key) + rem -- with ZERO realpath calls. In
    // nofollow mode the final component was already peeled into `rem` above, so all files in one
    // directory share the key (the per-DIRECTORY sharing a stat/open storm needs). Only rootfs/lower
    // jails are cached; a miss or an over-length path falls through to the untouched climb.
    int dcok = hl_fdcache_dentry_cacheable(jcanon);
    char hkey[DC_KEYMAX];
    if (dcok) {
        size_t hl = strlen(h);
        if (hl < sizeof hkey)
            memcpy(hkey, h, hl + 1);
        else
            dcok = 0;
    }
    if (dcok) {
        char dcanon[DC_KEYMAX];
        int k;
        if (hl_fdcache_dentry_lookup(hkey, dcanon, sizeof dcanon, &k)) {
            const char *p = hkey + strlen(hkey); // start of the k popped components ("" when k == 0)
            for (int i = 0; i < k; i++) {
                p--;
                while (p > hkey && *p != '/')
                    p--;
            }
            snprintf(out, n, "%s%s%s", dcanon, p, rem);
            if (missing) *missing = k;
            return 1;
        }
    }
    int pops = 0;
    for (;;) {
        char canon[4200];
        if (realpath(h, canon)) {
            int inside = strncmp(canon, jcanon, jclen) == 0 && (canon[jclen] == '/' || canon[jclen] == 0);
            if (!inside) {
                snprintf(out, n, "%s/.jail-escape-denied", jcanon);
                return 0;
            }
            snprintf(out, n, "%s%s", canon, rem);
            // Memoize the successful in-jail climb (canon was verified inside the jail just above);
            // escapes and exhausted climbs (the return-0 paths) are never cached.
            if (dcok) hl_fdcache_dentry_store(hkey, canon, pops);
            return 1;
        }
        // final missing? climb to the deepest existing dir
        char *sl = strrchr(h, '/');
        if (!sl || strlen(h) <= jclen) {
            snprintf(out, n, "%s/.jail-escape-denied", jcanon);
            return 0;
        }
        char tmp[4400];
        snprintf(tmp, sizeof tmp, "/%s%s", sl + 1, rem);
        snprintf(rem, sizeof rem, "%s", tmp);
        *sl = 0;
        pops++;
        if (missing) (*missing)++;
    }
}

static int confine_in(const char *jcanon, size_t jclen, const char *rel, char *out, size_t n, int nofollow) {
    return confine_in_m(jcanon, jclen, rel, out, n, nofollow, NULL);
}

// secure_resolve + two probe outputs the overlay's fast path uses (both optional):
//   `missing` -- trailing dir components of the path that do NOT exist under the chosen jail root
//                (see confine_in_m); lets overlay_lookup prove an upper entry/whiteout/opaque marker
//                cannot exist without paying the extra lstat probes.
//   `isvol`   -- the path routed to a bind-mount VOLUME jail, not the rootfs/overlay upper. Volume
//                backings are host-mutable (the user can create files from macOS at any time), so the
//                overlay's negative memo must never cache them (mirrors hl_fdcache_metadata_store's volume exclusion).
static int secure_resolve_probe(const char *guest, char *out, size_t n, int nofollow, int *missing, int *isvol) {
    if (isvol) *isvol = 0;
    if (missing) *missing = 0;
    // Normalize '.'/'//'/'..' and clamp at the ROOTFS root FIRST, then route. Jail selection must see the
    // post-`..` path: a `..` that pops above a volume's own root crosses the bind-mount boundary back to
    // the dir holding the mount point ("/x/y/.." -> "/x"), which lives in the rootfs/overlay jail, not the
    // volume. Routing the raw path would prefix-match "/x/y/.." to the volume and clamp `..` at the volume
    // root. confine() already collapses `..` lexically below (so this only changes WHICH jail is chosen,
    // not the symlink-via-realpath confinement) and never ascends past '/', so the result stays in rootfs.
    char cr[4200];
    if (g_chroot[0]) { // re-root under the guest's chroot first (no-op cost when no chroot is in effect)
        chroot_apply(guest, cr, sizeof cr);
        guest = cr;
    }
    char norm[4200];
    confine(guest, norm, sizeof norm);
    // Single-file bind-mount: the exact mount point maps straight to the bound host file (`hcanon` is the
    // realpath'd file, not a dir to walk). jail_match only matches a file vol on its exact path, so a hit
    // here IS that file -- emit it directly; confine_in would append rel ("/") and ENOTDIR on the file.
    int fvi = jail_match(norm);
    int exact_volume = fvi >= 0 && strcmp(norm, g_vols[fvi].guest) == 0;
    if (fvi >= 0 && g_vols[fvi].isfile && (!g_vols[fvi].issymlink || (nofollow && exact_volume))) {
        if (isvol) *isvol = 1;
        snprintf(out, n, "%s", g_vols[fvi].hcanon);
        return 1;
    }
    const char *jcanon;
    size_t jclen;
    const char *rel;
    // rootfs or a volume root (jcanon is absolute)
    jail_pick(norm, &jcanon, &jclen, &rel);
    if (isvol && jcanon != g_rootfs_canon) *isvol = 1; // jail_pick hands back the global array for rootfs
    return confine_in_m(jcanon, jclen, rel, out, n, nofollow, missing);
}

static int resolve_at(const char *guest, char *final, size_t fn, int nofollow);

static int secure_resolve(const char *guest, char *out, size_t n, int nofollow) {
    char final[512], parent[4200];
    int descriptor = resolve_at(guest, final, sizeof final, nofollow);
    if (descriptor >= 0) {
        int ok = hl_native_fd_path(descriptor, parent, sizeof parent) == 0 && path_join(out, n, parent, final) == 0;
        close(descriptor);
        if (ok) return 1;
    }
    return secure_resolve_probe(guest, out, n, nofollow, NULL, NULL);
}

#include "vfs/overlay.c"

static const struct hl_linux_vfs_namespace g_vfs_namespace = {
    g_rootfs_canon,
    &g_rootfs_canon_len,
    g_lower,
    &g_nlower,
};

// final NOT followed (readlink/lstat)
static const char *xlate(const char *p, char *buf, size_t n) {
    if (p && p[0] == '/' && (g_rootfs || jail_match(p) >= 0)) {
        secure_resolve(p, buf, n, 1);
        return buf;
    }
    return p;
}

// follow symlinks (open/stat/exec)
static const char *xresolve(const char *p, char *buf, size_t n) {
    if (p && p[0] == '/' && (g_rootfs || jail_match(p) >= 0)) {
        secure_resolve(p, buf, n, 0);
        return buf;
    }
    return p;
}

// Follow a guest-visible symlink chain while preserving its guest spelling.  Host-path resolution
// deliberately cannot represent synthetic targets such as /proc/self/fd/N: those names have no host
// inode, but open(2) still has to recognize them after traversing an image symlink such as
// /var/log/nginx/error.log -> /dev/stderr -> /proc/self/fd/2.  Return the last guest name even when it
// names a synthetic endpoint; callers classify that endpoint before attempting a host-backed open.
static const char *guest_symlink_target(const char *path, char *out, size_t capacity) {
    if (path == NULL || path[0] != '/' || capacity == 0) return path;
    char current[4200];
    if (path_copy(current, sizeof current, path) != 0) return path;
    for (int hop = 0; hop < 40; ++hop) {
        char host[4200];
        int present;
        if (g_nlower)
            present = overlay_resolve(current, host, sizeof host, 1);
        else {
            secure_resolve(current, host, sizeof host, 1);
            struct stat probe;
            present = lstat(host, &probe) == 0;
        }
        if (!present) break;
        struct stat metadata;
        if (lstat(host, &metadata) != 0 || !S_ISLNK(metadata.st_mode)) break;
        char target[4200];
        ssize_t length = readlink(host, target, sizeof target - 1);
        if (length <= 0) break;
        target[length] = 0;
        if (target[0] == '/') {
            if (path_copy(current, sizeof current, target) != 0) return path;
        } else {
            char directory[4200], joined[8400];
            if (path_copy(directory, sizeof directory, current) != 0) return path;
            char *separator = strrchr(directory, '/');
            if (separator != NULL)
                *separator = 0;
            else
                directory[0] = 0;
            if (snprintf(joined, sizeof joined, "%s/%s", directory, target) >= (int)sizeof joined ||
                path_copy(current, sizeof current, joined) != 0)
                return path;
        }
    }
    if (path_copy(out, capacity, current) != 0) return path;
    return out;
}

static int jail_at(int dirfd, const char *raw, char *final, size_t fn, int nofollow);

// Resolve an EXEC entrypoint (or PT_INTERP) to a host path, following symlinks the way the kernel
// would INSIDE the rootfs: an absolute symlink target (`/bin/sh -> /bin/busybox`) is rootfs-relative,
// not host-relative -- realpath() can't do this (it follows the target against the host root). Each
// hop is re-confined via secure_resolve, so an escaping link lands on .jail-escape-denied and fails.
static const char *xresolve_exec(const char *p, char *buf, size_t n) {
    if (!(p && p[0] == '/')) return p;
    // Bare launches can still have typed bind volumes.  They do not need the rootfs-specific absolute
    // symlink rewriting below, but must resolve inside the volume jail like every other followed lookup.
    if (!g_rootfs) {
        if (jail_match(p) < 0) return p;
        char final[512];
        int dfd = jail_at(-100, p, final, sizeof final, 0);
        if (dfd >= 0) {
            int fd = openat(dfd, final, O_RDONLY);
            close(dfd);
            if (fd >= 0) {
                if (hl_native_fd_path(fd, buf, n) == 0) {
                    close(fd);
                    return buf;
                }
                close(fd);
            }
        }
        return xresolve(p, buf, n);
    }
    char cur[4200];
    snprintf(cur, sizeof cur, "%s", p);
    // bounded symlink chain
    int hop;
    for (hop = 0; hop < 40; hop++) {
        char hb[4200];
        // host path, final component NOT followed
        secure_resolve(cur, hb, sizeof hb, 1);
        struct stat st;
        // missing -> let the loader report it
        if (lstat(hb, &st) != 0) break;
        if (!S_ISLNK(st.st_mode)) {
            snprintf(buf, n, "%s", hb);
            return buf;
            // real file -> done
        }
        char tgt[4200];
        ssize_t k = readlink(hb, tgt, sizeof tgt - 1);
        if (k <= 0) break;
        tgt[k] = 0;
        if (tgt[0] == '/')
            // absolute target: rootfs-relative
            snprintf(cur, sizeof cur, "%s", tgt);
        else {
            char d[4200];
            snprintf(d, sizeof d, "%s", cur);
            char *sl = strrchr(d, '/');
            if (sl) *sl = 0;
            char j[8400];
            snprintf(j, sizeof j, "%s/%s", d, tgt);
            if (path_copy(cur, sizeof cur, j) != 0) {
                if (n) buf[0] = 0;
                return buf;
            }
            // relative to its dir
        }
    }
    if (hop == 40)
        resolve_loop_mark(); // >40 symlink hops -> ELOOP (the guest-absolute self-loop the fallback can't follow)
    secure_resolve(cur, buf, n, 0);
    // fallback: realpath-confine the last hop
    return buf;
}

// Copy the container's PATH value (from the forwarded HL_GUEST_ENV, "K=V\nK=V") into `out`, or
// leave "" if PATH is unset/empty. This is the image-config PATH (e.g. golang's /usr/local/go/bin:...)
// merged with any `docker run/exec -e PATH=` override -- the authoritative search path for bare commands.
static void container_path_env(char *out, size_t n) {
    out[0] = 0;
    const char *ge = hl_process_guest_environment_get();
    if (!ge) return;
    for (const char *s = ge; *s;) {
        const char *e = s;
        while (*e && *e != '\n')
            e++;
        if (!strncmp(s, "PATH=", 5)) {
            size_t L = (size_t)(e - s) - 5;
            if (L >= n) L = n - 1;
            memcpy(out, s + 5, L);
            out[L] = 0;
            return;
        }
        s = *e ? e + 1 : e;
    }
}

// Resolve a bare program name (no '/') against the container PATH, like execvp -- docker passes `sh`,
// not `/bin/sh`. Returns a guest path ("/bin/sh") that exists in the rootfs, or `prog` unchanged.
// Searches the guest's ACTUAL PATH (image-config ENV + `-e PATH=`), split on ':' in order, so programs
// outside the FHS bin dirs (golang's /usr/local/go/bin, rust's /usr/local/cargo/bin) are found; falls
// back to the historical FHS defaults only when PATH is unset/empty (manual/direct mode, no daemon env).
static const char *find_in_path(const char *prog, char *gbuf, size_t n) {
    if (!prog || strchr(prog, '/')) return prog; // absolute/relative name: execvp bypasses PATH search
    char hb[4200];
    char pathenv[4200];
    container_path_env(pathenv, sizeof pathenv);
    if (pathenv[0]) {
        for (const char *s = pathenv;;) {
            const char *e = s;
            while (*e && *e != ':')
                e++;
            size_t dl = (size_t)(e - s);
            // An empty entry ("::", or a leading/trailing ':') means the cwd per POSIX; a relative dir is
            // likewise cwd-relative. Anchor both at the guest cwd so the result is a rootfs-absolute guest
            // path -- secure_resolve/xresolve_overlay then confine it inside the jail (an escaping dir lands
            // on .jail-escape-denied and simply fails to match), so this is safe.
            if (dl == 0) {
                if (path_join(gbuf, n, g_cwd, prog) != 0) continue;
            } else {
                char dir[4200];
                if (dl >= sizeof dir) dl = sizeof dir - 1;
                memcpy(dir, s, dl);
                dir[dl] = 0;
                if (dir[0] == '/') {
                    if (path_join(gbuf, n, dir, prog) != 0) continue;
                } else {
                    char rooted[8400];
                    if (path_join(rooted, sizeof rooted, g_cwd, dir) != 0 || path_join(gbuf, n, rooted, prog) != 0)
                        continue;
                }
            }
            // Search the FULL overlay (upper THEN lowers): a fresh container's upper is empty and the program
            // lives only in a read-only image lower, so a bare xresolve_exec would ENOENT every PATH dir.
            if (access(xresolve_overlay(gbuf, hb, sizeof hb), X_OK) == 0) return gbuf;
            if (!*e) break;
            s = e + 1;
        }
        return gbuf; // not found on PATH: let the loader report ENOENT against the last attempted path
    }
    // No container PATH forwarded: historical FHS defaults.
    static const char *const dirs[] = {"/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin",
                                       "/sbin",           "/bin",           NULL};
    for (int i = 0; dirs[i]; i++) {
        snprintf(gbuf, n, "%s/%s", dirs[i], prog);
        if (access(xresolve_overlay(gbuf, hb, sizeof hb), X_OK) == 0) return gbuf;
    }
    snprintf(gbuf, n, "/bin/%s", prog); // not found anywhere: let the loader report the error against /bin
    return gbuf;
}

#include "vfs/resolve.c"

// ===================== /proc/[self|pid] process introspection =====================
// macOS has no /proc, so the per-process files Linux servers read are synthesized here. All of these
// answer for the GUEST's own process only -- "self", the host pid, the container pid, or init's "1".

// Back a synthesized text file with an anonymous temp fd (mkstemp + immediate unlink): the fd holds the
// content, has no name, and behaves like an ordinary read-only file. Returns the fd, or -1 on error.
static int proc_text_fd(const char *buf, int n) {
    char tn[] = "/tmp/.hl-procXXXXXX";
    int fd = mkstemp(tn);
    if (fd >= 0) {
        unlink(tn);
        if (write(fd, buf, (size_t)n) < 0) {}
        lseek(fd, 0, SEEK_SET);
        if (fd < HL_NFD) g_proc_text_ro[fd] = 1;
    }
    return fd;
}

static char g_proc_text_desc[HL_NFD][64];

static int proc_text_fd_tagged(const char *buf, int n, const char *desc) {
    int fd = proc_text_fd(buf, n);
    if (fd >= 0 && fd < HL_NFD && desc) { snprintf(g_proc_text_desc[fd], sizeof g_proc_text_desc[fd], "%s", desc); }
    return fd;
}

static int proc_text_host_path(const char *path) {
    if (!path || !path[0]) return 0;
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    return !strncmp(base, ".hl-proc", 8);
}

// ---- guest comm + canonical-exe tracking (the /proc/self/exe surface) ----
// Linux sets a task's comm from the LAST component of the path PASSED to execve, BEFORE binfmt_script
// rewrites it -- so "./run.sh" keeps comm "run.sh" (not "sh"), and execve("/proc/self/exe") gets comm
// "exe" -- while /proc/<pid>/exe names the canonical FILE that was actually loaded. Track the two
// separately: set_guest_comm() records the exec-name at boot and on every execve; g_exe_path holds the
// canonical exe path (see exe_canon below).
static char g_comm_store[16];

static void set_guest_comm(const char *execpath) {
    const char *b = (execpath && execpath[0]) ? execpath : "init";
    const char *s = strrchr(b, '/');
    if (s) b = s + 1;
    snprintf(g_comm_store, sizeof g_comm_store, "%.15s", b[0] ? b : "init");
#if defined(__linux__)
    // Mirror onto the host task name so a peer reading /proc/<pid>/{stat,status,comm} sees this comm
    // (each guest process is its own host process; without this a peer read reports the engine binary).
    (void)prctl(PR_SET_NAME, (unsigned long)g_comm_store, 0, 0, 0);
#endif
}

// Set the task comm verbatim (not a basename): prctl(PR_SET_NAME) renames the running task, and Linux
// exposes that exact name through /proc/self/{comm,status:Name,stat:field2}. Keeps the procfs comm surface
// in sync with the prctl name so a rename after boot/exec is reflected everywhere.
// `leader` says whether the renamed task is the thread-group leader. Only the leader owns the PROCESS comm
// surface (/proc/<pid>/{comm,status,stat}); a worker renaming itself must not clobber it, or concurrent
// pthread_setname_np callers overwrite each other. Every task still renames its own HOST thread, which is
// what a peer's /proc/<pid>/task/<tid>/comm reads.
static void set_guest_comm_name(const char *name, int leader) {
    char resolved[16];
    snprintf(resolved, sizeof resolved, "%.15s", (name && name[0]) ? name : "init");
    if (leader) memcpy(g_comm_store, resolved, sizeof resolved);
#if defined(__linux__)
    (void)prctl(PR_SET_NAME, (unsigned long)resolved, 0, 0, 0); // keep the host task name in sync (see set_guest_comm)
#endif
}

// Normalize a guest path LEXICALLY: collapse "//" and "." components and fold ".." (clamped at "/").
// No fs access and no symlink resolution (exe_canon below adds that); always emits an absolute path.
static void path_norm_lex(const char *in, char *out, size_t n) {
    if (!n) return;
    size_t o = 0;
    const char *p = in;
    while (*p) {
        while (*p == '/')
            p++;
        if (!*p) break;
        const char *e = p;
        while (*e && *e != '/')
            e++;
        size_t cl = (size_t)(e - p);
        if (cl == 1 && p[0] == '.') {
            p = e;
            continue;
        }
        if (cl == 2 && p[0] == '.' && p[1] == '.') { // pop the previous component (stays at root)
            while (o > 0 && out[o - 1] != '/')
                o--;
            if (o > 0) o--;
            p = e;
            continue;
        }
        if (o + 1 + cl < n) {
            out[o++] = '/';
            memcpy(out + o, p, cl);
            o += cl;
        }
        p = e;
    }
    if (o == 0) out[o++] = '/';
    out[o < n ? o : n - 1] = 0;
}

// Canonical ABSOLUTE guest path of an executable -- what readlink("/proc/self/exe") must return. Joins
// a relative exec path to the guest cwd, folds "."/".."/"//", then resolves symlinks the way the
// kernel's d_path would: through the overlay to the backing host file, mapped back into the guest view
// (an exec of the /bin/sh -> busybox symlink reports /bin/busybox, exactly like Linux). glibc's
// static-pie startup ASSERTS on a non-absolute link value ("dl-origin.c: linkval[0]=='/'") and ld.so
// resolves $ORIGIN RUNPATHs through this path, so it must be absolute and canonical.
static void exe_canon(const char *guest, char *out, size_t n) {
    if (!guest || !guest[0]) {
        snprintf(out, n, "/");
        return;
    }
    char joined[8600];
    if (guest[0] != '/') {
        char cwd[4200];
        if (g_rootfs)
            snprintf(cwd, sizeof cwd, "%s", g_cwd[0] ? g_cwd : "/");
        else if (!getcwd(cwd, sizeof cwd))
            snprintf(cwd, sizeof cwd, "/");
        snprintf(joined, sizeof joined, "%s/%s", cwd, guest);
    } else
        snprintf(joined, sizeof joined, "%s", guest);
    char lex[4200];
    path_norm_lex(joined, lex, sizeof lex);
    // resolve symlinks to the backing file, then map back into the guest namespace
    char hb[4200];
    const char *hp = xresolve_overlay(lex, hb, sizeof hb); // confined resolution (upper, then lowers)
    if (!g_rootfs) {
        // bare mode: guest view == host view; host realpath IS the canonical answer
        char rp[4200];
        snprintf(out, n, "%s", realpath(hp, rp) ? rp : lex);
        return;
    }
    struct stat st;
    if (stat(hp, &st) != 0) { // unresolvable/dangling: keep the (absolute) lexical form
        snprintf(out, n, "%s", lex);
        return;
    }
    char gb[4200];
    int mapped = guest_from_host_raw(hp, gb, sizeof gb);
    // guest_from_host_raw answers "/" for a host path outside every layer (fail-safe); keep the lexical
    // guest path then rather than claiming the exe is "/".
    snprintf(out, n, "%s", (mapped <= 0 || (gb[0] == '/' && gb[1] == 0 && !(lex[0] == '/' && lex[1] == 0))) ? lex : gb);
}

// The guest task name (Linux comm, max 15 chars): the recorded exec-name (set_guest_comm), falling back
// to the basename of the running image (g_exe_path) for paths that never went through an exec hook.
static void proc_comm(char *out, size_t n) {
    if (g_comm_store[0]) {
        snprintf(out, n, "%s", g_comm_store);
        return;
    }
    const char *p = (g_exe_path && g_exe_path[0]) ? g_exe_path : "init";
    const char *base = strrchr(p, '/');
    base = base ? base + 1 : p;
    if (!base[0]) base = "init";
    snprintf(out, n, "%.15s", base);
}

// If `rp` addresses THIS process -- "/proc/self/<leaf>" or "/proc/<our-pid>/<leaf>" (host pid, container
// pid, or init's "1") -- return the <leaf> tail; else NULL. Foreign pids are not introspectable.
static const char *proc_self_leaf(const char *rp) {
    if (!rp) return NULL; // a NULL (bad) guest path resolves to NULL here; let the caller's host syscall EFAULT
    if (!strncmp(rp, "/proc/self/", 11)) return rp + 11;
    if (strncmp(rp, "/proc/", 6)) return NULL;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0 || q[i] != '/') return NULL;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    int pid = atoi(num);
    if (pid != (int)getpid() && pid != container_pid()) return NULL;
    return q + i + 1;
}

// One /proc/.../maps line for [lo,hi), plus the per-region smaps fields when `smaps` is set. The smaps
// fields are what redis's COW self-test parses; rss/dirty are reported equal to the region size (a
// resident mapping) so any field a parser looks up is present and consistent. Returns the length.
//
// The resident dirty bytes are reported under Shared_Dirty (not Private_Dirty): redis'
// checkLinuxMadvFreeForkBug forks and, in the CHILD, reads /proc/self/smaps Shared_Dirty for its
// MADV_FREE'd + rewritten private-anon page -- a value of 0 there is exactly its "buggy arm64 kernel"
// signature ("data corruption during background save", then it exits). A just-forked dirty COW page IS
// Shared_Dirty on real Linux (parent+child map it until COW breaks), so reporting the dirty bytes there
// both matches Linux for that query and clears the false positive. Rss stays == Shared_Clean +
// Shared_Dirty + Private_Clean + Private_Dirty (the kernel's invariant), so a summing parser is consistent.
static int proc_map_region_p(char *b, size_t n, unsigned long lo, unsigned long hi, const char *perms,
                             unsigned long long pgoff, unsigned dev_major, unsigned dev_minor, unsigned long long ino,
                             const char *name, int smaps) {
    unsigned long kb = (hi - lo) / 1024;
    // "Locked:" reports the mlock/mlockall'd bytes of THIS region (LTP mlock05 mlock()s a whole mapping
    // and reads its Locked back == the mapping size).
    unsigned long lockkb = (unsigned long)(hl_gmap_lock_region_bytes(lo, hi) / 1024);
    // A PROT_NONE region (perms "---p", e.g. the stack guard gap) is NOT resident: its resident/dirty
    // smaps fields must read 0 like the kernel, even though its virtual Size is the full span.
    int resident = (perms[0] != '-' || perms[1] != '-' || perms[2] != '-');
    unsigned long rkb = resident ? kb : 0;
    // Addresses use the kernel's own %08lx field width (min 8, NOT zero-padded to 12) so pmap/gdb and a
    // strict structural diff see the exact byte layout real Linux emits for the same address. A named row
    // reproduces seq_pad(): the name starts at offset 73 whatever the field widths, with at least one
    // separating space (measured against this host's kernel, every row type).
    int m = snprintf(b, n, "%08lx-%08lx %s %08llx %02x:%02x %llu ", lo, hi, perms, pgoff, dev_major, dev_minor, ino);
    if (name[0]) {
        if (m < 72) m += snprintf(b + m, (size_t)n - (size_t)m, "%*s", 72 - m, "");
        m += snprintf(b + m, (size_t)n - (size_t)m, " %s", name);
    }
    m += snprintf(b + m, (size_t)n - (size_t)m, "\n");
    if (smaps) {
        // The kernel's full per-region field set, in its order and its layout (name padded to 16, value
        // right-aligned at column 24). The set was short of Pss_Dirty/KSM/LazyFree/{Shmem,File}PmdMapped/
        // {Shared,Private}_Hugetlb/SwapPss/THPeligible/ProtectionKey, and a profiler that requires a field
        // it cannot find treats the region as unparsable rather than as a zero.
        // A FILE-backed region's resident pages are clean page-cache and carry no anonymous bytes -- report
        // them under Private_Clean with Anonymous 0, as the kernel does. The Shared_Dirty attribution above
        // is specific to private-anon COW and must not be extended to the image, or a parser summing
        // Anonymous over the regions counts the executable as anonymous memory.
        int fileback = ino != 0;
        unsigned long pclean = fileback ? rkb : 0, sdirty = fileback ? 0 : rkb, anon = fileback ? 0 : rkb;
        m += snprintf(b + m, (size_t)n - (size_t)m,
                      "Size:%19lu kB\nKernelPageSize:%9d kB\nMMUPageSize:%12d kB\n"
                      "Rss:%20lu kB\nPss:%20lu kB\nPss_Dirty:%14lu kB\n"
                      "Shared_Clean:%11d kB\nShared_Dirty:%11lu kB\n"
                      "Private_Clean:%10lu kB\nPrivate_Dirty:%10lu kB\nReferenced:%13lu kB\n"
                      "Anonymous:%14lu kB\nKSM:%20d kB\nLazyFree:%15d kB\nAnonHugePages:%10d kB\n"
                      "ShmemPmdMapped:%9d kB\nFilePmdMapped:%10d kB\n"
                      "Shared_Hugetlb:%9d kB\nPrivate_Hugetlb:%8d kB\n"
                      "Swap:%19d kB\nSwapPss:%16d kB\nLocked:%17lu kB\nTHPeligible:%12d\nProtectionKey:%10d\n",
                      kb, 4, 4, rkb, rkb, sdirty, 0, sdirty, pclean, 0UL, rkb, anon, 0, 0, 0, 0, 0, 0, 0, 0, 0, lockkb,
                      0, 0);
        // VmFlags follows the region's real protection (rd/wr/ex), not a fixed string: a PROT_NONE guard
        // claiming "rd wr" contradicts its own perms column. mr/mw/me are the may- bits, ac accountable.
        m += snprintf(b + m, (size_t)n - (size_t)m, "VmFlags:%s%s%s mr mw me ac \n", perms[0] == 'r' ? " rd" : "",
                      perms[1] == 'w' ? " wr" : "", perms[2] == 'x' ? " ex" : "");
    }
    return m;
}

// PT_LOAD segments of the main executable, read from the auxv the loader planted (AT_PHDR/AT_PHENT/
// AT_PHNUM) so /proc/self/maps shows the text as r-xp, rodata r--p, data rw-p -- the real per-segment
// protection, not a single flat rw-p span. Cross-arch (the Elf64_Phdr layout is arch-independent).
//
// Row geometry follows the kernel's ELF loader exactly, because that is what the file's readers model:
// a PT_LOAD is FILE-backed over [pgdown(vaddr), pgup(vaddr+filesz)) at file offset pgdown(p_offset), and
// the .bss remainder up to pgup(vaddr+memsz) is a separate ANONYMOUS row (offset 0, dev 00:00, no path).
struct mseg {
    uint64_t lo, hi, off;
    int prot;
    int file; // 1 -> carries the exe path + its dev:inode; 0 -> the anonymous .bss tail
};

// Guest -> host for a main-image address. A non-PIE ET_EXEC is linked low but mapped high (see
// g_nonpie_bias): every guest-visible image address, AT_PHDR included, is the LOW link value, and the bytes
// live at +bias. Dereferencing the guest value raw is what made this synthesis bail out entirely.
static uint64_t maps_image_host(uint64_t guest) {
    return (g_nonpie_lo && guest >= g_nonpie_lo && guest < g_nonpie_hi) ? guest + g_nonpie_bias : guest;
}

// The main image's program headers at their HOST location, with `phnum`/`phent` and the load bias that maps
// a link-time vaddr to the guest-visible one (0 for a non-PIE, whose guest addresses stay at the link
// values). NULL when the auxv is absent or the headers are no longer mapped -- callers then degrade rather
// than fault the engine.
static const uint8_t *maps_phdr_table(uint64_t *phnum_out, uint64_t *phent_out, uint64_t *bias_out) {
    uint64_t phdr = 0, phent = 0, phnum = 0;
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 3)
            phdr = v;
        else if (t == 4)
            phent = v;
        else if (t == 5)
            phnum = v;
    }
    if (!phdr || phent < 56 || phnum == 0 || phnum > 256) return NULL;
    /* Probe the HOST location of the headers: unprobed, a guest unmap would let any guest reading
     * /proc/self/maps SIGSEGV the engine. Bailing out only drops rows. */
    uint64_t hostphdr = maps_image_host(phdr);
    if (!hl_host_range_mapped((uintptr_t)hostphdr, (size_t)(phnum * phent))) return NULL;
    const uint8_t *ph = (const uint8_t *)(uintptr_t)hostphdr;
    // load bias: PT_PHDR's runtime address (AT_PHDR) minus its link vaddr; 0 for a non-PIE.
    uint64_t bias = 0;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type;
        memcpy(&type, e, 4);
        if (type == 6) {
            uint64_t pv;
            memcpy(&pv, e + 16, 8);
            bias = phdr - pv;
            break;
        } // PT_PHDR
    }
    *phnum_out = phnum;
    *phent_out = phent;
    *bias_out = bias;
    return ph;
}

static int maps_phdr_segs(struct mseg *seg, int maxn) {
    uint64_t phent = 0, phnum = 0, bias = 0;
    const uint8_t *ph = maps_phdr_table(&phnum, &phent, &bias);
    if (!ph) return 0;
    // PT_GNU_RELRO (0x6474e552): the prefix of the data segment the loader RE-PROTECTS read-only after
    // relocation. The kernel splits the writable load VMA there, so /proc/self/maps shows that prefix as
    // r--p then the rest rw-p. Toolchains that fold rodata into the r-xp text segment (aarch64 gcc default,
    // unlike x86 -z separate-code) otherwise expose NO r--p image row at all -- so replay the relro split.
    uint64_t relro_lo = 0, relro_hi = 0;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type;
        memcpy(&type, e, 4);
        if (type == 0x6474e552u) {
            uint64_t vaddr, memsz;
            memcpy(&vaddr, e + 16, 8);
            memcpy(&memsz, e + 40, 8);
            relro_lo = (bias + vaddr) & ~0xfffULL;
            relro_hi = (bias + vaddr + memsz + 0xfffULL) & ~0xfffULL;
            break;
        }
    }
    int nseg = 0;
#define MSEG_PUSH(LO, HI, PROT, OFF, FILE)                                                                             \
    do {                                                                                                               \
        if (nseg < maxn && (HI) > (LO)) {                                                                              \
            seg[nseg].lo = (LO);                                                                                       \
            seg[nseg].hi = (HI);                                                                                       \
            seg[nseg].prot = (PROT);                                                                                   \
            seg[nseg].off = (OFF);                                                                                     \
            seg[nseg].file = (FILE);                                                                                   \
            nseg++;                                                                                                    \
        }                                                                                                              \
    } while (0)
    for (uint64_t i = 0; i < phnum && nseg < maxn; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type, flags;
        uint64_t poff, vaddr, filesz, memsz;
        memcpy(&type, e, 4);
        memcpy(&flags, e + 4, 4);
        memcpy(&poff, e + 8, 8);
        memcpy(&vaddr, e + 16, 8);
        memcpy(&filesz, e + 32, 8);
        memcpy(&memsz, e + 40, 8);
        if (type != 1 || memsz == 0) continue; // PT_LOAD only
        uint64_t start = bias + vaddr;
        uint64_t lo = start & ~0xfffULL;
        uint64_t fhi = filesz ? ((start + filesz + 0xfffULL) & ~0xfffULL) : lo; // end of the file-backed part
        uint64_t hi = (start + memsz + 0xfffULL) & ~0xfffULL;
        uint64_t foff = poff - (start - lo); // the file offset the row's first page maps
        int prot = ((flags & 4) ? 4 : 0) | ((flags & 2) ? 2 : 0) | ((flags & 1) ? 1 : 0); // R|W|X
        // A writable segment whose start is covered by relro: emit the relro prefix as r--p, the rest rw-p.
        uint64_t rhi = relro_hi < fhi ? relro_hi : fhi;
        if ((prot & 2) && rhi > relro_lo && relro_lo >= lo && rhi > lo) {
            uint64_t rlo = relro_lo > lo ? relro_lo : lo;
            MSEG_PUSH(lo, rlo, prot, foff, 1);
            MSEG_PUSH(rlo, rhi, 4, foff + (rlo - lo), 1); // r--p (read-only after relocation)
            MSEG_PUSH(rhi, fhi, prot, foff + (rhi - lo), 1);
        } else {
            MSEG_PUSH(lo, fhi, prot, foff, 1);
        }
        MSEG_PUSH(fhi, hi, prot, 0, 0); // the .bss remainder: anonymous, like the kernel's set_brk()
    }
#undef MSEG_PUSH
    return nseg;
}

// mm->{start_code,end_code,start_data,end_data} as /proc/[pid]/stat fields 26/27/45/46, derived the way
// load_elf_binary derives them: the text bounds are the executable PT_LOAD's [vaddr, vaddr+filesz) and the
// data bounds the HIGHEST PT_LOAD's -- both un-rounded, unlike the maps rows. A backtrace/dladdr-alike asks
// "is this pc in the text?" here, so leaving them zero says the program has no code.
static void maps_code_data_bounds(uint64_t *sc, uint64_t *ec, uint64_t *sd, uint64_t *ed) {
    *sc = *ec = *sd = *ed = 0;
    uint64_t phent = 0, phnum = 0, bias = 0;
    const uint8_t *ph = maps_phdr_table(&phnum, &phent, &bias);
    if (!ph) return;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type, flags;
        uint64_t vaddr, filesz;
        memcpy(&type, e, 4);
        memcpy(&flags, e + 4, 4);
        memcpy(&vaddr, e + 16, 8);
        memcpy(&filesz, e + 32, 8);
        if (type != 1) continue; // PT_LOAD only
        uint64_t lo = bias + vaddr, hi = lo + filesz;
        if ((flags & 1) && (!*sc || lo < *sc)) *sc = lo;
        if ((flags & 1) && hi > *ec) *ec = hi;
        if (lo > *sd) *sd = lo;
        if (hi > *ed) *ed = hi;
    }
}

static void maps_perms_str(int prot, char *out) { // prot bits: 4=R 2=W 1=X
    out[0] = (prot & 4) ? 'r' : '-';
    out[1] = (prot & 2) ? 'w' : '-';
    out[2] = (prot & 1) ? 'x' : '-';
    out[3] = 'p';
    out[4] = 0;
}

// The guest brk arena bounds, defined (as file-scope statics) in syscall/dispatch.c which is #included
// AFTER this TU; a matching tentative declaration here lets the maps synth name the [heap] region. Both
// are static definitions of the same object in one translation unit, so this reads the live break.
static uint64_t brk_lo, brk_cur, brk_hi;

// One /proc/maps row, collected before emit so the whole file can be address-sorted (the kernel ALWAYS
// emits VMAs in ascending start order; pmap/gdb and jemalloc/glibc's sequential parse rely on it).
struct maprow {
    uint64_t lo, hi, off, ino;
    unsigned dev_major, dev_minor;
    char perms[5];
    const char *name;
};

static int maprow_cmp(const void *a, const void *b) {
    const struct maprow *p = (const struct maprow *)a, *q = (const struct maprow *)b;
    if (p->lo != q->lo) return p->lo < q->lo ? -1 : 1;
    // Equal starts: the NARROWER row first. A MAP_FIXED sub-mapping shares its start with the reservation
    // it replaced, and the overlap trim below keeps whichever row comes first -- which must be the
    // sub-mapping, exactly as the kernel's VMA split leaves it.
    return p->hi < q->hi ? -1 : p->hi > q->hi ? 1 : 0;
}

static int proc_fd_rebase(char *tgt, size_t capacity); // defined below; maps naming reuses /proc/self/fd's
static int synth_names_dir_open(const char *guestpath, const char *const *names, int kind);

// The maps rows for one read, plus the arena the file-backed rows' pathnames live in (`name` points into
// it). One table serves maps, smaps, numa_maps, smaps_rollup and map_files, so the five files cannot
// disagree about the guest's address space.
struct maptable {
    struct maprow *row;
    char *names;
    int n;
};

#define MAPTABLE_NAME_MAX 512 // per file-backed mapping; longer guest paths are dropped, never truncated

// The guest-visible path a file-backed mapping was created from. thread.c's g_filemap keeps a retained
// dup of the backing descriptor alive for the mapping's lifetime, so this resolves even after the guest
// closed its own fd. Rebased out of the rootfs/volume table exactly as /proc/self/fd is: an unrebasable
// host path is REFUSED (0), because an unnamed anon row is a loss of detail while a host path is a
// containment failure. Returns 1 on success.
static int filemap_guest_path(int fd, char *out, size_t n) {
    char hp[4200];
    if (fd < 0 || hl_native_fd_path(fd, hp, sizeof hp) != 0 || hp[0] != '/') return 0;
    int mapped = proc_fd_rebase(hp, sizeof hp);
    // Jailed and unrebased means the path lies outside every layer: refuse it. In bare mode the guest
    // namespace IS the host's, so the path is already the guest's own (same rule /proc/self/exe follows).
    if (mapped < 0 || (g_rootfs && mapped == 0)) return 0;
    if (strlen(hp) >= n) return 0;
    snprintf(out, n, "%s", hp);
    return 1;
}

// The g_filemap entry whose span contains [lo,hi), or -1. mmap registers one entry per file-backed
// mapping and filemap_unmap splits them on munmap/MAP_FIXED, so a containing entry names exactly one file.
static int filemap_row_index(uint64_t lo, uint64_t hi) {
    for (int i = 0; i < g_nfilemap; i++)
        if (lo >= g_filemap[i].lo && hi <= g_filemap[i].hi) return i;
    return -1;
}

// The guest protection registries thread.c keeps and mem.c maintains from mmap/mprotect: g_gna is the
// PROT_NONE intervals, g_gro the read-only (no PROT_WRITE) ones. They are the only live record of a guest's
// CURRENT protection -- the image rows are derived from the program headers, so without consulting these a
// guest that mprotects its own text keeps seeing the link-time permissions, and a mapping is rarely
// uniformly protected anyway (a glibc pthread stack is one mmap whose first page is the guard).
//
// Returns the intervals of `reg` overlapping [lo,hi), clipped and sorted ascending (insertion sort: the
// registries hold at most GNA_MAX entries and are not kept in order).
static int maps_prot_spans(const void *reg, int count, uint64_t lo, uint64_t hi, uint64_t *out, int maxn) {
    const struct {
        uint64_t lo, hi;
    } *iv = reg;

    int n = 0;
    for (int i = 0; i < count && n < maxn; i++) {
        uint64_t a = iv[i].lo > lo ? iv[i].lo : lo, b = iv[i].hi < hi ? iv[i].hi : hi;
        if (b <= a) continue;
        int at = n;
        while (at > 0 && out[2 * at - 2] > a) {
            out[2 * at] = out[2 * at - 2];
            out[2 * at + 1] = out[2 * at - 1];
            at--;
        }
        out[2 * at] = a;
        out[2 * at + 1] = b;
        n++;
    }
    return n;
}

// Whether `lo` sits inside one of `reg`'s intervals within [lo,hi), and how far the answer holds.
static int maps_prot_at(const void *reg, int count, uint64_t lo, uint64_t hi, uint64_t *edge) {
    uint64_t iv[64];
    int n = maps_prot_spans(reg, count, lo, hi, iv, 32), in = 0;
    for (int i = 0; i < n; i++) {
        if (iv[2 * i] <= lo && lo < iv[2 * i + 1]) {
            in = 1;
            if (iv[2 * i + 1] < *edge) *edge = iv[2 * i + 1];
        } else if (iv[2 * i] > lo && iv[2 * i] < *edge)
            *edge = iv[2 * i];
    }
    return in;
}

// The perms a row's [lo,hi) currently carries: `natural` (phdr-derived for the image, the mapping's own for
// a registry row) with the live protection registries applied. *until reports how far the answer holds, so
// the caller can split the row where the protection changes inside it.
static void maps_live_perms(uint64_t lo, uint64_t hi, const char *natural, char *out, uint64_t *until) {
    // The registries are written in TWO coordinate systems: the ELF loader (x86.c, elf.c) registers a
    // non-PIE image's segments at the HOST addresses its bytes occupy (+g_nonpie_bias), while mprotect
    // registers the GUEST address the guest passed. So query both and take the union -- reading only the
    // host fold missed the guest's own RELRO mprotect, reading only the guest address missed the loader's
    // whole image. (The mixed keying is itself a defect; see the non-PIE bias family.)
    uint64_t bias = maps_image_host(lo) - lo;
    uint64_t edge = hi, hedge = hi + bias;
    int in_none = maps_prot_at(g_gna, g_ngna, lo, hi, &edge);
    int in_ro = maps_prot_at(g_gro, g_ngro, lo, hi, &edge);
    if (bias) {
        in_none |= maps_prot_at(g_gna, g_ngna, lo + bias, hi + bias, &hedge);
        in_ro |= maps_prot_at(g_gro, g_ngro, lo + bias, hi + bias, &hedge);
        if (hedge - bias < edge) edge = hedge - bias;
    }
    snprintf(out, 5, "%s", natural);
    if (in_none) {
        out[0] = out[1] = out[2] = '-';
    } else if (in_ro) {
        out[1] = '-'; // read-only: keep whatever R/X the row already claims, drop W
        if (out[0] == '-') out[0] = 'r';
    } else if (out[0] == 'r') {
        // Readable and NOT in the read-only registry. The ELF loader registers every non-writable PT_LOAD
        // there at load time, so a phdr-derived row that has left it can only have been mprotect'd writable
        // by the guest -- which is the case a W^X audit or a JIT's own RW/RX toggle asks about, and which a
        // purely phdr-derived row answers with the stale link-time permission forever.
        out[1] = 'w';
    }
    *until = edge;
}

static void maptable_free(struct maptable *t) {
    free(t->row);
    free(t->names);
    t->row = NULL;
    t->names = NULL;
    t->n = 0;
}

// Collect the guest's address space as maps rows: the main image's PT_LOAD segments, the stack + its
// guard, the brk arena as [heap], and one row per remaining guest-map registry entry -- file-backed ones
// named from g_filemap. Sorted ascending and trimmed to be non-overlapping, the two invariants every
// consumer of this file (pmap, gdb, libunwind, jemalloc, glibc) assumes. Returns 0 on allocation failure.
static int maptable_build(struct maptable *t) {
    memset(t, 0, sizeof *t);
    // Capacity: main-exe PT_LOAD segs + stack + guard + heap split + one row per gmap entry, plus two per
    // protection-registry interval (a row splits at each). Dropping a row would truncate the file.
    size_t mapping_count = hl_gmap_count();
    int cap = (int)mapping_count + 4 * GNA_MAX + 32;
    struct maprow *rows = (struct maprow *)calloc((size_t)cap, sizeof *rows);
    char *names = (char *)calloc((size_t)(g_nfilemap > 0 ? g_nfilemap : 1), MAPTABLE_NAME_MAX);
    if (!rows || !names) {
        free(rows);
        free(names);
        return 0;
    }
    int nrow = 0;
    // An anonymous row: file offset 0, dev 00:00, inode 0 -- the tuple every maps parser uses to tell an
    // anonymous VMA from a file-backed one.
#define MAPROW_ADD(LO, HI, PERMS, NAME) MAPROW_ADD_F(LO, HI, PERMS, 0, 0, 0, 0, NAME)
#define MAPROW_ADD_F(LO, HI, PERMS, OFF, DMAJ, DMIN, INO, NAME)                                                        \
    do {                                                                                                               \
        if (nrow < cap && (HI) > (LO)) {                                                                               \
            rows[nrow].lo = (LO);                                                                                      \
            rows[nrow].hi = (HI);                                                                                      \
            rows[nrow].off = (OFF);                                                                                    \
            rows[nrow].dev_major = (DMAJ);                                                                             \
            rows[nrow].dev_minor = (DMIN);                                                                             \
            rows[nrow].ino = (INO);                                                                                    \
            snprintf(rows[nrow].perms, sizeof rows[nrow].perms, "%s", (PERMS));                                        \
            rows[nrow].name = (NAME);                                                                                  \
            nrow++;                                                                                                    \
        }                                                                                                              \
    } while (0)
    // The main executable's PT_LOAD segments, with their real per-segment protection (text r-xp, rodata
    // r--p, data rw-p) and the exe path as the mapping name -- read from the auxv program headers.
    struct mseg seg[32];
    int nseg = maps_phdr_segs(seg, 32);
    const char *hostexe = (g_exe_path && g_exe_path[0]) ? g_exe_path : "";
    // The pathname column is the path the GUEST knows: strip the rootfs prefix exactly as /proc/self/exe
    // does, else the two files disagree and the host's rootfs location leaks into the container.
    const char *exe = hostexe;
    if (g_rootfs && !strncmp(exe, g_rootfs_canon, g_rootfs_canon_len)) exe += g_rootfs_canon_len;
    if (!exe[0]) exe = hostexe;
    // dev:inode of the image, stat'd through the HOST path. A file-backed row must carry a non-zero pair:
    // that -- not the pathname, which the kernel also prints for [heap]/[stack] -- is how libunwind/ASan/
    // dladdr-alikes decide a row names an object on disk. Unstattable -> the anonymous tuple, not a lie.
    unsigned exe_dmaj = 0, exe_dmin = 0;
    unsigned long long exe_ino = 0;
    {
        struct stat es;
        if (hostexe[0] && stat(hostexe, &es) == 0) {
            exe_dmaj = (unsigned)major(es.st_dev);
            exe_dmin = (unsigned)minor(es.st_dev);
            exe_ino = (unsigned long long)es.st_ino;
        }
    }
    for (int i = 0; i < nseg; i++) {
        char perms[5];
        maps_perms_str(seg[i].prot, perms);
        if (seg[i].file)
            MAPROW_ADD_F(seg[i].lo, seg[i].hi, perms, seg[i].off, exe_dmaj, exe_dmin, exe_ino, exe);
        else
            MAPROW_ADD(seg[i].lo, seg[i].hi, perms, ""); // the .bss tail is anonymous
    }
    if (g_stack_hi) {
        unsigned long lo = (unsigned long)g_stack_lo, hi = (unsigned long)g_stack_hi;
        MAPROW_ADD(lo > 0x1000 ? lo - 0x1000 : 0, lo, "---p", ""); // guard gap below the stack
        MAPROW_ADD(lo, hi, "rw-p", "[stack]");
    }
    // The heap: emit exactly [brk_lo, brk_cur) as [heap], like the kernel (whose heap VMA ends at the
    // break). hl reserves a large brk arena up front (one gmap entry [brk_lo,brk_hi)); the reserved tail
    // above brk_cur is NOT part of the guest-visible heap, so it is dropped -- otherwise maps would show a
    // 256 MB anon region no real container has. jemalloc/glibc-malloc/redis/pmap look for this [heap] line.
    int have_heap = brk_hi && brk_cur > brk_lo;
    if (have_heap) MAPROW_ADD((unsigned long)brk_lo, (unsigned long)((brk_cur + 0xfff) & ~0xfffULL), "rw-p", "[heap]");
    for (size_t i = 0; i < mapping_count; i++) {
        hl_gmap_entry mapping;
        if (!hl_gmap_get(i, &mapping)) continue;
        // report the guest-VISIBLE length (glen) so a mapping's Size/Rss matches the guest's mmap length,
        // not hl's full extent including the 64 KB guard tail it reserves past anon maps (LTP mlock05 Rss).
        // Page-round the end as the kernel does: a VMA spans PAGE_ALIGN(len), so a guest that mmap'd a
        // non-multiple length must still see a page-granular row -- parsers divide the span by the page size.
        unsigned long lo = (unsigned long)mapping.address;
        unsigned long hi = (lo + (unsigned long)mapping.guest_length + 0xffful) & ~0xffful;
        if (g_stack_hi && lo >= (unsigned long)g_stack_lo && hi <= (unsigned long)g_stack_hi)
            continue; // already emitted as [stack]
        if (brk_hi && lo == (unsigned long)brk_lo)
            continue; // the brk arena -- rendered as [heap] above (tail beyond brk is not guest-visible)
        // skip a region already rendered as PT_LOAD segments (the image span the loader tracks as one entry).
        // For a non-PIE the loader's entry sits at the HIGH host address while the rows are at the guest link
        // addresses, so fold the entry back through the bias before comparing.
        int covered = 0;
        if (nseg > 0) {
            unsigned long glo = lo;
            if (g_nonpie_bias && lo >= g_nonpie_lo + g_nonpie_bias && lo < g_nonpie_hi + g_nonpie_bias)
                glo = lo - g_nonpie_bias;
            for (int s = 0; s < nseg; s++)
                if (glo >= seg[s].lo && glo < seg[s].hi) {
                    covered = 1;
                    break;
                }
        }
        if (covered) continue;
        // A file-backed mapping is named from g_filemap, which records the backing dev/inode/offset and
        // keeps a dup of the descriptor open. Without this every shared library -- ld.so included -- showed
        // as an unnamed anon rw-p row, so dladdr-alikes, libunwind and any W^X audit saw no objects at all.
        int fm = filemap_row_index(lo, hi);
        if (fm >= 0) {
            char *nm = names + (size_t)fm * MAPTABLE_NAME_MAX;
            if (!nm[0] && !filemap_guest_path(g_filemap[fm].fd, nm, MAPTABLE_NAME_MAX)) nm[0] = 0;
            MAPROW_ADD_F(lo, hi, "rw-p", g_filemap[fm].offset + (lo - g_filemap[fm].lo),
                         (unsigned)major((dev_t)g_filemap[fm].device), (unsigned)minor((dev_t)g_filemap[fm].device),
                         g_filemap[fm].inode, nm);
        } else
            MAPROW_ADD(lo, hi, "rw-p", "");
    }
    // Apply the live protection registries to every row, splitting where the protection changes inside one.
    // Image rows come from the program headers, so this is what makes a guest's own mprotect visible; the
    // registry rows have no protection of their own at all and would otherwise every one claim rw-p.
    for (int i = 0, collected = nrow; i < collected; i++) {
        uint64_t at = rows[i].lo, end = rows[i].hi;
        char natural[5];
        snprintf(natural, sizeof natural, "%s", rows[i].perms);
        int first = 1;
        while (at < end) {
            char perms[5];
            uint64_t until = end;
            maps_live_perms(at, end, natural, perms, &until);
            if (first) {
                rows[i].hi = until;
                snprintf(rows[i].perms, sizeof rows[i].perms, "%s", perms);
                first = 0;
            } else {
                MAPROW_ADD_F(at, until, perms, rows[i].ino ? rows[i].off + (at - rows[i].lo) : 0, rows[i].dev_major,
                             rows[i].dev_minor, rows[i].ino, rows[i].name);
            }
            at = until;
        }
    }
#undef MAPROW_ADD_F
#undef MAPROW_ADD
    qsort(rows, (size_t)nrow, sizeof *rows, maprow_cmp);
    // Ascending AND non-overlapping is the invariant every sequential parser relies on, and the two
    // sources (phdr segments, guest-map registry) can still collide -- a whole-span loader reservation
    // that a MAP_FIXED replaced in part, most of all. Clip each row to what the rows before it left free;
    // the narrower row sorted first, so the MAP_FIXED sub-mapping survives and the reservation yields, as
    // the kernel's VMA split leaves it.
    int keep = 0;
    uint64_t watermark = 0;
    for (int i = 0; i < nrow; i++) {
        if (rows[i].lo < watermark) {
            uint64_t shift = watermark - rows[i].lo;
            if (rows[i].hi <= watermark) continue; // fully swallowed
            rows[i].lo = watermark;
            if (rows[i].ino) rows[i].off += shift; // a file-backed row's offset tracks its start
        }
        watermark = rows[i].hi;
        rows[keep++] = rows[i];
    }
    t->row = rows;
    t->names = names;
    t->n = keep;
    return 1;
}

// Synthesize /proc/[pid]/maps (smaps=0) or /proc/[pid]/smaps (smaps=1). The [stack] line (with a guard
// line below it, as the kernel shows) is what glibc's pthread_getattr_np scans for; [heap] is what
// jemalloc/glibc-malloc/redis/pmap look for. Returns an anonymous fd holding the content, or -1 on error.
static int proc_maps_fd(int smaps) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char tn[] = "/tmp/.hl-procXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) {
        maptable_free(&t);
        return -1;
    }
    if (fd < HL_NFD) g_proc_text_ro[fd] = 1;
    unlink(tn);
    char b[5120]; // one row: the header line (a PATH_MAX pathname) plus a full smaps field block, whole --
                  // a truncated row would lose its newline and merge into the next one.
    for (int i = 0; i < t.n; i++) {
        int m = proc_map_region_p(b, sizeof b, t.row[i].lo, t.row[i].hi, t.row[i].perms, t.row[i].off,
                                  t.row[i].dev_major, t.row[i].dev_minor, t.row[i].ino, t.row[i].name, smaps);
        if (write(fd, b, (size_t)m) < 0) {}
    }
    maptable_free(&t);
    lseek(fd, 0, SEEK_SET);
    return fd;
}

// /proc/[pid]/numa_maps -- one line per VMA, ascending, "<start> <policy> [tag] <counters>". Unintercepted
// this fell through to the host and handed the guest the ENGINE's mappings: the engine binary's absolute
// host path, its load address, and every library the host process had open. A containment failure, not a
// completeness gap, so it is synthesized from the same row set maps/smaps use. The kernel prints a bare
// "<start> default" for a VMA with no resident pages, so a PROT_NONE guard needs no counters.
static int proc_numa_maps_fd(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char b[5120];
    char *out = NULL;
    int len = 0;
    for (int i = 0; i < t.n; i++) {
        const struct maprow *r = &t.row[i];
        unsigned long pages = (unsigned long)((r->hi - r->lo) / 4096);
        int resident = (r->perms[0] != '-' || r->perms[1] != '-' || r->perms[2] != '-');
        int m = snprintf(b, sizeof b, "%08lx default", (unsigned long)r->lo);
        if (r->name && !strcmp(r->name, "[heap]"))
            m += snprintf(b + m, sizeof b - (size_t)m, " heap");
        else if (r->name && !strcmp(r->name, "[stack]"))
            m += snprintf(b + m, sizeof b - (size_t)m, " stack");
        else if (r->ino && r->name && r->name[0]) {
            // The kernel escapes whitespace in the pathname as \040 here (numa_maps is space-delimited).
            m += snprintf(b + m, sizeof b - (size_t)m, " file=");
            for (const char *p = r->name; *p && m < (int)sizeof b - 8; p++) {
                if (*p == ' ')
                    m += snprintf(b + m, sizeof b - (size_t)m, "\\040");
                else
                    b[m++] = *p;
            }
            b[m] = 0;
        }
        if (resident && pages) {
            // Attribution matches smaps: a file-backed region's resident pages are page-cache (mapped=),
            // an anonymous one's are private dirty (anon=/dirty=). A summing reader must see both agree.
            if (r->ino)
                m += snprintf(b + m, sizeof b - (size_t)m, " mapped=%lu", pages);
            else
                m += snprintf(b + m, sizeof b - (size_t)m, " anon=%lu dirty=%lu", pages, pages);
            m += snprintf(b + m, sizeof b - (size_t)m, " active=0 N0=%lu kernelpagesize_kB=4", pages);
        }
        m += snprintf(b + m, sizeof b - (size_t)m, "\n");
        char *grown = (char *)realloc(out, (size_t)(len + m + 1));
        if (!grown) break;
        out = grown;
        memcpy(out + len, b, (size_t)m);
        len += m;
    }
    maptable_free(&t);
    int fd = proc_text_fd(out ? out : "", len);
    free(out);
    return fd;
}

// /proc/[pid]/smaps_rollup -- the whole-address-space totals, one "<first>-<last> ---p ... [rollup]" header
// plus the aggregate field block. Same leak as numa_maps unintercepted: the header alone published the
// engine's lowest and highest mapping. The fields are the per-region sums of what smaps already reports, so
// a reader that cross-checks rollup against smaps sees the two agree.
static int proc_smaps_rollup_fd(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    unsigned long rss = 0, pclean = 0, sdirty = 0, locked = 0;
    for (int i = 0; i < t.n; i++) {
        const struct maprow *r = &t.row[i];
        if (r->perms[0] == '-' && r->perms[1] == '-' && r->perms[2] == '-') continue; // PROT_NONE: not resident
        unsigned long kb = (unsigned long)((r->hi - r->lo) / 1024);
        rss += kb;
        if (r->ino)
            pclean += kb;
        else
            sdirty += kb;
        locked += (unsigned long)(hl_gmap_lock_region_bytes(r->lo, r->hi) / 1024);
    }
    unsigned long lo = t.n ? t.row[0].lo : 0, hi = t.n ? t.row[t.n - 1].hi : 0;
    maptable_free(&t);
    char b[2048];
    int m = snprintf(b, sizeof b, "%08lx-%08lx ---p 00000000 00:00 0", lo, hi);
    if (m < 72) m += snprintf(b + m, sizeof b - (size_t)m, "%*s", 72 - m, ""); // seq_pad: name at column 73
    m += snprintf(b + m, sizeof b - (size_t)m,
                  " [rollup]\nRss:%20lu kB\nPss:%20lu kB\nPss_Dirty:%14lu kB\nPss_Anon:%15lu kB\n"
                  "Pss_File:%15lu kB\nPss_Shmem:%14d kB\nShared_Clean:%11d kB\nShared_Dirty:%11lu kB\n"
                  "Private_Clean:%10lu kB\nPrivate_Dirty:%10d kB\nReferenced:%13lu kB\nAnonymous:%14lu kB\n"
                  "KSM:%20d kB\nLazyFree:%15d kB\nAnonHugePages:%10d kB\nShmemPmdMapped:%9d kB\n"
                  "FilePmdMapped:%10d kB\nShared_Hugetlb:%9d kB\nPrivate_Hugetlb:%8d kB\nSwap:%19d kB\n"
                  "SwapPss:%16d kB\nLocked:%17lu kB\n",
                  rss, rss, sdirty, sdirty, pclean, 0, 0, sdirty, pclean, 0, rss, sdirty, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                  locked);
    return proc_text_fd(b, m);
}

// The map_files/ entry name for a row: "<start>-<end>" in lowercase hex, unpadded -- the kernel's own
// naming. Only FILE-backed rows have one, which is what makes the directory a list of the objects the
// process has mapped. Returns 0 for an anonymous row.
static int map_files_name(const struct maprow *r, char *out, size_t n) {
    if (!r->ino || !r->name || !r->name[0]) return 0;
    snprintf(out, n, "%llx-%llx", (unsigned long long)r->lo, (unsigned long long)r->hi);
    return 1;
}

#define MAP_FILES_MAX 256

// /proc/[pid]/map_files/ -- a directory of "<start>-<end>" symlinks, one per file-backed VMA, each
// readlink'ing to the mapped path. Unintercepted this listed the ENGINE's own file mappings: its binary,
// the host loader and every host library, by absolute host path. Materialized as symlink placeholders;
// the targets are served by the readlink synth in fs.c (map_files_target).
static int proc_map_files_dir_open(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char (*names)[48] = (char (*)[48])calloc(MAP_FILES_MAX, 48);
    const char *ptr[MAP_FILES_MAX + 1];
    int n = 0;
    if (names)
        for (int i = 0; i < t.n && n < MAP_FILES_MAX; i++)
            if (map_files_name(&t.row[i], names[n], 48)) {
                ptr[n] = names[n];
                n++;
            }
    ptr[n] = NULL;
    maptable_free(&t);
    int fd = names ? synth_names_dir_open("/proc/self/map_files", ptr, 1) : -1;
    free(names);
    return fd;
}

// The readlink target of /proc/[pid]/map_files/<start>-<end>: the mapped path, or 0 if no file-backed row
// spans exactly that range (the kernel's names are exact VMA bounds, so a stale name must ENOENT).
static int map_files_target(const char *entry, char *out, size_t n) {
    struct maptable t;
    if (!entry || !entry[0] || !maptable_build(&t)) return 0;
    char nm[48];
    int found = 0;
    for (int i = 0; i < t.n && !found; i++)
        if (map_files_name(&t.row[i], nm, sizeof nm) && !strcmp(nm, entry)) {
            snprintf(out, n, "%s", t.row[i].name);
            found = 1;
        }
    maptable_free(&t);
    return found;
}

// /proc/[pid]/status -- the Name:/State:/VmRSS: key:value format (NOT the stat one-liner). VmRSS/VmSize
// reflect the cgroup memory charge so a reader sees a plausible footprint.
static unsigned long long self_rss_bytes(void); // defined after hl_get_procinfo (real engine resident floor)

// One current per-process footprint sample (resident + virtual, in bytes).
// /proc is live state on Linux: values may legitimately move between separate
// reads. Caching the first sample forever made statm claim that a faulted
// 32 MiB mapping consumed zero pages and that munmap never released anything.
static _Thread_local unsigned long long g_statm_charge;
static _Thread_local unsigned long long g_statm_rss;
static _Thread_local unsigned long long g_statm_vsize;
static _Thread_local int g_statm_sample;

static void self_vm_bytes(unsigned long long *rss, unsigned long long *vsize) {
    unsigned long long pgsz = (unsigned long long)hl_linux_host_page_size();
    unsigned long long r = (self_rss_bytes() / pgsz) * pgsz;
    unsigned long long v;
    if (r < pgsz) r = pgsz;
    v = g_mem_max ? (unsigned long long)g_mem_max : r + (4ull << 20);
    if (v < r) v = r;
    if (rss) *rss = r;
    if (vsize) *vsize = v;
}

static void self_vm_statm_bytes(unsigned long long *rss, unsigned long long *vsize) {
    self_vm_bytes(rss, vsize);
    g_statm_charge = (unsigned long long)atomic_load(&g_mem_charged);
    g_statm_rss = *rss;
    g_statm_vsize = *vsize;
    g_statm_sample = 1;
}

static void self_vm_status_bytes(unsigned long long *rss, unsigned long long *vsize) {
    unsigned long long charge = (unsigned long long)atomic_load(&g_mem_charged);
    if (g_statm_sample && g_statm_charge == charge) {
        *rss = g_statm_rss;
        *vsize = g_statm_vsize;
        g_statm_sample = 0;
        return;
    }
    self_vm_bytes(rss, vsize);
}

// /proc/[pid]/status Cpus_allowed / Cpus_allowed_list. A default container is allowed to run on ALL of its
// online CPUs (contiguous 0..N-1, N = container_online_cpus()), so this MUST agree with sched_getaffinity
// (dispatch.c cpu_online_mask) and nproc -- the old hardcoded "1"/"0" (CPU 0 only) contradicted both, and a
// reader like the JVM/tokio that cross-checks Cpus_allowed against availableProcessors saw an inconsistency
// no real container shows. Linux renders the mask as comma-separated 32-bit hex groups, most-significant
// first, no leading zeros on the top group (e.g. 18 CPUs -> "3ffff"); the list is the "0-(N-1)" range.
static void cpus_allowed_strs(char *mask, size_t mn, char *list, size_t ln) {
    int nc = container_online_cpus();
    if (nc < 1) nc = 1;
    uint32_t w[2] = {0, 0}; // container_online_cpus() caps at 64, so two 32-bit words cover every bit
    for (int c = 0; c < nc && c < 64; c++)
        w[c / 32] |= (uint32_t)1u << (c % 32);
    int hi = (nc - 1) / 32; // most-significant populated word
    int o = 0;
    for (int i = hi; i >= 0 && o < (int)mn; i--)
        o += snprintf(mask + o, mn - (size_t)o, i == hi ? "%x" : ",%08x", w[i]);
    if (nc == 1)
        snprintf(list, ln, "0");
    else
        snprintf(list, ln, "0-%d", nc - 1);
}

static int proc_status_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = pid == 1 ? 0 : (int)getppid();
    unsigned long long vm_rss, vm_vsize;
    self_vm_status_bytes(&vm_rss, &vm_vsize);
    unsigned long rss = (unsigned long)(vm_rss / 1024);
    unsigned long vsz = (unsigned long)(vm_vsize / 1024);
    if (vsz < rss) vsz = rss;
    unsigned long vmlck =
        (unsigned long)(hl_gmap_lock_total_bytes() / 1024); // mlock/mlockall'd bytes (LTP munlockall01)
    char groups[512]; // image-derived supplementary set (runc additionalGids), == getgroups(2)
    groups_status_str(groups, sizeof groups);
    char cpumask[40], cpulist[24];
    cpus_allowed_strs(cpumask, sizeof cpumask, cpulist, sizeof cpulist);
    // Identity must agree with getuid/geteuid/getgid/getegid (syscall/proc.c returns g_ruid/euid/…). A
    // hardcoded 0 made procfs report root even when the guest ran as a configured non-root uid/gid.
    cred_init(); // populate g_ruid/g_suid/… before we read them
    int uid_r = g_ruid, uid_e = cred_euid(), uid_s = g_suid, uid_fs = newfile_uid();
    int gid_r = g_rgid, gid_e = cred_egid(), gid_s = g_sgid, gid_fs = newfile_gid();
    int threads = thread_live_count(); // live pthreads (Threads: hid concurrency at a hardcoded 1)
    return snprintf(
        b, n,
        "Name:\t%s\nUmask:\t%04o\nState:\tR (running)\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
        "TracerPid:\t0\nUid:\t%d\t%d\t%d\t%d\nGid:\t%d\t%d\t%d\t%d\nFDSize:\t256\nGroups:\t%s\n"
        "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t%8lu kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
        "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
        "VmSwap:\t       0 kB\nThreads:\t%d\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
        "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
        // Capability + security context. A default `docker run` root container drops all but 14
        // caps: CapPrm/CapEff/CapBnd=00000000a80425fb, CapInh/CapAmb=0. NoNewPrivs follows the
        // sticky prctl flag; the docker default seccomp profile shows Seccomp:2/Seccomp_filters:1.
        // These MUST agree with capget(2) and PR_CAPBSET_READ (see syscall/proc.c). Speculation
        // lines match what the host kernel reports to a container.
        "CapInh:\t0000000000000000\nCapPrm:\t%016llx\nCapEff:\t%016llx\nCapBnd:\t%016llx\n"
        "CapAmb:\t0000000000000000\nNoNewPrivs:\t%d\nSeccomp:\t2\nSeccomp_filters:\t1\n"
        "Speculation_Store_Bypass:\tvulnerable\nSpeculationIndirectBranch:\tunknown\n"
        "Cpus_allowed:\t%s\nCpus_allowed_list:\t%s\nvoluntary_ctxt_switches:\t1\n"
        "nonvoluntary_ctxt_switches:\t0\n",
        comm, (unsigned)g_umask, pid, pid, ppid, uid_r, uid_e, uid_s, uid_fs, gid_r, gid_e, gid_s, gid_fs, groups, vsz,
        vsz, vmlck, rss, rss, rss, threads, (unsigned long long)HL_CAP_DEFAULT, (unsigned long long)g_cap_eff,
        (unsigned long long)g_cap_bnd, g_nnp, cpumask, cpulist);
}

// /proc/[pid]/stat -- the 52-field single line (pid (comm) state ppid ...). Field 23 = vsize (bytes),
// field 24 = rss (pages); the rest are plausible zeros. mongod's FTDC collector parses this.
static int proc_stat_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = pid == 1 ? 0 : (int)getppid();
    // Fields 5 (pgrp) and 6 (session) must match the guest's getpgrp()/getsid() -- for a forked child those
    // are its real host process group / session (init's real group/session mapped to guest 1), NOT the
    // child's own pid. The old code printed pid,pid, so a supervisor reconstructed a wrong process tree.
    int hpgrp = (int)getpgid(0), hsid = (int)getsid(0);
    int gpgrp = (g_init_hostpid && hpgrp == g_init_hostpid) ? 1 : hpgrp;
    int gsid = (g_init_hostpid && hsid == g_init_hostpid) ? 1 : hsid;
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long vm_rss, vm_vsize;
    self_vm_bytes(&vm_rss, &vm_vsize);
    unsigned long rss_pg = (unsigned long)(vm_rss / pgsz);
    unsigned long vsize = (unsigned long)vm_vsize;
    // 26/27 startcode/endcode, 45/46 start_data/end_data, 47 start_brk. Field 38 (exit_signal, SIGCHLD=17)
    // used to sit at 39: one zero too many followed it field 25, which shifted every field from 26 up by
    // one, so a reader indexing by position got the wrong column for all of them.
    uint64_t sc, ec, sd, ed;
    maps_code_data_bounds(&sc, &ec, &sd, &ed);
    return snprintf(b, n,
                    "%d (%s) R %d %d %d 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 100 %lu %lu 18446744073709551615 "
                    "%llu %llu 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 %llu %llu %llu 0 0 0 0 0\n",
                    pid, comm, ppid, gpgrp, gsid, vsize, rss_pg, (unsigned long long)sc, (unsigned long long)ec,
                    (unsigned long long)sd, (unsigned long long)ed, (unsigned long long)brk_lo);
}

// /proc/[pid]/environ -- the guest environment as NUL-separated KEY=VALUE. The authoritative source is
// HL_GUEST_ENV (the serialized guest environment, "K=V\nK=V"); absent it (direct mode), fall
// back to the same defaults build_stack hands the guest. Returns the byte count written.
// The running process's FINAL environment (container env + merged engine defaults), captured by build_stack
// -- the exact set placed on the guest stack, i.e. what hl_option_get() sees. /proc/self/environ was generated from
// the raw HL_GUEST_ENV instead, omitting the defaults (HOME/LANG/…) build_stack adds, so procfs disagreed
// with getenv. Using this blob makes them consistent. (build_stack in elf.c is compiled after vfs.c.)
static char g_self_environ[16384];
static int g_self_environ_len = 0;

static void set_guest_environ(const char *const *env, int envc) {
    int o = 0;
    for (int i = 0; i < envc && env && env[i]; i++) {
        int L = (int)strlen(env[i]);
        if (o + L + 1 > (int)sizeof g_self_environ) break;
        memcpy(g_self_environ + o, env[i], (size_t)L);
        o += L;
        g_self_environ[o++] = 0;
    }
    g_self_environ_len = o;
}

static int proc_environ_text(char *b, size_t n) {
    int o = 0;
    // Prefer the FINAL environment build_stack placed (== getenv), so procfs and getenv agree; this includes
    // the engine defaults (HOME/LANG/GLIBC_TUNABLES) the raw HL_GUEST_ENV path below omitted.
    if (g_self_environ_len > 0) {
        int L = g_self_environ_len > (int)n ? (int)n : g_self_environ_len;
        memcpy(b, g_self_environ, (size_t)L);
        return L;
    }
    const char *ge = hl_process_guest_environment_get();
    if (ge && ge[0]) {
        for (const char *s = ge; *s;) {
            const char *e = s;
            while (*e && *e != '\n')
                e++;
            int L = (int)(e - s);
            if (o + L + 1 > (int)n) break;
            memcpy(b + o, s, (size_t)L);
            o += L;
            b[o++] = 0;
            s = *e ? e + 1 : e;
        }
    } else {
        static const char *const def[] = {"PATH=/usr/bin:/bin", "HOME=/root", "LANG=C",
                                          NULL}; // no TERM (docker parity: unset unless -t)
        for (int i = 0; def[i]; i++) {
            int L = (int)strlen(def[i]);
            if (o + L + 1 > (int)n) break;
            memcpy(b + o, def[i], (size_t)L);
            o += L;
            b[o++] = 0;
        }
    }
    return o;
}

// A synthesized /proc/<pid>/fd directory is backed by a REAL temp dir of "N -> target" symlinks, so the
// guest's opendir/getdents enumerate it through the ordinary fdopendir path and readlink/lstat of an
// entry resolves the symlink. The dir persists until the guest closes its fd; we reap it lazily on the
// next open (when the tracked fd is no longer open) and fully at exit.
static struct {
    int fd;
    char path[32];
} g_procfd_dirs[64];

static void procfd_dir_empty(int fd) {
    int scan = dup(fd);
    if (scan < 0) return;
    DIR *d = fdopendir(scan);
    if (!d) {
        close(scan);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.' && (!e->d_name[1] || (e->d_name[1] == '.' && !e->d_name[2]))) continue;
        if (e->d_type == DT_DIR || e->d_type == DT_UNKNOWN) {
            int child = openat(fd, e->d_name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
            if (child >= 0) {
                procfd_dir_empty(child); // per-pid dirs nest a task/<tid>/ subtree
                close(child);
                (void)unlinkat(fd, e->d_name, AT_REMOVEDIR);
                continue;
            }
        }
        (void)unlinkat(fd, e->d_name, 0);
    }
    closedir(d);
}

static void procfd_dir_rm(const char *path) {
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (fd >= 0) {
        procfd_dir_empty(fd);
        close(fd);
    }
    (void)rmdir(path);
}

static void procfd_dirs_reap(int force) {
    for (int i = 0; i < 64; i++) {
        if (!g_procfd_dirs[i].path[0]) continue;
        if (force || fcntl(g_procfd_dirs[i].fd, F_GETFD) == -1) {
            procfd_dir_rm(g_procfd_dirs[i].path);
            g_procfd_dirs[i].path[0] = 0;
        }
    }
}

static void procfd_dirs_atexit(void) {
    procfd_dirs_reap(1);
}

static int proc_fd_dir_pid_open(int guest, int host);

// Build the temp dir of fd symlinks and return its fd. The guest fd numbers ARE the host fd numbers here,
// so this process's open fds are exactly the guest's; each link's target is the fd's path (or an
// anon_inode placeholder for a pipe/socket/eventfd with no path). -1 on error.
static int proc_fd_dir_open(void) {
    return proc_fd_dir_pid_open(0, (int)getpid());
}

static void proc_dir_register(int fd, const char *tmpl, const char *guestpath); // defined below (dir synth)

// Build the temp dir of /proc/self/fdinfo entries -- one REGULAR-file placeholder per open fd (content is
// served live by proc_open on the relative reopen). Linux exposes per-fd pos/flags/mnt_id here; runtimes
// read it for descriptor flags, eventfd counters, epoll details. Tagged "/proc/<pid>/fdinfo" so an
// openat(dirfd,"N") re-enters proc_open. Returns the fd, -1 on error.
static int proc_fdinfo_dir_open(const char *guestpath) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-fd-infoXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (eventfd_hidden_peer_fd(fd)) continue;
        if (fcntl(fd, F_GETFD) == -1) continue; // not open
        char p[96];
        snprintf(p, sizeof p, "%s/%d", tmpl, fd);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    int d = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (d < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(d, tmpl, guestpath);
    return d;
}

// The /proc/self/fdinfo/<N> body: Linux reports pos/flags/mnt_id (+ per-type extras). Returns the length or
// -1 if fd N is not open. `off` is the current file offset (lseek CUR), `flags` the O_* access/status bits.
static int proc_fdinfo_text(int fd, char *b, size_t n) {
    if (fd < 0 || fcntl(fd, F_GETFD) == -1) return -1; // not an open fd
    off_t pos = lseek(fd, 0, SEEK_CUR);
    if (pos < 0) pos = 0; // pipe/socket/eventfd: unseekable -> 0, like Linux
    int fl = fcntl(fd, F_GETFL);
    if (fl < 0) fl = 0;
    return snprintf(b, n, "pos:\t%lld\nflags:\t0%o\nmnt_id:\t1\nino:\t1\n", (long long)pos, (unsigned)fl);
}

static int proc_reg_read(int hostpid, char *comm, size_t csz, char *cmd, size_t cmdsz, int *cmdlen);

// The running process's own argv as a NUL-separated, NUL-terminated blob, captured by build_stack at every
// launch/exec. The registry (proc_reg_*) only exists in container/rootfs mode (g_init_hostpid); this global
// makes /proc/self/cmdline reflect the FULL argv even in bare mode -- where a fixed argv[0]-only fallback
// otherwise lost every argument after an exec with many args.
static char g_self_cmdline[8192];
static int g_self_cmdline_len = 0;

static void set_guest_cmdline(int argc, char *const argv[]) {
    int o = 0;
    int diagnostic_offset = 0;
    for (int i = 0; i < argc && argv && argv[i]; i++) {
        int L = (int)strlen(argv[i]);
        if (o + L + 1 > (int)sizeof g_self_cmdline) break;
        memcpy(g_self_cmdline + o, argv[i], (size_t)L);
        o += L;
        g_self_cmdline[o++] = 0;
        if (diagnostic_offset < (int)sizeof g_fault_cmdline - 1) {
            int remaining = (int)sizeof g_fault_cmdline - diagnostic_offset;
            int written = snprintf(g_fault_cmdline + diagnostic_offset, (size_t)remaining, "%s%s",
                                   diagnostic_offset ? " " : "", argv[i]);
            if (written < 0)
                diagnostic_offset = 0;
            else if (written >= remaining)
                diagnostic_offset = (int)sizeof g_fault_cmdline - 1;
            else
                diagnostic_offset += written;
        }
    }
    g_fault_cmdline[diagnostic_offset] = 0;
    g_self_cmdline_len = o;
}

// /proc/[pid]/cmdline -- the guest argv as NUL-separated, NUL-terminated arguments. Prefer the same
// published argv record used for peer /proc/<pid>/cmdline so self-introspection sees process arguments and
// service switches. Fall back to the captured argv blob (bare mode), then argv[0].
static int proc_cmdline_text(char *b, size_t n) {
    char comm[32], cmd[4096];
    int cl;
    if (proc_reg_read((int)getpid(), comm, sizeof comm, cmd, sizeof cmd, &cl) && cl > 0) {
        int L = cl > (int)n ? (int)n : cl;
        memcpy(b, cmd, (size_t)L);
        if (L == 0 || b[L - 1] != 0) {
            if (L < (int)n)
                b[L++] = 0;
            else
                b[L - 1] = 0;
        }
        return L;
    }
    if (g_self_cmdline_len > 0) { // bare mode: the captured argv (all of it, not just argv[0])
        int L = g_self_cmdline_len > (int)n ? (int)n : g_self_cmdline_len;
        memcpy(b, g_self_cmdline, (size_t)L);
        if (b[L - 1] != 0) b[L - 1] = 0;
        return L;
    }
    const char *p = (g_exe_path && g_exe_path[0]) ? g_exe_path : "init";
    int L = (int)strlen(p);
    if (L + 1 > (int)n) L = (int)n - 1;
    memcpy(b, p, (size_t)L);
    b[L] = 0; // cmdline is NUL-terminated (a single empty-tail arg, exactly as the kernel emits)
    return L + 1;
}

// /proc/[pid]/comm -- the task name (Linux comm: basename of the image, max 15 chars) plus a newline.
static int proc_comm_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    return snprintf(b, n, "%s\n", comm);
}

// Append the container's live bind-mount volumes (`-v`/`--mount`/`--tmpfs`) to a mount table. runc lists
// every bind as its own mount line; without them findmnt/df/JVM mount discovery see a namespace that omits
// the guest's binds. `fstab` picks the /proc/mounts (fstab, 6-field) form vs the /proc/self/mountinfo form.
// Single-file binds are skipped so the table shows only
// real directory mount points. Continues from byte `off`; returns the new length (never exceeds `cap-1`).
static size_t mount_binds_append(char *b, size_t cap, size_t off, int fstab) {
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE);
    int id = 100;
    for (int i = 0; i < nv; i++) {
        if (g_vols[i].dead || g_vols[i].isfile) continue;
        if (off + 1 >= cap) break;
        const char *ro = g_vols[i].ro ? "ro" : "rw";
        int w = fstab ? snprintf(b + off, cap - off, "/dev/root %s ext4 %s,relatime 0 0\n", g_vols[i].guest, ro)
                      : snprintf(b + off, cap - off, "%d 23 254:1 / %s %s,relatime - ext4 /dev/root %s\n", id++,
                                 g_vols[i].guest, ro, ro);
        if (w < 0 || (size_t)w >= cap - off) break; // truncated -> stop before overflowing
        off += (size_t)w;
    }
    return off;
}

// /proc/[pid]/mountinfo -- the mounted-filesystem table df/findmnt parse, and which the JVM scans to locate
// the cgroup mount. The rootfs is a single overlay mount at "/"; the pseudo-filesystems (proc, sysfs, the
// cgroup2 hierarchy, devtmpfs) round it out so a reader looking up any of these mount points finds a
// plausible, well-formed line. Field layout: id parent maj:min root mountpoint opts - fstype src superopts.
static int proc_mountinfo_text(char *b, size_t n) {
    // Field layout: id parent maj:min root mountpoint opts - fstype src superopts. The pseudo-mounts and
    // their PARENT ids mirror a real runc/OrbStack container exactly (verified vs the docker oracle): the
    // /dev tmpfs (25) parents /dev/pts, /dev/mqueue and /dev/shm; /sys (28) parents the cgroup2 leaf.
    //  - /sys is READ-ONLY (ro on both the line flags and the sysfs superblock) -- runc binds it ro.
    //  - /dev tmpfs carries size=65536k,mode=755 (docker's default 64M /dev).
    //  - /dev/pts devpts carries gid=5,mode=620,ptmxmode=666 (the devpts mount opts every container shows).
    //  - /dev/shm is its OWN tmpfs mount with src name "shm" (glibc shm_open/DSM back onto it); size=65536k
    //    is docker's default 64M (the host may enlarge it -- size is a host-variant field).
    //  - cgroup2 leaf is ro with src "cgroup" + nsdelegate (JVM/systemd v2 detection keys on this line).
    int len =
        snprintf(b, n,
                 "23 0 0:24 / / rw,relatime - overlay overlay rw\n"
                 "24 23 0:25 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n"
                 "25 23 0:26 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755\n"
                 "26 25 0:27 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666\n"
                 "27 23 0:28 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro\n"
                 "28 27 0:29 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate\n"
                 "29 25 0:30 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw\n"
                 "30 25 0:31 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k\n");
    if (len < 0 || (size_t)len >= n) return len;
    return (int)mount_binds_append(b, n, (size_t)len, 0);
}

// /proc/[pid]/mountstats -- the NFS-oriented per-mount statistics file. It fell through to the host, which
// published the entire HOST mount table (block-device names, docker overlay2 hashes, /run/user paths) to
// any guest that read it, while mounts and mountinfo next to it were both intercepted. Only the
// "device X mounted on Y with fstype Z" header lines apply to a container with no NFS mount; derive them
// from the same table mountinfo emits so the three files agree.
static int proc_mountstats_text(char *b, size_t n) {
    char mi[8192];
    int len = proc_mountinfo_text(mi, sizeof mi);
    if (len < 0) return -1;
    int o = 0;
    for (char *line = mi, *end = mi + len; line < end;) {
        char *nl = memchr(line, '\n', (size_t)(end - line));
        if (!nl) break;
        *nl = 0;
        // mountinfo fields: id parent maj:min root MOUNTPOINT opts - FSTYPE SRC superopts.
        char *f[11];
        int nf = 0;
        for (char *tok = strtok(line, " "); tok && nf < 11; tok = strtok(NULL, " "))
            f[nf++] = tok;
        if (nf >= 10) o += snprintf(b + o, n - (size_t)o, "device %s mounted on %s with fstype %s\n", f[8], f[4], f[7]);
        line = nl + 1;
        if ((size_t)o + 128 >= n) break;
    }
    return o;
}

// ================= REAL /proc process table (top/htop/ps) =====================================
// hl's process model: every guest process is its OWN host (macOS) process running this DBT; the
// container init is guest pid 1 (g_init_hostpid<->1), children keep their host pid as the guest pid
// (getpid() returns exactly that). macOS has no /proc, and one DBT process cannot see another's
// address space, so we (1) keep a tiny on-disk REGISTRY where each container process publishes its
// guest identity (comm + full argv), keyed by a per-container tmp dir, and (2) read LIVE per-process
// stats (rss, cpu time, state, ppid) from the host system interface. The union -- registry identity +
// native-process liveness -- lets any process (e.g. `ps`) enumerate the whole container
// and synthesize /proc/<pid>/{stat,status,cmdline,comm} for its peers, with GUEST pids throughout.
#include "../../host/system.h"

// ABI9 gives every launch an opaque ownership domain independent of networking, hostname, and filesystem
// generation. It is inherited in process memory across every guest fork and survives guest exec. Older
// direct-mode entry points retain the namespace/session fallback until they are removed.
static void proc_reg_key(char *out, size_t n) {
    const char *k = hl_option_get("HL_PROCESS_DOMAIN");
    if (k && strlen(k) == 32) {
        snprintf(out, n, "/tmp/.hl-domain.%s", k);
        return;
    }
    k = hl_option_get("HL_NETNS");
    if (!k || !k[0]) k = hl_option_get("HL_HOSTNAME");
    if (k && k[0]) {
        char s[48];
        int o = 0;
        for (const char *p = k; *p && o < 47; p++)
            if ((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9')) s[o++] = *p;
        s[o] = 0;
        if (o) {
            snprintf(out, n, "/tmp/.hl-pids.%s", s);
            return;
        }
    }
    snprintf(out, n, "/tmp/.hl-pids.s%d", (int)getsid(0));
}

/*
 * One activation may share HL_PROCESS_DOMAIN with other launches in the same
 * container. HL_LAUNCH_DOMAIN is its narrower, activation-owned tree identity.
 * Membership is a birth record only: /proc presentation continues to use the
 * container registry, while activation teardown needs only a PID-reuse-safe
 * list of processes to terminate.
 */
static int launch_reg_key(char *out, size_t n) {
    const char *key = hl_option_get("HL_LAUNCH_DOMAIN");
    size_t index;
    if (!key || strlen(key) != 32) return 0;
    for (index = 0; index < 32; ++index)
        if (!((key[index] >= '0' && key[index] <= '9') || (key[index] >= 'a' && key[index] <= 'f'))) return 0;
    snprintf(out, n, "/tmp/.hl-domain.%s", key);
    return 1;
}

static char g_launch_reg_birth_file[160];

static void launch_reg_publish(int hostpid, int remember) {
    char dir[80], birth[32], path[160];
    hl_host_process_info process;
    if (hostpid <= 0 || !launch_reg_key(dir, sizeof dir) || !hl_host_process_read(hostpid, &process)) return;
    hl_compat_mkdir(dir, 0777);
    int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
    snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
    if (size > 0 && hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size) == 0 && remember)
        snprintf(g_launch_reg_birth_file, sizeof g_launch_reg_birth_file, "%s", path);
}

static void launch_reg_unlink(void) {
    if (!g_launch_reg_birth_file[0]) return;
    (void)hl_host_file_unlink(&g_jit_services, g_launch_reg_birth_file);
    g_launch_reg_birth_file[0] = 0;
}

/* Linux tears down every remaining member of a PID namespace when its init
 * exits.  Each retained-C launch is one such guest domain even though its host
 * processes may escape the initial session with setsid().  Kill only members
 * whose recorded birth identity still matches the live host process, so a
 * recycled host pid can never inherit authority from a stale record.  Repeat
 * until two scans are empty (bounded at two seconds) to close the child-
 * publication race: fork publishes the birth record in the parent before
 * returning to guest code. */
static void launch_reg_terminate_peers(void) {
    char directory[80];
    unsigned empty = 0;
    if (!g_init_hostpid || getpid() != g_init_hostpid || !launch_reg_key(directory, sizeof directory)) return;
    for (unsigned round = 0; round < 200; ++round) {
        unsigned live = 0;
        DIR *entries = opendir(directory);
        if (entries == NULL) return;
        struct dirent *entry;
        while ((entry = readdir(entries)) != NULL) {
            char *end;
            char path[160];
            char text[32];
            long raw;
            uint64_t expected;
            hl_host_process_info process;
            if (entry->d_name[0] != 'b' || entry->d_name[1] < '1' || entry->d_name[1] > '9') continue;
            errno = 0;
            raw = strtol(entry->d_name + 1, &end, 10);
            if (errno != 0 || *end != 0 || raw <= 0 || raw > INT32_MAX || raw == (long)getpid()) continue;
            snprintf(path, sizeof path, "%s/b%ld", directory, raw);
            int descriptor = open(path, O_RDONLY | O_CLOEXEC);
            if (descriptor < 0) {
                (void)unlink(path);
                continue;
            }
            ssize_t count;
            do {
                count = read(descriptor, text, sizeof text - 1);
            } while (count < 0 && errno == EINTR);
            (void)close(descriptor);
            if (count <= 0) {
                (void)unlink(path);
                continue;
            }
            text[count] = 0;
            errno = 0;
            char *birth_end;
            expected = strtoull(text, &birth_end, 10);
            if (errno != 0 || birth_end == text || (*birth_end != '\n' && *birth_end != 0) || expected == 0 ||
                !hl_host_process_read(raw, &process) || process.start_time_ns != expected) {
                (void)unlink(path);
                continue;
            }
            ++live;
            (void)kill((pid_t)raw, SIGKILL);
            (void)unlink(path);
        }
        (void)closedir(entries);
        if (live == 0) {
            if (++empty == 2) {
                (void)rmdir(directory);
                return;
            }
        } else {
            empty = 0;
        }
        (void)poll(NULL, 0, 10);
    }
}

// This process's own registry file (unlinked on exit; the exit_group path calls proc_reg_unlink since
// _exit bypasses atexit). Stale files from a crash are pruned lazily by the enumerator (dead-pid check).
static char g_reg_file[128];
static char g_reg_exe_file[128];   // sibling "x<pid>" record: the canonical exe path (for /proc/<pid>/exe)
static char g_reg_birth_file[160]; // sibling "b<pid>": native start time, preventing PID-reuse kills
static char g_reg_last_buf[4096];
static int g_reg_last_len;
static char g_reg_last_exe[4200];

static void proc_reg_unlink(void) {
    launch_reg_unlink();
    if (g_reg_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_file);
        g_reg_file[0] = 0;
    }
    if (g_reg_exe_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_exe_file);
        g_reg_exe_file[0] = 0;
    }
    if (g_reg_birth_file[0]) {
        (void)hl_host_file_unlink(&g_jit_services, g_reg_birth_file);
        g_reg_birth_file[0] = 0;
    }
}

static void proc_reg_write_files(const char *dir, const char *buf, int len, const char *exe) {
    char tmp[144];
    snprintf(tmp, sizeof tmp, "%s/.t%d", dir, (int)getpid());
    if (hl_host_file_store(&g_jit_services, tmp, 0644, buf, (size_t)len) != 0) return;
    char final[128];
    snprintf(final, sizeof final, "%s/%d", dir, (int)getpid());
    if (hl_host_file_rename(&g_jit_services, tmp, final) == 0)
        snprintf(g_reg_file, sizeof g_reg_file, "%s", final);
    else
        (void)hl_host_file_unlink(&g_jit_services, tmp);
    // Publish the CANONICAL exe path as a sibling "x<pid>" record so a PEER process can serve
    // readlink("/proc/<pid>/exe") for this one (`ls -l /proc/<pid>`, ps tooling). The non-digit-leading
    // name keeps it invisible to the pid enumerators (proc_reg_count / the /proc listing digit scan).
    if (exe && exe[0] == '/') {
        char xtmp[152], xfin[144];
        snprintf(xtmp, sizeof xtmp, "%s/.xt%d", dir, (int)getpid());
        snprintf(xfin, sizeof xfin, "%s/x%d", dir, (int)getpid());
        if (hl_host_file_store(&g_jit_services, xtmp, 0644, exe, strlen(exe)) == 0) {
            if (hl_host_file_rename(&g_jit_services, xtmp, xfin) == 0) {
                if (path_copy(g_reg_exe_file, sizeof g_reg_exe_file, xfin) != 0)
                    (void)hl_host_file_unlink(&g_jit_services, xfin);
            } else
                (void)hl_host_file_unlink(&g_jit_services, xtmp);
        }
    }
    {
        hl_host_process_info process;
        char birth[32], path[144];
        if (hl_host_process_read(getpid(), &process)) {
            int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
            snprintf(path, sizeof path, "%s/b%d", dir, (int)getpid());
            if (size > 0 && hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size) == 0)
                snprintf(g_reg_birth_file, sizeof g_reg_birth_file, "%s", path);
        }
    }
}

// Publish THIS process's guest identity: "<comm>\n" then the full argv NUL-separated. Written to a temp
// name + renamed for an atomic publish. Called at startup and after each guest execve (comm changes).
static void proc_reg_publish(const char *exe, int argc, char *const argv[]) {
    launch_reg_publish((int)getpid(), 1);
    if (!g_init_hostpid) return; // process table is a container feature
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    static int reg = 0;
    if (!reg) {
        atexit(proc_reg_unlink);
        reg = 1;
    }
    char comm[16];
    proc_comm(comm, sizeof comm); // the recorded exec-name (set_guest_comm), NOT basename(exe): a script
                                  // exec keeps the script's name even though `exe` is the interpreter
    char buf[4096];
    int o = snprintf(buf, sizeof buf, "%s\n", comm), wrote = 0;
    if (argv)
        for (int i = 0; i < argc && argv[i] && o < (int)sizeof buf - 1; i++) {
            int L = (int)strlen(argv[i]);
            if (o + L + 1 > (int)sizeof buf) break;
            memcpy(buf + o, argv[i], (size_t)L);
            o += L;
            buf[o++] = 0;
            wrote = 1;
        }
    if (!wrote) { // no argv retained -> the exe path is the single cmdline arg (matches proc_cmdline_text)
        const char *e = (exe && exe[0]) ? exe : "init";
        int L = (int)strlen(e);
        if (o + L + 1 <= (int)sizeof buf) {
            memcpy(buf + o, e, (size_t)L);
            o += L;
            buf[o++] = 0;
        }
    }
    memcpy(g_reg_last_buf, buf, (size_t)o);
    g_reg_last_len = o;
    if (exe && exe[0])
        snprintf(g_reg_last_exe, sizeof g_reg_last_exe, "%s", exe);
    else
        g_reg_last_exe[0] = 0;
    proc_reg_write_files(dir, buf, o, g_reg_last_exe);
}

static void proc_reg_after_fork(void) {
    g_launch_reg_birth_file[0] = 0;
    launch_reg_publish((int)getpid(), 1);
    if (!g_init_hostpid) return;
    // A fork child inherits the parent's g_reg_file paths. Clear them before publishing, otherwise the
    // child's exit_group cleanup can unlink the parent's /proc registry entry.
    g_reg_file[0] = 0;
    g_reg_exe_file[0] = 0;
    g_reg_birth_file[0] = 0;
    if (g_reg_last_len <= 0) {
        char *argv[] = {(char *)g_exe_path, NULL};
        proc_reg_publish(g_exe_path, 1, argv);
        return;
    }
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    proc_reg_write_files(dir, g_reg_last_buf, g_reg_last_len, g_reg_last_exe);
}

// Read a peer's published canonical exe path (the "x<hostpid>" registry record). Returns 1 + fills out.
static int proc_reg_exe_read(int hostpid, char *out, size_t n) {
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/x%d", dir, hostpid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    char buf[4200];
    ssize_t nr = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (nr <= 0) return 0;
    buf[nr] = 0;
    if (buf[0] != '/') return 0;
    snprintf(out, n, "%s", buf);
    return 1;
}

// /proc/<peer>/maps for another process in the same container. hl cannot inspect a peer engine process's
// guest VMA registry from here, but Linux software is allowed to open this file and expects structured maps
// text rather than ENOENT. Publish a conservative non-empty shape using the peer's registered exe path plus
// plausible heap/stack rows; self reads still use the exact gmap-backed proc_maps_fd() above.
static int proc_maps_pid_fd(int gp, int host) {
    (void)gp;
    char exe[4200];
    if (!proc_reg_exe_read(host, exe, sizeof exe)) snprintf(exe, sizeof exe, "/proc/%d/exe", host);

    char buf[24576]; // 5 rows, each able to carry the full 4 KB exe path without being truncated mid-row
    int n = 0;
    // The peer's image rows carry its own dev:inode when the path is stattable, so a reader that keys on the
    // pair (rather than the pathname) classifies them as file-backed exactly as it would on Linux.
    unsigned dmaj = 0, dmin = 0;
    unsigned long long ino = 0;
    struct stat es;
    if (stat(exe, &es) == 0) {
        dmaj = (unsigned)major(es.st_dev);
        dmin = (unsigned)minor(es.st_dev);
        ino = (unsigned long long)es.st_ino;
    }
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x400000, 0x500000, "r-xp", 0, dmaj, dmin, ino, exe, 0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x500000, 0x510000, "r--p", 0x100000, dmaj, dmin, ino, exe,
                           0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x510000, 0x520000, "rw-p", 0x110000, dmaj, dmin, ino, exe,
                           0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x70000000, 0x70100000, "rw-p", 0, 0, 0, 0, "[heap]", 0);
    n += proc_map_region_p(buf + n, sizeof buf - (size_t)n, 0x7ffde000, 0x7ffff000, "rw-p", 0, 0, 0, 0, "[stack]", 0);
    char desc[64];
    snprintf(desc, sizeof desc, "pid:%d:maps", gp);
    return proc_text_fd_tagged(buf, n, desc);
}

// Read back a peer's published identity by host pid. Returns 1 + fills comm and the NUL-separated
// cmdline (cmdlen bytes); 0 if no record. The comm line is stripped from the returned cmdline.
static int proc_reg_read(int hostpid, char *comm, size_t csz, char *cmd, size_t cmdsz, int *cmdlen) {
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    char buf[4096];
    int nr = (int)read(fd, buf, sizeof buf - 1);
    close(fd);
    if (nr <= 0) return 0;
    buf[nr] = 0;
    char *nl = memchr(buf, '\n', (size_t)nr);
    int cl = nl ? (int)(nl - buf) : 0;
    if (cl >= (int)csz) cl = (int)csz - 1;
    memcpy(comm, buf, (size_t)cl);
    comm[cl] = 0;
    int off = nl ? (int)(nl - buf + 1) : nr, rem = nr - off;
    if (rem < 0) rem = 0;
    if (rem > (int)cmdsz) rem = (int)cmdsz;
    memcpy(cmd, buf + off, (size_t)rem);
    *cmdlen = rem;
    return 1;
}

// Live per-process stats from the host backend. rss/cpu-times/state are REAL (coarse beats
// zero); comm here is the HOST comm (the DBT binary) -- the guest comm comes from the registry instead.
struct hl_procinfo {
    int ppid_host, pgid_host, nthreads;
    char state;
    unsigned long long rss, vsize, utime_ns, stime_ns;
    long start_sec;
    char hostcomm[32];
};

static int hl_get_procinfo(int pid, struct hl_procinfo *pi) {
    hl_host_process_info host;
    if (!hl_host_process_read(pid, &host)) return 0;
    pi->ppid_host = (int)host.parent_pid;
    pi->pgid_host = (int)host.process_group;
    pi->start_sec = (long)host.start_time_seconds;
    pi->state = host.state;
    pi->rss = host.resident_bytes;
    pi->vsize = host.virtual_bytes;
    pi->utime_ns = host.user_time_ns;
    pi->stime_ns = host.system_time_ns;
    pi->nthreads = host.threads > 0 ? (int)host.threads : 1;
    snprintf(pi->hostcomm, sizeof pi->hostcomm, "%s", host.name);
    return 1;
}

// Rebase a host vnode path into the container's guest namespace (strip the rootfs prefix), in place.
static int proc_fd_rebase(char *tgt, size_t capacity) {
    int mapped = g_rootfs_canon_len != 0 && !strncmp(tgt, g_rootfs_canon, g_rootfs_canon_len) &&
                 (tgt[g_rootfs_canon_len] == '/' || tgt[g_rootfs_canon_len] == 0);
    for (int index = 0; !mapped && index < g_nvols; ++index)
        if (!g_vols[index].dead && !strncmp(tgt, g_vols[index].hcanon, g_vols[index].hlen) &&
            (tgt[g_vols[index].hlen] == '/' || tgt[g_vols[index].hlen] == 0))
            mapped = 1;
    if (mapped) {
        char guest[4200];
        int status = guest_from_host(tgt, guest, sizeof guest);
        if (status <= 0 || path_copy(tgt, capacity, guest) != 0) {
            if (capacity != 0) tgt[0] = 0;
            return status < 0 ? status : -ENAMETOOLONG;
        }
        return 1;
    }
    return 0;
}

static int proc_fdvis_resolve_host(int host, int guest_fd) {
    uint32_t kind;
    uint64_t device, object;
    size_t count = 0;
    if (!proc_fdvis_lookup(host, guest_fd, &kind, &device, &object)) return guest_fd;
    if (device == 0 || object == 0 || !hl_host_process_fds(host, NULL, 0, &count)) return -1;
    hl_host_process_fd *entries = count ? malloc(count * sizeof *entries) : NULL;
    if (count && !entries) return -1;
    if (!hl_host_process_fds(host, entries, count, &count)) {
        free(entries);
        return -1;
    }
    int resolved = -1;
    for (size_t index = 0; index < count; ++index) {
        hl_host_process_fd detail;
        size_t ignored;
        if (hl_host_process_fd_read(host, entries[index].descriptor, &detail, NULL, 0, &ignored) &&
            detail.stable_device == device && detail.stable_object == object &&
            (kind == HL_HOST_FD_OTHER || detail.kind == kind)) {
            resolved = entries[index].descriptor;
            break;
        }
    }
    free(entries);
    return resolved;
}

// The /proc/<pid>/fd/<fd> readlink target for a PEER container process (host pid `host`), the SYMLINK-TARGET
// view. A guest process is its own macOS process with a PRIVATE fd table, so the peer's fds aren't in our
// own table (procfd_num rejects a foreign pid) -- read them through host process inspection: a file's
// native path (rebased out of the rootfs), a pipe/socket/anon fd as the Linux-style
// "pipe:[..]"/"socket:[..]"/"anon_inode:[..]" name. Returns the byte length written to `out`, or -1 if the
// peer or fd is not resolvable (-> ENOENT). Guest fd numbers == host fd numbers, the same 1:1 mapping the
// self /proc/self/fd view relies on.
static int proc_fd_link_pid(int host, int fd, char *out, size_t n) {
    hl_host_process_fd entry;
    char tgt[4200] = {0};
    size_t target_size = 0;
    int inspected_fd;
    if (host <= 0 || fd < 0) return -1;
    uint32_t logical_kind = HL_HOST_FD_OTHER;
    uint64_t logical_device = 0, logical_object = 0;
    int logical_found = proc_fdvis_lookup(host, fd, &logical_kind, &logical_device, &logical_object);
    if (logical_found && logical_kind != HL_HOST_FD_FILE && logical_object != 0) {
        const char *logical_name = logical_kind == HL_HOST_FD_SOCKET ? "socket"
                                   : logical_kind == HL_HOST_FD_PIPE ? "pipe"
                                                                     : "anon_inode";
        char logical[64];
        int length = snprintf(logical, sizeof logical, "%s:[%llu]", logical_name, (unsigned long long)logical_object);
        if ((size_t)length > n) length = (int)n;
        memcpy(out, logical, (size_t)length);
        return length;
    }
    /* A provider-backed (bound-volume) regular file has no reliable native
     * descriptor in this process's fd table -- resolving it by device/object
     * identity can collide with an unrelated engine-private fd.  The engine's
     * own fd->path table is authoritative, so a self file descriptor with a
     * tracked host path resolves through it and is rebased into the guest
     * namespace, matching what native inspection produces for host-backed fds. */
    if (host == (int)getpid() && logical_found && logical_kind == HL_HOST_FD_FILE && fd >= 0 && fd < HL_NFD &&
        g_fdpath[fd][0]) {
        char tracked[4200];
        snprintf(tracked, sizeof tracked, "%s", g_fdpath[fd]);
        int mapped = proc_fd_rebase(tracked, sizeof tracked);
        if (mapped < 0 || (g_rootfs && mapped == 0)) return -1;
        size_t l = strlen(tracked);
        if (l > n) l = n;
        memcpy(out, tracked, l);
        return (int)l;
    }
    inspected_fd = proc_fdvis_resolve_host(host, fd);
    if (inspected_fd < 0) return -1;
    if (!hl_host_process_fd_read(host, inspected_fd, &entry, tgt, sizeof tgt - 1, &target_size)) return -1;
    if (entry.kind == HL_HOST_FD_FILE && target_size != 0) {
        tgt[target_size] = 0;
        /* A launch-scoped controlling terminal is the first slave in the
         * guest devpts namespace regardless of the host's global pty number.
         * Only typed launch stdio receives this projection; ordinary host
         * binds and guest-created ptys retain their own namespace identity. */
        int projected_tty = logical_found && fd >= 0 && fd <= STDERR_FILENO &&
                            (strncmp(tgt, "/dev/pts/", 9) == 0 || strncmp(tgt, "/dev/ttys", 9) == 0);
        if (projected_tty) snprintf(tgt, sizeof tgt, "/dev/pts/0");
        int mapped = projected_tty ? 1 : proc_fd_rebase(tgt, sizeof tgt);
        if (mapped < 0 || (g_rootfs && mapped == 0)) return -1;
        size_t l = strlen(tgt);
        if (l > n) l = n;
        memcpy(out, tgt, l);
        return (int)l;
    }
    const char *k = entry.kind == HL_HOST_FD_SOCKET ? "socket" : entry.kind == HL_HOST_FD_PIPE ? "pipe" : "anon_inode";
    char syn[64];
    int sl = snprintf(syn, sizeof syn, "%s:[%d]", k, fd);
    if ((size_t)sl > n) sl = (int)n;
    memcpy(out, syn, (size_t)sl);
    return sl;
}

// Is `fd` currently OPEN in the PEER process `host`? (For peer /proc/<pid>/fd/<N> lstat/stat: a live fd is a
// symlink, a closed one ENOENTs.) Returns 1 if open, 0 otherwise.
static int proc_fd_pid_open_one(int host, int fd) {
    hl_host_process_fd entry;
    size_t path_size;
    int inspected_fd;
    if (host <= 0 || fd < 0) return 0;
    inspected_fd = proc_fdvis_resolve_host(host, fd);
    if (inspected_fd < 0) return 0;
    return hl_host_process_fd_read(host, inspected_fd, &entry, NULL, 0, &path_size);
}

// Build a temp dir of "N -> target" symlinks for a PEER container process's open fds (host pid `host`), so
// a peer /proc/<pid>/fd is listable (getdents) and each entry readlinks to the fd's target -- the same
// symlink-dir mechanism proc_fd_dir_open() uses for self, but populated from the peer descriptor snapshot
// instead of our own host fd table. Self is delegated to proc_fd_dir_open (exact host table). Returns the
// dir fd, or -1. NOTE: this is the LISTING + readlink view only; actually OPENING a peer fd (using
// /proc/<pid>/fd/N as a working descriptor) needs the owner to hand the real fd across processes
// (SCM_RIGHTS-level fd passing) -- deferred; open of a peer fd link still ENOENTs.
static int proc_fd_dir_pid_open(int guest, int host) {
    int self = guest == 0;
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    size_t nfd = 0;
    if (!hl_host_process_fds(host, NULL, 0, &nfd)) return -1;
    size_t fd_capacity = nfd;
    hl_host_process_fd *fds = fd_capacity != 0 ? malloc(fd_capacity * sizeof *fds) : NULL;
    if (fd_capacity != 0 && !fds) return -1;
    if (!hl_host_process_fds(host, fds, fd_capacity, &nfd)) {
        free(fds);
        return -1;
    }
    if (nfd > fd_capacity) nfd = fd_capacity;
    int identity = self ? host : guest;
    size_t nviews = proc_fdvis_list(identity, NULL, 0);
    struct fdvis_view *views = nviews ? malloc(nviews * sizeof *views) : NULL;
    if (nviews && !views) {
        free(fds);
        return -1;
    }
    if (nviews) {
        size_t copied = proc_fdvis_list(identity, views, nviews);
        if (copied < nviews) nviews = copied;
    }
    char tmpl[] = "/tmp/.hl-proc-fd-dirXXXXXX";
    if (!mkdtemp(tmpl)) {
        free(views);
        free(fds);
        return -1;
    }
    for (size_t i = 0; i < nfd; i++) {
        int fd = fds[i].descriptor;
        char tgt[4200] = {0};
        size_t target_size = 0;
        hl_host_process_fd entry = {.descriptor = -1};
        int have = hl_host_process_fd_read(host, fd, &entry, tgt, sizeof tgt - 1, &target_size) &&
                   entry.kind == HL_HOST_FD_FILE && target_size != 0;
        int hidden = nviews != 0 || (fds[i].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0;
        for (size_t view = 0; view < nviews && !hidden; ++view)
            if (views[view].guest_fd == fd) hidden = 1;
        if (!hidden && have && strstr(tgt, "/.hl-proc-fd-dir") != NULL)
            for (size_t view = 0; view < nviews && !hidden; ++view)
                if (entry.stable_device != 0 && entry.stable_object != 0 && views[view].device == entry.stable_device &&
                    views[view].object == entry.stable_object)
                    hidden = 1;
        if (hidden) continue;
        if (entry.descriptor == fd) fds[i].kind = entry.kind;
        if (have) {
            tgt[target_size] = 0;
            int mapped = proc_fd_rebase(tgt, sizeof tgt);
            have = mapped >= 0 && (!g_rootfs || mapped > 0) && tgt[0] != 0;
        }
        if (!have) {
            const char *k = fds[i].kind == HL_HOST_FD_SOCKET ? "socket"
                            : fds[i].kind == HL_HOST_FD_PIPE ? "pipe"
                                                             : "anon_inode";
            snprintf(tgt, sizeof tgt, "%s:[%d]", k, fd);
        }
        char link[80];
        snprintf(link, sizeof link, "%s/%d", tmpl, fd);
        if (symlink(tgt, link) != 0) {}
    }
    for (size_t view = 0; view < nviews; ++view) {
        char tgt[4200] = {0};
        int length = proc_fd_link_pid(identity, views[view].guest_fd, tgt, sizeof tgt - 1);
        if (length <= 0) continue;
        tgt[length] = 0;
        char link[80];
        snprintf(link, sizeof link, "%s/%d", tmpl, views[view].guest_fd);
        if (symlink(tgt, link) != 0) {}
    }
    free(views);
    free(fds);
    int d = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (d < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    if (self) {
        struct stat status;
        char link[80];
        char target[64];
        snprintf(link, sizeof link, "%s/%d", tmpl, d);
        snprintf(target, sizeof target, "/proc/self/fd/%d", d);
        if (symlink(target, link) != 0 && errno != EEXIST) {}
        if (fstat(d, &status) == 0) {
            /* This directory is returned to the guest and therefore is not engine-private. Publish its
             * logical identity normally; private adoption would move it outside the guest fd range. */
            if (proc_fdvis_publish(d, HL_HOST_FD_FILE, (uint64_t)status.st_dev, (uint64_t)status.st_ino) != 0) {
                close(d);
                procfd_dir_rm(tmpl);
                return -1;
            }
        }
    }
    if (self) {
        /* Tag the materialized directory with its guest namespace path. Relative openat/stat/readlink
         * operations must re-enter procfd synthesis instead of following the temporary host symlinks. */
        proc_dir_register(d, tmpl, "/proc/self/fd");
    } else {
        for (int i = 0; i < 64; i++)
            if (!g_procfd_dirs[i].path[0]) {
                g_procfd_dirs[i].fd = d;
                snprintf(g_procfd_dirs[i].path, sizeof g_procfd_dirs[i].path, "%s", tmpl);
                break;
            }
    }
    return d;
}

// Resident footprint (bytes) for OUR OWN pid's VmRSS / statm-resident / stat-rss. The guest's tracked anon
// charge (g_mem_charged) is 0 for a process that has only faulted its static image, but a real Linux process
// ALWAYS has a non-zero VmRSS -- top/htop/ps would otherwise show this process at RES=0, a engine-specific divergence
// (a peer pid already reports a live resident size through host process stats; self must not read 0). Floor the tracked
// charge with this engine process's real resident size so the reported RSS is non-zero and plausible.
static unsigned long long self_rss_bytes(void) {
    unsigned long long charged = (unsigned long long)atomic_load(&g_mem_charged);
    struct hl_procinfo process;
    unsigned long long resident = hl_get_procinfo((int)getpid(), &process) ? process.rss : 0;
    return resident > charged ? resident : charged;
}

// Host boot epoch (seconds) -- the base for /proc/<pid> starttime and /proc/uptime. Cached.
static long host_btime(void) {
    static long bt = 0;
    if (bt) return bt;
    hl_host_system_info info;
    bt = hl_host_system_read(&info, NULL, 0) && info.boot_time_seconds <= LONG_MAX ? (long)info.boot_time_seconds
                                                                                   : time(NULL);
    return bt;
}

// Aggregate host CPU jiffies (user, system, idle, nice) -- monotonically increasing, so htop/top meters move.
static void host_cpu_ticks(unsigned long long t[4]) {
    hl_host_system_info info;
    if (hl_host_system_read(&info, NULL, 0)) {
        t[0] = info.aggregate.user;
        t[1] = info.aggregate.system;
        t[2] = info.aggregate.idle;
        t[3] = info.aggregate.nice;
    } else {
        t[0] = t[1] = t[2] = t[3] = 0;
    }
}

// Real host memory picture (kB): total from hw.memsize, free/available/cached from the Mach VM stats.
static void host_mem(unsigned long long *total, unsigned long long *fre, unsigned long long *avail,
                     unsigned long long *cached) {
    hl_host_system_info info;
    *total = 0;
    if (hl_host_system_read(&info, NULL, 0)) {
        *total = info.memory_total / 1024;
        *fre = info.memory_free / 1024;
        *avail = info.memory_available / 1024;
        *cached = info.memory_cached / 1024;
    } else {
        *fre = *avail = *total / 4;
        *cached = 0;
    }
}

// Count the live container processes (registry entries whose pid is still alive).
static int proc_reg_count(void) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (!d) return 1;
    int n = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        if (kill(atoi(e->d_name), 0) == 0 || errno != ESRCH) n++;
    }
    closedir(d);
    return n ? n : 1;
}

// /sys/fs/cgroup/cgroup.procs (and cgroup.threads) membership: the container is ONE cgroup, so this must
// list EVERY guest process -- the init AND every forked child -- not just container_pid(). The process
// registry already tracks that set cross-process (each engine process, incl. every fork child, publishes
// a file named by its host pid; see proc_reg_publish/after_fork), so enumerate it and map each host pid
// to its guest pid (init_hostpid -> 1). `with_threads` additionally appends THIS process's extra guest
// thread tids for cgroup.threads (a peer's threads aren't enumerable from here, so it lists their main
// task -- exactly like /proc/<pid>/task for a peer). Self is always included (the registry may lag our
// own just-published entry). Returns the byte length written.
static int cgroup_procs_text(char *buf, size_t n, int with_threads) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    int o = 0, me = (int)getpid(), have_self = 0;
    DIR *d = opendir(dir);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)) && (size_t)o < n - 16) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (host <= 0) continue;
            if (host != me && kill(host, 0) != 0 && errno == ESRCH) continue; // stale registry entry
            if (host == me) have_self = 1;
            int gp = (g_init_hostpid && host == g_init_hostpid) ? 1 : host;
            o += snprintf(buf + o, n - (size_t)o, "%d\n", gp);
        }
        closedir(d);
    }
    if (!have_self && (size_t)o < n - 16) o += snprintf(buf + o, n - (size_t)o, "%d\n", container_pid());
    if (with_threads && (size_t)o < n - 16) {
        int tids[256];
        int self_gp = container_pid();
        int nt = thread_tid_list(tids, 256, me);
        for (int i = 0; i < nt && (size_t)o < n - 16; i++)
            if (tids[i] != me && tids[i] != self_gp) // the main thread was already listed as our pid
                o += snprintf(buf + o, n - (size_t)o, "%d\n", tids[i]);
    }
    if (o == 0) o = snprintf(buf, n, "%d\n", container_pid());
    return o;
}

// /sys/fs/cgroup/memory.current aggregate across the whole container. Under a memory.max cap the
// per-process anon CHARGE is tracked (bounded, matches enforcement) -> sum the shared accounting slots.
// With no cap the charge model is inert, so fall back to the REAL resident size of every live container
// process (host process stats) -- what a native cgroup reports, and what makes a forked child's allocation visible
// to a parent reading memory.current. Cross-process either way (was a single engine process's local value).
static unsigned long long cgroup_mem_current(void) {
    if (g_mem_max) return acct_mem_total();
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    unsigned long long total = 0;
    int me = (int)getpid(), saw_self = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (host <= 0) continue;
            if (host == me) {
                total += self_rss_bytes();
                saw_self = 1;
                continue;
            }
            if (kill(host, 0) != 0 && errno == ESRCH) continue; // stale registry entry
            struct hl_procinfo pi;
            if (hl_get_procinfo(host, &pi)) total += pi.rss;
        }
        closedir(d);
    }
    if (!saw_self) total += self_rss_bytes(); // registry may lag our own publish
    return total;
}

// Parse "/proc/<digits>/<leaf>" for ANY pid (unlike proc_self_leaf, which matches only our own). Returns
// the <leaf> and fills *pid, or NULL.
static const char *proc_any_leaf(const char *rp, int *pid) {
    if (strncmp(rp, "/proc/", 6)) return NULL;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0 || q[i] != '/') return NULL;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    *pid = atoi(num);
    return q + i + 1;
}

// Is `host` inside OUR process tree? Walks the host ppid chain looking for this process or the container
// init. A daemonized descendant (setsid) leaves our session, so the same-session fallback below cannot see
// it: /proc/<pid>/* for a double-forked grandchild that reparented onto us read back ENOENT, and a
// supervisor comparing that ppid against getppid() saw them disagree. Bounded hops; a chain that leaves our
// tree climbs to the host init instead, so an unrelated pid is still rejected.
static int proc_pid_descendant(int host) {
    int self = (int)getpid();
    for (int hop = 0; hop < 32 && host > 1; hop++) {
        struct hl_procinfo pi;
        if (!hl_get_procinfo(host, &pi)) return 0;
        if (pi.ppid_host == self || (g_init_hostpid && pi.ppid_host == g_init_hostpid)) return 1;
        if (pi.ppid_host <= 1 || pi.ppid_host == host) return 0;
        host = pi.ppid_host;
    }
    return 0;
}

// Is guest pid `gp` a live member of this container? Fills *hostout with its host pid (gp==1 -> init).
static int proc_pid_member(int gp, int *hostout) {
    int host = (gp == 1 && g_init_hostpid) ? g_init_hostpid : gp;
    *hostout = host;
    if (host == (int)getpid()) return 1;
    if (host <= 0) return 0;
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, host);
    if (access(path, F_OK) == 0 && !(kill(host, 0) != 0 && errno == ESRCH)) return 1;
    if (kill(host, 0) != 0) return 0;
    // registry may lag (or is off outside container mode): accept a live session peer, or a descendant of
    // ours that left the session.
    return getsid(host) == getsid(0) || proc_pid_descendant(host);
}

// Does `rp` name a /proc/<pid>/... path for a pid other than this process? Such a path must never reach the
// host /proc, whether or not the pid is a container member: a bare run read the HOST's pid 1 (systemd)
// through /proc/1/{cmdline,status,stat} because the peer synthesis declined those leaves and the open fell
// through, and a MEMBER peer's host /proc describes the engine process running that guest, not the guest.
// Every leaf the peer synthesis does serve is answered before this. fs.c calls it after the /proc synth.
static int proc_pid_not_self(const char *rp) {
    if (!rp) return 0;
    int pid = 0;
    if (!proc_any_leaf(rp, &pid) || pid <= 0) return 0;
    return pid != (int)getpid() && pid != container_pid();
}

// The container's namespace magic-link target for <name> ("net" -> "net:[<inode>]"), or -1 if <name>
// is not a known namespace. A container is a SINGLE namespace set, so self and every peer process share
// one inode per namespace. The inode MUST equal the one a stat() of the same ns file reports (synth_stat
// follows the magic link to the engine's REAL host nsfs node), or lsns/nsenter -- which compare the
// readlink text against the st_ino -- see the link and the file as different namespaces. On a Linux host
// the engine process already lives in the guest's namespace set, so its own /proc/self/ns/<name> readlink
// IS that authoritative, stable "<name>:[<inode>]" string (and correctly renders pid_for_children ->
// "pid:[...]"). Read it directly; fall back to the initial-namespace constants only when the host does not
// expose it (e.g. the macOS build), keeping a well-formed link. Writes the string into `out`, returns len.
static int ns_link_target(const char *name, char *out, size_t cap) {
    static const struct {
        const char *nm;  // guest ns-dir entry name
        const char *tgt; // link target namespace name (pid_for_children -> "pid")
        unsigned ino;    // initial-namespace fallback inode
    } NS[] = {{"cgroup", "cgroup", 4026531835u},
              {"ipc", "ipc", 4026531839u},
              {"mnt", "mnt", 4026531841u},
              {"net", "net", 4026531840u},
              {"pid", "pid", 4026531836u},
              {"pid_for_children", "pid", 4026531836u},
              {"time", "time", 4026531834u},
              {"time_for_children", "time", 4026531834u},
              {"user", "user", 4026531837u},
              {"uts", "uts", 4026531838u},
              {0, 0, 0}};

    for (int i = 0; NS[i].nm; i++) {
        if (strcmp(name, NS[i].nm)) continue;
        char hp[64], link[64];
        snprintf(hp, sizeof hp, "/proc/self/ns/%s", NS[i].nm);
        ssize_t r = readlink(hp, link, sizeof link - 1);
        // Accept only a well-formed "<tgt>:[<digits>]" host answer; anything else uses the fallback so a
        // partial/odd host read never yields a malformed link.
        if (r > 0 && (size_t)r < sizeof link) {
            link[r] = 0;
            size_t tl = strlen(NS[i].tgt);
            if (!strncmp(link, NS[i].tgt, tl) && link[tl] == ':' && link[tl + 1] == '[' && link[r - 1] == ']')
                return snprintf(out, cap, "%s", link);
        }
        return snprintf(out, cap, "%s:[%u]", NS[i].tgt, NS[i].ino);
    }
    return -1;
}

// ================= guest-pid namespace (kill/pidfd host-authority containment) =================
// hl runs every guest process as a real host (macOS) process, and historically used the host pid 1:1 as
// the guest pid. That let a guest kill(2)/pidfd_send_signal an ARBITRARY same-user HOST pid -- a sibling
// engine (another container), the launcher, or any of the hl user's processes -- because the target was
// resolved straight to the host with no namespace boundary. The per-container process REGISTRY (proc_reg_*,
// keyed by HL_NETNS/HL_HOSTNAME so every engine process of one guest agrees and two guests never
// collide) is that boundary: a host pid belongs to this container iff it published a `<dir>/<hostpid>`
// record. The signal syscalls resolve the guest target to a host pid and then require membership here,
// turning "any host pid" into "only a process inside THIS container" (a non-member -> ESRCH), exactly like
// a real PID namespace. A member that is a genuine peer stays reachable, so legitimate cross-guest-process
// signalling (the case rare.c pidfd + kill(-pgid) rely on) is preserved.

// STRICT host-pid membership for the security boundary (kill/pidfd reject). Unlike proc_pid_member (which
// tolerates registry lag with a permissive same-session fallback for /proc DISPLAY -- too loose here, since
// sibling engines share our host session), this demands a published registry record AND a live process, so
// a pid outside the container, or a stale marker whose pid is gone, is NOT a member. Self and the container
// init are always members. Every fork publishes the child's marker in the PARENT before it returns (see
// proc_reg_mark_child), so a just-forked descendant is a member the instant its pid exists (no fork race).
static int container_host_member(int h) {
    if (h <= 0) return 0;
    if (h == (int)getpid() || (g_init_hostpid && h == g_init_hostpid)) return 1;
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, h);
    if (access(path, F_OK) != 0) return 0;       // no record in THIS container's registry -> not a member
    return !(kill(h, 0) != 0 && errno == ESRCH); // reject a stale marker whose process is already gone
}

// Resolve a GUEST pid to its container-local host pid and require membership. gp==1 -> the init. Returns 1
// and fills *hostout when gp names a process inside this container; 0 (leaving *hostout resolved) otherwise.
static int container_gpid_member(int gp, int *hostout) __attribute__((unused));

static int container_gpid_member(int gp, int *hostout) {
    int host = (gp == 1 && g_init_hostpid) ? g_init_hostpid : gp;
    if (hostout) *hostout = host;
    return container_host_member(host);
}

// Publish a fresh child's membership marker from the PARENT, synchronously at fork, so the child is a
// registry member before the parent can return and signal it (the child's own proc_reg_after_fork later
// replaces this empty marker with its full comm/argv via an atomic rename). Cheap (one create); only in
// container mode. Closes the fork-window race where a strict membership check would wrongly ESRCH a
// legitimate just-forked descendant that had not yet run its own publish.
static void proc_reg_mark_child(int hostpid) {
    launch_reg_publish(hostpid, 0);
    if (!g_init_hostpid || hostpid <= 0) return;
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    // EXCL: never clobber the child's real record.
    (void)hl_host_file_exclusive(&g_jit_services, path, 0644);
    {
        hl_host_process_info process;
        char birth[32];
        if (hl_host_process_read(hostpid, &process)) {
            int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
            snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
            if (size > 0) (void)hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size);
        }
    }
}

// Drop a reaped child's registry records from the PARENT at wait4/waitid time. A child that exits cleanly
// unlinks its own record, but one killed by a signal (SIGKILL) never runs that cleanup -- and a host pid
// cannot be reused until it is reaped, so removing the marker exactly at reap keeps a recycled pid from
// inheriting stale in-container membership. Idempotent (unlink of an absent path is a no-op).
static void proc_reg_reap(int hostpid) {
    char launch_dir[80], launch_path[160];
    if (hostpid > 0 && launch_reg_key(launch_dir, sizeof launch_dir)) {
        snprintf(launch_path, sizeof launch_path, "%s/b%d", launch_dir, hostpid);
        (void)unlink(launch_path);
    }
    if (!g_init_hostpid || hostpid <= 0) return;
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    unlink(path);
    snprintf(path, sizeof path, "%s/x%d", dir, hostpid);
    unlink(path);
    snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
    unlink(path);
}

// kill(0,sig) / own-process-group delivery, contained to this engine's container. Linux kill(0,sig) signals
// every process in the CALLER's process group; hl forwards setpgid to the host so the host process group
// MIRRORS the guest's, but the engine shares its host group/session with the launcher + sibling engines --
// so a raw kill(-getpgrp()) would escape the container. Instead enumerate the container registry and signal
// each MEMBER whose host process-group == want_hpgid, skipping self (the caller delivers to itself via
// raise_guest_signal). `msig` is the already-macOS-translated signo. Returns the number of peers signalled.
static int container_group_kill(int want_hpgid, int msig, int self_hpid) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (!d) return 0;
    int n = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue; // pid records only (skip the x<pid> exe recs)
        int h = atoi(e->d_name);
        if (h <= 0 || h == self_hpid) continue;
        struct hl_procinfo pi;
        if (!hl_get_procinfo(h, &pi)) continue;   // dead/unknown host pid -> skip
        if (pi.pgid_host != want_hpgid) continue; // not in the caller's process group
        if (kill(h, msig) == 0) n++;
    }
    closedir(d);
    return n;
}

// /proc/<pid>/stat for a peer -- the 52-field line with GUEST pid/ppid and REAL rss/cpu/state/starttime.
static int proc_stat_pid_text(char *b, size_t n, int gp, int host) {
    struct hl_procinfo pi;
    int ok = hl_get_procinfo(host, &pi);
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl))
        snprintf(comm, sizeof comm, "%.15s", ok ? pi.hostcomm : "proc");
    char state = ok ? pi.state : 'S';
    // pbi_status can't distinguish a running task from one asleep in a blocking wait (BSD p_stat is SRUN
    // for both). Prefer the guest's own published run state when it has one; keep pbi authoritative for the
    // states it CAN report faithfully -- 'Z' (zombie, post-exit) and 'T' (SIGSTOP/traced host-suspended).
    int ov = ts_lookup(host);
    if (ov && state != 'Z' && state != 'T') state = (char)ov;
    int ppid = 0;
    if (gp != 1 && ok) {
        int hp;
        if (pi.ppid_host == g_init_hostpid)
            ppid = 1;
        else if (proc_pid_member(pi.ppid_host, &hp))
            ppid = pi.ppid_host;
    }
    int pgrp = ok ? (pi.pgid_host == g_init_hostpid ? 1 : pi.pgid_host) : gp;
    // Field 6 (session): the peer's real host session id (init's session -> guest 1), NOT its own pid. The
    // old code printed gp (the pid), so getsid() and /proc/<pid>/stat disagreed for a normal child.
    int hsid = (int)getsid(host);
    int psess = (hsid > 0) ? ((g_init_hostpid && hsid == g_init_hostpid) ? 1 : hsid) : gp;
    long hz = sysconf(_SC_CLK_TCK);
    if (hz <= 0) hz = 100;
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long utime = ok ? pi.utime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long long stime = ok ? pi.stime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long rss_pg = ok ? (unsigned long)(pi.rss / pgsz) : 0;
    // The host virtual size is the whole DBT process (code cache + big anon reservations) -> tens of GB,
    // which makes top's VSZ/%VSZ nonsensical. Report a bounded, believable footprint (rss + a modest
    // overhead) instead; there is no visibility into a PEER's true guest vsize from another process.
    unsigned long long vsize = (unsigned long long)rss_pg * pgsz + (128ULL << 20);
    long long since = ok ? (long long)pi.start_sec - host_btime() : 0;
    unsigned long long start_ticks = since > 0 ? (unsigned long long)since * (unsigned long long)hz : 0;
    int nthreads = 1; // Peer /proc/<pid>/task currently exposes one synthetic task.
    return snprintf(b, n,
                    // Field 38 (exit_signal, SIGCHLD=17) sat at 39 here -- the same one-too-many zero after
                    // field 25 that proc_stat_text carried, shifting every field from 26 up by one.
                    "%d (%s) %c %d %d %d 0 -1 4194560 0 0 0 0 %llu %llu 0 0 20 0 %d 0 %llu %llu %lu "
                    "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
                    gp, comm, state, ppid, pgrp, psess, utime, stime, nthreads, start_ticks, vsize, rss_pg);
}

// /proc/<pid>/status for a peer -- the key:value form with GUEST Pid/PPid and REAL VmRSS.
static int proc_status_pid_text(char *b, size_t n, int gp, int host) {
    struct hl_procinfo pi;
    int ok = hl_get_procinfo(host, &pi);
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl))
        snprintf(comm, sizeof comm, "%.15s", ok ? pi.hostcomm : "proc");
    int ppid = 0;
    if (gp != 1 && ok) {
        int hp;
        if (pi.ppid_host == g_init_hostpid)
            ppid = 1;
        else if (proc_pid_member(pi.ppid_host, &hp))
            ppid = pi.ppid_host;
    }
    unsigned long rss = ok ? (unsigned long)(pi.rss / 1024) : 0;
    unsigned long vsz = rss + (128UL << 10); // bounded footprint, not the huge host DBT vsize (see stat text)
    char state = ok ? pi.state : 'S';        // same run-state override as proc_stat_pid_text (see there)
    int ov = ts_lookup(host);
    if (ov && state != 'Z' && state != 'T') state = (char)ov;
    const char *state_name = "unknown";
    switch (state) {
    case 'R': state_name = "running"; break;
    case 'S': state_name = "sleeping"; break;
    case 'D': state_name = "disk sleep"; break;
    case 'T': state_name = "stopped"; break;
    case 'Z': state_name = "zombie"; break;
    }
    char groups[512]; // peers carry the same container supplementary set (image-derived, see self)
    groups_status_str(groups, sizeof groups);
    char cpumask[40], cpulist[24];
    cpus_allowed_strs(cpumask, sizeof cpumask, cpulist, sizeof cpulist);
    return snprintf(
        b, n,
        "Name:\t%s\nUmask:\t0022\nState:\t%c (%s)\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
        "TracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t256\nGroups:\t%s\n"
        "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t       0 kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
        "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
        "VmSwap:\t       0 kB\nThreads:\t%d\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
        "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
        // Peer processes carry the same docker default cap set (see proc_status_text). We don't
        // track a peer's live effective/nnp, so report the container default.
        "CapInh:\t0000000000000000\nCapPrm:\t%016llx\nCapEff:\t%016llx\nCapBnd:\t%016llx\n"
        "CapAmb:\t0000000000000000\nNoNewPrivs:\t0\nSeccomp:\t2\nSeccomp_filters:\t1\n"
        "Speculation_Store_Bypass:\tvulnerable\nSpeculationIndirectBranch:\tunknown\n"
        "Cpus_allowed:\t%s\nCpus_allowed_list:\t%s\nvoluntary_ctxt_switches:\t1\n"
        "nonvoluntary_ctxt_switches:\t0\n",
        comm, state, state_name, gp, gp, ppid, groups, vsz, vsz, rss, rss, rss, 1, (unsigned long long)HL_CAP_DEFAULT,
        (unsigned long long)HL_CAP_DEFAULT, (unsigned long long)HL_CAP_DEFAULT, cpumask, cpulist);
}

// /proc/<pid>/cmdline for a peer -- the published NUL-separated argv (fallback: the comm).
static int proc_cmdline_pid_text(char *b, size_t n, int host) {
    char comm[32], cmd[4096];
    int cl;
    if (proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl) && cl > 0) {
        int L = cl > (int)n ? (int)n : cl;
        memcpy(b, cmd, (size_t)L);
        if (L == 0 || b[L - 1] != 0) {
            if (L < (int)n)
                b[L++] = 0;
            else
                b[L - 1] = 0;
        }
        return L;
    }
    struct hl_procinfo pi;
    const char *c = hl_get_procinfo(host, &pi) ? pi.hostcomm : "proc";
    int L = (int)strlen(c);
    if (L + 1 > (int)n) L = (int)n - 1;
    memcpy(b, c, (size_t)L);
    b[L] = 0;
    return L + 1;
}

// /proc/<pid>/comm for a peer.
static int proc_comm_pid_text(char *b, size_t n, int host) {
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl)) {
        struct hl_procinfo pi;
        snprintf(comm, sizeof comm, "%.15s", hl_get_procinfo(host, &pi) ? pi.hostcomm : "proc");
    }
    return snprintf(b, n, "%s\n", comm);
}

// /proc/[pid]/statm -- the 7-field page-count line (size resident shared text lib data dt). htop's
// MEM% column reads `resident` from HERE (not status VmRSS), so it must be present and non-zero.
static int proc_statm_common(char *b, size_t n, unsigned long size_pg, unsigned long rss_pg) {
    return snprintf(b, n, "%lu %lu %lu 1 0 %lu 0\n", size_pg, rss_pg, rss_pg / 2, size_pg);
}

static int proc_statm_text(char *b, size_t n) { // our own pid
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long vm_rss, vm_vsize;
    self_vm_statm_bytes(&vm_rss, &vm_vsize);
    unsigned long rss_pg = (unsigned long)(vm_rss / pgsz);
    unsigned long size_pg = (unsigned long)(vm_vsize / pgsz);
    if (size_pg < rss_pg) size_pg = rss_pg;
    return proc_statm_common(b, n, size_pg, rss_pg);
}

static int proc_statm_pid_text(char *b, size_t n, int host) { // a peer -- real host-backed RSS
    struct hl_procinfo pi;
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long rss_pg = hl_get_procinfo(host, &pi) ? (unsigned long)(pi.rss / pgsz) : 0;
    unsigned long overhead_pg = (unsigned long)((128ULL << 20) / pgsz);
    return proc_statm_common(b, n, rss_pg + overhead_pg, rss_pg);
}

// Register a materialized proc temp dir (fd + host temp path for reaping) AND tag the fd's GUEST /proc
// path in g_fdpath. The tag is the key trick: a RELATIVE openat/readlink against this dir fd (htop uses
// openat(pid_dirfd,"stat"/"task"/...) exclusively) then resolves via abs_guest back to the /proc path,
// so it re-enters this same synthesis instead of hitting the real (empty) temp entry. abs_guest strips
// g_rootfs_canon, so we store "<canon><guestpath>".
static void proc_dir_register(int fd, const char *tmpl, const char *guestpath) {
    for (int i = 0; i < 64; i++)
        if (!g_procfd_dirs[i].path[0]) {
            g_procfd_dirs[i].fd = fd;
            snprintf(g_procfd_dirs[i].path, sizeof g_procfd_dirs[i].path, "%s", tmpl);
            break;
        }
    if (fd >= 0 && fd < 1024 && path_concat(g_fdpath[fd], sizeof g_fdpath[fd], g_rootfs_canon, guestpath) != 0)
        g_fdpath[fd][0] = 0; // unrepresentable tags fail closed in relative-atpath handling
}

// Materialize a /proc/<gp> (or task/<tid>) directory as a temp dir of placeholder entries so
// opendir/getdents works and htop can descend; the CONTENT of each entry is served live on the
// (re-intercepted) relative open by proc_open. `guestpath` is the /proc path this dir represents;
// with_task adds the "task" subdir entry (omitted for a task/<tid> dir, which never nests another).
static int proc_leaf_dir_open(const char *guestpath, int with_task) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-pidXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    // The per-pid file set. Direct open/stat serve every name here (proc_open), so listing them makes
    // readdir-based discovery agree with direct probing (mountinfo/limits/environ/smaps/pagemap/io were
    // openable but hidden from `ls /proc/self`).
    static const char *const files[] = {"stat",          "statm",        "status",     "cmdline",   "comm",   "maps",
                                        "oom_score_adj", "oom_adj",      "oom_score",  "mountinfo", "limits", "environ",
                                        "smaps",         "pagemap",      "io",         "mounts",    "cgroup", "auxv",
                                        "numa_maps",     "smaps_rollup", "mountstats", "syscall",   0};
    for (int i = 0; files[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, files[i]);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    if (with_task) {
        char p[64];
        snprintf(p, sizeof p, "%s/task", tmpl);
        hl_compat_mkdir(p, 0555);
        snprintf(p, sizeof p, "%s/fd", tmpl);
        hl_compat_mkdir(p, 0555); // placeholder: an open of /proc/<pid>/fd re-enters the synthesis (proc_fd_dir_open)
        snprintf(p, sizeof p, "%s/map_files", tmpl);
        hl_compat_mkdir(p, 0555); // ditto -> proc_map_files_dir_open
    }
    // Magic-link placeholders (exe/cwd/root) so getdents lists them with d_type DT_LNK, like Linux. Every
    // ACCESS to them goes by path or by (tagged dirfd, relative) and is intercepted -- readlink/stat/open
    // of /proc/<pid>/{exe,cwd,root} are served by proc_self_exe / the root|cwd synthesis in fs.c;
    // the inert "." target exists only so a host-side follow can never dangle out of the temp dir.
    static const char *const links[] = {"exe", "cwd", "root", 0};
    for (int i = 0; links[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, links[i]);
        if (symlink_idempotent(".", p) != 0) {
            procfd_dir_rm(tmpl);
            return -1;
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// Materialize /proc/<gp>/task -- a dir whose sole entry is the main thread tid (== gp for the common
// single-threaded case; enough for htop to count the process). Returns the fd or -1.
static int proc_task_dir_open(int gp) {
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-taskXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char p[64];
    snprintf(p, sizeof p, "%s/%d", tmpl, gp);
    hl_compat_mkdir(p, 0555); // the main thread tid (== pid)
    // For OUR OWN process, enumerate every live guest thread's tid so a /proc/self/task walk sees them all
    // (thread enumerators, profilers, debuggers). Peer processes keep just the main entry (no cross-process
    // thread registry yet).
    if (gp == (int)getpid() || gp == container_pid()) {
        int tids[256];
        int nt = thread_tid_list(tids, 256, gp);
        for (int i = 0; i < nt; i++) {
            if (tids[i] == gp) continue; // main already created
            snprintf(p, sizeof p, "%s/%d", tmpl, tids[i]);
            hl_compat_mkdir(p, 0555);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    char gpath[48];
    snprintf(gpath, sizeof gpath, "/proc/%d/task", gp);
    proc_dir_register(fd, tmpl, gpath);
    return fd;
}

// Rewrite a leading /proc/self/ or /proc/thread-self/ (WITH a tail) to /proc/<our-pid>/ so the
// numeric-pid synth (proc_dir_try_open, the synth_stat task-dir block) resolves the CALLER's own
// subtrees -- e.g. /proc/self/task, /proc/self/task/<tid>. Bare /proc/self (the magic symlink) is
// left untouched (it stays a symlink). Returns `out` on rewrite, else the original `rp` unchanged.
static const char *proc_deself(const char *rp, char *out, size_t osz) {
    if (!rp) return rp;
    const char *tail = NULL;
    if (!strncmp(rp, "/proc/self/", 11))
        tail = rp + 10; // keep the leading '/'
    else if (!strncmp(rp, "/proc/thread-self/", 18))
        tail = rp + 17;
    if (!tail) return rp;
    snprintf(out, osz, "/proc/%d%s", container_pid(), tail);
    return out;
}

static int proc_task_tid_visible(int pid, int tid) {
    if (tid <= 0) return 0;
    int is_self = (pid == (int)getpid() || pid == container_pid());
    if (is_self) return tid == pid || thread_tid_alive(tid);
    return tid == pid; // Peer thread registry is not cross-process yet.
}

// If `rp` is a /proc/<pid> DIRECTORY path (the pid dir, its task/ dir, or a task/<tid>/ dir) for a live
// container pid, materialize it and return the fd. Returns -1 on error, or -2 if `rp` is not such a
// directory (a per-pid FILE like stat/status -> the caller falls through to proc_open). fs.c calls this.
static int proc_dir_try_open(const char *rp) {
    char dsb[4200];
    rp = proc_deself(rp, dsb, sizeof dsb); // /proc/self/task -> /proc/<cpid>/task
    if (!rp || strncmp(rp, "/proc/", 6)) return -2;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0) return -2;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    int pid = atoi(num), host;
    if (pid != (int)getpid() && pid != container_pid() && pid != 1 && !proc_pid_member(pid, &host)) return -2;
    const char *rest = q + i; // "" | "/task" | "/task/<tid>" | "/task/<tid>/<leaf>" | "/<leaf>"
    if (rest[0] == 0 || (rest[0] == '/' && rest[1] == 0)) {
        char gpath[32];
        snprintf(gpath, sizeof gpath, "/proc/%d", pid);
        return proc_leaf_dir_open(gpath, 1);
    }
    if (!strncmp(rest, "/task", 5) && (rest[5] == 0 || (rest[5] == '/' && rest[6] == 0)))
        return proc_task_dir_open(pid);
    // map_files/ for OUR OWN pid: one "<start>-<end>" symlink per file-backed VMA. A peer's is left
    // unsynthesized rather than passed through -- the host directory is the ENGINE's mapping list.
    if (!strncmp(rest, "/map_files", 10) && (rest[10] == 0 || (rest[10] == '/' && rest[11] == 0)))
        return (pid == (int)getpid() || pid == container_pid()) ? proc_map_files_dir_open() : -1;
    if (!strncmp(rest, "/task/", 6)) {
        const char *t = rest + 6;
        int j = 0;
        while (t[j] >= '0' && t[j] <= '9')
            j++;
        if (j > 0 && (t[j] == 0 || (t[j] == '/' && t[j + 1] == 0))) {
            int tid = atoi(t);
            if (!proc_task_tid_visible(pid, tid)) return -2;
            char gpath[48];
            snprintf(gpath, sizeof gpath, "/proc/%d/task/%d", pid, tid);
            return proc_leaf_dir_open(gpath, 0);
        }
    }
    return -2; // a per-pid FILE -> proc_open serves it
}

// Materialize /proc as a real temp directory of entries (static files + one numeric name per live
// container process) so the guest's ordinary opendir/getdents enumerates it. Entries are empty regular
// files -- ps/top/htop identify pids by digit-name and then open /proc/<pid>/stat BY PATH (served by
// proc_open), so the entry type is irrelevant; empty files keep cleanup trivial (procfd_dir_rm). The
// dir is reaped when the guest closes the fd (shared g_procfd_dirs machinery). Returns the fd or -1.
static int proc_root_dir_open(void) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-rootXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    // ONLY names proc_open()/synth_stat actually serve -- listing an unserved name makes `ls /proc` stat it
    // and print "No such file or directory". "self" is the magic symlink (handled in synth_stat).
    static const char *const st[] = {"meminfo", "stat",   "cpuinfo", "uptime",  "loadavg",
                                     "version", "mounts", "self",    "cmdline", "filesystems",
                                     "swaps",   "vmstat", "modules", "devices", 0};
    for (int i = 0; st[i]; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/%s", tmpl, st[i]);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (kill(host, 0) != 0 && errno == ESRCH) { // dead -> prune the stale registry record
                char rp[352];
                if (path_join(rp, sizeof rp, dir, e->d_name) == 0) unlink(rp);
                continue;
            }
            int guest = (g_init_hostpid && host == g_init_hostpid) ? 1 : host;
            char p[96];
            snprintf(p, sizeof p, "%s/%d", tmpl, guest);
            hl_compat_mkdir(p, 0555); // a real (empty) subdir: getdents reports DT_DIR, and htop opens /proc/<pid>
        }
        closedir(d);
    }
    { // always list ourselves (our registry write may have lagged the first `ps`)
        char p[96];
        snprintf(p, sizeof p, "%s/%d", tmpl, container_pid());
        hl_compat_mkdir(p, 0555);
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, "/proc"); // tag the fd's guest path so relative opens re-enter /proc synth
    return fd;
}

// materialize a /sys/class/net directory as a real temp dir the guest's opendir/getdents can
// walk. The class dir lists the two interfaces (lo, eth0) as subdirs; an interface dir lists its
// attribute files. FILE content is served live via proc_open on the (re-intercepted) relative/absolute
// open. Returns the fd, -1 on error, or -2 if `gp` is not a sysfs-net directory we synthesize.
static int sysnet_hidden(const char *gp) {
    static const char prefix[] = "/sys/class/net/eth0";
    return net_isolate() && gp != NULL && strncmp(gp, prefix, sizeof(prefix) - 1) == 0 &&
           (gp[sizeof(prefix) - 1] == 0 || gp[sizeof(prefix) - 1] == '/');
}

static int sysnet_dir_open(const char *gp) {
    if (!gp || strncmp(gp, "/sys/class/net", 14)) return -2;
    const char *r = gp + 14;
    const char *const *entries;
    // --network none: loopback-only, so /sys/class/net lists just `lo` (no eth0).
    static const char *const ifaces[] = {"lo", "eth0", 0};
    static const char *const ifaces_lo[] = {"lo", 0};
    static const char *const attrs[] = {
        "address", "addr_len", "broadcast",    "flags", "mtu",    "operstate",       "type",       "carrier",
        "ifindex", "iflink",   "tx_queue_len", "speed", "duplex", "carrier_changes", "statistics", 0};
    // per-net_device statistics counters (fixed kernel set) node_exporter/ifstat read directly from sysfs.
    static const char *const stats[] = {
        "collisions",       "multicast",           "rx_bytes",       "rx_compressed",    "rx_crc_errors",
        "rx_dropped",       "rx_errors",           "rx_fifo_errors", "rx_frame_errors",  "rx_length_errors",
        "rx_missed_errors", "rx_nohandler",        "rx_over_errors", "rx_packets",       "tx_aborted_errors",
        "tx_bytes",         "tx_carrier_errors",   "tx_compressed",  "tx_dropped",       "tx_errors",
        "tx_fifo_errors",   "tx_heartbeat_errors", "tx_packets",     "tx_window_errors", 0};
    int as_dirs; // class dir -> iface subdirs; iface dir -> attribute files
    if (r[0] == 0 || (r[0] == '/' && r[1] == 0)) {
        entries = net_isolate() ? ifaces_lo : ifaces;
        as_dirs = 1;
    } else if (r[0] == '/' && (!strcmp(r + 1, "lo") || (!net_isolate() && !strcmp(r + 1, "eth0")))) {
        entries = attrs;
        as_dirs = 0;
    } else if (r[0] == '/' &&
               (!strcmp(r + 1, "lo/statistics") || (!net_isolate() && !strcmp(r + 1, "eth0/statistics")))) {
        entries = stats; // the statistics/ subdir: one counter file per entry
        as_dirs = 0;
    } else
        return -2;
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-netXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int i = 0; entries[i]; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/%s", tmpl, entries[i]);
        if (as_dirs || !strcmp(entries[i], "statistics")) // statistics/ is a subdir even within an iface dir
            hl_compat_mkdir(p, 0555);
        else {
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    char gpath[64];
    snprintf(gpath, sizeof gpath, "/sys/class/net%s", (r[0] == '/') ? r : "");
    proc_dir_register(fd, tmpl, gpath); // tag guest path so a relative reopen re-enters this synth
    return fd;
}

// materialize the CPU-topology sysfs DIRECTORY so getdents enumerates one cpuN subdir per online
// CPU. htop's LinuxMachine_updateCPUcount opendir()s /sys/devices/system/cpu, counts the cpuN subdirs
// (reading each cpuN/online to mark it active), and -- crucially -- when it finds NO cpuN dir it early-
// returns keeping its built-in default of ONE CPU. macOS has no /sys, and hl previously served only the
// online/possible/present FILES (absolute-path reads), never the directory, so htop's opendir hit the
// (missing) host /sys and htop showed 1 CPU on a many-core host. glibc __get_nprocs_conf and tcmalloc
// NumPossibleCPUs likewise count these cpuN dirs. Two shapes:
//   - base "/sys/devices/system/cpu": a temp dir holding cpu0..cpu(N-1) as real SUBDIRS (htop only
//     accepts DT_DIR/DT_UNKNOWN entries) plus the online/possible/present placeholder files (so a plain
//     readdir sees them too -- their CONTENT is still served by the absolute-path synth in fs.c).
//   - a "/sys/devices/system/cpu/cpuN" leaf: an EMPTY temp dir. htop opens it O_DIRECTORY|O_PATH and then
//     openat(cpuN,"online") -> ENOENT (res<1) which htop counts as active -- exactly the real-Linux shape
//     (cpuN has no per-cpu `online` file). The dir must OPEN successfully or htop `continue`s past the CPU.
// Returns the fd, -1 on error, or -2 if `gp` is not the cpu-topology dir / a cpuN subdir we synthesize.
static int syscpu_dir_open(const char *gp) {
    if (!gp || strncmp(gp, "/sys/devices/system/cpu", 23)) return -2;
    const char *r = gp + 23;
    int is_base = (r[0] == 0 || (r[0] == '/' && r[1] == 0));
    int cpuN = -1;
    if (!is_base) {
        if (r[0] != '/' || strncmp(r + 1, "cpu", 3)) return -2; // not a /sys/devices/system/cpu/cpuN leaf
        const char *d = r + 4;
        if (*d < '0' || *d > '9') return -2;
        cpuN = 0;
        for (; *d >= '0' && *d <= '9'; d++)
            cpuN = cpuN * 10 + (*d - '0');
        if (*d != 0) return -2; // trailing junk (cpufreq/cpuidle/... are files/dirs, not our cpuN synth)
    }
    int nc = container_online_cpus();                    // host online count, docker --cpus capped (state.c)
    if (!is_base && (cpuN < 0 || cpuN >= nc)) return -2; // an out-of-range cpuN: not one we advertise
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-cpu-dirXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char gpath[48];
    if (is_base) {
        for (int i = 0; i < nc; i++) {
            char p[96];
            snprintf(p, sizeof p, "%s/cpu%d", tmpl, i);
            hl_compat_mkdir(p, 0555); // real SUBDIR: getdents reports DT_DIR so htop counts it
        }
        static const char *const files[] = {"online", "possible", "present", "offline", 0};
        for (int i = 0; files[i]; i++) {
            char p[96];
            snprintf(p, sizeof p, "%s/%s", tmpl, files[i]);
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f); // content served on the absolute-path open (fs.c), not from this placeholder
        }
        snprintf(gpath, sizeof gpath, "/sys/devices/system/cpu");
    } else {
        snprintf(gpath, sizeof gpath, "/sys/devices/system/cpu/cpu%d", cpuN); // empty dir (no `online` leaf)
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, gpath); // tag guest path so a relative openat(cpuN)/readfileat re-enters synth
    return fd;
}

// Materialize an arbitrary synthetic directory as a temp dir of placeholder entries so opendir/getdents
// enumerate `names`; the CONTENT/target of each entry is served live on the (re-intercepted) open /
// readlink by proc_open / the fs.c readlink synth. kind: 0 = regular-file placeholders, 1 = symlink
// placeholders (namespace/fd magic links), 2 = subdir placeholders. `guestpath` tags the fd so a relative
// reopen re-enters the synth. Returns the fd, or -1 on error.
static int synth_names_dir_open(const char *guestpath, const char *const *names, int kind) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-sys-dirXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int i = 0; names[i]; i++) {
        char p[160];
        snprintf(p, sizeof p, "%s/%s", tmpl, names[i]);
        if (kind == 2)
            hl_compat_mkdir(p, 0555);
        else if (kind == 1) {
            if (symlink_idempotent(".", p) != 0) {
                procfd_dir_rm(tmpl);
                return -1;
            }
        } else {
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// If `gp` is one of the synthetic non-pid directories we enumerate (/proc/net, /proc/[self|pid]/ns,
// /sys/fs/cgroup, /sys/class/block, /sys/block, a cpuN/topology dir), materialize + return its fd; -2 if
// `gp` is not such a directory (caller falls through). Peer/self ns share the same name set.
// Predicate form (no materialization side effect): is `gp` one of the synthetic directories above? Used by
// synth_stat so a tool that stats the dir before opening it sees it as present.
static int synth_misc_dir_is(const char *gp) {
    if (!gp) return 0;
    if (!strcmp(gp, "/proc/net") || !strcmp(gp, "/proc/net/")) return 1;
    if (!strcmp(gp, "/proc/tty") || !strcmp(gp, "/proc/tty/")) return 1;
    if (!strcmp(gp, "/sys/fs/cgroup") || !strcmp(gp, "/sys/fs/cgroup/")) return 1;
    if (!strcmp(gp, "/sys/class/block") || !strcmp(gp, "/sys/class/block/")) return 1;
    if (!strcmp(gp, "/sys/block") || !strcmp(gp, "/sys/block/")) return 1;
    {
        char dsb[4200];
        const char *rp = proc_deself(gp, dsb, sizeof dsb);
        const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
        if (q) {
            int i = 0;
            while (q[i] >= '0' && q[i] <= '9')
                i++;
            if (i > 0 && (!strcmp(q + i, "/ns") || !strcmp(q + i, "/ns/"))) return 1;
            if (i > 0 && (!strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/"))) return 1;
        }
    }
    if (!strncmp(gp, "/sys/devices/system/cpu/cpu", 27)) {
        const char *d = gp + 27;
        if (*d >= '0' && *d <= '9') {
            while (*d >= '0' && *d <= '9')
                d++;
            if (!strcmp(d, "/topology") || !strcmp(d, "/topology/")) return 1;
        }
    }
    return 0;
}

static int synth_proc_fd_dir_is(const char *gp) {
    if (!gp) return 0;
    char dsb[4200];
    const char *rp = proc_deself(gp, dsb, sizeof dsb);
    const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
    if (!q) return 0;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9')
        i++;
    if (!i) return 0;
    return !strcmp(q + i, "/fd") || !strcmp(q + i, "/fd/") || !strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/");
}

static int synth_misc_dir_open(const char *gp) {
    if (!gp) return -2;
    if (!strcmp(gp, "/dev/fd") || !strcmp(gp, "/dev/fd/")) return proc_fd_dir_open(); // /dev/fd == /proc/self/fd
    // /proc/net: direct leaves (tcp/dev/unix/…) exist but the dir must enumerate them too.
    if (!strcmp(gp, "/proc/net") || !strcmp(gp, "/proc/net/")) {
        static const char *const net[] = {"tcp",       "tcp6",       "udp",  "udp6",  "unix",    "dev",
                                          "route",     "if_inet6",   "snmp", "snmp6", "netstat", "sockstat",
                                          "sockstat6", "ipv6_route", "arp",  "igmp",  0};
        return synth_names_dir_open("/proc/net", net, 0);
    }
    // /proc/tty: tty discovery tools (agetty, `ls /proc/tty`) walk this before reading drivers.
    if (!strcmp(gp, "/proc/tty") || !strcmp(gp, "/proc/tty/")) {
        static const char *const tty[] = {"drivers", "ldiscs", 0};
        return synth_names_dir_open("/proc/tty", tty, 0);
    }
    // /proc/[self|<pid>]/ns: enumerate the namespace magic links (readlink served in fs.c).
    {
        char dsb[4200];
        const char *rp = proc_deself(gp, dsb, sizeof dsb);
        const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
        if (q) {
            int i = 0;
            while (q[i] >= '0' && q[i] <= '9')
                i++;
            if (i > 0 && (!strcmp(q + i, "/fd") || !strcmp(q + i, "/fd/"))) {
                int guest = atoi(q);
                return guest == (int)getpid() ? proc_fd_dir_open() : proc_fd_dir_pid_open(guest, guest);
            }
            if (i > 0 && (!strcmp(q + i, "/ns") || !strcmp(q + i, "/ns/"))) {
                static const char *const ns[] = {
                    "cgroup", "ipc", "mnt", "net", "pid", "pid_for_children", "time", "time_for_children",
                    "user",   "uts", 0};
                return synth_names_dir_open(rp, ns, 1);
            }
            if (i > 0 && (!strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/"))) return proc_fdinfo_dir_open(rp);
        }
    }
    // /sys/fs/cgroup root: advertised in mountinfo, so a directory walk of the hierarchy must list it.
    if (!strcmp(gp, "/sys/fs/cgroup") || !strcmp(gp, "/sys/fs/cgroup/")) {
        static const char *const cg[] = {"cgroup.controllers",
                                         "cgroup.subtree_control",
                                         "cgroup.type",
                                         "cgroup.procs",
                                         "cgroup.threads",
                                         "cgroup.events",
                                         "cgroup.stat",
                                         "cgroup.max.depth",
                                         "cgroup.max.descendants",
                                         "cpu.max",
                                         "cpu.stat",
                                         "cpu.weight",
                                         "cpuset.cpus",
                                         "cpuset.mems",
                                         "cpuset.cpus.effective",
                                         "cpuset.mems.effective",
                                         "memory.max",
                                         "memory.min",
                                         "memory.low",
                                         "memory.high",
                                         "memory.current",
                                         "memory.peak",
                                         "memory.events",
                                         "memory.stat",
                                         "memory.swap.max",
                                         "memory.swap.current",
                                         "memory.oom.group",
                                         "pids.max",
                                         "pids.current",
                                         "pids.peak",
                                         "pids.events",
                                         "io.max",
                                         "io.stat",
                                         "io.weight",
                                         0};
        return synth_names_dir_open("/sys/fs/cgroup", cg, 0);
    }
    // /sys/class/block + /sys/block: storage sysfs (lsblk/installers). No real block devices are backed,
    // but the directories must EXIST and be enumerable (Linux exposes them inside containers).
    if (!strcmp(gp, "/sys/class/block") || !strcmp(gp, "/sys/class/block/") || !strcmp(gp, "/sys/block") ||
        !strcmp(gp, "/sys/block/")) {
        static const char *const empty[] = {0};
        return synth_names_dir_open(gp, empty, 2);
    }
    // /sys/devices/system/cpu/cpuN/topology: lscpu enumerates this dir before opening the leaves.
    if (!strncmp(gp, "/sys/devices/system/cpu/cpu", 27)) {
        const char *d = gp + 27;
        if (*d >= '0' && *d <= '9') {
            while (*d >= '0' && *d <= '9')
                d++;
            if (!strcmp(d, "/topology") || !strcmp(d, "/topology/")) {
                static const char *const topo[] = {"core_id",
                                                   "physical_package_id",
                                                   "cluster_id",
                                                   "thread_siblings",
                                                   "thread_siblings_list",
                                                   "core_siblings",
                                                   "core_siblings_list",
                                                   "core_cpus",
                                                   "core_cpus_list",
                                                   "package_cpus",
                                                   "package_cpus_list",
                                                   0};
                return synth_names_dir_open(gp, topo, 0);
            }
        }
    }
    return -2;
}

// Format a Linux cpumask hex string (as /sys topology mask files print it): zero-padded groups of up to 32
// bits, most-significant group first, comma-separated. `all` -> every online CPU set; else just bit `bit`.
// `ndig` is the low-group width the kernel pads to for this machine (DIV_ROUND_UP(nc,4)); e.g. nc=18 -> 5.
static void cpumask_hex(char *out, size_t n, int nc, int all, int bit, int ndig) {
    if (!out || n == 0) return;
    if (nc < 1) nc = 1;
    if (nc > 64) nc = 64;
    if (ndig < 1) ndig = 1;
    if (ndig > 8) ndig = 8;
    unsigned long long v = all ? (nc >= 64 ? ~0ULL : ((1ULL << nc) - 1ULL)) : (1ULL << (bit & 63));
    if (nc <= 32) {
        snprintf(out, n, "%0*llx", ndig, v & 0xffffffffULL);
        return;
    }
    int hidig = ((nc - 32) + 3) / 4;
    if (hidig < 1) hidig = 1;
    snprintf(out, n, "%0*x,%08x", hidig, (unsigned)(v >> 32), (unsigned)(v & 0xffffffffULL));
}

// The CONTENT of one /sys/devices/system/cpu/cpuN/topology/<leaf> attribute. hl advertises a FLAT topology:
// single socket (physical_package_id 0), no SMT (each logical CPU is its own core -> core_id = cpuN, thread
// siblings = {cpuN}), all online CPUs in one package. lscpu/util-linux reconstruct sockets/cores/threads
// from exactly these files; real docker always serves them, so an ENOENT here is a engine-specific divergence that
// makes lscpu mis-count or error. Returns the NUL-terminated length, or -1 if `leaf` is not one we serve.
static int syscpu_topology_str(const char *leaf, int cpuN, int nc, char *out, size_t n) {
    int ndig = (nc + 3) / 4;
    if (ndig < 1) ndig = 1;
    if (!strcmp(leaf, "core_id")) return snprintf(out, n, "%d\n", cpuN);
    if (!strcmp(leaf, "physical_package_id") || !strcmp(leaf, "cluster_id")) return snprintf(out, n, "0\n");
    if (!strcmp(leaf, "thread_siblings_list") || !strcmp(leaf, "core_cpus_list")) return snprintf(out, n, "%d\n", cpuN);
    if (!strcmp(leaf, "core_siblings_list") || !strcmp(leaf, "package_cpus_list") || !strcmp(leaf, "cluster_cpus_list"))
        return nc > 1 ? snprintf(out, n, "0-%d\n", nc - 1) : snprintf(out, n, "0\n");
    char m[96];
    if (!strcmp(leaf, "thread_siblings") || !strcmp(leaf, "core_cpus")) {
        cpumask_hex(m, sizeof m, nc, 0, cpuN, ndig);
        return snprintf(out, n, "%s\n", m);
    }
    if (!strcmp(leaf, "core_siblings") || !strcmp(leaf, "package_cpus") || !strcmp(leaf, "cluster_cpus")) {
        cpumask_hex(m, sizeof m, nc, 1, 0, ndig);
        return snprintf(out, n, "%s\n", m);
    }
    return -1;
}

// Parse+serve a full /sys/devices/system/cpu/cpuN/topology/<leaf> path. Returns content length (out is
// NUL-terminated) or -1 if `rp` is not a topology file we synthesize (bad cpuN, unknown leaf, wrong shape).
static int syscpu_topology_content(const char *rp, char *out, size_t n) {
    if (!rp || strncmp(rp, "/sys/devices/system/cpu/cpu", 27)) return -1;
    const char *d = rp + 27;
    if (*d < '0' || *d > '9') return -1;
    int cpuN = 0;
    for (; *d >= '0' && *d <= '9'; d++)
        cpuN = cpuN * 10 + (*d - '0');
    if (strncmp(d, "/topology/", 10)) return -1;
    const char *leaf = d + 10;
    if (!*leaf || strchr(leaf, '/')) return -1;
    int nc = container_online_cpus();
    if (cpuN < 0 || cpuN >= nc) return -1;
    return syscpu_topology_str(leaf, cpuN, nc, out, n);
}

// Format 16 raw bytes as a Linux UUID string ("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\n"), stamping the
// RFC-4122 version-4 (b[6]) and variant (b[8]) bits so the result parses as a valid random UUID. Writes
// 37 bytes (36 + '\n') plus a NUL into out (needs >= 38). Returns the byte count (37).
static int uuid_fmt(char *out, size_t cap, uint8_t b[16]) {
    b[6] = (uint8_t)((b[6] & 0x0f) | 0x40);
    b[8] = (uint8_t)((b[8] & 0x3f) | 0x80);
    return snprintf(out, cap, "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x\n", b[0], b[1],
                    b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]);
}

// The 16 raw bytes of the container's boot identity. Must be STABLE for the container's whole life AND
// IDENTICAL across every process in it (each guest process is a separate host engine, so a per-process
// arc4random value would disagree between peers). Derived DETERMINISTICALLY from the per-container
// registry key (HL_NETNS, minted at startup and inherited across fork/execve so every peer
// agrees -- see proc_reg_key) via FNV-1a expanded to 16 bytes. Same container -> same bytes everywhere;
// different containers -> different bytes. Backs both boot_id (UUID) and machine-id (32 hex).
static void boot_id_bytes(uint8_t b[16]) {
    char key[80];
    proc_reg_key(key, sizeof key);       // HL_NETNS -> HL_HOSTNAME -> session id fallback
    uint64_t h = 1469598103934665603ULL; // FNV-1a offset basis
    for (const char *p = key; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 1099511628211ULL;
    }
    for (int i = 0; i < 16; i++) {
        b[i] = (uint8_t)(h >> ((i & 7) * 8));
        if ((i & 7) == 7) h = h * 6364136223846793005ULL + 1442695040888963407ULL; // advance for hi 8 bytes
    }
}

// /proc/sys/kernel/random/boot_id (systemd/dbus/libuuid/journald key machine state off it).
static int proc_boot_id(char *out, size_t cap) {
    uint8_t b[16];
    boot_id_bytes(b);
    return uuid_fmt(out, cap, b);
}

// /proc/[self|<pid>]/limits -- the rlimit table (Go runtime, nginx, java, systemd read RLIMIT_NOFILE from
// it). Values mirror the engine's own getrlimit/prlimit answers (svc_fill_rlimit: stack 8MB, nofile
// 20480/1048576, everything else unlimited) so the file and the syscall agree.
static int proc_limits_text(char *buf, size_t cap) {
    // name, soft, hard, units ("" -> no unit column value). "unlimited" for RLIM_INFINITY rows.
    static const struct {
        const char *nm, *soft, *hard, *unit;
    } L[] = {
        {"Max cpu time", "unlimited", "unlimited", "seconds"},
        {"Max file size", "unlimited", "unlimited", "bytes"},
        {"Max data size", "unlimited", "unlimited", "bytes"},
        {"Max stack size", "8388608", "unlimited", "bytes"},
        {"Max core file size", "0", "unlimited", "bytes"}, // cores OFF (soft=0), matching getrlimit(RLIMIT_CORE)
        {"Max resident set", "unlimited", "unlimited", "bytes"},
        {"Max processes", "unlimited", "unlimited", "processes"},
        {"Max open files", "20480", "1048576", "files"}, // oracle (docker default soft): was 1024
        {"Max locked memory", "unlimited", "unlimited", "bytes"},
        {"Max address space", "unlimited", "unlimited", "bytes"},
        {"Max file locks", "unlimited", "unlimited", "locks"},
        {"Max pending signals", "unlimited", "unlimited", "signals"},
        {"Max msgqueue size", "unlimited", "unlimited", "bytes"},
        {"Max nice priority", "0", "0", ""},
        {"Max realtime priority", "0", "0", ""},
        {"Max realtime timeout", "unlimited", "unlimited", "us"},
    };

    // NOFILE hard cap is the enforceable guest fd ceiling (hl_engine_guest_fd_limit, derived from the host
    // RLIMIT_NOFILE and HL_LINUX_FD_LIMIT). getrlimit/prlimit64 report exactly this value (svc_fill_rlimit),
    // so the /proc row must render the same number rather than a stale hard-coded 1048576 -- otherwise the
    // syscall surface and /proc/self/limits disagree (glibc/JVM/systemd read both).
    char nofile_hard[24];
    {
        uint32_t guest_limit = hl_engine_guest_fd_limit();
        snprintf(nofile_hard, sizeof nofile_hard, "%u", guest_limit > 0 ? guest_limit : 20480u);
    }

    int n = snprintf(buf, cap, "%-25s %-20s %-20s %-10s\n", "Limit", "Soft Limit", "Hard Limit", "Units");
    for (size_t i = 0; i < sizeof L / sizeof *L; i++) {
        const char *soft = L[i].soft, *hard = L[i].hard;
        if (i == 7) hard = nofile_hard; // RLIMIT_NOFILE: mirror getrlimit's enforceable hard cap
        // docker --ulimit override (g_limits, resource number == table index): render the requested values
        // so /proc/self/limits agrees with getrlimit (svc_fill_rlimit). RLIM_INFINITY -> "unlimited".
        char sb[24], hb[24];
        uint64_t current, maximum;
        if (i < HL_LIMIT_COUNT && hl_limit_table_get(&g_limits, (int)i, &current, &maximum)) {
            if (current == ~0ull)
                soft = "unlimited";
            else {
                snprintf(sb, sizeof sb, "%llu", (unsigned long long)current);
                soft = sb;
            }
            if (maximum == ~0ull)
                hard = "unlimited";
            else {
                snprintf(hb, sizeof hb, "%llu", (unsigned long long)maximum);
                hard = hb;
            }
        }
        n += snprintf(buf + n, cap - (size_t)n, "%-25s %-20s %-20s %-10s\n", L[i].nm, soft, hard, L[i].unit);
    }
    return n;
}

// ---- runc/containerd MaskedPaths + ReadonlyPaths (container isolation, spec.go DefaultSpec) ----
// Masked paths must EXIST but be empty/inaccessible (NOT ENOENT), so monitoring agents and systemd unit
// `ConditionPathExists` checks that stat them behave as under runc. Kind: 1 = masked FILE (opens as an empty
// file, reads 0 bytes -- runc binds /dev/null over it); 2 = masked DIR (opens as an empty dir -- runc mounts
// an empty tmpfs). `rp` is the container-absolute path. Exact list = containerd pkg/oci spec.go MaskedPaths.
static int proc_masked_kind(const char *rp) {
    if (!rp) return 0;
    static const char *const files[] = {"/proc/kcore",
                                        "/proc/keys",
                                        "/proc/latency_stats",
                                        "/proc/timer_list",
                                        "/proc/timer_stats",
                                        "/proc/sched_debug",
                                        0};
    static const char *const dirs[] = {
        "/proc/asound", "/proc/acpi", "/proc/scsi", "/sys/firmware", "/sys/devices/virtual/powercap", 0};
    for (int i = 0; files[i]; i++)
        if (!strcmp(rp, files[i])) return 1;
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 2; // the dir or anything within it
    }
    return 0;
}

// 1 if `rp` is a runc ReadonlyPath (/proc/bus /proc/fs /proc/irq /proc/sys /proc/sysrq-trigger): reads are
// allowed (served by the /proc synth or an empty dir), writes fail EROFS -- runc bind-mounts these read-only.
static int proc_ro_path(const char *rp) {
    if (!rp) return 0;
    if (!strcmp(rp, "/proc/sysrq-trigger")) return 1;
    static const char *const dirs[] = {"/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", 0};
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 1;
    }
    return 0;
}

// 1 if `rp` is one of the ReadonlyPath DIRECTORIES that has no other synth (so stat/opendir see an empty,
// read-only directory). /proc/sys is served by proc_open; /proc/sysrq-trigger is a file (handled separately).
static int proc_ro_dir(const char *rp) {
    if (!rp) return 0;
    static const char *const dirs[] = {"/proc/bus", "/proc/fs", "/proc/irq", 0};
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 1;
    }
    return 0;
}

// Materialize a fresh EMPTY temp directory and return an O_DIRECTORY fd to it (reaped when the guest closes
// the fd, via the shared g_procfd_dirs machinery). Backs masked dirs + read-only proc dirs: getdents yields
// nothing, exactly like runc's empty-tmpfs mask. -1 on error.
static int empty_dir_fd(const char *guestpath) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-maskXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// Serve a masked / read-only-dir proc path as an open fd (empty file or empty dir). Returns the fd, or -2 if
// `rp` is not one hl masks (so the caller falls through to the normal path). Reserved for READ opens; the
// write-intent EROFS for ReadonlyPaths is enforced in openat before this is reached.
static int proc_masked_open(const char *rp) {
    int mk = proc_masked_kind(rp);
    if (mk == 1) return proc_text_fd("", 0);                            // empty regular file
    if (mk == 2) return empty_dir_fd(rp);                               // empty directory
    if (proc_ro_dir(rp)) return empty_dir_fd(rp);                       // /proc/bus,/fs,/irq: exist, empty, read-only
    if (!strcmp(rp, "/proc/sysrq-trigger")) return proc_text_fd("", 0); // exists, empty on read
    return -2;
}

// Real macOS stat -> Linux struct stat (the fake S_IFCHR version corrupted libc buffering).
// fill_linux_stat (the guest struct-stat layout) is per-arch -> translator/guest/<arch>/stat.c
// Synthesize the common /proc files Linux programs read (macOS has no /proc). Returns an fd
// holding the content, -1 on mkstemp error, or -2 if rp isn't a path we synthesize.
// Guest ISA from the auxv AT_PLATFORM string (type 15: "x86_64" vs "aarch64") the loader planted -- lets
// this shared TU tailor arch-specific pseudo-file content (e.g. /proc/cpuinfo) without a per-arch macro.
static int guest_is_x86(void) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 15 && v) return strncmp((const char *)(uintptr_t)v, "x86", 3) == 0;
    }
    return 0;
}

// ---- /proc/cpuinfo, one CPU model, two renderings --------------------------
// Both blocks below are DERIVED, never restated: the guest must not be able to get two different answers
// to "what CPU is this" from CPUID/auxv and from /proc. Each side reads the same single source the auxv
// reads -- hl_x86_cpuid() for the x86-64 guest, AT_HWCAP/AT_HWCAP2 (copied verbatim out of
// g_aarch64_cpu_model by the loader) for the aarch64 guest. tests/compat/procfs/cpumodel.c gates both.
// The arch is a compile-time property of the engine binary (one guest frontend per build), so the split is
// the same G_* seam every other per-guest detail uses -- and only the x86-64 build links hl_x86_cpuid.
#if G_SECCOMP_ARCH == 0xC000003Eu // AUDIT_ARCH_X86_64
#include "../../translator/guest/x86_64/cpuid.h"

// One CPUID leaf/subleaf, exactly as the guest's own CPUID instruction answers it -> {eax,ebx,ecx,edx}.
static void cpuinfo_cpuid(uint32_t leaf, uint32_t sub, uint32_t out[4]) {
    struct cpu probe = {0}; // hl_x86_cpuid reads RAX/RCX and writes RAX..RDX; nothing else is touched
    probe.r[RAX] = leaf;
    probe.r[RCX] = sub;
    hl_x86_cpuid(&probe);
    out[0] = (uint32_t)probe.r[RAX];
    out[1] = (uint32_t)probe.r[RBX];
    out[2] = (uint32_t)probe.r[RCX];
    out[3] = (uint32_t)probe.r[RDX];
}

// CPUID bit -> /proc/cpuinfo flag token, in the order Linux prints them (x86_cap_flags word order).
// `reg` indexes {eax,ebx,ecx,edx}. constant_tsc/nonstop_tsc are both the one invariant-TSC bit; `cpuid`
// and `nopl` are Linux synthetics every long-mode CPU gets, so they hang off LM. Nothing here is a
// standing claim: a flag appears iff hl_x86_cpuid sets its bit, so withholding MOVBE drops `movbe` too.
static const struct {
    uint32_t leaf, sub;
    uint8_t reg, bit;
    const char *name;
} X86_FLAG[] = {
    {1, 0, 3, 0, "fpu"},
    {1, 0, 3, 4, "tsc"},
    {1, 0, 3, 8, "cx8"},
    {1, 0, 3, 11, "sep"},
    {1, 0, 3, 13, "pge"},
    {1, 0, 3, 15, "cmov"},
    {1, 0, 3, 19, "clflush"},
    {1, 0, 3, 23, "mmx"},
    {1, 0, 3, 24, "fxsr"},
    {1, 0, 3, 25, "sse"},
    {1, 0, 3, 26, "sse2"},
    {0x80000001, 0, 3, 11, "syscall"},
    {0x80000001, 0, 3, 20, "nx"},
    {0x80000001, 0, 3, 27, "rdtscp"},
    {0x80000001, 0, 3, 29, "lm"},
    {0x80000007, 0, 3, 8, "constant_tsc"},
    {0x80000007, 0, 3, 8, "nonstop_tsc"},
    {0x80000001, 0, 3, 29, "cpuid"},
    {0x80000001, 0, 3, 29, "nopl"},
    {1, 0, 2, 0, "pni"},
    {1, 0, 2, 1, "pclmulqdq"},
    {1, 0, 2, 9, "ssse3"},
    {1, 0, 2, 13, "cx16"},
    {1, 0, 2, 19, "sse4_1"},
    {1, 0, 2, 20, "sse4_2"},
    {1, 0, 2, 22, "movbe"},
    {1, 0, 2, 23, "popcnt"},
    {1, 0, 2, 25, "aes"},
    {0x80000001, 0, 2, 0, "lahf_lm"},
    {7, 0, 1, 3, "bmi1"},
    {7, 0, 1, 8, "bmi2"},
    {7, 0, 1, 9, "erms"},
    {7, 0, 1, 29, "sha_ni"},
    {7, 0, 3, 4, "fsrm"},
};

// x86-64 /proc/cpuinfo block for one logical CPU: vendor, family/model/stepping, brand string, cpuid
// level, address sizes and the flag list all decoded out of the CPUID leaves themselves.
static int cpuinfo_x86_block(char *b, size_t n, int idx, int ncpu) {
    uint32_t l0[4], l1[4], ext[4], sizes[4];
    cpuinfo_cpuid(0, 0, l0);
    cpuinfo_cpuid(1, 0, l1);
    cpuinfo_cpuid(0x80000000u, 0, ext);
    cpuinfo_cpuid(0x80000008u, 0, sizes);
    char vendor[13];
    memcpy(vendor, &l0[1], 4);
    memcpy(vendor + 4, &l0[3], 4);
    memcpy(vendor + 8, &l0[2], 4);
    vendor[12] = 0;
    unsigned family = (l1[0] >> 8) & 0xf, model = (l1[0] >> 4) & 0xf;
    if (family == 0xf) family += (l1[0] >> 20) & 0xff;
    if (family == 6 || family == 0xf) model |= ((l1[0] >> 16) & 0xf) << 4;
    char brand[49] = {0}; // brand leaves are space-padded; Linux prints the trimmed string
    if (ext[0] >= 0x80000004u)
        for (uint32_t i = 0; i < 3; i++) {
            uint32_t r[4];
            cpuinfo_cpuid(0x80000002u + i, 0, r);
            memcpy(brand + i * 16, r, 16);
        }
    const char *name = brand;
    while (*name == ' ')
        name++;
    char flags[512];
    int fn = 0;
    flags[0] = 0;
    for (size_t i = 0; i < sizeof X86_FLAG / sizeof X86_FLAG[0]; i++) {
        uint32_t r[4];
        cpuinfo_cpuid(X86_FLAG[i].leaf, X86_FLAG[i].sub, r);
        if (!((r[X86_FLAG[i].reg] >> X86_FLAG[i].bit) & 1u)) continue;
        int w = snprintf(flags + fn, sizeof flags - (size_t)fn, "%s%s", fn ? " " : "", X86_FLAG[i].name);
        if (w < 0 || (size_t)w >= sizeof flags - (size_t)fn) break;
        fn += w;
    }
    return snprintf(b, n,
                    "processor\t: %d\nvendor_id\t: %s\ncpu family\t: %u\nmodel\t\t: %u\n"
                    "model name\t: %s\nstepping\t: %u\nmicrocode\t: 0x1\ncpu MHz\t\t: 2500.000\n"
                    "cache size\t: 8192 KB\nphysical id\t: 0\nsiblings\t: %d\ncore id\t\t: %d\ncpu cores\t: %d\n"
                    "apicid\t\t: %d\ninitial apicid\t: %d\nfpu\t\t: yes\nfpu_exception\t: yes\ncpuid level\t: %u\n"
                    "wp\t\t: yes\nflags\t\t: %s\n"
                    "bugs\t\t:\nbogomips\t: 5000.00\nclflush size\t: 64\ncache_alignment\t: 64\n"
                    "address sizes\t: %u bits physical, %u bits virtual\npower management:\n\n",
                    idx, vendor, family, model, name, l1[0] & 0xf, ncpu, idx, ncpu, idx, idx, l0[0], flags,
                    sizes[0] & 0xff, (sizes[0] >> 8) & 0xff);
}

#define cpuinfo_block(b, n, i, nc) cpuinfo_x86_block((b), (n), (i), (nc))
#else
// HWCAP/HWCAP2 bit -> the token arch/arm64/kernel/cpuinfo.c prints; NULL is a bit Linux does not name.
static const char *const ARM_HWCAP[64] = {
    "fp",    "asimd",    "evtstrm", "aes",   "pmull",  "sha1",  "sha2", "crc32", "atomics", "fphp",    "asimdhp",
    "cpuid", "asimdrdm", "jscvt",   "fcma",  "lrcpc",  "dcpop", "sha3", "sm3",   "sm4",     "asimddp", "sha512",
    "sve",   "asimdfhm", "dit",     "uscat", "ilrcpc", "flagm", "ssbs", "sb",    "paca",    "pacg"};
static const char *const ARM_HWCAP2[64] = {"dcpodp",  "sve2",   "sveaes", "svepmull", "svebitperm", "svesha3",
                                           "svesm4",  "flagm2", "frint",  "svei8mm",  "svef32mm",   "svef64mm",
                                           "svebf16", "i8mm",   "bf16",   "dgh",      "rng",        "bti",
                                           "mte",     "ecv",    "afp",    "rpres"};

// The value the loader planted for auxv entry `type`, or 0 when there is none.
static uint64_t guest_auxv(uint64_t type) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == type) return v;
    }
    return 0;
}

// aarch64 /proc/cpuinfo block for one logical CPU. `Features` is the decode of the SAME AT_HWCAP/AT_HWCAP2
// pair the guest reads from its own auxv, so the seven features hl advertises beyond fp/asimd (aes pmull
// sha1 sha2 crc32 atomics asimddp) can no longer be missing from one surface and present on the other.
static int cpuinfo_arm_block(char *b, size_t n, int idx) {
    const uint64_t caps[2] = {guest_auxv(16), guest_auxv(26)};
    const char *const *names[2] = {ARM_HWCAP, ARM_HWCAP2};
    char feat[512];
    int fn = 0;
    feat[0] = 0;
    for (int word = 0; word < 2; word++)
        for (int i = 0; i < 64; i++) {
            if (!((caps[word] >> i) & 1u) || !names[word][i]) continue;
            int w = snprintf(feat + fn, sizeof feat - (size_t)fn, "%s%s", fn ? " " : "", names[word][i]);
            if (w < 0 || (size_t)w >= sizeof feat - (size_t)fn) break;
            fn += w;
        }
    return snprintf(b, n,
                    "processor\t: %d\nBogoMIPS\t: 100.00\nFeatures\t: %s\nCPU implementer\t: 0x61\n"
                    "CPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0x000\nCPU revision\t: 0\n\n",
                    idx, feat);
}

#define cpuinfo_block(b, n, i, nc) ((void)(nc), cpuinfo_arm_block((b), (n), (i)))
#endif

// Defined later in netns.c (same TU, included after vfs.c): emit the LISTEN rows for /proc/net/tcp[6].
static int netns_tcp_emit(char *out, size_t cap, int v6);

static int proc_open(const char *rp) {
    char buf[8192];
    int n = -1;
    // Per-thread files mirror the main process for a single-threaded proc: fold
    // /proc/<pid>/task/<tid>/<leaf> -> /proc/<pid>/<leaf> so htop's per-thread reads are served.
    char taskbuf[4200];
    {
        const char *t = strstr(rp, "/task/");
        if (t && !strncmp(rp, "/proc/", 6)) {
            const char *q = rp + 6;
            int k = 0;
            while (q[k] >= '0' && q[k] <= '9')
                k++;
            const char *s = t + 6; // after "/task/"
            while (*s >= '0' && *s <= '9')
                s++;
            if (s > t + 6 && *s == '/') { // a real /task/<tid>/ segment with a trailing leaf
                // The pid segment between /proc/ and /task is EITHER numeric OR the "self"/"thread-self"
                // magic name -- /proc/self/task/<tid>/<leaf> must fold just like the numeric form (else a
                // task walker that descends /proc/self/task/<tid> can list but not open its files).
                int seglen = (int)(t - q);
                int is_self =
                    (seglen == 4 && !strncmp(q, "self", 4)) || (seglen == 11 && !strncmp(q, "thread-self", 11));
                int is_num = (k > 0 && q + k == t);
                if (!is_self && !is_num) return -2;
                char tbuf[16];
                int tlen = (int)(s - (t + 6));
                tlen = tlen < (int)sizeof tbuf ? tlen : (int)sizeof tbuf - 1;
                memcpy(tbuf, t + 6, (size_t)tlen);
                tbuf[tlen] = 0;
                int pid = is_self ? container_pid() : atoi(q);
                if (!proc_task_tid_visible(pid, atoi(tbuf))) return -2;
                int head = (int)(t - rp);
                snprintf(taskbuf, sizeof taskbuf, "%.*s%s", head, rp, s);
                rp = taskbuf;
            }
        }
    }
    // the per-process network files are namespaced but a container is one net-namespace, so
    // /proc/[self|<pid>]/net/<leaf> mirrors the shared /proc/net/<leaf>. Fold it (ss/some Go/netlink
    // fallbacks read /proc/self/net/*). Without this those reads ENOENT'd under hl.
    char netbuf[4200];
    if (!strncmp(rp, "/proc/", 6)) {
        const char *q = rp + 6;
        const char *leaf2 = NULL;
        if (!strncmp(q, "self/net/", 9))
            leaf2 = q + 9;
        else {
            const char *d = q;
            while (*d >= '0' && *d <= '9')
                d++;
            if (d > q && !strncmp(d, "/net/", 5)) leaf2 = d + 5;
        }
        if (leaf2) {
            snprintf(netbuf, sizeof netbuf, "/proc/net/%s", leaf2);
            rp = netbuf;
        }
    }
    // Per-process files for the guest's own pid: /proc/[self|pid]/{fd,maps,smaps,status,stat,environ}.
    const char *leaf = proc_self_leaf(rp);
    if (leaf) {
        if (!strcmp(leaf, "fd")) return proc_fd_dir_open();
        if (!strncmp(leaf, "fdinfo/", 7) && leaf[7]) { // /proc/self/fdinfo/<N> body
            int isnum = 1;
            for (const char *t = leaf + 7; *t; t++)
                if (*t < '0' || *t > '9') isnum = 0;
            if (isnum) {
                int fn = atoi(leaf + 7);
                int m = proc_fdinfo_text(fn, buf, sizeof buf);
                if (m < 0) return -2; // closed/invalid fd -> ENOENT
                return proc_text_fd(buf, m);
            }
        }
        if (!strcmp(leaf, "pagemap")) {
            // VA-indexed binary pagemap: back it with an empty seekable regular fd (lseek to vaddr/pg*8
            // works natively) and synthesize the 8-byte-per-page entries on read (io.c). LTP mmap12.
            int fd = proc_text_fd("", 0);
            if (fd >= 0 && fd < HL_NFD) g_pagemap_fd[fd] = 1;
            return fd;
        }
        if (!strcmp(leaf, "maps") || !strcmp(leaf, "task/1/maps")) return proc_maps_fd(0);
        if (!strcmp(leaf, "smaps")) return proc_maps_fd(1);
        if (!strcmp(leaf, "numa_maps")) return proc_numa_maps_fd();
        if (!strcmp(leaf, "smaps_rollup")) return proc_smaps_rollup_fd();
        // /proc/self/mem is the process's OWN address space. Unintercepted it was the host open, i.e. the
        // ENGINE's address space: a guest could pread the engine's text and pwrite it back (pwrite there
        // bypasses page protection), which is an escape, not a leak. The guest's memory is the engine's
        // memory at a different address, so there is no correct pass-through -- deny it. EACCES is what a
        // reader without PTRACE_MODE_ATTACH already gets, so callers have the path.
        if (!strcmp(leaf, "mem")) {
            errno = EACCES;
            return -1;
        }
        // /proc/self/syscall published the ENGINE's stack pointer and program counter -- its ASLR slide.
        // The guest is never mid-syscall when it reads its own, so the kernel's "running" form is right.
        if (!strcmp(leaf, "syscall"))
            n = snprintf(buf, sizeof buf, "running\n");
        else if (!strcmp(leaf, "mountstats"))
            // The host's whole mount table, device names included, came through here while mounts/mountinfo
            // were intercepted. Same view as those two, in mountstats' "device X mounted on Y" form.
            n = proc_mountstats_text(buf, sizeof buf);
        else if (!strcmp(leaf, "status"))
            n = proc_status_text(buf, sizeof buf);
        else if (!strcmp(leaf, "stat"))
            n = proc_stat_text(buf, sizeof buf);
        else if (!strcmp(leaf, "statm"))
            n = proc_statm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "environ"))
            n = proc_environ_text(buf, sizeof buf);
        else if (!strcmp(leaf, "cmdline"))
            n = proc_cmdline_text(buf, sizeof buf);
        else if (!strcmp(leaf, "comm"))
            n = proc_comm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "mountinfo"))
            n = proc_mountinfo_text(buf, sizeof buf);
        else if (!strcmp(leaf, "limits"))
            n = proc_limits_text(buf, sizeof buf); // rlimit table
        else if (!strcmp(leaf, "oom_score_adj"))
            n = snprintf(buf, sizeof buf, "%d\n", g_proc_oom_score_adj);
        else if (!strcmp(leaf, "oom_adj") || !strcmp(leaf, "oom_score"))
            n = snprintf(buf, sizeof buf, "0\n");
        else if (!strcmp(leaf, "loginuid"))
            n = snprintf(buf, sizeof buf, "4294967295\n"); // unset (pam)
        else if (!strcmp(leaf, "cgroup"))
            n = snprintf(buf, sizeof buf, "0::/\n"); // cgroup v2 unified; also reached as /proc/<ourpid>/cgroup
        else if (!strcmp(leaf, "io"))
            // Per-process IO accounting. Monitoring agents (cAdvisor, language runtimes) read it
            // opportunistically; hl tracks no real per-process byte counters, so present the canonical
            // key set with a deterministic baseline (structural fidelity, like memory.stat/cpu.stat).
            n = snprintf(buf, sizeof buf,
                         "rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\n"
                         "cancelled_write_bytes: 0\n");
        if (n >= 0) {
            char desc[64];
            snprintf(desc, sizeof desc, "self:%s", leaf);
            return proc_text_fd_tagged(buf, n, desc);
        }
    }
    // A PEER container process: /proc/<otherpid>/{stat,status,cmdline,comm}. proc_self_leaf matched only
    // our own pid above, so a numeric pid reaching here is a peer -- synthesize from the registry (guest
    // comm/argv) + host process stats (live rss/cpu/state). This is what makes ps/top/htop show the whole
    // container.
    {
        int gp2;
        const char *fl = proc_any_leaf(rp, &gp2);
        if (fl && gp2 > 0) {
            int host;
            int is_oom_leaf = !strcmp(fl, "oom_score_adj") || !strcmp(fl, "oom_adj") || !strcmp(fl, "oom_score");
            if (proc_pid_member(gp2, &host) ||
                (is_oom_leaf && (host = (gp2 == 1 && g_init_hostpid) ? g_init_hostpid : gp2) > 0 &&
                 !(kill(host, 0) != 0 && errno == ESRCH))) {
                // Peer /proc/<pid>/fd: a listable dir of symlinks built from the peer descriptor snapshot, so
                // each entry readlinks to the fd's target. (Opening a peer fd link stays deferred -- needs
                // cross-process fd passing; see proc_fd_dir_pid_open.)
                if (!strcmp(fl, "fd")) return proc_fd_dir_pid_open(gp2, host);
                if (!strcmp(fl, "stat"))
                    n = proc_stat_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "status"))
                    n = proc_status_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "statm"))
                    n = proc_statm_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "maps"))
                    return proc_maps_pid_fd(gp2, host);
                else if (!strcmp(fl, "cmdline"))
                    n = proc_cmdline_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "comm"))
                    n = proc_comm_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "oom_score_adj") || !strcmp(fl, "oom_adj") || !strcmp(fl, "oom_score"))
                    n = snprintf(buf, sizeof buf, "0\n");
                else if (!strcmp(fl, "cgroup"))
                    // A container is ONE cgroup, so a peer's line is our own. Previously unserved, so it fell
                    // through to the host and published the engine's real cgroup path (a user@1000.service
                    // scope under a desktop session) as the guest's.
                    n = snprintf(buf, sizeof buf, "0::/\n");
                if (n >= 0) {
                    char desc[64];
                    snprintf(desc, sizeof desc, "pid:%d:%s", gp2, fl);
                    return proc_text_fd_tagged(buf, n, desc);
                }
            }
        }
    }
    if (!strcmp(rp, "/proc/cpuinfo")) {
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        // One block per online CPU, and container_online_cpus() caps at 64. The x86 block is 656 bytes today
        // and its flag list is derived, so bound it by that list's own 512-byte ceiling rather than by a
        // measurement: 1KB/CPU covers any model. (640 did not even cover today's block, and the shared 8KB
        // `buf` silently truncated cpuinfo to ~14 processors on a many-core host.) Each snprintf is still
        // clamped so a would-be overflow cannot inflate `cn` -- proc_text_fd writes exactly `cn` bytes.
        char cib[64 * 1024]; // per-call (proc_open is reentrant across guest threads); 64KB stack
        int cn = 0;
        for (int i = 0; i < nc; i++) {
            size_t rem = sizeof cib - (size_t)cn;
            int w = cpuinfo_block(cib + cn, rem, i, nc);
            if (w < 0 || (size_t)w >= rem) break; // truncated -> stop rather than over-report length
            cn += w;
        }
        return proc_text_fd(cib, cn);
    } else if (!strcmp(rp, "/proc/meminfo")) {
        // Real-ish figures: a cgroup memory.max caps MemTotal (used = the tracked anon charge); otherwise
        // report the host backend's memory snapshot so htop's memory meter reflects a believable,
        // non-zero footprint instead of "0K used".
        unsigned long long tot, fre, avail, cached;
        if (g_mem_max) {
            tot = g_mem_max / 1024;
            unsigned long long used = (unsigned long long)atomic_load(&g_mem_charged) / 1024;
            fre = tot > used ? tot - used : 0;
            avail = fre;
            cached = 0;
        } else {
            host_mem(&tot, &fre, &avail, &cached);
        }
        // Present the standard field set common procfs consumers read (Active/Inactive/Dirty/AnonPages/…);
        // omitting them disabled monitoring heuristics. Accounting figures hl does not track are zero.
        n = snprintf(buf, sizeof buf,
                     "MemTotal:    %11llu kB\nMemFree:     %11llu kB\n"
                     "MemAvailable:%11llu kB\nBuffers:               0 kB\nCached:      %11llu kB\n"
                     "SwapCached:            0 kB\nActive:                0 kB\nInactive:              0 kB\n"
                     "Active(anon):          0 kB\nInactive(anon):        0 kB\nActive(file):          0 kB\n"
                     "Inactive(file):        0 kB\nUnevictable:           0 kB\nMlocked:               0 kB\n"
                     "SwapTotal:             0 kB\nSwapFree:              0 kB\n"
                     "Dirty:                 0 kB\nWriteback:             0 kB\nAnonPages:             0 kB\n"
                     "Mapped:                0 kB\nShmem:                 0 kB\nKReclaimable:          0 kB\n"
                     "Slab:                  0 kB\nSReclaimable:          0 kB\nSUnreclaim:            0 kB\n"
                     "KernelStack:           0 kB\nPageTables:            0 kB\nWritebackTmp:          0 kB\n"
                     "CommitLimit: %11llu kB\nCommitted_AS:          0 kB\nVmallocTotal:   34359738367 kB\n"
                     "VmallocUsed:           0 kB\nVmallocChunk:          0 kB\n",
                     tot, fre, avail, cached, tot);
    } else if (!strcmp(rp, "/proc/stat")) {
        // Real host CPU jiffies -> the cpu line increments between reads, so htop/top meters move. The
        // aggregate `cpu` line and each per-core `cpuN` line come from the host system snapshot. The old code
        // split the aggregate EVENLY across cores (aggregate/ncpu), so every cpuN line was byte-identical
        // and htop/top showed every core meter moving in lockstep at the same %. Per-core real ticks make
        // the deltas differ, so a busy core reads hot while idle cores read cold -- exactly like Linux.
        unsigned long long t[4];
        host_cpu_ticks(t);
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        n = snprintf(buf, sizeof buf, "cpu  %llu %llu %llu %llu 0 0 0 0 0 0\n", t[0], t[3], t[1], t[2]);
        hl_host_cpu_ticks cores[64];
        hl_host_system_info system_info;
        int have_cores = hl_host_system_read(&system_info, cores, sizeof cores / sizeof cores[0]);
        for (int i = 0; i < nc; i++) {
            unsigned long long u, ni, sy, id;
            if (have_cores && i < (int)system_info.reported_cores) {
                u = cores[i].user;
                ni = cores[i].nice;
                sy = cores[i].system;
                id = cores[i].idle;
            } else { // API failed, or --cpus capped ABOVE the host core count: fall back to the even split
                u = t[0] / (unsigned)nc;
                ni = t[3] / (unsigned)nc;
                sy = t[1] / (unsigned)nc;
                id = t[2] / (unsigned)nc;
            }
            n += snprintf(buf + n, sizeof buf - (size_t)n, "cpu%d %llu %llu %llu %llu 0 0 0 0 0 0\n", i, u, ni, sy, id);
        }
        // intr/ctxt are cumulative-since-boot counters; monitoring heuristics divide by the interval and
        // treat a flat 0 as a dead system. Derive a monotone nonzero from host jiffies so consumers see live
        // counters. `processes` is cumulative forks since boot (Linux), not the live registry count.
        unsigned long long jif = t[0] + t[1] + t[2] + t[3];
        n += snprintf(buf + n, sizeof buf - (size_t)n,
                      "intr %llu\nctxt %llu\nbtime %ld\nprocesses %llu\nprocs_running 1\nprocs_blocked 0\n",
                      jif * 137ull + 1, jif * 509ull + 1, host_btime(),
                      (unsigned long long)atomic_load(&g_forks_since_boot) + 256ull);
    } else if (!strcmp(rp, "/proc/mounts") || !strcmp(rp, "/proc/self/mounts")) {
        // The fstab-style mount table (mirror of mountinfo). Name the root mount "overlay", not the legacy
        // "rootfs": busybox/util-linux df filters out a pseudo "rootfs" entry, leaving df unable to find the
        // mount for "/". The pseudo-filesystems are listed too so a reader enumerating mounts sees them.
        // Mirror of proc_mountinfo_text in fstab form (6 fields). Same set of pseudo-mounts docker lists so a
        // reader enumerating mounts (df/mount/findmnt) sees /dev/shm, /dev/pts, /dev/mqueue and the cgroup2
        // hierarchy. sysfs is ro (runc binds it ro); the /dev tmpfs carries its size/mode; /dev/shm is a
        // separate tmpfs with src "shm". Verified field-for-field vs the docker (runc) oracle.
        n = snprintf(buf, sizeof buf,
                     "overlay / overlay rw,relatime 0 0\n"
                     "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "tmpfs /dev tmpfs rw,nosuid,size=65536k,mode=755 0 0\n"
                     "devpts /dev/pts devpts rw,nosuid,noexec,relatime,gid=5,mode=620,ptmxmode=666 0 0\n"
                     "sysfs /sys sysfs ro,nosuid,nodev,noexec,relatime 0 0\n"
                     "cgroup /sys/fs/cgroup cgroup2 ro,nosuid,nodev,noexec,relatime,nsdelegate 0 0\n"
                     "mqueue /dev/mqueue mqueue rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "shm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=65536k 0 0\n");
        if (n > 0 && (size_t)n < sizeof buf) n = (int)mount_binds_append(buf, sizeof buf, (size_t)n, 1);
    } else if (!strcmp(rp, "/proc/uptime")) {
        unsigned long long t[4];
        host_cpu_ticks(t);
        long hz = sysconf(_SC_CLK_TCK);
        if (hz <= 0) hz = 100;
        double up = (double)(time(NULL) - host_btime());
        n = snprintf(buf, sizeof buf, "%.2f %.2f\n", up > 0 ? up : 0.0, (double)t[2] / (double)hz);
    } else if (!strcmp(rp, "/proc/loadavg")) {
        double la[3] = {0, 0, 0};
        getloadavg(la, 3);
        n = snprintf(buf, sizeof buf, "%.2f %.2f %.2f 1/%d %d\n", la[0], la[1], la[2], proc_reg_count(),
                     container_pid());
    } else if (!strcmp(rp, "/proc/sys/vm/overcommit_memory")) {
        // OrbStack/docker default is 1 (heuristic-off, "always overcommit"). redis-server prints
        // "WARNING overcommit_memory is set to 0! Background save may fail..." when it reads anything but 1,
        // so serving 0 made hl emit a startup warning a real-docker user never sees. Match the oracle: 1.
        n = snprintf(buf, sizeof buf, "1\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/hostname")) {
        // UTS ns (hostname cmd reads this)
        n = snprintf(buf, sizeof buf, "%s\n", g_hostname[0] ? g_hostname : "jit");
    } else if (!strcmp(rp, "/proc/sys/kernel/random/boot_id")) {
        // stable per-boot UUID (systemd/dbus/libuuid/curl/journald read it; without it tools print
        // "cannot find current boot id"). Deterministic from the container key -> same for every peer.
        n = proc_boot_id(buf, sizeof buf);
    } else if (!strcmp(rp, "/proc/sys/kernel/random/uuid")) {
        // Linux yields a FRESH type-4 UUID on every read of this file -- glibc/libuuid use it as a
        // uuid_generate_random source, so it must differ each open.
        uint8_t b[16];
        arc4random_buf(b, sizeof b);
        n = uuid_fmt(buf, sizeof buf, b);
    } else if (!strcmp(rp, "/proc/sys/kernel/random/entropy_avail")) {
        n = snprintf(buf, sizeof buf, "256\n"); // pool always "full" (host arc4random backs /dev/*random)
    } else if (!strcmp(rp, "/proc/sys/kernel/ostype")) {
        n = snprintf(buf, sizeof buf, "Linux\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/osrelease")) {
        n = snprintf(buf, sizeof buf, "6.1.0\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/version")) {
        n = snprintf(buf, sizeof buf, "#1 SMP hl-engine\n");
    } else if (!strcmp(rp, "/proc/self/cgroup")) {
        // cgroup v2 unified
        n = snprintf(buf, sizeof buf, "0::/\n");
    } else if (!strcmp(rp, "/proc/version")) {
        // The version banner embeds the build ISA; x86_64 guests see `uname -m`=x86_64, so /proc/version
        // must agree (a mismatched aarch64 token here confuses platform probes and diagnostics).
        n = snprintf(buf, sizeof buf, "Linux version 6.1.0 (hl-engine) %s\n", guest_is_x86() ? "x86_64" : "aarch64");
        // ---- container network introspection: lo + eth0 (see netif_* in state.c) --------------
    } else if (!strcmp(rp, "/proc/net/dev")) {
        // per-interface counters; zeros are fine (hl runs no real stack -- this is introspection only).
        // --network none: loopback-only, so eth0 is omitted (only the lo counters line).
        n = snprintf(buf, sizeof buf,
                     "Inter-|   Receive                                                |  Transmit\n"
                     " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets "
                     "errs drop fifo colls carrier compressed\n"
                     "    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n%s",
                     net_isolate() ? "" : "  eth0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n");
    } else if (!strcmp(rp, "/proc/net/route")) {
        // Destination/Gateway/Mask are %08X of the network-order addr (netif_* already store that form).
        // --network none: no eth0 routes -> just the header (loopback carries no routing table entries).
        if (net_isolate()) {
            n = snprintf(buf, sizeof buf,
                         "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
        } else {
            uint32_t net = netif_eth0_net(), gw = netif_eth0_gw();
            int pfx = netif_eth0_prefix();
            uint32_t mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u);
            n = snprintf(buf, sizeof buf,
                         "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n"
                         "eth0\t00000000\t%08X\t0003\t0\t0\t0\t00000000\t0\t0\t0\n"
                         "eth0\t%08X\t00000000\t0001\t0\t0\t0\t%08X\t0\t0\t0\n",
                         gw, net, mask);
        }
    } else if (!strcmp(rp, "/proc/net/if_inet6")) {
        // addr(32 hex) ifindex(hex) prefix(hex) scope(hex) flags(hex) devname -- lo ::1 only.
        n = snprintf(buf, sizeof buf, "00000000000000000000000000000001 01 80 10 80        lo\n");
    } else if (!strcmp(rp, "/proc/net/tcp")) {
        // v4 table: header + a LISTEN row per socket the guest bind()+listen()ed (ss/netstat -l depend on it).
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode\n");
        n += netns_tcp_emit(buf + n, sizeof buf - n, 0);
    } else if (!strcmp(rp, "/proc/net/tcp6")) {
        // tcp6 has a DISTINCT header from tcp4: the v6 address columns are 32 hex wide and the second column
        // is "remote_address" (not "rem_address"). Reusing the v4 header here was a engine-specific divergence.
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address                         remote_address                        st "
                     "tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");
        n += netns_tcp_emit(buf + n, sizeof buf - n, 1);
    } else if (!strcmp(rp, "/proc/net/udp")) {
        n = snprintf(buf, sizeof buf,
                     "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode ref pointer drops\n");
    } else if (!strcmp(rp, "/proc/net/udp6")) {
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address                         remote_address                        st "
                     "tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n");
    } else if (!strncmp(rp, "/sys/class/net/", 15)) {
        // per-interface attribute files tools stat/read (address, flags, mtu, operstate, type, ...).
        const char *rest = rp + 15;
        // --network none: eth0 does not exist, so its attribute files must not be served through the
        // direct (non-readdir) read path either -- otherwise a tool that reads /sys/class/net/eth0/address
        // sees an interface that readdir hid.
        int islo = !strncmp(rest, "lo/", 3), iseth = !net_isolate() && !strncmp(rest, "eth0/", 5);
        const char *file = islo ? rest + 3 : iseth ? rest + 5 : NULL;
        if (file) {
            if (!strcmp(file, "address")) {
                if (islo)
                    n = snprintf(buf, sizeof buf, "00:00:00:00:00:00\n");
                else {
                    uint8_t m[6];
                    netif_eth0_mac(m);
                    n = snprintf(buf, sizeof buf, "%02x:%02x:%02x:%02x:%02x:%02x\n", m[0], m[1], m[2], m[3], m[4],
                                 m[5]);
                }
            } else if (!strcmp(file, "addr_len"))
                n = snprintf(buf, sizeof buf, "6\n");
            else if (!strcmp(file, "broadcast"))
                n = snprintf(buf, sizeof buf, islo ? "00:00:00:00:00:00\n" : "ff:ff:ff:ff:ff:ff\n");
            else if (!strcmp(file, "flags"))
                n = snprintf(buf, sizeof buf, islo ? "0x9\n" : "0x1003\n");
            else if (!strcmp(file, "mtu"))
                n = snprintf(buf, sizeof buf, islo ? "65536\n" : "1500\n");
            else if (!strcmp(file, "operstate"))
                n = snprintf(buf, sizeof buf, islo ? "unknown\n" : "up\n");
            else if (!strcmp(file, "type"))
                n = snprintf(buf, sizeof buf, islo ? "772\n" : "1\n");
            else if (!strcmp(file, "carrier"))
                n = snprintf(buf, sizeof buf, "1\n");
            else if (!strcmp(file, "ifindex"))
                n = snprintf(buf, sizeof buf, islo ? "1\n" : "2\n");
            else if (!strcmp(file, "iflink"))
                n = snprintf(buf, sizeof buf, islo ? "1\n" : "2\n");
            else if (!strcmp(file, "tx_queue_len"))
                n = snprintf(buf, sizeof buf, islo ? "0\n" : "1000\n");
            else if (!strcmp(file, "mtu"))
                n = snprintf(buf, sizeof buf, islo ? "65536\n" : "1500\n");
            else if (!strcmp(file, "speed"))
                n = snprintf(buf, sizeof buf, "-1\n");
            else if (!strcmp(file, "duplex"))
                n = snprintf(buf, sizeof buf, "unknown\n");
            else if (!strcmp(file, "carrier_changes"))
                n = snprintf(buf, sizeof buf, "0\n");
            // statistics/<counter>: hl runs no real IP stack -> zero counters (consistent with /proc/net/dev).
            // node_exporter/ifstat read these per-interface files directly. Any known counter name -> "0\n".
            else if (!strncmp(file, "statistics/", 11) && file[11])
                n = snprintf(buf, sizeof buf, "0\n");
        }
        // cgroup v2: memory limit
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.max")) {
        if (g_mem_max)
            n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)g_mem_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.current")) {
        n = snprintf(buf, sizeof buf, "%llu\n", cgroup_mem_current()); // container-wide (all engine procs)
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.max")) {
        if (g_pids_max)
            n = snprintf(buf, sizeof buf, "%d\n", g_pids_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.current")) {
        n = snprintf(buf, sizeof buf, "%d\n", acct_pids_total()); // container-wide task count (all engine procs)
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.peak")) {
        n = snprintf(buf, sizeof buf, "%d\n", acct_pids_total()); // no historical peak tracked -> live
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.events") || !strcmp(rp, "/sys/fs/cgroup/pids.events.local")) {
        n = snprintf(buf, sizeof buf, "max 0\n"); // pids limit never hit (structural)
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpuset.cpus.effective") || !strcmp(rp, "/sys/fs/cgroup/cpuset.cpus")) {
        // The CPUs this cgroup may run on. cpuset.cpus.effective is what cpuset-aware runtimes read; advertise
        // the container's online set so a cpuset walk sees a populated range (was ENOENT -> walk failed).
        int nc = container_online_cpus();
        if (nc < 1) nc = 1;
        n = (nc == 1) ? snprintf(buf, sizeof buf, "0\n") : snprintf(buf, sizeof buf, "0-%d\n", nc - 1);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpuset.mems.effective") || !strcmp(rp, "/sys/fs/cgroup/cpuset.mems")) {
        n = snprintf(buf, sizeof buf, "0\n"); // single (emulated) memory node
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.stat.local")) {
        n = snprintf(buf, sizeof buf, "throttled_usec 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.oom.group")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.events")) {
        n = snprintf(buf, sizeof buf, "high 0\nmax 0\nfail 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.peak")) {
        n = snprintf(buf, sizeof buf, "0\n");
        // ---- cgroup v2 unified-hierarchy surface real runtimes SIZE THEMSELVES from ----------------------
        // The JVM (-XX:+UseContainerSupport), the Go runtime (GOMAXPROCS/GOMEMLIMIT tooling), Node/libuv, and
        // systemd read these to pick heap size, GC/CommonPool/worker thread counts, and to detect that they are
        // in a v2 container at all. Values MUST reflect the docker --cpus/--memory caps (state.c g_cpu_max /
        // g_mem_max); unconstrained -> the kernel "max" sentinels. Verified byte-identical to runc (OrbStack
        // Docker 29.4) both unconstrained and under --memory=512m --cpus=2. Host-variant accounting figures
        // (memory.stat/cpu.stat live counters) are structural-only: the KEYS a runtime parses must be present,
        // the values are informational so we report zeros (a bare-guest deterministic baseline).
        // ---- cgroup core interface files (v2 markers a runtime detects the unified hierarchy by) ----------
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.controllers")) {
        // The controllers available in this cgroup. runc enables exactly these for a container leaf.
        n = snprintf(buf, sizeof buf, "cpuset cpu io memory pids\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.subtree_control")) {
        n = 0;
        buf[0] = 0; // a leaf cgroup delegates nothing downward -> empty (matches runc)
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.type")) {
        n = snprintf(buf, sizeof buf, "domain\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.procs")) {
        // The processes in this cgroup. The container is ONE cgroup, so this is EVERY guest process (init +
        // every forked child), enumerated from the cross-process registry -- not just container_pid().
        n = cgroup_procs_text(buf, sizeof buf, 0);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.threads")) {
        // Every task (thread) in the cgroup: the per-process registry set plus THIS process's extra threads.
        n = cgroup_procs_text(buf, sizeof buf, 1);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.events")) {
        n = snprintf(buf, sizeof buf, "populated 1\nfrozen 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.max.depth") || !strcmp(rp, "/sys/fs/cgroup/cgroup.max.descendants")) {
        n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.stat")) {
        n = snprintf(buf, sizeof buf, "nr_descendants 0\nnr_dying_descendants 0\n");
        // ---- memory controller: JVM UseContainerSupport + GOMEMLIMIT tooling read memory.max/.high/.swap ---
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.min") || !strcmp(rp, "/sys/fs/cgroup/memory.low")) {
        n = snprintf(buf, sizeof buf, "0\n"); // no reclaim protection reserved (runc default)
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.high")) {
        n = snprintf(buf, sizeof buf, "max\n"); // docker sets only the hard limit (memory.max), never .high
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.max")) {
        // v2 memory.swap.max is the SWAP-ONLY ceiling. Docker's default --memory-swap (unset) = 2*--memory,
        // and runc writes swap.max = memoryswap - memory = --memory. So under --memory it equals g_mem_max;
        // unconstrained -> "max". (Verified: --memory=512m -> 536870912, matching --memory bytes.)
        if (g_mem_max)
            n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)g_mem_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.current")) {
        n = snprintf(buf, sizeof buf, "0\n"); // no swap accounted (hl runs no swap)
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.high")) {
        n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.peak")) {
        n = snprintf(buf, sizeof buf, "%llu\n", cgroup_mem_current()); // container-wide (no historical peak)
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.stat")) {
        // The per-type breakdown. The JVM's CgroupSubsystemController reads this for "file" (page cache) to
        // refine its container-memory estimate; the exact byte figures are host-variant, so we present the
        // full canonical key set with the tracked anon charge and zeros elsewhere (structural fidelity).
        unsigned long long anon = (unsigned long long)atomic_load(&g_mem_charged);
        n = snprintf(buf, sizeof buf,
                     "anon %llu\nfile 0\nkernel %llu\nkernel_stack 0\npagetables 0\nsec_pagetables 0\n"
                     "percpu 0\nsock 0\nvmalloc 0\nshmem 0\nfile_mapped 0\nfile_dirty 0\nfile_writeback 0\n"
                     "swapcached 0\nanon_thp 0\nfile_thp 0\nshmem_thp 0\ninactive_anon %llu\nactive_anon 0\n"
                     "inactive_file 0\nactive_file 0\nunevictable 0\nslab_reclaimable 0\nslab_unreclaimable 0\n"
                     "slab 0\nworkingset_refault_anon 0\nworkingset_refault_file 0\npgfault 0\npgmajfault 0\n",
                     anon, anon, anon);
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.events") || !strcmp(rp, "/sys/fs/cgroup/memory.events.local")) {
        n = snprintf(buf, sizeof buf, "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\n");
        // ---- cpu controller: JVM ActiveProcessorCount + Go GOMAXPROCS derive from cpu.max quota/period ------
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.max")) {
        // "<quota> <period>" under --cpus, "max <period>" unconstrained. Docker's period is 100000us; the
        // quota is --cpus * period. g_cpu_max is the container's integer core allotment (state.c). A runtime
        // computes cpus = quota/period, so this is what makes a --cpus=2 container self-size Go GOMAXPROCS /
        // JVM availableProcessors to 2. (Verified: --cpus=2 -> "200000 100000".)
        if (g_cpu_max > 0)
            n = snprintf(buf, sizeof buf, "%lld 100000\n", (long long)g_cpu_max * 100000);
        else
            n = snprintf(buf, sizeof buf, "max 100000\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.max.burst")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.weight")) {
        n = snprintf(buf, sizeof buf, "100\n"); // docker default share weight (no --cpu-shares override)
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.weight.nice")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.idle")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.stat")) {
        // usage/throttling counters. The KEY NAMES are what a runtime/systemd parse; the values are
        // host-variant accounting, so zeros are a correct deterministic baseline (hl tracks no per-cgroup
        // cpu accounting). nr_throttled/throttled_usec present so a throttle-aware scheduler sees "0".
        n = snprintf(buf, sizeof buf,
                     "usage_usec 0\nuser_usec 0\nsystem_usec 0\nnr_periods 0\nnr_throttled 0\n"
                     "throttled_usec 0\nnr_bursts 0\nburst_usec 0\n");
        // ---- io controller (lower value; present so a full-cgroup walk finds it) --------------------------
    } else if (!strcmp(rp, "/sys/fs/cgroup/io.max")) {
        n = 0;
        buf[0] = 0; // no per-device io limits set (docker without --device-*-bps) -> empty
    } else if (!strcmp(rp, "/sys/fs/cgroup/io.stat")) {
        n = 0;
        buf[0] = 0; // no real block device backs the overlay -> empty (host-variant otherwise)
    } else if (!strcmp(rp, "/sys/fs/cgroup/io.weight")) {
        n = snprintf(buf, sizeof buf, "default 100\n");
        // ---- the broad /proc + /proc/sys surface real software reads --------------------------------
    } else if (!strcmp(rp, "/proc/cmdline")) {
        n = snprintf(buf, sizeof buf, "root=/dev/sda1 ro quiet\n"); // kernel cmdline (distinct from self/cmdline)
    } else if (!strcmp(rp, "/proc/filesystems")) {
        n = snprintf(buf, sizeof buf,
                     "nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\nnodev\tdevpts\n"
                     "nodev\tmqueue\nnodev\tcgroup2\nnodev\toverlay\n\text3\n\text2\n\text4\n");
    } else if (!strcmp(rp, "/proc/cgroups")) {
        // The v1 subsystem summary. On a pure-v2 (unified) host every controller lives in hierarchy 0; some
        // older runtimes (and `lscgroup`) read this to enumerate available controllers. Mirror the OrbStack
        // oracle: all subsystems enabled, hierarchy 0 (v2 unified), num_cgroups is host-variant -> report 1.
        n = snprintf(buf, sizeof buf,
                     "#subsys_name\thierarchy\tnum_cgroups\tenabled\n"
                     "cpuset\t0\t1\t1\ncpu\t0\t1\t1\ncpuacct\t0\t1\t1\nblkio\t0\t1\t1\nmemory\t0\t1\t1\n"
                     "devices\t0\t1\t1\nfreezer\t0\t1\t1\nnet_cls\t0\t1\t1\nperf_event\t0\t1\t1\n"
                     "net_prio\t0\t1\t1\npids\t0\t1\t1\n");
    } else if (!strcmp(rp, "/proc/swaps")) {
        n = snprintf(buf, sizeof buf, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"); // no swap
    } else if (!strcmp(rp, "/proc/modules")) {
        n = 0;
        buf[0] = 0; // no loadable modules
    } else if (!strcmp(rp, "/proc/devices")) {
        // The block-device section must list standard majors (loop/sd/device-mapper/blkext) or installers
        // and device-major discovery see a false empty device surface.
        n = snprintf(buf, sizeof buf,
                     "Character devices:\n  1 mem\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n"
                     "136 pts\n\nBlock devices:\n  7 loop\n  8 sd\n 253 device-mapper\n 259 blkext\n");
    } else if (!strcmp(rp, "/proc/tty/drivers")) {
        // tty driver table (`/proc/tty/drivers`) tty-discovery tools read; the exact rows are host-variant,
        // so present the canonical container set (pty pair + console/serial) so the file is non-empty.
        n = snprintf(buf, sizeof buf,
                     "/dev/tty             /dev/tty        5       0 system:/dev/tty\n"
                     "/dev/console         /dev/console    5       1 system:console\n"
                     "/dev/ptmx            /dev/ptmx       5       2 system\n"
                     "unknown              /dev/tty        4    1-63 console\n"
                     "pty_slave            /dev/pts      136 0-1048575 pty:slave\n"
                     "pty_master           /dev/ptm      128 0-1048575 pty:master\n");
    } else if (!strcmp(rp, "/proc/vmstat")) {
        n = snprintf(buf, sizeof buf,
                     "nr_free_pages 262144\nnr_zone_inactive_anon 0\nnr_zone_active_anon 0\n"
                     "nr_dirty 0\nnr_writeback 0\nnr_slab_reclaimable 0\nnr_slab_unreclaimable 0\n"
                     "pgpgin 0\npgpgout 0\npswpin 0\npswpout 0\npgfault 0\npgmajfault 0\n");
    } else if (!strcmp(rp, "/proc/net/sockstat")) {
        // Socket accounting (`ss -s`, monitoring agents). hl runs no real IP stack, so the counters are a
        // deterministic zero baseline -- but the SECTIONS must exist with the exact kernel key names.
        n = snprintf(buf, sizeof buf,
                     "sockets: used 1\nTCP: inuse 0 orphan 0 tw 0 alloc 0 mem 0\n"
                     "UDP: inuse 0 mem 0\nUDPLITE: inuse 0\nRAW: inuse 0\n"
                     "FRAG: inuse 0 memory 0\n");
    } else if (!strcmp(rp, "/proc/net/sockstat6")) {
        n = snprintf(buf, sizeof buf,
                     "TCP6: inuse 0\nUDP6: inuse 0\nUDPLITE6: inuse 0\nRAW6: inuse 0\nFRAG6: inuse 0 memory 0\n");
    } else if (!strcmp(rp, "/proc/net/unix")) {
        n = snprintf(buf, sizeof buf, "Num       RefCount Protocol Flags    Type St Inode Path\n");
        // One row per live guest-bound AF_UNIX socket (socket-inventory tools read this). Columns match the
        // kernel: a bound listener is Flags 00010000, St 01 (LISTEN); the inode is a stable synthetic id.
        for (int fd = 0; fd < HL_NFD && n < (int)sizeof buf - 128; fd++) {
            if (!g_unix_bind[fd][0]) continue;
            if (fcntl(fd, F_GETFD) == -1) {
                g_unix_bind[fd][0] = 0;
                continue;
            } // closed -> drop
            n += snprintf(buf + n, sizeof buf - (size_t)n, "%016x: %08x %08x %08x %04x %02x %5d %s\n", fd, 2u, 0u,
                          0x10000u, 1u, 1u, 100000 + fd, g_unix_bind[fd]);
        }
    } else if (!strcmp(rp, "/proc/net/snmp")) {
        // The full protocol-counter table `netstat -s` / `ss -s` parse: paired header+value lines for
        // Ip/Icmp/IcmpMsg/Tcp/Udp/UdpLite. hl runs no real IP stack, so the counters are zero -- but the
        // SECTIONS must exist with the exact kernel column names or the parser aborts. Tcp's RtoAlgorithm/
        // RtoMin/RtoMax/MaxConn carry the conventional 1/200/120000/-1 the kernel reports.
        n = snprintf(
            buf, sizeof buf,
            "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos "
            "InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs "
            "ReasmFails FragOKs FragFails FragCreates\n"
            "Ip: 2 64 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
            "Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs "
            "InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs "
            "OutErrors OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos "
            "OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps\n"
            "Icmp: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
            "IcmpMsg: InType3 OutType3\nIcmpMsg: 0 0\n"
            "Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets "
            "CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n"
            "Tcp: 1 200 120000 -1 0 0 0 0 0 0 0 0 0 0 0\n"
            "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti "
            "MemErrors\n"
            "Udp: 0 0 0 0 0 0 0 0 0\n"
            "UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors "
            "IgnoredMulti MemErrors\n"
            "UdpLite: 0 0 0 0 0 0 0 0 0\n");
    } else if (!strcmp(rp, "/proc/net/netstat")) {
        // `netstat -s` / `ss -s` parse the TcpExt + IpExt extended-counter tables. hl runs no IP stack, so
        // every counter is zero -- but the SECTIONS with the exact kernel column names must exist (a missing
        // file makes those stats silently vanish). The zero value-line is generated with exactly as many
        // fields as its header (one " 0" per space) so a positional parser stays aligned.
        static const char *const th =
            "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled RcvPruned "
            "OfoPruned OutOfWindowIcmps LockDroppedIcmps ArpFilter TW TWRecycled TWKilled PAWSActive "
            "PAWSEstab BeyondWindow TSEcrRejected PAWSOldAck PAWSTimewait DelayedACKs DelayedACKLocked "
            "DelayedACKLost ListenOverflows ListenDrops TCPHPHits TCPPureAcks TCPHPAcks TCPRenoRecovery "
            "TCPSackRecovery TCPSACKReneging TCPSACKReorder TCPRenoReorder TCPTSReorder TCPFullUndo "
            "TCPPartialUndo TCPDSACKUndo TCPLossUndo TCPLostRetransmit TCPRenoFailures TCPSackFailures "
            "TCPLossFailures TCPFastRetrans TCPSlowStartRetrans TCPTimeouts TCPLossProbes "
            "TCPLossProbeRecovery TCPRenoRecoveryFail TCPSackRecoveryFail TCPRcvCollapsed TCPBacklogCoalesce "
            "TCPDSACKOldSent TCPDSACKOfoSent TCPDSACKRecv TCPDSACKOfoRecv TCPAbortOnData TCPAbortOnClose "
            "TCPAbortOnMemory TCPAbortOnTimeout TCPAbortOnLinger TCPAbortFailed TCPMemoryPressures "
            "TCPMemoryPressuresChrono TCPSACKDiscard TCPDSACKIgnoredOld TCPDSACKIgnoredNoUndo TCPSpuriousRTOs "
            "TCPMD5NotFound TCPMD5Unexpected TCPMD5Failure TCPSackShifted TCPSackMerged TCPSackShiftFallback "
            "TCPBacklogDrop PFMemallocDrop TCPMinTTLDrop TCPDeferAcceptDrop IPReversePathFilter "
            "TCPTimeWaitOverflow TCPReqQFullDoCookies TCPReqQFullDrop TCPRetransFail TCPRcvCoalesce "
            "TCPOFOQueue TCPOFODrop TCPOFOMerge TCPChallengeACK TCPSYNChallenge TCPFastOpenActive "
            "TCPFastOpenActiveFail TCPFastOpenPassive TCPFastOpenPassiveFail TCPFastOpenListenOverflow "
            "TCPFastOpenCookieReqd TCPFastOpenBlackhole TCPSpuriousRtxHostQueues BusyPollRxPackets "
            "TCPAutoCorking TCPFromZeroWindowAdv TCPToZeroWindowAdv TCPWantZeroWindowAdv TCPSynRetrans "
            "TCPOrigDataSent TCPHystartTrainDetect TCPHystartTrainCwnd TCPHystartDelayDetect "
            "TCPHystartDelayCwnd TCPACKSkippedSynRecv TCPACKSkippedPAWS TCPACKSkippedSeq TCPACKSkippedFinWait2 "
            "TCPACKSkippedTimeWait TCPACKSkippedChallenge TCPWinProbe TCPKeepAlive TCPMTUPFail TCPMTUPSuccess "
            "TCPDelivered TCPDeliveredCE TCPAckCompressed TCPZeroWindowDrop TCPRcvQDrop TCPWqueueTooBig "
            "TCPFastOpenPassiveAltKey TcpTimeoutRehash TcpDuplicateDataRehash TCPDSACKRecvSegs "
            "TCPDSACKIgnoredDubious TCPMigrateReqSuccess TCPMigrateReqFailure TCPPLBRehash TCPAORequired "
            "TCPAOBad TCPAOKeyNotFound TCPAOGood TCPAODroppedIcmps";
        static const char *const ih =
            "IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets "
            "OutOctets InMcastOctets OutMcastOctets InBcastOctets OutBcastOctets InCsumErrors InNoECTPkts "
            "InECT1Pkts InECT0Pkts InCEPkts ReasmOverlaps";
        n = 0;
        const char *hdrs[2] = {th, ih};
        const char *labs[2] = {"TcpExt:", "IpExt:"};
        for (int pass = 0; pass < 2; pass++) {
            int fields = 0;
            for (const char *p = hdrs[pass]; *p; p++)
                if (*p == ' ') fields++;
            n += snprintf(buf + n, sizeof buf - n, "%s\n%s", hdrs[pass], labs[pass]);
            for (int i = 0; i < fields && n < (int)sizeof buf - 4; i++)
                n += snprintf(buf + n, sizeof buf - n, " 0");
            n += snprintf(buf + n, sizeof buf - n, "\n");
        }
    } else if (!strcmp(rp, "/proc/net/ipv6_route")) {
        // `ip -6 route` / `netstat -6 -r` parse this. Loopback-only container v6 routing table (matches a
        // real --network bridge container that has no global v6): the ::/0-ish + ::1 host route on lo.
        n = snprintf(buf, sizeof buf,
                     "00000000000000000000000000000000 00 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n"
                     "00000000000000000000000000000001 80 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 00000000 00000002 00000000 80200001       lo\n"
                     "00000000000000000000000000000000 00 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n");
    } else if (!strcmp(rp, "/proc/net/snmp6")) {
        // IPv6 counter table `netstat -s` reads for its "Ip6/Icmp6/Udp6" sections. Zero counters (no real
        // stack); the KEY NAMES must match the kernel or the section is dropped.
        n = snprintf(buf, sizeof buf,
                     "Ip6InReceives                   \t0\nIp6InHdrErrors                  \t0\n"
                     "Ip6InTooBigErrors               \t0\nIp6InNoRoutes                   \t0\n"
                     "Ip6InAddrErrors                 \t0\nIp6InUnknownProtos              \t0\n"
                     "Ip6InTruncatedPkts              \t0\nIp6InDiscards                   \t0\n"
                     "Ip6InDelivers                   \t0\nIp6OutForwDatagrams             \t0\n"
                     "Ip6OutRequests                  \t0\nIp6OutDiscards                  \t0\n"
                     "Ip6OutNoRoutes                  \t0\nIp6ReasmTimeout                 \t0\n"
                     "Ip6ReasmReqds                   \t0\nIp6ReasmOKs                     \t0\n"
                     "Ip6ReasmFails                   \t0\nIp6FragOKs                      \t0\n"
                     "Ip6FragFails                    \t0\nIp6FragCreates                  \t0\n"
                     "Ip6InMcastPkts                  \t0\nIp6OutMcastPkts                 \t0\n"
                     "Ip6InOctets                     \t0\nIp6OutOctets                    \t0\n"
                     "Icmp6InMsgs                     \t0\nIcmp6InErrors                   \t0\n"
                     "Icmp6OutMsgs                    \t0\nIcmp6OutErrors                  \t0\n"
                     "Udp6InDatagrams                 \t0\nUdp6NoPorts                     \t0\n"
                     "Udp6InErrors                    \t0\nUdp6OutDatagrams                \t0\n"
                     "Udp6RcvbufErrors                \t0\nUdp6SndbufErrors                \t0\n"
                     "Udp6InCsumErrors                \t0\nUdp6IgnoredMulti                \t0\n"
                     "Udp6MemErrors                   \t0\n");
    } else if (!strcmp(rp, "/proc/net/arp")) {
        // Neighbour table (`arp -a`, `ip neigh`). The container is its own net namespace: it must NOT expose
        // the HOST's ARP cache (gateway/neighbour MACs) that the raw host /proc/net/arp passthrough leaked.
        // A freshly-started bridge container has resolved no neighbours yet, so the correct, container-safe
        // view is the header with an empty table -- well-formed for any parser.
        n = snprintf(buf, sizeof buf,
                     "IP address       HW type     Flags       HW address            Mask     Device\n");
    } else if (!strcmp(rp, "/proc/net/igmp")) {
        // Multicast group memberships per interface. Must reflect the SAME container interface set as
        // /proc/net/dev (lo [+ eth0]) -- the host passthrough leaked the host's docker0/host-iface rows,
        // an isolation break and an iface-set inconsistency vs the synthesized /proc/net/dev. Every up
        // multicast interface joins the all-hosts group 224.0.0.1 (010000E0, little-endian hex).
        n = snprintf(buf, sizeof buf,
                     "Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n"
                     "1\tlo        :     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n");
        if (!net_isolate())
            n += snprintf(buf + n, sizeof buf - (size_t)n,
                          "2\teth0      :     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n");
    } else if (!strncmp(rp, "/proc/net/", 10)) {
        // Isolation backstop: every /proc/net leaf the container legitimately exposes is synthesized above
        // (a container view). Any remaining /proc/net/<leaf> -- fib_trie, rt_cache, netlink, packet,
        // softnet_stat, protocols, dev_mcast, icmp, raw, xfrm_stat, ... -- would otherwise fall through to a
        // raw host open and leak the HOST network stack (host routes/subnets, host processes' sockets, host
        // CPU count, host-wide socket counts). Serve a well-formed EMPTY table instead of the host file: the
        // namespaced file exists (open succeeds) but carries no host data.
        n = 0;
        buf[0] = 0;
    } else if (!strcmp(rp, "/proc/pressure/cpu")) {
        n = snprintf(buf, sizeof buf, "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
    } else if (!strcmp(rp, "/proc/pressure/memory") || !strcmp(rp, "/proc/pressure/io")) {
        n = snprintf(buf, sizeof buf,
                     "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
                     "full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
    } else {
        // Constant sysctl-style files (values mirror a modern Linux default). A single table keeps the
        // /proc/sys/{kernel,vm,net,fs} surface complete for the sysctl/config probes Go/JVM/nginx/redis/
        // postgres/systemd issue. Multi-value files use TAB separators exactly like the kernel.
        static const struct {
            const char *p, *v;
        } K[] = {
            // kernel
            {"/proc/sys/kernel/pid_max", "4194304\n"},
            {"/proc/sys/kernel/threads-max", "63488\n"},
            {"/proc/sys/kernel/cap_last_cap", "40\n"},
            {"/proc/sys/kernel/ngroups_max", "65536\n"},
            {"/proc/sys/kernel/tainted", "0\n"},
            {"/proc/sys/kernel/domainname", "(none)\n"},
            {"/proc/sys/kernel/overflowuid", "65534\n"},
            {"/proc/sys/kernel/overflowgid", "65534\n"},
            {"/proc/sys/kernel/core_pattern", "core\n"},
            {"/proc/sys/kernel/sched_child_runs_first", "0\n"},
            {"/proc/sys/kernel/shmmax", "18446744073692774399\n"},
            {"/proc/sys/kernel/shmall", "18446744073692774399\n"},
            {"/proc/sys/kernel/shmmni", "4096\n"},
            {"/proc/sys/kernel/sem", "256\t131072\t500\t512\n"},
            {"/proc/sys/kernel/msgmax", "8192\n"},
            {"/proc/sys/kernel/msgmnb", "16384\n"},
            {"/proc/sys/kernel/msgmni", "512\n"},
            {"/proc/sys/kernel/yama/ptrace_scope", "1\n"},
            {"/proc/sys/kernel/random/poolsize", "256\n"},
            {"/proc/sys/kernel/printk", "4\t4\t1\t7\n"},
            {"/proc/sys/kernel/panic", "10\n"}, // oracle: 10s reboot-on-panic (was 0)
            // ASLR posture. A guest/security probe (Go's runtime, glibc, hardening scanners) reads this to
            // learn whether the kernel randomizes mmap/stack/brk; hl omitted it -> ENOENT where real docker
            // serves 2 (full ASLR: mmap + stack + brk + VDSO). Oracle: 2.
            {"/proc/sys/kernel/randomize_va_space", "2\n"},
            // vm
            {"/proc/sys/vm/overcommit_ratio", "50\n"},
            {"/proc/sys/vm/overcommit_kbytes", "0\n"},
            // elasticsearch REFUSES to start if max_map_count < 262144. hl served 65530 -> ES bootstrap
            // check fails, a warning/refusal a real-docker user never sees. Oracle: 1048576.
            {"/proc/sys/vm/max_map_count", "1048576\n"},
            {"/proc/sys/vm/mmap_min_addr", "32768\n"}, // oracle (was 65536)
            {"/proc/sys/vm/swappiness", "20\n"},       // oracle (was 60)
            {"/proc/sys/vm/dirty_ratio", "20\n"},
            {"/proc/sys/vm/dirty_background_ratio", "10\n"},
            {"/proc/sys/vm/nr_hugepages", "0\n"},
            {"/proc/sys/vm/panic_on_oom", "0\n"},
            {"/proc/sys/vm/vfs_cache_pressure", "100\n"},
            // net.core
            {"/proc/sys/net/core/somaxconn", "4096\n"},
            {"/proc/sys/net/core/netdev_max_backlog", "1000\n"},
            {"/proc/sys/net/core/rmem_max", "7500000\n"},    // oracle (was 212992)
            {"/proc/sys/net/core/wmem_max", "7500000\n"},    // oracle (was 212992)
            {"/proc/sys/net/core/rmem_default", "229376\n"}, // oracle (was 212992)
            {"/proc/sys/net/core/wmem_default", "229376\n"}, // oracle (was 212992)
            {"/proc/sys/net/core/optmem_max", "131072\n"},   // oracle (was 20480)
            // net.ipv4
            {"/proc/sys/net/ipv4/ip_local_port_range", "32768\t60999\n"},
            {"/proc/sys/net/ipv4/ip_unprivileged_port_start", "0\n"}, // oracle (was 1024)
            {"/proc/sys/net/ipv4/ip_forward", "1\n"},                 // oracle (was 0)
            {"/proc/sys/net/ipv4/ip_nonlocal_bind", "0\n"},
            {"/proc/sys/net/ipv4/tcp_fin_timeout", "60\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_time", "7200\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_intvl", "75\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_probes", "9\n"},
            {"/proc/sys/net/ipv4/tcp_max_syn_backlog", "1024\n"}, // oracle (was 128)
            {"/proc/sys/net/ipv4/tcp_syncookies", "1\n"},
            {"/proc/sys/net/ipv4/tcp_tw_reuse", "2\n"},
            {"/proc/sys/net/ipv4/tcp_rmem", "4096\t131072\t33554432\n"}, // oracle max (was 6291456)
            {"/proc/sys/net/ipv4/tcp_wmem", "4096\t16384\t4194304\n"},
            {"/proc/sys/net/ipv4/tcp_congestion_control", "cubic\n"},
            {"/proc/sys/net/ipv4/tcp_available_congestion_control", "reno cubic\n"},
            // fs. On modern (cgroup-era) kernels the global file-max cap is effectively removed: the oracle
            // reports LONG_MAX for file-max and the file-nr high-water field. Serving 1048576 made programs
            // that size their fd budget off file-max under-provision vs a real-docker run.
            {"/proc/sys/fs/file-max", "9223372036854775807\n"},         // oracle LONG_MAX (was 1048576)
            {"/proc/sys/fs/nr_open", "2147483584\n"},                   // oracle (was 1048576)
            {"/proc/sys/fs/file-nr", "1024\t0\t9223372036854775807\n"}, // 3rd field == file-max (was 1048576)
            {"/proc/sys/fs/pipe-max-size", "1048576\n"},
            {"/proc/sys/fs/pipe-user-pages-hard", "0\n"},
            {"/proc/sys/fs/pipe-user-pages-soft", "16384\n"},
            {"/proc/sys/fs/aio-max-nr", "1048576\n"}, // oracle (was 65536)
            {"/proc/sys/fs/aio-nr", "0\n"},
            {"/proc/sys/fs/protected_hardlinks", "1\n"},
            {"/proc/sys/fs/protected_symlinks", "1\n"},
            {"/proc/sys/fs/suid_dumpable", "2\n"}, // oracle (was 0)
            {"/proc/sys/fs/inotify/max_user_watches", "524288\n"},
            // VS Code / node chokidar / systemd watchers exhaust these and print "ENOSPC: inotify watch
            // limit reached" when they are low. Oracle bumps both far above the old 128 / 16384.
            {"/proc/sys/fs/inotify/max_user_instances", "524288\n"}, // oracle (was 128)
            {"/proc/sys/fs/inotify/max_queued_events", "1048576\n"}, // oracle (was 16384)
            // POSIX message-queue limits (fs/mqueue/*) -- hl omitted these entirely, so a reader (glibc
            // mq_* tuning, systemd) got ENOENT where real docker serves a value. Oracle kernel defaults.
            {"/proc/sys/fs/mqueue/msg_max", "10\n"},
            {"/proc/sys/fs/mqueue/msgsize_max", "8192\n"},
            {"/proc/sys/fs/mqueue/queues_max", "256\n"},
            {"/proc/sys/fs/mqueue/msg_default", "10\n"},
            {"/proc/sys/fs/mqueue/msgsize_default", "8192\n"},
            // Transparent-hugepage policy. jemalloc/tcmalloc, the JVM (-XX:+UseTransparentHugePages), redis
            // (THP warning), and mongod all read this; hl omitted it -> ENOENT, where real docker exposes the
            // host's setting with the active mode bracketed. Oracle: "always [madvise] never".
            {"/sys/kernel/mm/transparent_hugepage/enabled", "always [madvise] never\n"},
        };

        for (size_t i = 0; i < sizeof K / sizeof *K; i++)
            if (!strcmp(rp, K[i].p)) {
                n = snprintf(buf, sizeof buf, "%s", K[i].v);
                break;
            }
    }
    if (n < 0) return -2;
    return proc_text_fd(buf, n);
}

// Linux-layout stat for a synthesized /proc or /sys file (so stat()/access() see it -- find, du,
// container runtimes that stat /etc/mtab -> /proc/mounts, JVM that stats cgroup files, etc.).
static void fill_linux_stat(uint8_t *d, const struct stat *s, const char *hostpath, int fd);

// The pseudo /dev nodes the rootfs lacks but open() (fs.c) backs with a real host device. Returns the
// host path open() would use, else NULL. stat()/access() consult this so the nodes report as EXISTING
// character devices -- e.g. libgcrypt detects its RNG via access("/dev/urandom",R_OK); an ENOENT there
// makes it abort ("no entropy gathering module detected"), which breaks gpgv and thus `apt-get update`.
// The container's controlling terminal. `docker run -t` makes the daemon call login_tty, which hands the
// guest fd 0/1/2 as ONE pty slave. On Linux/devpts that slave is /dev/pts/0, but hl's host pty is a mac
// /dev/ttysNNN (or a host /dev/pts/N) whose raw name would otherwise leak into the guest via
// F_GETPATH -- so `tty`, ttyname(3), the `ps` TTY column, and any program that reopens open(ttyname(0))
// would see a device that doesn't exist in the container. We present it uniformly as /dev/pts/0.
// ctty_anchor() returns the host fd that IS the controlling terminal (the first of 0/1/2 that is a tty),
// or -1 when stdio is piped (no tty) -- exactly matching real docker, where a non -t container has no tty.
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
    return ok && S_ISCHR(s->st_mode);
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
    if (symlink_idempotent("/proc/self/fd", DEVP("fd")) != 0 ||
        symlink_idempotent("/proc/self/fd/0", DEVP("stdin")) != 0 ||
        symlink_idempotent("/proc/self/fd/1", DEVP("stdout")) != 0 ||
        symlink_idempotent("/proc/self/fd/2", DEVP("stderr")) != 0)
        return;
    // char-device placeholders so they list in /dev; open()/stat() are intercepted by the fs.c synth
    // (dev_node_hostpath), so the empty file is never actually read/written.
    static const char *const chr[] = {"null", "zero", "full", "random", "urandom", "tty", "console", "ptmx"};
    for (size_t i = 0; i < sizeof chr / sizeof *chr; i++) {
        int fd = open(DEVP(chr[i]), O_CREAT | O_WRONLY, 0666);
        if (fd >= 0) close(fd);
    }
    hl_compat_mkdir(DEVP("pts"), 0755); // devpts mount point; /dev/pts/N slaves resolve via ptsname in fs.c
    // devpts publishes a /dev/pts/ptmx multiplexer node (docker mounts it with ptmxmode=0666); `ls /dev/pts`
    // lists it, and open("/dev/pts/ptmx") is intercepted like /dev/ptmx in fs.c.
    {
        int fd = open(DEVP("pts/ptmx"), O_CREAT | O_WRONLY, 0666);
        if (fd >= 0) close(fd);
    }
    // When the container was handed a controlling terminal (docker run -t: the daemon's login_tty made fd
    // 0/1/2 the pty slave), Linux/devpts names it /dev/pts/0. Materialize that entry so `ls /dev/pts` lists
    // it; stat()/open()/readlink of /dev/pts/0 are intercepted (synth_stat_raw + fs.c) and routed to the
    // real controlling tty, so ttyname(3)/`tty`/`ps` resolve it instead of leaking the host pty device name.
    if (isatty(0) || isatty(1) || isatty(2)) {
        int fd = open(DEVP("pts/0"), O_CREAT | O_WRONLY, 0620);
        if (fd >= 0) close(fd);
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

static int synth_stat_raw(const char *gp, struct stat *s) {
    if (!gp) return 0; // NULL (bad) guest path: not a synthetic node; let the caller's host stat EFAULT
    // A renameat2(RENAME_WHITEOUT) source: report the Linux whiteout inode (char device, rdev 0,0, mode 0).
    if (whiteout_present(gp)) {
        memset(s, 0, sizeof *s);
        s->st_mode = S_IFCHR; // whiteout char device, permission bits 0 (as overlayfs/Linux create it)
        s->st_rdev = 0;       // makedev(0,0)
        s->st_nlink = 1;
        return 1;
    }
    // Synthetic non-pid directories (/proc/net, /proc/[self|pid]/ns, /sys/fs/cgroup, /sys/class/block,
    // /sys/block, cpuN/topology): a tool that stats the dir before opening it must see it as present.
    if (synth_misc_dir_is(gp)) {
        memset(s, 0, sizeof *s);
        s->st_mode = S_IFDIR | 0555;
        s->st_nlink = 2;
        return 1;
    }
    // The controlling terminal, named /dev/pts/0 in the container: fstat the real pty slave so it reports as
    // a character device with the correct rdev. ttyname(3) reads /proc/self/fd/0 -> "/dev/pts/0", then
    // stat()s it and checks S_ISCHR + rdev == fstat(0).rdev; this makes that check pass so `tty` prints
    // /dev/pts/0 instead of "not a tty".
    if (gp && !strcmp(gp, "/dev/pts/0")) {
        int a = ctty_anchor();
        if (a >= 0 && fstat(a, s) == 0) return 1;
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
    if (dev) {
        if (stat(dev, s) != 0) return 0;

        static const struct {
            const char *p;
            int maj, min;
            unsigned mode;
        } D[] = {{"/dev/null", 1, 3, 0666},    {"/dev/zero", 1, 5, 0666},
                 {"/dev/full", 1, 7, 0666},    {"/dev/random", 1, 8, 0666},
                 {"/dev/urandom", 1, 9, 0666}, {"/dev/tty", 5, 0, 0666},
                 {"/dev/console", 5, 1, 0600}, {0, 0, 0, 0}};

        for (int i = 0; D[i].p; i++)
            if (!strcmp(gp, D[i].p)) {
                s->st_rdev = (dev_t)(((uint64_t)D[i].maj << 8) | (unsigned)D[i].min); // Linux dev_t encoding
                s->st_mode = S_IFCHR | D[i].mode;
                break;
            }
        return 1;
    }
    // runc MaskedPaths / ReadonlyPaths: these must EXIST (a masked file is an empty regular file; a masked or
    // read-only dir is an empty directory), so stat()/`test -e` see them present -- matching runc, not ENOENT.
    if (g_rootfs) {
        int mk = proc_masked_kind(gp);
        if (mk == 1) { // masked file -> empty regular file
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFREG | 0444;
            s->st_nlink = 1;
            return 1;
        }
        if (mk == 2 || proc_ro_dir(gp)) { // masked dir / read-only proc dir -> empty directory
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFDIR | 0555;
            s->st_nlink = 2;
            return 1;
        }
        if (!strcmp(gp, "/proc/sysrq-trigger")) { // write-only trigger file: present, empty on read
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFREG | 0644;
            s->st_nlink = 1;
            return 1;
        }
    }
    // /sys/class/net: the class dir + per-iface dirs are directories; attribute files are regular.
    if (gp && !strncmp(gp, "/sys/class/net", 14)) {
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
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFDIR | 0555;
            s->st_nlink = 2;
            return 1;
        }
        int fd = proc_open(gp); // attribute file -> confirm we serve it, then present as a regular file
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
    // the CPU-topology sysfs tree must stat as PRESENT so tools that stat a path BEFORE opening it
    // (busybox `ls`/glob, `find`, `test -d`, coreutils stat) don't bail ENOENT under the rootfs overlay --
    // those synthetic paths live in no image layer. htop's opendir bypasses stat, but everyone else needs
    // this. Directories: the base /sys/devices/system/cpu and each cpuN in [0, online-count). Regular files:
    // the online/possible/present/offline range files (content served on open via the fs.c cpu synth).
    if (gp && !strncmp(gp, "/sys/devices/system/cpu", 23)) {
        const char *r = gp + 23;
        int hit = 0, isdir = 0;
        if (r[0] == 0 || (r[0] == '/' && r[1] == 0)) {
            hit = 1;
            isdir = 1; // the base directory
        } else if (r[0] == '/') {
            const char *leaf = r + 1;
            if (!strcmp(leaf, "online") || !strcmp(leaf, "possible") || !strcmp(leaf, "present") ||
                !strcmp(leaf, "offline")) {
                hit = 1; // a range file
            } else if (!strncmp(leaf, "cpu", 3) && leaf[3] >= '0' && leaf[3] <= '9') {
                const char *d = leaf + 3;
                int n = 0;
                for (; *d >= '0' && *d <= '9'; d++)
                    n = n * 10 + (*d - '0');
                if (n < container_online_cpus()) {
                    if (*d == 0 || !strcmp(d, "/topology")) {
                        hit = 1;
                        isdir = 1; // the cpuN directory (or its topology/ subdir) we advertise
                    } else if (!strncmp(d, "/topology/", 10)) {
                        char tb[96];
                        if (syscpu_topology_content(gp, tb, sizeof tb) >= 0) hit = 1; // a topology attribute file
                    }
                }
            }
        }
        if (hit) {
            memset(s, 0, sizeof *s);
            s->st_mode = isdir ? (S_IFDIR | 0555) : (S_IFREG | 0444);
            s->st_nlink = isdir ? 2 : 1;
            return 1;
        }
    }
    if (!gp || (strncmp(gp, "/proc/", 6) && strncmp(gp, "/sys/fs/cgroup/", 15))) return 0;
    // A bare /proc/self (the magic symlink) or /proc/<pid> directory for an introspectable pid (this
    // process, the container init "1", or our container pid): report the right type so stat()/opendir()
    // succeed and `ps`/`ls /proc` can descend. proc_self_leaf only matches paths WITH a leaf, so handle
    // the no-leaf directory form here.
    if (!strcmp(gp, "/proc/self")) {
        memset(s, 0, sizeof *s);
        s->st_mode = S_IFLNK | 0777;
        s->st_nlink = 1;
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
                memset(s, 0, sizeof *s);
                s->st_mode = S_IFDIR | 0555;
                s->st_nlink = 8;
                return 1;
            }
        }
    }
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
                    memset(s, 0, sizeof *s);
                    s->st_mode = S_IFDIR | 0555;
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
    // /proc/<pid>/fd is a directory and /proc/<pid>/fd/N is a symlink -- answer these directly so stat()
    // sees the right type WITHOUT proc_open() materializing a temp dir as a stat side effect.
    const char *leaf = proc_self_leaf(gp);
    if (leaf) {
        if (!strcmp(leaf, "fd") || !strcmp(leaf, "fd/")) {
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFDIR | 0555;
            s->st_nlink = 2;
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
                memset(s, 0, sizeof *s);
                s->st_mode = S_IFREG | 0444;
                s->st_nlink = 1;
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
                memset(s, 0, sizeof *s);
                s->st_mode = S_IFLNK | 0777;
                s->st_nlink = 1;
                s->st_size = 64; // Linux reports a fixed 64 for /proc/<pid>/fd/N links
                return 1;
            }
        }
    }
    // Peer /proc/<pid>/fd (a directory) and /proc/<pid>/fd/<N> (a symlink to the peer fd's target) -- answer
    // stat directly (a live peer fd from its host descriptor snapshot) so lstat/stat see the right type WITHOUT
    // proc_open() materializing a temp dir as a side effect. proc_self_leaf matched only our own pid above.
    {
        int peer = -1, hp = 0;
        const char *aleaf = proc_any_leaf(gp, &peer);
        if (aleaf && proc_pid_member(peer, &hp)) {
            if (!strcmp(aleaf, "fd")) {
                memset(s, 0, sizeof *s);
                s->st_mode = S_IFDIR | 0555;
                s->st_nlink = 2;
                return 1;
            }
            if (!strncmp(aleaf, "fd/", 3) && aleaf[3]) {
                int isnum = 1;
                for (const char *t = aleaf + 3; *t; t++)
                    if (*t < '0' || *t > '9') isnum = 0;
                if (isnum) {
                    if (!proc_fd_pid_open_one(hp, atoi(aleaf + 3))) return 0; // closed/absent -> ENOENT
                    memset(s, 0, sizeof *s);
                    s->st_mode = S_IFLNK | 0777;
                    s->st_nlink = 1;
                    s->st_size = 64;
                    return 1;
                }
            }
        }
    }
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

// (synth_stat wrapper removed: dead — all callers use synth_stat_raw directly)

#include "route.c"
