// hl/linux_abi/container -- the container VFS: TOCTOU-free path jail, overlay image layers
// (lower/upper + copy-up + whiteout + merged readdir), and /proc + /sys synthesis.

#include "../../open_plan.h"
#include "../../page.h" // hl_linux_host_page_size
#include "../../memory_arena.h"
#include "../../../host/libc_compat.h" // hl_compat_mkdir: the UCRT's mkdir takes no mode
#include "../../../host/file.h"
#include "../../../host/resolve.h"
#include "../../../engine/provider/files.h"
#include "../../../engine/provider/namespace.h"
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

#include "../namespace.h"

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
// g_fdpath normally holds a canonical host path. Provider and synthetic-device descriptors instead
// carry an already guest-absolute name; mark that representation so procfs does not rebase it twice.
static uint8_t g_fdpath_guest[HL_NFD];
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
/* Long Linux pathname binds use /proc/<pid>/fd/<anchor>/<leaf> so recvfrom preserves an absolute,
 * reverse-mappable sender. Keep the parent directory open for the lifetime of the bound socket. */
static int g_unix_path_anchor[HL_NFD];

static void unix_bind_note(int fd, const char *guestname) {
    if (fd >= 0 && fd < HL_NFD && guestname) snprintf(g_unix_bind[fd], sizeof g_unix_bind[fd], "%s", guestname);
}

static void unix_bind_clear(int fd) {
    if (fd >= 0 && fd < HL_NFD) {
        g_unix_bind[fd][0] = 0;
        if (g_unix_path_anchor[fd] > 0) {
            hl_host_process_fd_private_remove(g_unix_path_anchor[fd] - 1);
            close(g_unix_path_anchor[fd] - 1);
            g_unix_path_anchor[fd] = 0;
        }
    }
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
static void ovldents_duplicate(int source, int destination);
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
#define FDPATH_N 8192

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

struct fdpath_slot {
    uint64_t key;
    uint64_t owner_start_ns;
    uint8_t path_is_guest;
    char path[sizeof g_fdpath[0]];
};
static struct fdpath_slot *g_fdpaths;

struct fdvis_control {
    _Atomic uint64_t owner;
    uint64_t generation;
};
static struct fdvis_control *g_fdvis_control;
static int g_fdvis_fork_parent;
static uint64_t fdvis_key(int pid, int fd);
static void fdpath_sweep_stale_locked(void);
#if defined(HL_NATIVE_TEST_HOOKS)
static int fdvis_after_fork_rollback_test(void);
static int fdvis_stalled_parent_test(void);
#endif

static struct fdpath_slot *fdpath_find(uint64_t key, uint64_t owner_start_ns, int claim) {
    unsigned start = (unsigned)((key ^ (key >> 32)) * UINT64_C(2654435761)) & (FDPATH_N - 1);
    struct fdpath_slot *tombstone = NULL;
    for (unsigned probe = 0; probe < FDPATH_N; ++probe) {
        struct fdpath_slot *slot = &g_fdpaths[(start + probe) & (FDPATH_N - 1)];
        if (slot->key == key && slot->owner_start_ns == owner_start_ns) return slot;
        if (slot->key == UINT64_MAX) {
            if (claim && !tombstone) tombstone = slot;
            continue;
        }
        if (slot->key == 0) {
            if (!claim) return NULL;
            slot = tombstone ? tombstone : slot;
            memset(slot, 0, sizeof *slot);
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
    }
    if (claim && tombstone) {
        memset(tombstone, 0, sizeof *tombstone);
        tombstone->key = key;
        tombstone->owner_start_ns = owner_start_ns;
        return tombstone;
    }
    return NULL;
}

static void fdpath_delete_locked(struct fdpath_slot *slot) {
    memset(slot, 0, sizeof *slot);
    slot->key = UINT64_MAX;
}

static void fdpath_cleanup_owner_locked(int owner, uint64_t owner_start_ns) {
    for (unsigned index = 0; index < FDPATH_N; ++index)
        if (g_fdpaths[index].key != UINT64_MAX && (int)(uint32_t)(g_fdpaths[index].key >> 32) == owner &&
            g_fdpaths[index].owner_start_ns == owner_start_ns)
            fdpath_delete_locked(&g_fdpaths[index]);
}

static int proc_fdvis_publish_path_locked(int pid, uint64_t owner_start_ns, int guest_fd) {
    uint64_t key = fdvis_key(pid, guest_fd);
    size_t length = strnlen(g_fdpath[guest_fd], sizeof g_fdpath[guest_fd]);
    if (length == sizeof g_fdpath[guest_fd]) return -ENAMETOOLONG;
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, g_fdpath[guest_fd][0] != '\0');
    if (!slot && g_fdpath[guest_fd][0] != '\0') {
        fdpath_sweep_stale_locked();
        slot = fdpath_find(key, owner_start_ns, 1);
    }
    if (g_fdpath[guest_fd][0] == '\0') {
        if (slot) fdpath_delete_locked(slot);
        return 0;
    }
    if (!slot) return -ENOSPC;
    slot->path_is_guest = g_fdpath_guest[guest_fd];
    memcpy(slot->path, g_fdpath[guest_fd], length + 1);
    return 0;
}

static int fdpath_snapshot_locked(uint64_t key, uint64_t owner_start_ns, char *path, uint8_t *path_is_guest) {
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, 0);
    if (!slot) {
        path[0] = 0;
        *path_is_guest = 0;
        return 0;
    }
    memcpy(path, slot->path, sizeof slot->path);
    *path_is_guest = slot->path_is_guest;
    return 1;
}

static int fdpath_restore_locked(uint64_t key, uint64_t owner_start_ns, const char *path, uint8_t path_is_guest) {
    if (!path[0]) return 0;
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, 1);
    if (!slot) {
        fdpath_sweep_stale_locked();
        slot = fdpath_find(key, owner_start_ns, 1);
    }
    if (!slot) return -ENOSPC;
    slot->path_is_guest = path_is_guest;
    memcpy(slot->path, path, sizeof slot->path);
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int HL_TARGET_LOCAL(fdvis_path_publication_test)(uint32_t scenario) {
    struct fdpath_slot *paths = calloc(FDPATH_N, sizeof *paths);
    struct fdpath_slot *saved_paths = g_fdpaths;
    const int descriptor = HL_NFD - 1;
    char saved_path[sizeof g_fdpath[descriptor]];
    uint8_t saved_is_guest = g_fdpath_guest[descriptor];
    memcpy(saved_path, g_fdpath[descriptor], sizeof saved_path);
    if (scenario == 2)
        memset(g_fdpath[descriptor], 'x', sizeof g_fdpath[descriptor]);
    else
        snprintf(g_fdpath[descriptor], sizeof g_fdpath[descriptor],
                 scenario == 1 ? "/fork/inherited" : "/checkpoint/restored");
    g_fdpath_guest[descriptor] = 1;
    if (!paths) return 0;
    g_fdpaths = paths;
    if (scenario == 3) {
        uint64_t first = 1, second_key = 1 + FDPATH_N;
        struct fdpath_slot *a = fdpath_find(first, 9, 1);
        struct fdpath_slot *b = fdpath_find(second_key, 9, 1);
        int collision_ok = 0;
        if (a && b) {
            snprintf(b->path, sizeof b->path, "second");
            fdpath_delete_locked(a);
            struct fdpath_slot *observed = fdpath_find(second_key, 9, 0);
            struct fdpath_slot *republished = fdpath_find(first, 9, 1);
            collision_ok = observed == b && strcmp(observed->path, "second") == 0 && republished == a &&
                           fdpath_find(first, 9, 0) == a;
        }
        g_fdpaths = saved_paths;
        free(paths);
        return collision_ok;
    }
    if (scenario == 4) {
        for (unsigned index = 0; index < FDPATH_N; ++index) paths[index].key = (uint64_t)index + 1;
        int full = fdpath_find(UINT64_C(0x100000001), 9, 1) == NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return full;
    }
    if (scenario == 5) {
        uint64_t key = fdvis_key(77, descriptor);
        struct fdpath_slot *owned = fdpath_find(key, 99, 1);
        fdpath_cleanup_owner_locked(77, 99);
        int cleaned = owned && fdpath_find(key, 99, 0) == NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return cleaned;
    }
    if (scenario == 6) {
        for (unsigned index = 0; index < FDPATH_N; ++index) paths[index].key = (uint64_t)index + 1;
        int propagated = fdpath_restore_locked(fdvis_key(88, descriptor), 100, "/fork/full", 1);
        g_fdpaths = saved_paths;
        free(paths);
        return propagated == -ENOSPC;
    }
    if (scenario == 7) {
        const int dead = 2147483647;
        for (unsigned index = 0; index < FDPATH_N; ++index) {
            paths[index].key = fdvis_key(dead, (int)index);
            paths[index].owner_start_ns = 1;
        }
        int reclaimed = proc_fdvis_publish_path_locked(7, 9, descriptor) == 0 &&
                        fdpath_find(fdvis_key(7, descriptor), 9, 0) != NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return reclaimed && fdvis_after_fork_rollback_test() && fdvis_stalled_parent_test();
    }
    int first = proc_fdvis_publish_path_locked(7, 9, descriptor);
    struct fdpath_slot *slot = fdpath_find(fdvis_key(7, descriptor), 9, 0);
    const char *expected = scenario == 1 ? "/fork/inherited" : "/checkpoint/restored";
    int populated = scenario == 2 ? first == -ENAMETOOLONG && slot == NULL
                                  : first == 0 && slot && slot->path_is_guest == 1 && strcmp(slot->path, expected) == 0;
    if (scenario == 1 && populated) {
        char inherited[sizeof g_fdpath[0]];
        uint8_t inherited_is_guest;
        uint64_t parent_key = fdvis_key(7, descriptor), child_key = fdvis_key(8, descriptor);
        populated = fdpath_snapshot_locked(parent_key, 9, inherited, &inherited_is_guest) == 1 &&
                    fdpath_restore_locked(child_key, 10, inherited, inherited_is_guest) == 0;
        struct fdpath_slot *child = fdpath_find(child_key, 10, 0);
        populated = populated && child && child->path_is_guest == 1 && strcmp(child->path, expected) == 0;
    }
    g_fdpath[descriptor][0] = 0;
    g_fdpath_guest[descriptor] = 0;
    int second = proc_fdvis_publish_path_locked(7, 9, descriptor);
    int cleared = second == 0 && fdpath_find(fdvis_key(7, descriptor), 9, 0) == NULL;
    memcpy(g_fdpath[descriptor], saved_path, sizeof saved_path);
    g_fdpath_guest[descriptor] = saved_is_guest;
    g_fdpaths = saved_paths;
    free(paths);
    return populated && (scenario == 2 || cleared);
}
#endif
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
    uint8_t path_is_guest;
    char path[sizeof g_fdpath[0]];
};

struct fdvis_fork_plan {
    struct fdvis_fork_entry *entries;
    size_t count;
};

static uint64_t fdvis_key(int pid, int fd) {
    return pid > 0 && fd >= 0 ? ((uint64_t)(uint32_t)pid << 32) | ((uint32_t)fd + 1u) : 0;
}

static uint64_t fdvis_process_token(int pid) {
    uint64_t start_time_ns = 0;
    return hl_host_process_start_time_ns(pid, &start_time_ns) ? start_time_ns : 0;
}

/* This process's own (pid, start-time token) pair. Every fdvis operation on a descriptor this process
 * owns needs both, and resolving them as getpid() + fdvis_process_token(getpid()) cost two host calls
 * per lock acquisition and per publish/close -- 13 of the 68 getpid() the engine issued per guest
 * open(). hl_host_process_self_identity() serves both from a memo retired by a fork epoch, so the pair
 * is still this process's own after any fork. PEER pids keep going through fdvis_process_token(), which
 * never memoizes: a remembered start time on a recycled peer pid is precisely the stale ownership the
 * owner_start_ns stamp exists to reject. */
static int fdvis_self(int *pid, uint64_t *token) {
    int64_t self = 0;
    uint64_t start = 0;
    if (!hl_host_process_self_identity(&self, &start)) {
        *pid = (int)getpid();
        *token = fdvis_process_token(*pid);
        return *token != 0;
    }
    *pid = (int)self;
    *token = start;
    return 1;
}

static uint64_t fdvis_identity(int pid, uint64_t start_ns) {
    uint32_t fingerprint = (uint32_t)start_ns ^ (uint32_t)(start_ns >> 32);
    return ((uint64_t)(uint32_t)pid << 32) | fingerprint;
}

static void fdvis_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_fdvis != NULL) return;
    size_t bytes = sizeof(struct fdvis_slot) * FDVIS_N + sizeof(struct fdpath_slot) * FDPATH_N +
                   sizeof(*g_fdvis_control);
    if (hl_linux_shared_create(host, bytes, &arena) != HL_STATUS_OK) return;
    g_fdvis = arena;
    g_fdpaths = (void *)((unsigned char *)arena + sizeof(struct fdvis_slot) * FDVIS_N);
    g_fdvis_control = (void *)((unsigned char *)g_fdpaths + sizeof(struct fdpath_slot) * FDPATH_N);
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
    int me = 0;
    uint64_t me_token = 0;
    (void)fdvis_self(&me, &me_token);
    uint64_t mine = fdvis_identity(me, me_token);
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

static void fdpath_sweep_stale_locked(void) {
    int previous_owner = 0;
    uint64_t previous_start = 0;
    for (unsigned index = 0; index < FDPATH_N; ++index) {
        struct fdpath_slot *slot = &g_fdpaths[index];
        int owner = slot->key == UINT64_MAX ? 0 : (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        uint64_t live_start;
        if (owner == previous_owner) {
            live_start = previous_start;
        } else {
            live_start = fdvis_process_token(owner);
            previous_owner = owner;
            previous_start = live_start;
        }
        if (live_start == 0 || live_start != slot->owner_start_ns) fdpath_delete_locked(slot);
    }
}

static void fdvis_sweep_stale_locked(void) {
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        int owner = (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        uint64_t live_start = fdvis_process_token(owner);
        if (live_start == 0 || live_start != slot->owner_start_ns) memset(slot, 0, sizeof *slot);
    }
    fdpath_sweep_stale_locked();
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
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
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
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
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
    if (guest_fd >= 0 && guest_fd < HL_NFD) {
        (void)proc_fdvis_publish_path_locked(pid, owner_start, guest_fd);
    }
    slot->owner_start_ns = owner_start;
    slot->generation = ++g_fdvis_control->generation;
    slot->key = fdvis_key(pid, guest_fd);
    fdvis_unlock();
    reservation->active = 0;
}

static int proc_fdvis_publish(int guest_fd, uint32_t kind, uint64_t device, uint64_t object) {
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
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
    int path_status = proc_fdvis_publish_path_locked(pid, owner_start, guest_fd);
    slot->generation = generation;
    fdvis_unlock();
    return path_status;
}

static void proc_fdvis_publish_path(int guest_fd) {
    int pid = 0;
    uint64_t owner_start = 0;
    struct fdvis_slot *slot;
    if (guest_fd < 0 || guest_fd >= HL_NFD || !g_fdvis_control) return;
    (void)fdvis_self(&pid, &owner_start);
    fdvis_lock();
    slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (slot) {
        (void)proc_fdvis_publish_path_locked(pid, owner_start, guest_fd);
        slot->generation = ++g_fdvis_control->generation;
    }
    fdvis_unlock();
}

static int proc_fdvis_publish_native_fd(int guest_fd) {
    hl_host_process_fd detail;
    size_t ignored = 0;
    if (guest_fd < 0 || !hl_host_process_fd_read(getpid(), guest_fd, &detail, NULL, 0, &ignored)) return -EBADF;
    return proc_fdvis_publish(guest_fd, detail.kind, detail.stable_device, detail.stable_object);
}

static int proc_fdvis_publish_pipe_pair(int first, int second) {
    uint64_t sequence = atomic_fetch_add_explicit(&g_pipe_identity_next, 1, memory_order_relaxed);
    int self_pid = 0;
    uint64_t self_token = 0;
    (void)fdvis_self(&self_pid, &self_token);
    uint64_t identity = fdvis_identity(self_pid, self_token) ^ sequence;
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
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (slot) memset(slot, 0, sizeof *slot);
    struct fdpath_slot *path = fdpath_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (path) fdpath_delete_locked(path);
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

static int proc_fdvis_lookup_path(int pid, int guest_fd, char *path, size_t capacity, int *path_is_guest) {
    struct fdpath_slot *slot;
    int found = 0;
    if (!g_fdvis_control || path == NULL || capacity == 0) return 0;
    fdvis_lock();
    slot = fdpath_find(fdvis_key(pid, guest_fd), fdvis_process_token(pid), 0);
    if (slot && slot->path[0] != '\0') {
        size_t length = strnlen(slot->path, sizeof slot->path);
        if (length < sizeof slot->path && length < capacity) {
            memcpy(path, slot->path, length + 1);
            if (path_is_guest) *path_is_guest = slot->path_is_guest != 0;
            found = 1;
        }
    }
    fdvis_unlock();
    return found;
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
            (void)fdpath_snapshot_locked(slot->key, slot->owner_start_ns, entries[count].path,
                                         &entries[count].path_is_guest);
            ++count;
            continue;
        }
        uint64_t live_start = fdvis_process_token(owner);
        if (live_start == 0 || live_start != slot->owner_start_ns) memset(slot, 0, sizeof *slot);
    }
    fdpath_sweep_stale_locked();
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

static void proc_fdvis_fork_child_abort(struct fdvis_fork_plan *plan, int child) {
    uint64_t child_start = fdvis_process_token(child);
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (slot->key == UINT64_MAX) {
            memset(slot, 0, sizeof *slot);
            continue;
        }
        if (slot->key != key || (slot->owner_start_ns != 0 && slot->owner_start_ns != child_start)) continue;
        struct fdpath_slot *path = fdpath_find(key, slot->owner_start_ns, 0);
        if (path) fdpath_delete_locked(path);
        memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

static int proc_fdvis_fork_child_timeout(struct fdvis_fork_plan *plan, int child) {
    fdvis_lock();
    int published = 1;
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        if (g_fdvis[entry->slot].key != fdvis_key(child, entry->guest_fd)) {
            published = 0;
            break;
        }
    }
    if (published) {
        fdvis_unlock();
        return 1;
    }
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (slot->key != UINT64_MAX) continue;
        memset(slot, 0, sizeof *slot);
        slot->key = key;
        slot->owner_start_ns = UINT64_MAX;
        slot->generation = ++g_fdvis_control->generation;
    }
    fdvis_unlock();
    return 0;
}

static void proc_fdvis_fork_parent_clear_timeout(struct fdvis_fork_plan *plan, int child) {
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        if (slot->key == fdvis_key(child, entry->guest_fd) && slot->owner_start_ns == UINT64_MAX)
            memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t g_fdvis_fork_wait_milliseconds = UINT64_C(5000);
#endif

static uint64_t fdvis_fork_wait_milliseconds(void) {
#if defined(HL_NATIVE_TEST_HOOKS)
    return g_fdvis_fork_wait_milliseconds;
#else
    return UINT64_C(5000);
#endif
}

static uint64_t fdvis_monotonic_milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000) + (uint64_t)now.tv_nsec / UINT64_C(1000000);
}

struct fdvis_fork_journal {
    struct fdvis_slot *identity;
    struct fdvis_slot previous_identity;
    uint64_t key;
    uint64_t owner_start_ns;
    uint64_t generation;
    struct fdpath_slot *path;
    struct fdpath_slot previous_path;
    struct fdpath_slot written_path;
    struct fdpath_slot provisional_path;
    uint8_t reservation_owned;
    uint8_t identity_written;
    uint8_t path_written;
    uint8_t path_existed;
    uint8_t identity_replaced;
    uint8_t provisional_path_existed;
};

static int fdvis_fork_entry_matches_locked(const struct fdvis_slot *identity, const struct fdvis_fork_entry *entry,
                                           uint64_t key, uint64_t owner_start_ns) {
    if (identity->key != key || identity->owner_start_ns != owner_start_ns || identity->kind != entry->kind ||
        identity->device != entry->device || identity->object != entry->object)
        return 0;
    struct fdpath_slot *path = fdpath_find(key, owner_start_ns, 0);
    if (entry->path[0] == '\0') return path == NULL;
    return path && path->path_is_guest == entry->path_is_guest && strcmp(path->path, entry->path) == 0;
}

static void fdvis_fork_rollback_locked(struct fdvis_fork_journal *journal, size_t count) {
    while (count > 0) {
        struct fdvis_fork_journal *change = &journal[--count];
        if (change->path_written && change->path && change->path->key == change->key &&
            change->path->owner_start_ns == change->owner_start_ns &&
            change->path->path_is_guest == change->written_path.path_is_guest &&
            strcmp(change->path->path, change->written_path.path) == 0) {
            if (change->path_existed)
                *change->path = change->previous_path;
            else
                fdpath_delete_locked(change->path);
        }
        if (change->identity_written && change->identity->key == change->key &&
            change->identity->owner_start_ns == change->owner_start_ns &&
            change->identity->generation == change->generation) {
            if (change->identity_replaced)
                *change->identity = change->previous_identity;
            else
                memset(change->identity, 0, sizeof *change->identity);
        } else if (change->reservation_owned && change->identity->key == UINT64_MAX)
            memset(change->identity, 0, sizeof *change->identity);
        if (change->identity_replaced && change->provisional_path_existed) {
            struct fdpath_slot *provisional =
                fdpath_find(change->provisional_path.key, change->provisional_path.owner_start_ns, 1);
            if (provisional) *provisional = change->provisional_path;
        }
    }
}

static void fdvis_fork_commit_locked(struct fdvis_fork_journal *journal, size_t count) {
    for (size_t index = 0; index < count; ++index) {
        struct fdvis_fork_journal *change = &journal[index];
        if (!change->identity_replaced || !change->provisional_path_existed) continue;
        struct fdpath_slot *provisional =
            fdpath_find(change->provisional_path.key, change->provisional_path.owner_start_ns, 0);
        if (provisional && provisional->path_is_guest == change->provisional_path.path_is_guest &&
            strcmp(provisional->path, change->provisional_path.path) == 0)
            fdpath_delete_locked(provisional);
    }
}

static int proc_fdvis_after_fork(struct fdvis_fork_plan *plan, int child, int in_child) {
    uint64_t child_start = fdvis_process_token(child);
    if (!g_fdvis || !g_fdvis_control || child <= 0) return -EINVAL;
    /* The parent owns publication of the pre-fork reservations.  Letting both
     * branches publish races the child's immediate exit cleanup against the
     * parent's commit: cleanup can clear a slot between the two commits and
     * turn the parent's still-valid reservation into EAGAIN.  The child may
     * not expose its inherited descriptors until the parent's atomic batch is
     * visible, so wait here and then use the transaction below only for the
     * possible start-token upgrade. */
    if (in_child) {
        uint64_t deadline = fdvis_monotonic_milliseconds() + fdvis_fork_wait_milliseconds();
        for (;;) {
            int published = 1;
            fdvis_lock();
            for (size_t index = 0; index < plan->count; ++index) {
                const struct fdvis_fork_entry *entry = &plan->entries[index];
                const struct fdvis_slot *slot = &g_fdvis[entry->slot];
                if (slot->key != fdvis_key(child, entry->guest_fd)) {
                    published = 0;
                    break;
                }
            }
            fdvis_unlock();
            if (published) break;
            if ((int)getppid() != g_fdvis_fork_parent) {
                proc_fdvis_fork_child_abort(plan, child);
                return -ECHILD;
            }
            uint64_t now = fdvis_monotonic_milliseconds();
            if (now == 0 || now >= deadline) {
                if (proc_fdvis_fork_child_timeout(plan, child)) break;
                return -ETIMEDOUT;
            }
            struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
            (void)nanosleep(&pause, NULL);
        }
        child_start = fdvis_process_token(child);
    }
    struct fdvis_fork_journal *journal = calloc(plan->count, sizeof *journal);
    if (plan->count != 0 && !journal) {
        proc_fdvis_fork_cancel(plan);
        return -ENOMEM;
    }
    int status = 0;
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        journal[index].identity = &g_fdvis[plan->entries[index].slot];
        journal[index].key = fdvis_key(child, plan->entries[index].guest_fd);
        journal[index].owner_start_ns = child_start;
        journal[index].reservation_owned = journal[index].identity->key == UINT64_MAX;
    }
    for (size_t index = 0; index < plan->count; ++index) {
        struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *copy = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (fdvis_fork_entry_matches_locked(copy, entry, key, child_start)) continue;
        if (child_start == 0 && copy->key == key && copy->owner_start_ns != 0 &&
            fdvis_fork_entry_matches_locked(copy, entry, key, copy->owner_start_ns))
            continue;
        int token_upgrade = copy->key == key && copy->owner_start_ns == 0 && child_start != 0 &&
                            fdvis_fork_entry_matches_locked(copy, entry, key, 0);
        if (!journal[index].reservation_owned && !token_upgrade) {
            status = -EAGAIN;
            break;
        }
        if (token_upgrade) {
            journal[index].previous_identity = *copy;
            journal[index].identity_replaced = 1;
            struct fdpath_slot *provisional = fdpath_find(key, 0, 0);
            if (provisional) {
                journal[index].provisional_path = *provisional;
                journal[index].provisional_path_existed = 1;
            }
        }
        copy->device = entry->device;
        copy->object = entry->object;
        copy->kind = entry->kind;
        copy->owner_start_ns = child_start;
        copy->generation = ++g_fdvis_control->generation;
        copy->key = key;
        journal[index].generation = copy->generation;
        journal[index].identity_written = 1;
        struct fdpath_slot *previous_path = fdpath_find(key, child_start, 0);
        if (previous_path) {
            journal[index].previous_path = *previous_path;
            journal[index].path_existed = 1;
        }
        int restored = fdpath_restore_locked(key, child_start, entry->path, entry->path_is_guest);
        if (restored != 0) {
            status = restored;
            break;
        }
        if (entry->path[0] != '\0') {
            journal[index].path = fdpath_find(key, child_start, 0);
            journal[index].path_written = journal[index].path != NULL;
            if (journal[index].path_written) journal[index].written_path = *journal[index].path;
        }
    }
    if (status != 0) {
        fdvis_fork_rollback_locked(journal, plan->count);
    } else {
        fdvis_fork_commit_locked(journal, plan->count);
        if (in_child) {
            g_fdvis_fork_parent = child;
            g_fdvis_fork_parent_start = child_start;
        }
    }
    fdvis_unlock();
    if (status != 0 && !in_child) proc_fdvis_fork_parent_clear_timeout(plan, child);
    free(journal);
    return status;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int fdvis_after_fork_rollback_test(void) {
    struct fdvis_slot *identities = calloc(FDVIS_N, sizeof *identities);
    struct fdpath_slot *paths = calloc(FDPATH_N, sizeof *paths);
    struct fdvis_control *control = calloc(1, sizeof *control);
    if (!identities || !paths || !control) {
        free(identities);
        free(paths);
        free(control);
        return 0;
    }
    struct fdvis_slot *saved_identities = g_fdvis;
    struct fdpath_slot *saved_paths = g_fdpaths;
    struct fdvis_control *saved_control = g_fdvis_control;
    int child = (int)getpid();
    uint64_t child_start = fdvis_process_token(child);
    for (unsigned index = 0; index + 1 < FDPATH_N; ++index) {
        paths[index].key = fdvis_key(child, (int)index);
        paths[index].owner_start_ns = child_start;
    }
    identities[0].key = UINT64_MAX;
    identities[1].key = UINT64_MAX;
    struct fdvis_fork_entry entries[2] = {
        {.slot = 0,
         .guest_fd = HL_NFD - 2,
         .kind = 1,
         .device = 2,
         .object = 3,
         .path_is_guest = 1,
         .path = "/rollback/first"},
        {.slot = 1,
         .guest_fd = HL_NFD - 1,
         .kind = 4,
         .device = 5,
         .object = 6,
         .path_is_guest = 1,
         .path = "/rollback/second"},
    };
    struct fdvis_fork_plan plan = {.entries = entries, .count = 2};
    g_fdvis = identities;
    g_fdpaths = paths;
    g_fdvis_control = control;
    int status = proc_fdvis_after_fork(&plan, child, 0);
    uint64_t first_key = fdvis_key(child, entries[0].guest_fd);
    uint64_t second_key = fdvis_key(child, entries[1].guest_fd);
    int rolled_back = status == -ENOSPC && identities[0].key == 0 && identities[1].key == 0 &&
                      fdpath_find(first_key, child_start, 0) == NULL && fdpath_find(second_key, child_start, 0) == NULL;
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    identities[0].key = UINT64_MAX;
    struct fdvis_fork_plan first_only = {.entries = entries, .count = 1};
    int first_status = proc_fdvis_after_fork(&first_only, child, 0);
    struct fdvis_slot successful_identity = identities[0];
    struct fdpath_slot *successful_path = fdpath_find(first_key, child_start, 0);
    struct fdpath_slot successful_path_value = {0};
    if (successful_path) successful_path_value = *successful_path;
    identities[1].key = UINT64_C(0x1234);
    int competing_status = proc_fdvis_after_fork(&plan, child, 0);
    successful_path = fdpath_find(first_key, child_start, 0);
    int preserved = first_status == 0 && competing_status == -EAGAIN &&
                    memcmp(&identities[0], &successful_identity, sizeof successful_identity) == 0 && successful_path &&
                    successful_path->key == successful_path_value.key &&
                    successful_path->owner_start_ns == successful_path_value.owner_start_ns &&
                    successful_path->path_is_guest == successful_path_value.path_is_guest &&
                    strcmp(successful_path->path, successful_path_value.path) == 0 &&
                    identities[1].key == UINT64_C(0x1234);
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    identities[0] = (struct fdvis_slot){.key = first_key,
                                        .owner_start_ns = 0,
                                        .generation = 7,
                                        .kind = entries[0].kind,
                                        .device = entries[0].device,
                                        .object = entries[0].object};
    struct fdpath_slot *provisional = fdpath_find(first_key, 0, 1);
    if (provisional) {
        provisional->path_is_guest = entries[0].path_is_guest;
        snprintf(provisional->path, sizeof provisional->path, "%s", entries[0].path);
    }
    identities[1].key = UINT64_C(0x5678);
    int upgrade_failure = proc_fdvis_after_fork(&plan, child, 0);
    provisional = fdpath_find(first_key, 0, 0);
    int upgrade_rolled_back = upgrade_failure == -EAGAIN && identities[0].key == first_key &&
                              identities[0].owner_start_ns == 0 && identities[0].generation == 7 && provisional &&
                              strcmp(provisional->path, entries[0].path) == 0 &&
                              fdpath_find(first_key, child_start, 0) == NULL;
    identities[1].key = 0;
    int upgrade_success = proc_fdvis_after_fork(&first_only, child, 0);
    struct fdpath_slot *upgraded = fdpath_find(first_key, child_start, 0);
    int upgraded_cleanly = upgrade_success == 0 && identities[0].owner_start_ns == child_start && upgraded &&
                           strcmp(upgraded->path, entries[0].path) == 0 && fdpath_find(first_key, 0, 0) == NULL;
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    identities[0] = (struct fdvis_slot){.key = first_key, .owner_start_ns = child_start};
    identities[1].key = UINT64_MAX;
    struct fdpath_slot *partial_path = fdpath_find(first_key, child_start, 1);
    if (partial_path) snprintf(partial_path->path, sizeof partial_path->path, "%s", entries[0].path);
    proc_fdvis_fork_child_abort(&plan, child);
    int abandoned_cleanly =
        identities[0].key == 0 && identities[1].key == 0 && fdpath_find(first_key, child_start, 0) == NULL;
    struct fdvis_reservation reusable;
    int reserve_status = proc_fdvis_reserve(&reusable);
    abandoned_cleanly = abandoned_cleanly && reserve_status == 0;
    if (reserve_status == 0) proc_fdvis_reservation_cancel(&reusable);
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    identities[0] = (struct fdvis_slot){.key = first_key,
                                        .owner_start_ns = child_start,
                                        .generation = 11,
                                        .kind = entries[0].kind,
                                        .device = entries[0].device,
                                        .object = entries[0].object};
    identities[1] = (struct fdvis_slot){.key = second_key,
                                        .owner_start_ns = child_start,
                                        .generation = 12,
                                        .kind = entries[1].kind,
                                        .device = entries[1].device,
                                        .object = entries[1].object};
    struct fdpath_slot *committed_path = fdpath_find(first_key, child_start, 1);
    if (committed_path) snprintf(committed_path->path, sizeof committed_path->path, "%s", entries[0].path);
    int timeout_observed_commit = proc_fdvis_fork_child_timeout(&plan, child);
    committed_path = fdpath_find(first_key, child_start, 0);
    int commit_survived_timeout = timeout_observed_commit == 1 && identities[0].key == first_key &&
                                  identities[0].generation == 11 && identities[1].key == second_key &&
                                  identities[1].generation == 12 && committed_path &&
                                  strcmp(committed_path->path, entries[0].path) == 0;
    g_fdvis = saved_identities;
    g_fdpaths = saved_paths;
    g_fdvis_control = saved_control;
    free(identities);
    free(paths);
    free(control);
    return rolled_back && preserved && upgrade_rolled_back && upgraded_cleanly && abandoned_cleanly &&
           commit_survived_timeout;
}

static int fdvis_stalled_parent_test(void) {
    struct fdvis_slot *identities =
        mmap(NULL, sizeof *identities * FDVIS_N, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct fdpath_slot *paths =
        mmap(NULL, sizeof *paths * FDPATH_N, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct fdvis_control *control =
        mmap(NULL, sizeof *control, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (identities == MAP_FAILED || paths == MAP_FAILED || control == MAP_FAILED) {
        if (identities != MAP_FAILED) (void)munmap(identities, sizeof *identities * FDVIS_N);
        if (paths != MAP_FAILED) (void)munmap(paths, sizeof *paths * FDPATH_N);
        if (control != MAP_FAILED) (void)munmap(control, sizeof *control);
        return 0;
    }
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    struct fdvis_slot *saved_identities = g_fdvis;
    struct fdpath_slot *saved_paths = g_fdpaths;
    struct fdvis_control *saved_control = g_fdvis_control;
    g_fdvis = identities;
    g_fdpaths = paths;
    g_fdvis_control = control;
    int parent = (int)getpid();
    uint64_t parent_start = fdvis_process_token(parent);
    identities[0] = (struct fdvis_slot){.key = fdvis_key(parent, 3),
                                        .owner_start_ns = parent_start,
                                        .generation = 1,
                                        .kind = 1,
                                        .device = 2,
                                        .object = 3};
    struct fdvis_fork_plan plan;
    int prepared = proc_fdvis_fork_prepare(&plan);
    pid_t child = prepared == 0 ? fork() : -1;
    if (child == 0) {
        g_fdvis_fork_wait_milliseconds = 20;
        int status = proc_fdvis_after_fork(&plan, (int)getpid(), 1);
        _exit(status == -ETIMEDOUT ? 0 : 1);
    }
    int child_status = 1;
    int parent_status = -1;
    if (child > 0) {
        struct timespec hold = {.tv_sec = 0, .tv_nsec = 100000000};
        (void)nanosleep(&hold, NULL);
        parent_status = proc_fdvis_after_fork(&plan, (int)child, 0);
        while (waitpid(child, &child_status, 0) < 0 && errno == EINTR) {}
    }
    struct fdvis_fork_plan retry;
    int retry_status = proc_fdvis_fork_prepare(&retry);
    if (retry_status == 0) proc_fdvis_fork_cancel(&retry);
    int clean = child > 0 && WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0 && parent_status != 0 &&
                identities[1].key == 0 && retry_status == 0;
    free(plan.entries);
    g_fdvis = saved_identities;
    g_fdpaths = saved_paths;
    g_fdvis_control = saved_control;
    (void)munmap(identities, sizeof *identities * FDVIS_N);
    (void)munmap(paths, sizeof *paths * FDPATH_N);
    (void)munmap(control, sizeof *control);
    return clean;
}
#endif

static void proc_fdvis_cleanup(void) {
    int owner = (int)getpid();
    uint64_t owner_start = fdvis_process_token(owner);
    if (!g_fdvis || !g_fdvis_control) return;
    fdvis_lock();
    for (unsigned index = 0; index < FDVIS_N; ++index)
        if ((int)(uint32_t)(g_fdvis[index].key >> 32) == owner && g_fdvis[index].owner_start_ns == owner_start)
            memset(&g_fdvis[index], 0, sizeof g_fdvis[index]);
    fdpath_cleanup_owner_locked(owner, owner_start);
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
// syscalls (guest fork/vfork/clone in proc.c and checkpoint restore) is a glibc
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
