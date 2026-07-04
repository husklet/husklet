// dd/runtime/os/linux/container -- the container VFS: TOCTOU-free path jail, overlay image layers
// (lower/upper + copy-up + whiteout + merged readdir), and /proc + /sys synthesis.

// ---- rootfs path rewriting (ported from mac_elf.c) ----
static const char *g_rootfs = NULL;
// guest CWD (within the rootfs) -- AT_FDCWD resolution + getcwd
static char g_cwd[4200] = "/";
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
static char g_chroot[4200];
// Re-root an absolute guest path under the active chroot: clamp its `..` (after chroot the guest's own
// root IS the chroot dir) and prepend the prefix. The result is still a rootfs-absolute guest path, which
// the resolvers below confine to g_root_fd as usual. Callers invoke this only while a chroot is active.
static void chroot_apply(const char *guest, char *out, size_t n) {
    char norm[4200];
    confine(guest ? guest : "/", norm, sizeof norm);
    if (!g_chroot[0])
        snprintf(out, n, "%s", norm);
    else if (norm[1] == 0)
        snprintf(out, n, "%s", g_chroot); // the chroot root itself
    else
        snprintf(out, n, "%s%s", g_chroot, norm);
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
// realpath(g_rootfs) -- the true rootfs boundary
static char g_rootfs_canon[4200];
static size_t g_rootfs_canon_len;
// fd -> host path it was opened with (dir-fd confinement + cache)
static char g_fdpath[1024][192];
// overlay: dir-fd -> its GUEST path (for merged getdents); "" = not an overlay dir
static char g_ovldir[1024][192];
// O_PATH: fd opened with Linux O_PATH -- it names a file (fstat / *at dirfd / fchdir) but is NOT open for
// I/O, so read/write/pread/pwrite/readv/writev through it must fail EBADF (macOS has no O_PATH; we open a
// normal read fd for the metadata ops and gate the I/O family on this flag). 1 = O_PATH.
static uint8_t g_opath[1024];
// /dev/full: reads return zeros (backed by /dev/zero) but every WRITE fails ENOSPC. macOS has no
// /dev/full, so we flag the fd here and gate the write family in svc_io. 1 = /dev/full.
static uint8_t g_devfull[1024];
// Overlay merged-getdents snapshot cursor reset (rewinddir/seekdir on an overlay dir). Defined in fs.c
// where g_ovldents lives, but the lseek handler (io.c) is included before fs.c, so forward-declare it.
static void ovldents_rewind(int fd, int pos);
// eventfd(read-end) -> pipe write-end + 1 (0 = not an eventfd)
static int g_eventfd_peer[1024];
// eventfd accumulating counter: write() adds, read() returns + resets (the pipe is only readiness).
// _xproc-eventfd-lockf_: the counter array lives in a MAP_SHARED anonymous region so a child created by
// dd's real host fork() updates the SAME physical counters the parent reads -- the readiness pipe is
// already fork-shared (inherited fds), but the accumulating count must be too, or the parent reads 0
// while the child's write()s land in its COW-private copy. Created ONCE at startup (constructor, before
// any guest fork) so every forked worker inherits the same physical array. All g_eventfd_count[fd]
// indexing (io.c, the eventfd2 creation in service.c) is unchanged.
static uint64_t *g_eventfd_count;
static void eventfd_count_init(void) {
    if (g_eventfd_count) return;
    size_t sz = sizeof(uint64_t) * 1024;
    void *mem = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (mem == MAP_FAILED) // cross-process counters degrade, but in-process eventfd still works
        mem = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (mem == MAP_FAILED) abort();
    g_eventfd_count = (uint64_t *)mem;
}
__attribute__((constructor)) static void eventfd_count_ctor(void) { eventfd_count_init(); }
static uint8_t g_eventfd_sema[1024]; // EFD_SEMAPHORE: read() returns 1 and decrements by 1, not the whole counter
// /proc/<pid>/pagemap emulation: the file is VA-indexed (8 bytes per page, addressed by lseek to
// vaddr/pagesize*8), so it can't be materialized as static text. We back it with a real empty seekable fd
// (lseek to any offset works natively) and synthesize the 8-byte entries in the read path (io.c). This
// marks which fds are pagemap backings; cleared on close (fd_reset_emul).
static uint8_t g_pagemap_fd[1024];

// ===================== cross-process guest task-state table (#404) =====================
// Linux's /proc/<pid>/stat field 3 is the task run state (R/S/D/T/Z). dd used to synthesize it from the
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
    _Atomic int pid;         // host pid owning this slot (0 = free)
    _Atomic unsigned char st; // Linux state char: 'R' 'S' 'D' 'T' 'Z'
};
#define TS_N 4096 // power of two; open-addressed by host pid
static struct ts_slot *g_ts_tab;
static void ts_init(void) {
    if (g_ts_tab) return;
    size_t sz = sizeof(struct ts_slot) * TS_N;
    void *m = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (m == MAP_FAILED) // cross-process state degrades to pbi_status, but self-reads still work
        m = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    g_ts_tab = (m == MAP_FAILED) ? NULL : (struct ts_slot *)m;
}
__attribute__((constructor)) static void ts_ctor(void) { ts_init(); }
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
// This thread's/process's own slot, cached. getpid() is libc-cached and re-derived after fork(), so a
// child that inherited the parent's ts_self value transparently re-claims a fresh slot on its first use.
static _Thread_local struct ts_slot *ts_self;
static _Thread_local int ts_self_pid;
static struct ts_slot *ts_mine(void) {
    int pid = (int)getpid();
    if (ts_self && ts_self_pid == pid) return ts_self;
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
static inline void ts_wait_enter(void) { ts_set_self('S'); }
static inline void ts_wait_leave(void) { ts_set_self('R'); }
static inline void ts_running(void) { ts_set_self('R'); } // every non-blocking syscall = we were running
// Reader side: the published state char for host pid `host`, or 0 if this task has no published slot.
static int ts_lookup(int host) {
    struct ts_slot *s = ts_slot_for(host, 0);
    return s ? (int)atomic_load_explicit(&s->st, memory_order_acquire) : 0;
}
// A guest fork child re-claims its own slot lazily (getpid mismatch), but drop the inherited cache eagerly
// so its very first published state is its OWN, not a stale pointer into the parent's slot.
static void ts_after_fork(void) { ts_self = NULL; ts_self_pid = 0; }

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
static struct memf *g_memf[1024];
static _Atomic uint64_t g_memf_total; // sum of logical sizes of all backed files

static int memf_disabled(void) {
    static int v = -1;
    if (v < 0) v = getenv("NOTMPFS") ? 1 : 0;
    return v;
}
static inline struct memf *memf_get(int fd) { return (fd >= 0 && fd < 1024) ? g_memf[fd] : NULL; }

// grow buf to >= need bytes, zero-filling the new tail (so a sparse write reads back as zeros).
static int memf_reserve(struct memf *m, size_t need) {
    if (need <= m->cap) return 0;
    size_t nc = m->cap ? m->cap : 65536;
    while (nc < need) nc = nc < (16u << 20) ? nc << 1 : nc + (16u << 20); // double, then +16MiB chunks
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
    if (memf_disabled() || fd < 0 || fd >= 1024 || g_memf[fd]) return 0;
    if (init < 0 || (uint64_t)init > MEMF_CAP) return 0;
    if (atomic_load(&g_memf_total) + (uint64_t)init > MEMF_TOTAL_CAP) return 0;
    struct memf *m = calloc(1, sizeof *m);
    if (!m) return 0;
    if (init > 0) {
        if (memf_reserve(m, (size_t)init)) { free(m); return 0; }
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
    for (int fd = 0; fd < 1024; fd++)
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
    if (whence == 0) np = off;                  // SEEK_SET
    else if (whence == 1) np = m->pos + off;    // SEEK_CUR
    else if (whence == 2) np = (off_t)m->size + off; // SEEK_END
    else return -2;                             // SEEK_DATA/SEEK_HOLE: let the host fd handle it
    if (np < 0) return -1;
    m->pos = np;
    return np;
}
static int memf_fstat(int fd, struct stat *s) { // real-file metadata, RAM size/blocks
    if (fstat(fd, s) != 0) return -1;
    struct memf *m = g_memf[fd];
    s->st_size = (off_t)m->size;
    s->st_blocks = (blkcnt_t)((m->size + 511) / 512);
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
static void memf_try_adopt(uint64_t dev, uint64_t ino) {
    if (memf_disabled() || !ino) return;
    int found = -1;
    for (int fd = 0; fd < 1024; fd++) {
        if (g_memf[fd]) continue;
        struct stat s;
        if (fstat(fd, &s) != 0) continue;
        if ((uint64_t)s.st_dev == dev && (uint64_t)s.st_ino == ino) {
            if (found >= 0) return; // duped: shared description -> don't risk it
            found = fd;
        }
    }
    if (found < 0) return;
    struct stat s;
    if (fstat(found, &s) != 0 || !S_ISREG(s.st_mode) || s.st_nlink != 0) return;
    int fl = fcntl(found, F_GETFL); // only adopt an O_RDWR fd: a RAM cache serves both reads and writes, so
    if (fl < 0 || (fl & O_ACCMODE) != O_RDWR) return; // adopting an O_RDONLY/O_WRONLY scratch fd would accept
    memf_attach(found, s.st_size, lseek(found, 0, SEEK_CUR)); // I/O the kernel would reject with EBADF (F2).
}

#include "vfs/gmap.c"
// A non-PIE ET_EXEC is linked at a fixed low vaddr but __PAGEZERO forbids mapping there, so load_elf biases
// it high. Its un-relocated absolute refs still point at the low link range; when the guest takes an
// absolute jump there, the dispatcher redirects pc into the biased image (pc += bias) instead of faulting
// on the unmapped low address. [lo,hi) is the un-biased link span of the current main image (0 if PIE).
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;
// fd is a timerfd (a kqueue with an EVFILT_TIMER) -> read() drains it
static uint8_t g_timerfd[1024];
// fd is an inotify (a kqueue with EVFILT_VNODE watches) -> read() drains it
static uint8_t g_inotify[1024];
// inotify-on-a-directory emulation: kqueue says "the dir changed" but not which entry, so we keep the
// watched dir's path + a snapshot of its names and diff on read() to synthesize IN_CREATE/IN_DELETE+name.
static char g_inotify_wpath[1024][512];
static char *g_inotify_snap[1024]; // newline-joined entry names of the last snapshot (malloc'd)
// inotify: which inotify-instance fd owns each watch fd (wd) -> read(instance) drains that wd's move queue.
static int g_inotify_owner[1024];
// timerfd remaining-time tracking (lsys-timerfd-gettime): absolute CLOCK_MONOTONIC deadline (ns) of the
// next expiry + the interval (ns). timerfd_settime records them so timerfd_gettime reports it_value/interval.
static int64_t g_tfd_deadline[1024];
static int64_t g_tfd_interval[1024];
// memfd sealing (lsys-memfd-seal): g_memfd_is[fd]=1 marks an anonymous memfd; g_memfd_seal[fd] carries the
// F_SEAL_* bitmask (F_SEAL_SEAL=1,SHRINK=2,GROW=4,WRITE=8,FUTURE_WRITE=16). A non-ALLOW_SEALING memfd starts
// already F_SEAL_SEAL'd, so further F_ADD_SEALS fail EPERM exactly as on Linux.
static uint8_t g_memfd_is[1024];
static int g_memfd_seal[1024];
// pipe read-pushback (tee(2)): tee() consumes bytes from the source pipe to copy them, then re-queues them
// here so the next read()/readv() on that fd re-serves them -> tee leaves the source pipe intact.
static uint8_t *g_fd_pushback[1024];
static size_t g_fd_pb_len[1024];
// pinned O_DIRECTORY fd to the rootfs (set at startup)
static int g_root_fd = -1;
// Engine-private host fds (the rootfs dir-fd + each bind-mount volume dir-fd) share the guest's descriptor
// table in dd's in-process model. Opened at startup, right after stdio, they otherwise squat the LOW numbers
// Linux would leave free for the guest: g_root_fd lands on fd 3, shifting every guest fd allocation up by one
// AND becoming visible to the guest at a number a native run has free. s6-linux-init (#299) reads its
// notification pipe on the by-convention-lowest fd 3, which under dd was g_root_fd -- a DIRECTORY -> the
// read returns EISDIR ("unable to read from fd 3: Is a directory") and stage 1 aborts. Hoist each startup
// engine fd above a high floor so the guest's low fd space is exactly as on Linux (only 0/1/2 taken). Mirrors
// engine_fd_reloc's F_DUPFD floor (io.c) but relocates unconditionally, not just off a collision. Lazily
// created engine fds (the timer kqueue, the signalfd self-pipe) are made after the guest is running and take
// whatever is free then, so they never squat a fd the just-started guest relies on.
static int engine_fd_hoist(int fd) {
    if (fd < 3) return fd; // stdio (or a failed open) -> nothing to move
    int hi = fcntl(fd, F_DUPFD, 1 << 20); // high floor; F_DUPFD returns the lowest free fd >= floor
    if (hi < 0) hi = fcntl(fd, F_DUPFD, 64); // floor beyond the guest's active low fds under a small RLIMIT
    if (hi < 0) return fd;                    // relocation failed -> keep the original (still functional)
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
    int ro;     // 1 = read-only bind (`-v …:ro`): write-intent syscalls under `guest` fail EROFS
    int isfile; // 1 = single-file bind (`-v host/f:/ctr/f`): `fd` is the host file's PARENT dir, `hcanon`
                // is the file itself, and `guest` matches ONLY its exact path (a file has no children).
};
static struct vol g_vols[32];
static int g_nvols;
// Materialize a volume's mount point (and every ancestor) as empty dirs in the writable rootfs/upper, the
// way Docker mkdir -p's each mount target inside the container rootfs. Without it a NESTED mount leaves its
// parent absent: `-v H:/x/y` makes `/x/y` resolve to the host dir, but `ls /x` ENOENTs because `/x` exists
// in no layer. Creating /x (and /x/y) in the upper lets the merged readdir list `/x` -> `y`; the mount
// itself still wins in jail_pick(), so `/x/y` shows the host files, not the empty placeholder. The rootfs
// is the per-container overlay upper (daemon) or the plain rootfs (manual) -- both writable & private.
// No-op until the rootfs is known (the bridge sets DDVOL after container_init resolves g_rootfs_canon).
// A file mount's leaf is created as an empty placeholder FILE (not a dir) so a parent `ls` shows it as a
// file, exactly as Docker materializes a single-file bind target inside the rootfs.
static void vol_mkmountpoint(const char *guest, int isfile) {
    if (!g_rootfs_canon[0] || !guest || guest[0] != '/') return;
    char mp[4300];
    if ((size_t)snprintf(mp, sizeof mp, "%s%s", g_rootfs_canon, guest) >= sizeof mp) return;
    for (char *s = mp + g_rootfs_canon_len + 1; *s; s++)
        if (*s == '/') {
            *s = 0;
            mkdir(mp, 0755);
            *s = '/';
        }
    if (isfile) {
        int fd = open(mp, O_CREAT | O_RDONLY, 0644);
        if (fd >= 0) close(fd);
    } else
        mkdir(mp, 0755);
}
static void add_vol(const char *spec) { // "[ro:]guestpath:hostdir" -> a confined bind-mount volume
    if (g_nvols >= 32) return;
    // Optional read-only marker. A guest path always begins with '/', so a leading "ro:"/"rw:" token is
    // unambiguous; absent (the legacy `guest:host` form) it defaults to read-write -> byte-identical.
    int ro = 0;
    if (!strncmp(spec, "ro:", 3)) { ro = 1; spec += 3; }
    else if (!strncmp(spec, "rw:", 3)) { spec += 3; }
    char tmp[4096];
    snprintf(tmp, sizeof tmp, "%s", spec);
    char *col = strchr(tmp, ':');
    if (!col || tmp[0] != '/') return;
    *col = 0;
    struct vol *v = &g_vols[g_nvols];
    v->ro = ro;
    snprintf(v->guest, sizeof v->guest, "%s", tmp);
    v->glen = strlen(v->guest);
    while (v->glen > 1 && v->guest[v->glen - 1] == '/') v->guest[--v->glen] = 0;
    if (!realpath(col + 1, v->hcanon)) return;
    v->hlen = strlen(v->hcanon);
    struct stat hst;
    if (stat(v->hcanon, &hst) == 0 && S_ISREG(hst.st_mode)) {
        // Single-file bind: openat's jail base must be a directory, so pin the file's PARENT dir as `fd`
        // and route the exact mount point straight to `hcanon`. Dropping the O_DIRECTORY requirement here
        // is what lets a regular-file source register at all (it ENOTDIRs otherwise -> the mount was lost).
        v->isfile = 1;
        char par[1024];
        snprintf(par, sizeof par, "%s", v->hcanon);
        char *sl = strrchr(par, '/');
        if (!sl) return;
        if (sl == par) par[1] = 0; // file directly under "/" -> parent is "/"
        else *sl = 0;
        if ((v->fd = open(par, O_RDONLY | O_DIRECTORY)) < 0) return;
    } else if ((v->fd = open(v->hcanon, O_RDONLY | O_DIRECTORY)) < 0)
        return;
    v->fd = engine_fd_hoist(v->fd); // keep this engine dir-fd out of the guest's low fd range (#299)
    g_nvols++;
    vol_mkmountpoint(v->guest, v->isfile);
}
// Longest matching bind-mount volume for an absolute guest path (the DEEPEST mount wins, exactly as the
// kernel routes a path to the innermost mount), or -1 for the rootfs/overlay jail. Longest-prefix so a
// nested volume (`-v H1:/x/y -v H2:/x/y/z`) routes /x/y/z to the inner mount regardless of registration
// order; for non-nested volumes (no guest is a prefix of another) it is identical to a first-match scan.
static int jail_match(const char *abs) {
    int best = -1;
    size_t blen = 0;
    for (int i = 0; i < g_nvols; i++) {
        char b = abs[g_vols[i].glen];
        // A directory mount owns its children too (b=='/'); a file mount matches ONLY its exact path.
        int hit = g_vols[i].isfile ? (b == 0) : (b == '/' || b == 0);
        if (g_vols[i].glen > blen && hit && !strncmp(abs, g_vols[i].guest, g_vols[i].glen)) {
            best = i;
            blen = g_vols[i].glen;
        }
    }
    return best;
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
// hard-reset on fork/chroot (rc_reset), volumes never cached, DD_NOPATHCACHE=1 kills it. See the full
// correctness model at the impl. DC_KEYMAX bounds the fixed-size slots (longer paths bypass, safely).
#define DC_KEYMAX 320
static int dc_lookup(const char *key, char *canon, size_t n, int *nmiss);
static void dc_store(const char *key, const char *canon, int nmiss);
static int dc_jail_cacheable(const char *jcanon);
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
    int dcok = dc_jail_cacheable(jcanon);
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
        if (dc_lookup(hkey, dcanon, sizeof dcanon, &k)) {
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
            if (dcok) dc_store(hkey, canon, pops);
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
//                overlay's negative memo must never cache them (mirrors mc_store's volume exclusion).
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
    if (fvi >= 0 && g_vols[fvi].isfile) {
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
static int secure_resolve(const char *guest, char *out, size_t n, int nofollow) {
    return secure_resolve_probe(guest, out, n, nofollow, NULL, NULL);
}
#include "vfs/overlay.c"
// final NOT followed (readlink/lstat)
static const char *xlate(const char *p, char *buf, size_t n) {
    if (g_rootfs && p && p[0] == '/') {
        secure_resolve(p, buf, n, 1);
        return buf;
    }
    return p;
}
// follow symlinks (open/stat/exec)
static const char *xresolve(const char *p, char *buf, size_t n) {
    if (g_rootfs && p && p[0] == '/') {
        secure_resolve(p, buf, n, 0);
        return buf;
    }
    return p;
}
// Resolve an EXEC entrypoint (or PT_INTERP) to a host path, following symlinks the way the kernel
// would INSIDE the rootfs: an absolute symlink target (`/bin/sh -> /bin/busybox`) is rootfs-relative,
// not host-relative -- realpath() can't do this (it follows the target against the host root). Each
// hop is re-confined via secure_resolve, so an escaping link lands on .jail-escape-denied and fails.
static const char *xresolve_exec(const char *p, char *buf, size_t n) {
    if (!(g_rootfs && p && p[0] == '/')) return p;
    char cur[4200];
    snprintf(cur, sizeof cur, "%s", p);
    // bounded symlink chain
    for (int i = 0; i < 40; i++) {
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
            snprintf(cur, sizeof cur, "%s", j);
        // relative to its dir
        }
    }
    secure_resolve(cur, buf, n, 0);
    // fallback: realpath-confine the last hop
    return buf;
}

// Copy the container's PATH value (from the daemon-forwarded DD_GUEST_ENV, "K=V\nK=V") into `out`, or
// leave "" if PATH is unset/empty. This is the image-config PATH (e.g. golang's /usr/local/go/bin:...)
// merged with any `docker run/exec -e PATH=` override -- the authoritative search path for bare commands.
static void container_path_env(char *out, size_t n) {
    out[0] = 0;
    const char *ge = getenv("DD_GUEST_ENV");
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
            if (dl == 0)
                snprintf(gbuf, n, "%s/%s", g_cwd, prog);
            else {
                char dir[4200];
                if (dl >= sizeof dir) dl = sizeof dir - 1;
                memcpy(dir, s, dl);
                dir[dl] = 0;
                if (dir[0] == '/')
                    snprintf(gbuf, n, "%s/%s", dir, prog);
                else
                    snprintf(gbuf, n, "%s/%s/%s", g_cwd, dir, prog);
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
    static const char *const dirs[] = {"/usr/local/sbin", "/usr/local/bin", "/usr/sbin",
                                       "/usr/bin",        "/sbin",          "/bin", NULL};
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
    char tn[] = "/tmp/.ddprocXXXXXX";
    int fd = mkstemp(tn);
    if (fd >= 0) {
        unlink(tn);
        if (write(fd, buf, (size_t)n) < 0) {}
        lseek(fd, 0, SEEK_SET);
    }
    return fd;
}
// ---- guest comm + canonical-exe tracking (#370/#317: the /proc/self/exe surface) ----
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
        if (cl == 1 && p[0] == '.') { p = e; continue; }
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
// resolves $ORIGIN RUNPATHs through this path, so it must be absolute and canonical (#370).
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
    guest_from_host_raw(hp, gb, sizeof gb);
    // guest_from_host_raw answers "/" for a host path outside every layer (fail-safe); keep the lexical
    // guest path then rather than claiming the exe is "/".
    snprintf(out, n, "%s", (gb[0] == '/' && gb[1] == 0 && !(lex[0] == '/' && lex[1] == 0)) ? lex : gb);
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
                             const char *name, int smaps) {
    unsigned long kb = (hi - lo) / 1024;
    // "Locked:" reports the mlock/mlockall'd bytes of THIS region (LTP mlock05 mlock()s a whole mapping
    // and reads its Locked back == the mapping size).
    unsigned long lockkb = (unsigned long)(mlk_region_locked(lo, hi) / 1024);
    int m = snprintf(b, n, "%012lx-%012lx %s 00000000 00:00 0 %*s%s\n", lo, hi, perms, name[0] ? 20 : 0, "", name);
    if (smaps)
        m += snprintf(b + m, (size_t)n - (size_t)m,
                      "Size:%15lu kB\nKernelPageSize:%6d kB\nMMUPageSize:%9d kB\n"
                      "Rss:%16lu kB\nPss:%16lu kB\nShared_Clean:%7d kB\nShared_Dirty:%7lu kB\n"
                      "Private_Clean:%6d kB\nPrivate_Dirty:%6lu kB\nReferenced:%9lu kB\n"
                      "Anonymous:%10lu kB\nAnonHugePages:%6d kB\nSwap:%15d kB\nLocked:%13lu kB\n"
                      "VmFlags: rd wr mr mw me ac\n",
                      kb, 4, 4, kb, kb, 0, kb, 0, 0UL, kb, kb, 0, 0, lockkb);
    return m;
}
// back-compat wrapper: the old rw-p default for anon/heap/stack regions with no per-segment prot.
static int proc_map_region(char *b, size_t n, unsigned long lo, unsigned long hi, const char *name, int smaps) {
    return proc_map_region_p(b, n, lo, hi, "rw-p", name, smaps);
}
// PT_LOAD segments of the main executable, read from the auxv the loader planted (AT_PHDR/AT_PHENT/
// AT_PHNUM) so /proc/self/maps shows the text as r-xp, rodata r--p, data rw-p -- the real per-segment
// protection, not a single flat rw-p span. Cross-arch (the Elf64_Phdr layout is arch-independent).
struct mseg { uint64_t lo, hi; int prot; };
static int maps_phdr_segs(struct mseg *seg, int maxn) {
    uint64_t phdr = 0, phent = 0, phnum = 0;
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 3) phdr = v; else if (t == 4) phent = v; else if (t == 5) phnum = v;
    }
    if (!phdr || phent < 56 || phnum == 0 || phnum > 256) return 0;
    const uint8_t *ph = (const uint8_t *)(uintptr_t)phdr;
    // load bias: PT_PHDR's runtime address (AT_PHDR) minus its link vaddr; 0 for a non-PIE.
    uint64_t bias = 0;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type; memcpy(&type, e, 4);
        if (type == 6) { uint64_t pv; memcpy(&pv, e + 16, 8); bias = phdr - pv; break; } // PT_PHDR
    }
    int nseg = 0;
    for (uint64_t i = 0; i < phnum && nseg < maxn; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type, flags; uint64_t vaddr, memsz;
        memcpy(&type, e, 4); memcpy(&flags, e + 4, 4); memcpy(&vaddr, e + 16, 8); memcpy(&memsz, e + 40, 8);
        if (type != 1 || memsz == 0) continue; // PT_LOAD only
        uint64_t lo = (bias + vaddr) & ~0xfffULL;
        uint64_t hi = (bias + vaddr + memsz + 0xfff) & ~0xfffULL;
        int prot = ((flags & 4) ? 4 : 0) | ((flags & 2) ? 2 : 0) | ((flags & 1) ? 1 : 0); // R|W|X
        seg[nseg].lo = lo; seg[nseg].hi = hi; seg[nseg].prot = prot; nseg++;
    }
    return nseg;
}
static void maps_perms_str(int prot, char *out) { // prot bits: 4=R 2=W 1=X
    out[0] = (prot & 4) ? 'r' : '-';
    out[1] = (prot & 2) ? 'w' : '-';
    out[2] = (prot & 1) ? 'x' : '-';
    out[3] = 'p';
    out[4] = 0;
}
// Synthesize /proc/[pid]/maps (smaps=0) or /proc/[pid]/smaps (smaps=1) from the tracked guest mappings
// (g_gmap) plus the published main-stack bounds. The [stack] line (with a guard line below it, as the
// kernel shows) is what glibc's pthread_getattr_np scans for; the region list makes the file non-empty
// and parseable. Returns an anonymous fd holding the content, or -1 on error.
static int proc_maps_fd(int smaps) {
    char tn[] = "/tmp/.ddprocXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    char b[768];
    // The main executable's PT_LOAD segments FIRST, with their real per-segment protection (text r-xp,
    // rodata r--p, data rw-p) and the exe path as the mapping name -- read from the auxv program headers.
    struct mseg seg[16];
    int nseg = maps_phdr_segs(seg, 16);
    const char *exe = (g_exe_path && g_exe_path[0]) ? g_exe_path : "";
    for (int i = 0; i < nseg; i++) {
        char perms[5];
        maps_perms_str(seg[i].prot, perms);
        int m = proc_map_region_p(b, sizeof b, (unsigned long)seg[i].lo, (unsigned long)seg[i].hi, perms, exe, smaps);
        if (write(fd, b, (size_t)m) < 0) {}
    }
    if (g_stack_hi) {
        unsigned long lo = (unsigned long)g_stack_lo, hi = (unsigned long)g_stack_hi;
        int m = snprintf(b, sizeof b, "%012lx-%012lx ---p 00000000 00:00 0 \n", lo > 0x1000 ? lo - 0x1000 : 0, lo);
        if (write(fd, b, (size_t)m) < 0) {}
        m = proc_map_region(b, sizeof b, lo, hi, "[stack]", smaps);
        if (write(fd, b, (size_t)m) < 0) {}
    }
    for (int i = 0; i < g_ngmap; i++) {
        // report the guest-VISIBLE length (glen) so a mapping's Size/Rss matches the guest's mmap length,
        // not dd's full extent including the 64 KB guard tail it reserves past anon maps (LTP mlock05 Rss).
        unsigned long lo = (unsigned long)g_gmap[i].addr, hi = lo + (unsigned long)g_gmap[i].glen;
        if (g_stack_hi && lo >= (unsigned long)g_stack_lo && hi <= (unsigned long)g_stack_hi)
            continue; // already emitted as [stack]
        // skip a region already rendered as PT_LOAD segments (the image span the loader tracks as one entry)
        int covered = 0;
        for (int s = 0; s < nseg; s++)
            if (lo >= seg[s].lo && lo < seg[s].hi) { covered = 1; break; }
        if (covered) continue;
        int m = proc_map_region(b, sizeof b, lo, hi, "", smaps);
        if (write(fd, b, (size_t)m) < 0) {}
    }
    lseek(fd, 0, SEEK_SET);
    return fd;
}
// /proc/[pid]/status -- the Name:/State:/VmRSS: key:value format (NOT the stat one-liner). VmRSS/VmSize
// reflect the cgroup memory charge so a reader sees a plausible footprint.
static int proc_status_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = pid == 1 ? 0 : (int)getppid();
    unsigned long rss = (unsigned long)(atomic_load(&g_mem_charged) / 1024);
    unsigned long vsz = g_mem_max ? (unsigned long)(g_mem_max / 1024) : rss + 4096;
    if (vsz < rss) vsz = rss;
    unsigned long vmlck = (unsigned long)(mlk_total_locked() / 1024); // mlock/mlockall'd bytes (LTP munlockall01)
    return snprintf(b, n,
                    "Name:\t%s\nUmask:\t0022\nState:\tR (running)\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
                    "TracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t256\nGroups:\t\n"
                    "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t%8lu kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
                    "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
                    "VmSwap:\t       0 kB\nThreads:\t1\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
                    "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
                    "Cpus_allowed:\t1\nCpus_allowed_list:\t0\nvoluntary_ctxt_switches:\t1\n"
                    "nonvoluntary_ctxt_switches:\t0\n",
                    comm, pid, pid, ppid, vsz, vsz, vmlck, rss, rss, rss);
}
// /proc/[pid]/stat -- the 52-field single line (pid (comm) state ppid ...). Field 23 = vsize (bytes),
// field 24 = rss (pages); the rest are plausible zeros. mongod's FTDC collector parses this.
static int proc_stat_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = pid == 1 ? 0 : (int)getppid();
    long pg = sysconf(_SC_PAGESIZE);
    unsigned long pgsz = pg > 0 ? (unsigned long)pg : 4096;
    unsigned long rss_pg = (unsigned long)(atomic_load(&g_mem_charged)) / pgsz;
    unsigned long vsize = g_mem_max ? (unsigned long)g_mem_max : rss_pg * pgsz + (1ul << 20);
    return snprintf(b, n,
                    "%d (%s) R %d %d %d 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 100 %lu %lu 18446744073709551615 "
                    "0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
                    pid, comm, ppid, pid, pid, vsize, rss_pg);
}
// /proc/[pid]/environ -- the guest environment as NUL-separated KEY=VALUE. The authoritative source is
// DD_GUEST_ENV (the container env the daemon forwards, "K=V\nK=V"); absent it (manual/direct mode), fall
// back to the same defaults build_stack hands the guest. Returns the byte count written.
static int proc_environ_text(char *b, size_t n) {
    int o = 0;
    const char *ge = getenv("DD_GUEST_ENV");
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
        static const char *const def[] = {"PATH=/usr/bin:/bin", "HOME=/root", "LANG=C", NULL}; // no TERM (docker parity: unset unless -t)
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
static void procfd_dir_rm(const char *path) {
    DIR *d = opendir(path);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.' && (!e->d_name[1] || (e->d_name[1] == '.' && !e->d_name[2]))) continue;
            char p[160];
            snprintf(p, sizeof p, "%s/%s", path, e->d_name);
            if (e->d_type == DT_DIR) procfd_dir_rm(p); // per-pid dirs nest a task/<tid>/ subtree
            else unlink(p);
        }
        closedir(d);
    }
    rmdir(path);
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
static void procfd_dirs_atexit(void) { procfd_dirs_reap(1); }
// Build the temp dir of fd symlinks and return its fd. The guest fd numbers ARE the host fd numbers here,
// so this process's open fds are exactly the guest's; each link's target is the fd's path (or an
// anon_inode placeholder for a pipe/socket/eventfd with no path). -1 on error.
static int proc_fd_dir_open(void) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.ddfddirXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int fd = 0; fd < 1024; fd++) {
        if (fcntl(fd, F_GETFD) == -1) continue; // not open
        char tgt[4200];
        if (fcntl(fd, F_GETPATH, tgt) == 0 && tgt[0]) {
            if (g_rootfs && !strncmp(tgt, g_rootfs_canon, g_rootfs_canon_len)) {
                const char *g = tgt + g_rootfs_canon_len;
                if (!g[0]) g = "/";
                memmove(tgt, g, strlen(g) + 1);
            }
        } else {
            snprintf(tgt, sizeof tgt, "anon_inode:[%d]", fd);
        }
        char link[64];
        snprintf(link, sizeof link, "%s/%d", tmpl, fd);
        if (symlink(tgt, link) != 0) {}
    }
    int d = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (d < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    for (int i = 0; i < 64; i++)
        if (!g_procfd_dirs[i].path[0]) {
            g_procfd_dirs[i].fd = d;
            snprintf(g_procfd_dirs[i].path, sizeof g_procfd_dirs[i].path, "%s", tmpl);
            break;
        }
    return d;
}
// /proc/[pid]/cmdline -- the guest argv as NUL-separated, NUL-terminated arguments. The full argv vector
// isn't retained past process start, so report argv[0] (the running image path) as the single argument;
// that is what the usual readers (ps, error messages, language runtimes) consult. Returns the byte count.
static int proc_cmdline_text(char *b, size_t n) {
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
// /proc/[pid]/mountinfo -- the mounted-filesystem table df/findmnt parse, and which the JVM scans to locate
// the cgroup mount. The rootfs is a single overlay mount at "/"; the pseudo-filesystems (proc, sysfs, the
// cgroup2 hierarchy, devtmpfs) round it out so a reader looking up any of these mount points finds a
// plausible, well-formed line. Field layout: id parent maj:min root mountpoint opts - fstype src superopts.
static int proc_mountinfo_text(char *b, size_t n) {
    return snprintf(b, n,
                    "23 0 0:21 / / rw,relatime - overlay overlay rw\n"
                    "24 23 0:22 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n"
                    "25 23 0:23 / /sys rw,nosuid,nodev,noexec,relatime - sysfs sysfs rw\n"
                    "26 23 0:24 / /dev rw,nosuid - tmpfs tmpfs rw,mode=755\n"
                    "27 23 0:25 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime - cgroup2 cgroup2 rw\n");
}
// ================= REAL /proc process table (top/htop/ps) =====================================
// dd's process model: every guest process is its OWN host (macOS) process running this DBT; the
// container init is guest pid 1 (g_init_hostpid<->1), children keep their host pid as the guest pid
// (getpid() returns exactly that). macOS has no /proc, and one DBT process cannot see another's
// address space, so we (1) keep a tiny on-disk REGISTRY where each container process publishes its
// guest identity (comm + full argv), keyed by a per-container tmp dir, and (2) read LIVE per-process
// stats (rss, cpu time, state, ppid) from the host kernel via libproc (proc_pidinfo). The union --
// registry identity + libproc liveness -- lets any process (e.g. `ps`) enumerate the whole container
// and synthesize /proc/<pid>/{stat,status,cmdline,comm} for its peers, with GUEST pids throughout.
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/sysctl.h>
#include <mach/mach_host.h>
#include <mach/host_info.h>
#include <mach/vm_statistics.h>
#include <mach/machine.h>
#include <mach/processor_info.h>  // host_processor_info(PROCESSOR_CPU_LOAD_INFO) — real PER-CORE ticks (#412p3)
#include <mach/vm_map.h>          // vm_deallocate for the processor_info array

// The registry directory is keyed per-container (DD_NETNS / DD_HOSTNAME are set once by the daemon and
// inherited across fork + survive guest execve), so two containers on the same host never collide; a
// direct-mode run with neither falls back to the host session id. All peers compute the SAME key.
static void proc_reg_key(char *out, size_t n) {
    const char *k = getenv("DD_NETNS");
    if (!k || !k[0]) k = getenv("DD_HOSTNAME");
    if (k && k[0]) {
        char s[48];
        int o = 0;
        for (const char *p = k; *p && o < 47; p++)
            if ((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9')) s[o++] = *p;
        s[o] = 0;
        if (o) {
            snprintf(out, n, "/tmp/.ddpids.%s", s);
            return;
        }
    }
    snprintf(out, n, "/tmp/.ddpids.s%d", (int)getsid(0));
}
// This process's own registry file (unlinked on exit; the exit_group path calls proc_reg_unlink since
// _exit bypasses atexit). Stale files from a crash are pruned lazily by the enumerator (dead-pid check).
static char g_reg_file[128];
static char g_reg_exe_file[128]; // sibling "x<pid>" record: the canonical exe path (for /proc/<pid>/exe)
static void proc_reg_unlink(void) {
    if (g_reg_file[0]) {
        unlink(g_reg_file);
        g_reg_file[0] = 0;
    }
    if (g_reg_exe_file[0]) {
        unlink(g_reg_exe_file);
        g_reg_exe_file[0] = 0;
    }
}
// Publish THIS process's guest identity: "<comm>\n" then the full argv NUL-separated. Written to a temp
// name + renamed for an atomic publish. Called at startup and after each guest execve (comm changes).
static void proc_reg_publish(const char *exe, int argc, char *const argv[]) {
    if (!g_init_hostpid) return; // process table is a container feature
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    mkdir(dir, 0777);
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
    char tmp[144];
    snprintf(tmp, sizeof tmp, "%s/.t%d", dir, (int)getpid());
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return;
    if (write(fd, buf, (size_t)o) < 0) {}
    close(fd);
    char final[128];
    snprintf(final, sizeof final, "%s/%d", dir, (int)getpid());
    if (rename(tmp, final) == 0) snprintf(g_reg_file, sizeof g_reg_file, "%s", final);
    else unlink(tmp);
    // Publish the CANONICAL exe path as a sibling "x<pid>" record so a PEER process can serve
    // readlink("/proc/<pid>/exe") for this one (`ls -l /proc/<pid>`, ps tooling). The non-digit-leading
    // name keeps it invisible to the pid enumerators (proc_reg_count / the /proc listing digit scan).
    if (exe && exe[0] == '/') {
        char xtmp[152], xfin[144];
        snprintf(xtmp, sizeof xtmp, "%s/.xt%d", dir, (int)getpid());
        snprintf(xfin, sizeof xfin, "%s/x%d", dir, (int)getpid());
        int xfd = open(xtmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (xfd >= 0) {
            if (write(xfd, exe, strlen(exe)) < 0) {}
            close(xfd);
            if (rename(xtmp, xfin) == 0) snprintf(g_reg_exe_file, sizeof g_reg_exe_file, "%s", xfin);
            else unlink(xtmp);
        }
    }
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
// Live per-process stats from the host kernel (libproc). rss/cpu-times/state are REAL (coarse beats
// zero); comm here is the HOST comm (the DBT binary) -- the guest comm comes from the registry instead.
struct dd_procinfo {
    int ppid_host, pgid_host, nthreads;
    char state;
    unsigned long long rss, vsize, utime_ns, stime_ns;
    long start_sec;
    char hostcomm[32];
};
static int dd_get_procinfo(int pid, struct dd_procinfo *pi) {
    struct proc_bsdinfo bsd;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &bsd, sizeof bsd) != (int)sizeof bsd) return 0;
    pi->ppid_host = (int)bsd.pbi_ppid;
    pi->pgid_host = (int)bsd.pbi_pgid;
    pi->start_sec = (long)bsd.pbi_start_tvsec;
    snprintf(pi->hostcomm, sizeof pi->hostcomm, "%s", bsd.pbi_comm);
    switch (bsd.pbi_status) { // SIDL=1 SRUN=2 SSLEEP=3 SSTOP=4 SZOMB=5
    case 2: pi->state = 'R'; break;
    case 4: pi->state = 'T'; break;
    case 5: pi->state = 'Z'; break;
    default: pi->state = 'S'; break;
    }
    struct proc_taskinfo ti;
    if (proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &ti, sizeof ti) == (int)sizeof ti) {
        pi->rss = ti.pti_resident_size;
        pi->vsize = ti.pti_virtual_size;
        pi->utime_ns = ti.pti_total_user;
        pi->stime_ns = ti.pti_total_system;
        pi->nthreads = ti.pti_threadnum > 0 ? ti.pti_threadnum : 1;
    } else {
        pi->rss = pi->vsize = pi->utime_ns = pi->stime_ns = 0;
        pi->nthreads = 1;
    }
    return 1;
}
// Host boot epoch (seconds) -- the base for /proc/<pid> starttime and /proc/uptime. Cached.
static long host_btime(void) {
    static long bt = 0;
    if (bt) return bt;
    struct timeval tv;
    size_t len = sizeof tv;
    int mib[2] = {CTL_KERN, KERN_BOOTTIME};
    bt = (sysctl(mib, 2, &tv, &len, NULL, 0) == 0 && tv.tv_sec) ? tv.tv_sec : time(NULL);
    return bt;
}
// Aggregate host CPU jiffies (user, system, idle, nice) -- monotonically increasing, so htop/top meters
// move. HOST_CPU_LOAD_INFO ticks are already in USER_HZ and summed across cores.
static void host_cpu_ticks(unsigned long long t[4]) {
    host_cpu_load_info_data_t info;
    mach_msg_type_number_t cnt = HOST_CPU_LOAD_INFO_COUNT;
    if (host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, (host_info_t)&info, &cnt) == KERN_SUCCESS) {
        t[0] = info.cpu_ticks[CPU_STATE_USER];
        t[1] = info.cpu_ticks[CPU_STATE_SYSTEM];
        t[2] = info.cpu_ticks[CPU_STATE_IDLE];
        t[3] = info.cpu_ticks[CPU_STATE_NICE];
    } else {
        t[0] = t[1] = t[2] = t[3] = 0;
    }
}
// Real host memory picture (kB): total from hw.memsize, free/available/cached from the Mach VM stats.
static void host_mem(unsigned long long *total, unsigned long long *fre, unsigned long long *avail,
                     unsigned long long *cached) {
    uint64_t memsize = 0;
    size_t len = sizeof memsize;
    sysctlbyname("hw.memsize", &memsize, &len, NULL, 0);
    *total = memsize / 1024;
    vm_size_t pgsz = 4096;
    host_page_size(mach_host_self(), &pgsz);
    vm_statistics64_data_t vm;
    mach_msg_type_number_t cnt = HOST_VM_INFO64_COUNT;
    if (host_statistics64(mach_host_self(), HOST_VM_INFO64, (host_info_t)&vm, &cnt) == KERN_SUCCESS) {
        unsigned long long freep = (unsigned long long)vm.free_count * pgsz;
        unsigned long long inact = (unsigned long long)vm.inactive_count * pgsz;
        unsigned long long spec = (unsigned long long)vm.speculative_count * pgsz;
        *fre = (freep + spec) / 1024;
        *cached = inact / 1024;
        *avail = (freep + inact + spec) / 1024;
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
    return kill(host, 0) == 0 && getsid(host) == getsid(0); // registry may lag; accept a live session peer
}
// /proc/<pid>/stat for a peer -- the 52-field line with GUEST pid/ppid and REAL rss/cpu/state/starttime.
static int proc_stat_pid_text(char *b, size_t n, int gp, int host) {
    struct dd_procinfo pi;
    int ok = dd_get_procinfo(host, &pi);
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
        if (pi.ppid_host == g_init_hostpid) ppid = 1;
        else if (proc_pid_member(pi.ppid_host, &hp)) ppid = pi.ppid_host;
    }
    int pgrp = ok ? (pi.pgid_host == g_init_hostpid ? 1 : pi.pgid_host) : gp;
    long hz = sysconf(_SC_CLK_TCK);
    if (hz <= 0) hz = 100;
    long pg = sysconf(_SC_PAGESIZE);
    unsigned long pgsz = pg > 0 ? (unsigned long)pg : 4096;
    unsigned long long utime = ok ? pi.utime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long long stime = ok ? pi.stime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long rss_pg = ok ? (unsigned long)(pi.rss / pgsz) : 0;
    // The host virtual size is the whole DBT process (code cache + big anon reservations) -> tens of GB,
    // which makes top's VSZ/%VSZ nonsensical. Report a bounded, believable footprint (rss + a modest
    // overhead) instead; there is no visibility into a PEER's true guest vsize from another process.
    unsigned long long vsize = (unsigned long long)rss_pg * pgsz + (128ULL << 20);
    long long since = ok ? (long long)pi.start_sec - host_btime() : 0;
    unsigned long long start_ticks = since > 0 ? (unsigned long long)since * (unsigned long long)hz : 0;
    int nthreads = ok ? pi.nthreads : 1;
    return snprintf(b, n,
                    "%d (%s) %c %d %d %d 0 -1 4194560 0 0 0 0 %llu %llu 0 0 20 0 %d 0 %llu %llu %lu "
                    "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
                    gp, comm, state, ppid, pgrp, gp, utime, stime, nthreads, start_ticks, vsize, rss_pg);
}
// /proc/<pid>/status for a peer -- the key:value form with GUEST Pid/PPid and REAL VmRSS.
static int proc_status_pid_text(char *b, size_t n, int gp, int host) {
    struct dd_procinfo pi;
    int ok = dd_get_procinfo(host, &pi);
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl))
        snprintf(comm, sizeof comm, "%.15s", ok ? pi.hostcomm : "proc");
    int ppid = 0;
    if (gp != 1 && ok) {
        int hp;
        if (pi.ppid_host == g_init_hostpid) ppid = 1;
        else if (proc_pid_member(pi.ppid_host, &hp)) ppid = pi.ppid_host;
    }
    unsigned long rss = ok ? (unsigned long)(pi.rss / 1024) : 0;
    unsigned long vsz = rss + (128UL << 10); // bounded footprint, not the huge host DBT vsize (see stat text)
    char state = ok ? pi.state : 'S'; // same run-state override as proc_stat_pid_text (see there)
    int ov = ts_lookup(host);
    if (ov && state != 'Z' && state != 'T') state = (char)ov;
    return snprintf(b, n,
                    "Name:\t%s\nUmask:\t0022\nState:\t%c\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
                    "TracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t256\nGroups:\t\n"
                    "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t       0 kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
                    "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
                    "VmSwap:\t       0 kB\nThreads:\t%d\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
                    "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
                    "Cpus_allowed:\t1\nCpus_allowed_list:\t0\nvoluntary_ctxt_switches:\t1\n"
                    "nonvoluntary_ctxt_switches:\t0\n",
                    comm, state, gp, gp, ppid, vsz, vsz, rss, rss, rss, ok ? pi.nthreads : 1);
}
// /proc/<pid>/cmdline for a peer -- the published NUL-separated argv (fallback: the comm).
static int proc_cmdline_pid_text(char *b, size_t n, int host) {
    char comm[32], cmd[4096];
    int cl;
    if (proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl) && cl > 0) {
        int L = cl > (int)n ? (int)n : cl;
        memcpy(b, cmd, (size_t)L);
        if (L == 0 || b[L - 1] != 0) {
            if (L < (int)n) b[L++] = 0;
            else b[L - 1] = 0;
        }
        return L;
    }
    struct dd_procinfo pi;
    const char *c = dd_get_procinfo(host, &pi) ? pi.hostcomm : "proc";
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
        struct dd_procinfo pi;
        snprintf(comm, sizeof comm, "%.15s", dd_get_procinfo(host, &pi) ? pi.hostcomm : "proc");
    }
    return snprintf(b, n, "%s\n", comm);
}
// /proc/[pid]/statm -- the 7-field page-count line (size resident shared text lib data dt). htop's
// MEM% column reads `resident` from HERE (not status VmRSS), so it must be present and non-zero.
static int proc_statm_common(char *b, size_t n, unsigned long size_pg, unsigned long rss_pg) {
    return snprintf(b, n, "%lu %lu %lu 1 0 %lu 0\n", size_pg, rss_pg, rss_pg / 2, size_pg);
}
static int proc_statm_text(char *b, size_t n) { // our own pid
    long pg = sysconf(_SC_PAGESIZE);
    unsigned long pgsz = pg > 0 ? (unsigned long)pg : 4096;
    unsigned long rss_pg = (unsigned long)(atomic_load(&g_mem_charged)) / pgsz;
    unsigned long size_pg = g_mem_max ? (unsigned long)(g_mem_max / pgsz) : rss_pg + 256;
    if (size_pg < rss_pg) size_pg = rss_pg;
    return proc_statm_common(b, n, size_pg, rss_pg);
}
static int proc_statm_pid_text(char *b, size_t n, int host) { // a peer -- REAL rss from libproc
    struct dd_procinfo pi;
    long pg = sysconf(_SC_PAGESIZE);
    unsigned long pgsz = pg > 0 ? (unsigned long)pg : 4096;
    unsigned long rss_pg = dd_get_procinfo(host, &pi) ? (unsigned long)(pi.rss / pgsz) : 0;
    return proc_statm_common(b, n, rss_pg + 256, rss_pg);
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
    if (fd >= 0 && fd < 1024) snprintf(g_fdpath[fd], sizeof g_fdpath[fd], "%s%s", g_rootfs_canon, guestpath);
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
    char tmpl[] = "/tmp/.ddppidXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    static const char *const files[] = {"stat", "statm", "status", "cmdline", "comm", 0};
    for (int i = 0; files[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, files[i]);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    if (with_task) {
        char p[64];
        snprintf(p, sizeof p, "%s/task", tmpl);
        mkdir(p, 0555);
        snprintf(p, sizeof p, "%s/fd", tmpl);
        mkdir(p, 0555); // placeholder: an open of /proc/<pid>/fd re-enters the synthesis (proc_fd_dir_open)
    }
    // Magic-link placeholders (exe/cwd/root) so getdents lists them with d_type DT_LNK, like Linux. Every
    // ACCESS to them goes by path or by (tagged dirfd, relative) and is intercepted -- readlink/stat/open
    // of /proc/<pid>/{exe,cwd,root} are served by proc_self_exe / the root|cwd synthesis in fs.c (#370);
    // the inert "." target exists only so a host-side follow can never dangle out of the temp dir.
    static const char *const links[] = {"exe", "cwd", "root", 0};
    for (int i = 0; links[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, links[i]);
        symlink(".", p);
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
    char tmpl[] = "/tmp/.ddptaskXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char p[64];
    snprintf(p, sizeof p, "%s/%d", tmpl, gp);
    mkdir(p, 0555);
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
// If `rp` is a /proc/<pid> DIRECTORY path (the pid dir, its task/ dir, or a task/<tid>/ dir) for a live
// container pid, materialize it and return the fd. Returns -1 on error, or -2 if `rp` is not such a
// directory (a per-pid FILE like stat/status -> the caller falls through to proc_open). fs.c calls this.
static int proc_dir_try_open(const char *rp) {
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
    if (!strncmp(rest, "/task/", 6)) {
        const char *t = rest + 6;
        int j = 0;
        while (t[j] >= '0' && t[j] <= '9')
            j++;
        if (j > 0 && (t[j] == 0 || (t[j] == '/' && t[j + 1] == 0))) {
            int tid = atoi(t);
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
    char tmpl[] = "/tmp/.ddprootXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    // ONLY names proc_open()/synth_stat actually serve -- listing an unserved name makes `ls /proc` stat it
    // and print "No such file or directory". "self" is the magic symlink (handled in synth_stat).
    static const char *const st[] = {"meminfo",  "stat",        "cpuinfo", "uptime",  "loadavg",
                                     "version",  "mounts",      "self",    "cmdline", "filesystems",
                                     "swaps",    "vmstat",      "modules", "devices", 0};
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
                char rp[144];
                snprintf(rp, sizeof rp, "%s/%s", dir, e->d_name);
                unlink(rp);
                continue;
            }
            int guest = (g_init_hostpid && host == g_init_hostpid) ? 1 : host;
            char p[96];
            snprintf(p, sizeof p, "%s/%d", tmpl, guest);
            mkdir(p, 0555); // a real (empty) subdir: getdents reports DT_DIR, and htop opens /proc/<pid>
        }
        closedir(d);
    }
    { // always list ourselves (our registry write may have lagged the first `ps`)
        char p[96];
        snprintf(p, sizeof p, "%s/%d", tmpl, container_pid());
        mkdir(p, 0555);
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, "/proc"); // tag the fd's guest path so relative opens re-enter /proc synth
    return fd;
}
// #289: materialize a /sys/class/net directory as a real temp dir the guest's opendir/getdents can
// walk. The class dir lists the two interfaces (lo, eth0) as subdirs; an interface dir lists its
// attribute files. FILE content is served live via proc_open on the (re-intercepted) relative/absolute
// open. Returns the fd, -1 on error, or -2 if `gp` is not a sysfs-net directory we synthesize.
static int sysnet_dir_open(const char *gp) {
    if (!gp || strncmp(gp, "/sys/class/net", 14)) return -2;
    const char *r = gp + 14;
    const char *const *entries;
    // --network none: loopback-only, so /sys/class/net lists just `lo` (no eth0).
    static const char *const ifaces[] = {"lo", "eth0", 0};
    static const char *const ifaces_lo[] = {"lo", 0};
    static const char *const attrs[] = {"address",   "addr_len",  "broadcast", "flags",   "mtu",
                                        "operstate", "type",      "carrier",   "ifindex", "iflink",
                                        "tx_queue_len", "speed",   "duplex",    0};
    int as_dirs; // class dir -> iface subdirs; iface dir -> attribute files
    if (r[0] == 0 || (r[0] == '/' && r[1] == 0)) {
        entries = net_isolate() ? ifaces_lo : ifaces;
        as_dirs = 1;
    } else if (r[0] == '/' && (!strcmp(r + 1, "lo") || (!net_isolate() && !strcmp(r + 1, "eth0")))) {
        entries = attrs;
        as_dirs = 0;
    } else
        return -2;
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.ddnetXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int i = 0; entries[i]; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/%s", tmpl, entries[i]);
        if (as_dirs)
            mkdir(p, 0555);
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
// #412: materialize the CPU-topology sysfs DIRECTORY so getdents enumerates one cpuN subdir per online
// CPU. htop's LinuxMachine_updateCPUcount opendir()s /sys/devices/system/cpu, counts the cpuN subdirs
// (reading each cpuN/online to mark it active), and -- crucially -- when it finds NO cpuN dir it early-
// returns keeping its built-in default of ONE CPU. macOS has no /sys, and dd previously served only the
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
        for (; *d >= '0' && *d <= '9'; d++) cpuN = cpuN * 10 + (*d - '0');
        if (*d != 0) return -2; // trailing junk (cpufreq/cpuidle/... are files/dirs, not our cpuN synth)
    }
    int nc = container_online_cpus(); // host online count, docker --cpus capped (state.c)
    if (!is_base && (cpuN < 0 || cpuN >= nc)) return -2; // an out-of-range cpuN: not one we advertise
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.ddcpudXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char gpath[48];
    if (is_base) {
        for (int i = 0; i < nc; i++) {
            char p[96];
            snprintf(p, sizeof p, "%s/cpu%d", tmpl, i);
            mkdir(p, 0555); // real SUBDIR: getdents reports DT_DIR so htop counts it
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
// Format 16 raw bytes as a Linux UUID string ("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\n"), stamping the
// RFC-4122 version-4 (b[6]) and variant (b[8]) bits so the result parses as a valid random UUID. Writes
// 37 bytes (36 + '\n') plus a NUL into out (needs >= 38). Returns the byte count (37).
static int uuid_fmt(char *out, size_t cap, uint8_t b[16]) {
    b[6] = (uint8_t)((b[6] & 0x0f) | 0x40);
    b[8] = (uint8_t)((b[8] & 0x3f) | 0x80);
    return snprintf(out, cap,
                    "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x\n", b[0], b[1],
                    b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]);
}
// The 16 raw bytes of the container's boot identity. Must be STABLE for the container's whole life AND
// IDENTICAL across every process in it (each guest process is a separate host engine, so a per-process
// arc4random value would disagree between peers). Derived DETERMINISTICALLY from the per-container
// registry key (DD_NETNS, minted+exported at startup and inherited across fork/execve so every peer
// agrees -- see proc_reg_key) via FNV-1a expanded to 16 bytes. Same container -> same bytes everywhere;
// different containers -> different bytes. Backs both boot_id (UUID) and machine-id (32 hex).
static void boot_id_bytes(uint8_t b[16]) {
    char key[80];
    proc_reg_key(key, sizeof key); // DD_NETNS -> DD_HOSTNAME -> session id fallback
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
// 1024/1048576, everything else unlimited) so the file and the syscall agree.
static int proc_limits_text(char *buf, size_t cap) {
    // name, soft, hard, units ("" -> no unit column value). "unlimited" for RLIM_INFINITY rows.
    static const struct { const char *nm, *soft, *hard, *unit; } L[] = {
        {"Max cpu time", "unlimited", "unlimited", "seconds"},
        {"Max file size", "unlimited", "unlimited", "bytes"},
        {"Max data size", "unlimited", "unlimited", "bytes"},
        {"Max stack size", "8388608", "unlimited", "bytes"},
        {"Max core file size", "unlimited", "unlimited", "bytes"},
        {"Max resident set", "unlimited", "unlimited", "bytes"},
        {"Max processes", "unlimited", "unlimited", "processes"},
        {"Max open files", "1024", "1048576", "files"},
        {"Max locked memory", "unlimited", "unlimited", "bytes"},
        {"Max address space", "unlimited", "unlimited", "bytes"},
        {"Max file locks", "unlimited", "unlimited", "locks"},
        {"Max pending signals", "unlimited", "unlimited", "signals"},
        {"Max msgqueue size", "unlimited", "unlimited", "bytes"},
        {"Max nice priority", "0", "0", ""},
        {"Max realtime priority", "0", "0", ""},
        {"Max realtime timeout", "unlimited", "unlimited", "us"},
    };
    int n = snprintf(buf, cap, "%-25s %-20s %-20s %-10s\n", "Limit", "Soft Limit", "Hard Limit", "Units");
    for (size_t i = 0; i < sizeof L / sizeof *L; i++) {
        const char *soft = L[i].soft, *hard = L[i].hard;
        // docker --ulimit override (g_ulimit, resource number == table index): render the requested values
        // so /proc/self/limits agrees with getrlimit (svc_fill_rlimit). RLIM_INFINITY -> "unlimited".
        char sb[24], hb[24];
        if (i < DD_RLIM_MAX && g_ulimit[i].set) {
            if (g_ulimit[i].cur == ~0ull) soft = "unlimited";
            else { snprintf(sb, sizeof sb, "%llu", (unsigned long long)g_ulimit[i].cur); soft = sb; }
            if (g_ulimit[i].max == ~0ull) hard = "unlimited";
            else { snprintf(hb, sizeof hb, "%llu", (unsigned long long)g_ulimit[i].max); hard = hb; }
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
    static const char *const files[] = {"/proc/kcore",     "/proc/keys",        "/proc/latency_stats",
                                        "/proc/timer_list", "/proc/timer_stats", "/proc/sched_debug", 0};
    static const char *const dirs[] = {"/proc/asound", "/proc/acpi", "/proc/scsi",
                                       "/sys/firmware", "/sys/devices/virtual/powercap", 0};
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
    char tmpl[] = "/tmp/.ddmaskXXXXXX";
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
// `rp` is not one dd masks (so the caller falls through to the normal path). Reserved for READ opens; the
// write-intent EROFS for ReadonlyPaths is enforced in openat before this is reached.
static int proc_masked_open(const char *rp) {
    int mk = proc_masked_kind(rp);
    if (mk == 1) return proc_text_fd("", 0);        // empty regular file
    if (mk == 2) return empty_dir_fd(rp);           // empty directory
    if (proc_ro_dir(rp)) return empty_dir_fd(rp);   // /proc/bus,/fs,/irq: exist, empty, read-only
    if (!strcmp(rp, "/proc/sysrq-trigger")) return proc_text_fd("", 0); // exists, empty on read
    return -2;
}

// Real macOS stat -> Linux struct stat (the fake S_IFCHR version corrupted libc buffering).
// fill_linux_stat (the guest struct-stat layout) is per-arch -> frontend/<arch>/fill_stat.c
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
// x86-64 /proc/cpuinfo block for one logical CPU. The `flags` list mirrors EXACTLY the feature set the JIT's
// CPUID leaf reports (x86_ops.c do_cpuid) -- every token here is backed by a CPUID bit dd actually sets, and
// every CPUID bit dd sets appears here, so a guest gets the SAME answer from `cpuid` and from /proc:
//   leaf 1 EDX:   fpu tsc cx8 sep pge cmov clflush mmx fxsr sse sse2
//   leaf 1 ECX:   pni(sse3) pclmulqdq ssse3 cx16 sse4_1 sse4_2 popcnt aes  (movbe is deliberately WITHHELD --
//                 see do_cpuid: the "movbe && !xsave" Atom fingerprint pessimizes openssl -- so it is absent here too)
//   leaf 7 EBX/EDX: bmi1 bmi2 erms sha_ni fsrm
//   ext 0x80000001: syscall nx rdtscp lm lahf_lm     ext 0x80000007: constant_tsc/nonstop_tsc (invariant TSC)
//   synthetic (Linux always adds): cpuid nopl. NO AVX/xsave (xgetbv reports only x87+SSE). family 6 model 44
//   stepping 2 decode leaf-1 EAX 0x000206c2; model name matches the CPUID brand string (0x80000002..4).
static int cpuinfo_x86_block(char *b, size_t n, int idx, int ncpu) {
    return snprintf(b, n,
        "processor\t: %d\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 44\n"
        "model name\t: dd JIT x86-64 processor\nstepping\t: 2\nmicrocode\t: 0x1\ncpu MHz\t\t: 2500.000\n"
        "cache size\t: 8192 KB\nphysical id\t: 0\nsiblings\t: %d\ncore id\t\t: %d\ncpu cores\t: %d\n"
        "apicid\t\t: %d\ninitial apicid\t: %d\nfpu\t\t: yes\nfpu_exception\t: yes\ncpuid level\t: 7\nwp\t\t: yes\n"
        "flags\t\t: fpu tsc cx8 sep pge cmov clflush mmx fxsr sse sse2 syscall nx rdtscp lm constant_tsc "
        "nonstop_tsc cpuid nopl pni pclmulqdq ssse3 cx16 sse4_1 sse4_2 popcnt aes lahf_lm bmi1 bmi2 erms "
        "sha_ni fsrm\n"
        "bugs\t\t:\nbogomips\t: 5000.00\nclflush size\t: 64\ncache_alignment\t: 64\n"
        "address sizes\t: 39 bits physical, 48 bits virtual\npower management:\n\n",
        idx, ncpu, idx, ncpu, idx, idx);
}
static int proc_open(const char *rp) {
    char buf[8192];
    int n = -1;
    // Per-thread files mirror the main process for a single-threaded proc: fold
    // /proc/<pid>/task/<tid>/<leaf> -> /proc/<pid>/<leaf> so htop's per-thread reads are served.
    char taskbuf[4200];
    {
        const char *t = strstr(rp, "/task/");
        if (t && !strncmp(rp, "/proc/", 6)) {
            const char *s = t + 6; // after "/task/"
            while (*s >= '0' && *s <= '9')
                s++;
            if (s > t + 6 && *s == '/') { // a real /task/<tid>/ segment with a trailing leaf
                int head = (int)(t - rp);
                snprintf(taskbuf, sizeof taskbuf, "%.*s%s", head, rp, s);
                rp = taskbuf;
            }
        }
    }
    // Per-process files for the guest's own pid: /proc/[self|pid]/{fd,maps,smaps,status,stat,environ}.
    const char *leaf = proc_self_leaf(rp);
    if (leaf) {
        if (!strcmp(leaf, "fd")) return proc_fd_dir_open();
        if (!strcmp(leaf, "pagemap")) {
            // VA-indexed binary pagemap: back it with an empty seekable regular fd (lseek to vaddr/pg*8
            // works natively) and synthesize the 8-byte-per-page entries on read (io.c). LTP mmap12.
            int fd = proc_text_fd("", 0);
            if (fd >= 0 && fd < 1024) g_pagemap_fd[fd] = 1;
            return fd;
        }
        if (!strcmp(leaf, "maps") || !strcmp(leaf, "task/1/maps")) return proc_maps_fd(0);
        if (!strcmp(leaf, "smaps")) return proc_maps_fd(1);
        if (!strcmp(leaf, "status")) n = proc_status_text(buf, sizeof buf);
        else if (!strcmp(leaf, "stat")) n = proc_stat_text(buf, sizeof buf);
        else if (!strcmp(leaf, "statm")) n = proc_statm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "environ")) n = proc_environ_text(buf, sizeof buf);
        else if (!strcmp(leaf, "cmdline")) n = proc_cmdline_text(buf, sizeof buf);
        else if (!strcmp(leaf, "comm")) n = proc_comm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "mountinfo")) n = proc_mountinfo_text(buf, sizeof buf);
        else if (!strcmp(leaf, "limits")) n = proc_limits_text(buf, sizeof buf); // #348: rlimit table
        else if (!strcmp(leaf, "oom_score_adj") || !strcmp(leaf, "oom_adj") || !strcmp(leaf, "oom_score"))
            n = snprintf(buf, sizeof buf, "0\n"); // #348: not OOM-adjusted (systemd/containerd read/probe)
        else if (!strcmp(leaf, "loginuid")) n = snprintf(buf, sizeof buf, "4294967295\n"); // #348: unset (pam)
        if (n >= 0) return proc_text_fd(buf, n);
    }
    // A PEER container process: /proc/<otherpid>/{stat,status,cmdline,comm}. proc_self_leaf matched only
    // our own pid above, so a numeric pid reaching here is a peer -- synthesize from the registry (guest
    // comm/argv) + libproc (live rss/cpu/state). This is what makes ps/top/htop show the whole container.
    {
        int gp2;
        const char *fl = proc_any_leaf(rp, &gp2);
        if (fl && gp2 > 0) {
            int host;
            if (proc_pid_member(gp2, &host)) {
                if (!strcmp(fl, "stat")) n = proc_stat_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "status")) n = proc_status_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "statm")) n = proc_statm_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "cmdline")) n = proc_cmdline_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "comm")) n = proc_comm_pid_text(buf, sizeof buf, host);
                if (n >= 0) return proc_text_fd(buf, n);
            }
        }
    }
    if (!strcmp(rp, "/proc/cpuinfo")) {
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        // One block per online CPU. The x86 block is ~570 bytes, so up to 64 CPUs need ~37KB -- far past the
        // shared 8KB `buf` (which silently truncated cpuinfo to ~14 processors on a many-core host, #412).
        // Use a dedicated buffer sized for the 64-CPU ceiling and clamp each snprintf so a would-be overflow
        // can never inflate `cn` past the buffer (proc_text_fd writes exactly `cn` bytes).
        char cib[64 * 640]; // per-call (proc_open is reentrant across guest threads); ~40KB stack
        int cn = 0;
        for (int i = 0; i < nc; i++) {
            size_t rem = sizeof cib - (size_t)cn;
            int w = guest_is_x86()
                ? cpuinfo_x86_block(cib + cn, rem, i, nc)
                : snprintf(cib + cn, rem,
                           "processor\t: %d\nBogoMIPS\t: 100.00\nFeatures\t: fp asimd\nCPU implementer\t: 0x61\n"
                           "CPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0x000\nCPU revision\t: 0\n\n",
                           i);
            if (w < 0 || (size_t)w >= rem) break; // truncated -> stop rather than over-report length
            cn += w;
        }
        return proc_text_fd(cib, cn);
    } else if (!strcmp(rp, "/proc/meminfo")) {
        // Real-ish figures: a cgroup memory.max caps MemTotal (used = the tracked anon charge); otherwise
        // report the host machine's memory (total from hw.memsize, free/available/cached from the Mach VM
        // stats) so htop's memory meter reflects a believable, non-zero footprint instead of "0K used".
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
        n = snprintf(buf, sizeof buf,
                     "MemTotal:    %11llu kB\nMemFree:     %11llu kB\n"
                     "MemAvailable:%11llu kB\nBuffers:               0 kB\nCached:      %11llu kB\n"
                     "SwapTotal:             0 kB\nSwapFree:              0 kB\n"
                     "Shmem:                 0 kB\nSReclaimable:          0 kB\n",
                     tot, fre, avail, cached);
    } else if (!strcmp(rp, "/proc/stat")) {
        // Real host CPU jiffies -> the cpu line increments between reads, so htop/top meters move. The
        // aggregate `cpu` line comes from HOST_CPU_LOAD_INFO; each per-core `cpuN` line comes from that
        // core's OWN ticks via host_processor_info(PROCESSOR_CPU_LOAD_INFO). #412 part 3: the old code
        // split the aggregate EVENLY across cores (aggregate/ncpu), so every cpuN line was byte-identical
        // and htop/top showed every core meter moving in lockstep at the same %. Per-core real ticks make
        // the deltas differ, so a busy core reads hot while idle cores read cold -- exactly like Linux.
        unsigned long long t[4];
        host_cpu_ticks(t);
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        n = snprintf(buf, sizeof buf, "cpu  %llu %llu %llu %llu 0 0 0 0 0 0\n", t[0], t[3], t[1], t[2]);
        processor_info_array_t pinfo = NULL;
        mach_msg_type_number_t picnt = 0;
        natural_t pncpu = 0;
        processor_cpu_load_info_t pl = NULL;
        if (host_processor_info(mach_host_self(), PROCESSOR_CPU_LOAD_INFO, &pncpu, &pinfo, &picnt) ==
            KERN_SUCCESS)
            pl = (processor_cpu_load_info_t)pinfo;
        for (int i = 0; i < nc; i++) {
            unsigned long long u, ni, sy, id;
            if (pl && i < (int)pncpu) { // this core's real ticks (order: user nice system idle)
                u = pl[i].cpu_ticks[CPU_STATE_USER];
                ni = pl[i].cpu_ticks[CPU_STATE_NICE];
                sy = pl[i].cpu_ticks[CPU_STATE_SYSTEM];
                id = pl[i].cpu_ticks[CPU_STATE_IDLE];
            } else { // API failed, or --cpus capped ABOVE the host core count: fall back to the even split
                u = t[0] / (unsigned)nc;
                ni = t[3] / (unsigned)nc;
                sy = t[1] / (unsigned)nc;
                id = t[2] / (unsigned)nc;
            }
            n += snprintf(buf + n, sizeof buf - (size_t)n, "cpu%d %llu %llu %llu %llu 0 0 0 0 0 0\n", i, u,
                          ni, sy, id);
        }
        if (pl) vm_deallocate(mach_task_self(), (vm_address_t)pinfo, picnt * sizeof(integer_t));
        n += snprintf(buf + n, sizeof buf - (size_t)n,
                      "intr 0\nctxt 0\nbtime %ld\nprocesses %d\nprocs_running 1\nprocs_blocked 0\n",
                      host_btime(), proc_reg_count());
    } else if (!strcmp(rp, "/proc/mounts") || !strcmp(rp, "/proc/self/mounts")) {
        // The fstab-style mount table (mirror of mountinfo). Name the root mount "overlay", not the legacy
        // "rootfs": busybox/util-linux df filters out a pseudo "rootfs" entry, leaving df unable to find the
        // mount for "/". The pseudo-filesystems are listed too so a reader enumerating mounts sees them.
        n = snprintf(buf, sizeof buf,
                     "overlay / overlay rw,relatime 0 0\n"
                     "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "tmpfs /dev tmpfs rw,nosuid 0 0\n");
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
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/hostname")) {
        // UTS ns (hostname cmd reads this)
        n = snprintf(buf, sizeof buf, "%s\n", g_hostname[0] ? g_hostname : "jit");
    } else if (!strcmp(rp, "/proc/sys/kernel/random/boot_id")) {
        // #348: stable per-boot UUID (systemd/dbus/libuuid/curl/journald read it; without it tools print
        // "cannot find current boot id"). Deterministic from the container key -> same for every peer.
        n = proc_boot_id(buf, sizeof buf);
    } else if (!strcmp(rp, "/proc/sys/kernel/random/uuid")) {
        // #348: Linux yields a FRESH type-4 UUID on every read of this file -- glibc/libuuid use it as a
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
        n = snprintf(buf, sizeof buf, "#1 SMP ddockerd\n");
    } else if (!strcmp(rp, "/proc/self/cgroup")) {
        // cgroup v2 unified
        n = snprintf(buf, sizeof buf, "0::/\n");
    } else if (!strcmp(rp, "/proc/version")) {
        n = snprintf(buf, sizeof buf, "Linux version 6.1.0 (ddockerd) aarch64\n");
    // ---- container network introspection (#289): lo + eth0 (see netif_* in state.c) --------------
    } else if (!strcmp(rp, "/proc/net/dev")) {
        // per-interface counters; zeros are fine (dd runs no real stack -- this is introspection only).
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
    } else if (!strcmp(rp, "/proc/net/tcp") || !strcmp(rp, "/proc/net/tcp6")) {
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode\n");
    } else if (!strcmp(rp, "/proc/net/udp") || !strcmp(rp, "/proc/net/udp6")) {
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode ref pointer drops\n");
    } else if (!strncmp(rp, "/sys/class/net/", 15)) {
        // per-interface attribute files tools stat/read (address, flags, mtu, operstate, type, ...).
        const char *rest = rp + 15;
        int islo = !strncmp(rest, "lo/", 3), iseth = !strncmp(rest, "eth0/", 5);
        const char *file = islo ? rest + 3 : iseth ? rest + 5 : NULL;
        if (file) {
            if (!strcmp(file, "address")) {
                if (islo)
                    n = snprintf(buf, sizeof buf, "00:00:00:00:00:00\n");
                else {
                    uint8_t m[6];
                    netif_eth0_mac(m);
                    n = snprintf(buf, sizeof buf, "%02x:%02x:%02x:%02x:%02x:%02x\n", m[0], m[1], m[2], m[3],
                                 m[4], m[5]);
                }
            } else if (!strcmp(file, "addr_len")) n = snprintf(buf, sizeof buf, "6\n");
            else if (!strcmp(file, "broadcast"))
                n = snprintf(buf, sizeof buf, islo ? "00:00:00:00:00:00\n" : "ff:ff:ff:ff:ff:ff\n");
            else if (!strcmp(file, "flags")) n = snprintf(buf, sizeof buf, islo ? "0x9\n" : "0x1003\n");
            else if (!strcmp(file, "mtu")) n = snprintf(buf, sizeof buf, islo ? "65536\n" : "1500\n");
            else if (!strcmp(file, "operstate")) n = snprintf(buf, sizeof buf, islo ? "unknown\n" : "up\n");
            else if (!strcmp(file, "type")) n = snprintf(buf, sizeof buf, islo ? "772\n" : "1\n");
            else if (!strcmp(file, "carrier")) n = snprintf(buf, sizeof buf, "1\n");
            else if (!strcmp(file, "ifindex")) n = snprintf(buf, sizeof buf, islo ? "1\n" : "2\n");
            else if (!strcmp(file, "iflink")) n = snprintf(buf, sizeof buf, islo ? "1\n" : "2\n");
            else if (!strcmp(file, "tx_queue_len")) n = snprintf(buf, sizeof buf, islo ? "0\n" : "1000\n");
            else if (!strcmp(file, "mtu")) n = snprintf(buf, sizeof buf, islo ? "65536\n" : "1500\n");
            else if (!strcmp(file, "speed")) n = snprintf(buf, sizeof buf, "-1\n");
            else if (!strcmp(file, "duplex")) n = snprintf(buf, sizeof buf, "unknown\n");
        }
    // cgroup v2: memory limit
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.max")) {
        if (g_mem_max)
            n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)g_mem_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.current")) {
        n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)atomic_load(&g_mem_charged));
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.max")) {
        if (g_pids_max)
            n = snprintf(buf, sizeof buf, "%d\n", g_pids_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.current")) {
        n = snprintf(buf, sizeof buf, "%d\n", atomic_load(&g_pids_cur));
    // ---- #348: the broad /proc + /proc/sys surface real software reads --------------------------------
    } else if (!strcmp(rp, "/proc/cmdline")) {
        n = snprintf(buf, sizeof buf, "root=/dev/sda1 ro quiet\n"); // kernel cmdline (distinct from self/cmdline)
    } else if (!strcmp(rp, "/proc/filesystems")) {
        n = snprintf(buf, sizeof buf,
                     "nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\nnodev\tdevpts\n"
                     "nodev\tmqueue\nnodev\tcgroup2\nnodev\toverlay\n\text3\n\text2\n\text4\n");
    } else if (!strcmp(rp, "/proc/swaps")) {
        n = snprintf(buf, sizeof buf, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"); // no swap
    } else if (!strcmp(rp, "/proc/modules")) {
        n = 0; buf[0] = 0; // no loadable modules
    } else if (!strcmp(rp, "/proc/devices")) {
        n = snprintf(buf, sizeof buf,
                     "Character devices:\n  1 mem\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n"
                     "136 pts\n\nBlock devices:\n");
    } else if (!strcmp(rp, "/proc/vmstat")) {
        n = snprintf(buf, sizeof buf,
                     "nr_free_pages 262144\nnr_zone_inactive_anon 0\nnr_zone_active_anon 0\n"
                     "nr_dirty 0\nnr_writeback 0\nnr_slab_reclaimable 0\nnr_slab_unreclaimable 0\n"
                     "pgpgin 0\npgpgout 0\npswpin 0\npswpout 0\npgfault 0\npgmajfault 0\n");
    } else if (!strcmp(rp, "/proc/net/unix")) {
        n = snprintf(buf, sizeof buf, "Num       RefCount Protocol Flags    Type St Inode Path\n");
    } else if (!strcmp(rp, "/proc/net/snmp")) {
        // The full protocol-counter table `netstat -s` / `ss -s` parse: paired header+value lines for
        // Ip/Icmp/IcmpMsg/Tcp/Udp/UdpLite. dd runs no real IP stack, so the counters are zero -- but the
        // SECTIONS must exist with the exact kernel column names or the parser aborts. Tcp's RtoAlgorithm/
        // RtoMin/RtoMax/MaxConn carry the conventional 1/200/120000/-1 the kernel reports.
        n = snprintf(buf, sizeof buf,
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
        static const struct { const char *p, *v; } K[] = {
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
            {"/proc/sys/kernel/sem", "32000\t1024000000\t500\t32000\n"},
            {"/proc/sys/kernel/msgmax", "8192\n"},
            {"/proc/sys/kernel/msgmnb", "16384\n"},
            {"/proc/sys/kernel/msgmni", "32000\n"},
            {"/proc/sys/kernel/yama/ptrace_scope", "1\n"},
            {"/proc/sys/kernel/random/poolsize", "256\n"},
            {"/proc/sys/kernel/printk", "4\t4\t1\t7\n"},
            {"/proc/sys/kernel/panic", "0\n"},
            // vm
            {"/proc/sys/vm/overcommit_ratio", "50\n"},
            {"/proc/sys/vm/overcommit_kbytes", "0\n"},
            {"/proc/sys/vm/max_map_count", "65530\n"},
            {"/proc/sys/vm/mmap_min_addr", "65536\n"},
            {"/proc/sys/vm/swappiness", "60\n"},
            {"/proc/sys/vm/dirty_ratio", "20\n"},
            {"/proc/sys/vm/dirty_background_ratio", "10\n"},
            {"/proc/sys/vm/nr_hugepages", "0\n"},
            {"/proc/sys/vm/panic_on_oom", "0\n"},
            {"/proc/sys/vm/vfs_cache_pressure", "100\n"},
            // net.core
            {"/proc/sys/net/core/somaxconn", "4096\n"},
            {"/proc/sys/net/core/netdev_max_backlog", "1000\n"},
            {"/proc/sys/net/core/rmem_max", "212992\n"},
            {"/proc/sys/net/core/wmem_max", "212992\n"},
            {"/proc/sys/net/core/rmem_default", "212992\n"},
            {"/proc/sys/net/core/wmem_default", "212992\n"},
            {"/proc/sys/net/core/optmem_max", "20480\n"},
            // net.ipv4
            {"/proc/sys/net/ipv4/ip_local_port_range", "32768\t60999\n"},
            {"/proc/sys/net/ipv4/ip_unprivileged_port_start", "1024\n"},
            {"/proc/sys/net/ipv4/ip_forward", "0\n"},
            {"/proc/sys/net/ipv4/ip_nonlocal_bind", "0\n"},
            {"/proc/sys/net/ipv4/tcp_fin_timeout", "60\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_time", "7200\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_intvl", "75\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_probes", "9\n"},
            {"/proc/sys/net/ipv4/tcp_max_syn_backlog", "128\n"},
            {"/proc/sys/net/ipv4/tcp_syncookies", "1\n"},
            {"/proc/sys/net/ipv4/tcp_tw_reuse", "2\n"},
            {"/proc/sys/net/ipv4/tcp_rmem", "4096\t131072\t6291456\n"},
            {"/proc/sys/net/ipv4/tcp_wmem", "4096\t16384\t4194304\n"},
            {"/proc/sys/net/ipv4/tcp_congestion_control", "cubic\n"},
            {"/proc/sys/net/ipv4/tcp_available_congestion_control", "reno cubic\n"},
            // fs
            {"/proc/sys/fs/file-max", "1048576\n"},
            {"/proc/sys/fs/nr_open", "1048576\n"},
            {"/proc/sys/fs/file-nr", "1024\t0\t1048576\n"},
            {"/proc/sys/fs/pipe-max-size", "1048576\n"},
            {"/proc/sys/fs/pipe-user-pages-hard", "0\n"},
            {"/proc/sys/fs/pipe-user-pages-soft", "16384\n"},
            {"/proc/sys/fs/aio-max-nr", "65536\n"},
            {"/proc/sys/fs/aio-nr", "0\n"},
            {"/proc/sys/fs/protected_hardlinks", "1\n"},
            {"/proc/sys/fs/protected_symlinks", "1\n"},
            {"/proc/sys/fs/suid_dumpable", "0\n"},
            {"/proc/sys/fs/inotify/max_user_watches", "524288\n"},
            {"/proc/sys/fs/inotify/max_user_instances", "128\n"},
            {"/proc/sys/fs/inotify/max_queued_events", "16384\n"},
        };
        for (size_t i = 0; i < sizeof K / sizeof *K; i++)
            if (!strcmp(rp, K[i].p)) { n = snprintf(buf, sizeof buf, "%s", K[i].v); break; }
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
// guest fd 0/1/2 as ONE pty slave. On Linux/devpts that slave is /dev/pts/0, but dd's host pty is a mac
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
// ---- devpts: a guest-created pty must look like /dev/pts/<N> everywhere (#227/#280) --------------
// Real Linux/devpts numbers pty slaves sequentially from the lowest free index. `docker run -t` takes
// index 0 for the container's controlling terminal, so a guest that then openpty()s gets 1, 2, ...; with
// no controlling terminal the guest may take 0. dd's host pty is a macOS /dev/ttysNNN (or a host
// /dev/pts/M) whose raw name must NEVER leak into the guest -- the slave has to appear as /dev/pts/<N>
// everywhere: open() (ahead of the overlay resolver, #227), ptsname(3)/ttyname(3), readlink(/proc/self/
// fd/K), `ls /dev/pts`, and stat as a char device whose dev/ino/rdev match the real slave (glibc/musl
// ttyname compare these; #280). We map each index N to the host pty MASTER fd -- ptsname(master) resolves
// the host slave device the slave opens -- and stamp the index onto every open master/slave fd so the
// fd->path surface can rewrite it. Keeps the existing #411/#219 master-termios cache (keyed by master fd).
#define DEVPTS_MAX 1024
static int g_pts_master[DEVPTS_MAX];      // pts index N -> (host master fd + 1); 0 = free
static char g_pts_slavename[DEVPTS_MAX][64]; // pts index N -> host slave device path (ptsname of the master),
                                          // cached at pts_alloc. #420: after a (forked) process closes its
                                          // master fd, pts_master_fd(N) can no longer resolve the slave via
                                          // ptsname(master), yet the pty is still alive if ANY other process
                                          // (e.g. the parent) holds the master -- so /dev/pts/N must resolve
                                          // by this cached host path. A host open() of it naturally succeeds
                                          // iff the pty is still alive and fails once it is truly gone.
static int g_fd_ptsn[1024];               // host fd -> (pts index + 1); 0 = not a pty fd
static uint8_t g_fd_ptsmaster[1024];      // 1 = this fd is the MASTER end, 0 = a slave
// Materialize/remove the on-disk /dev/pts/<N> node so `ls /dev/pts` reflects the live slaves (devpts
// creates the node when a slave is allocated and drops it when the pty is gone). Backed by an empty upper
// file; its stat()/open()/readlink are intercepted. No-op when the container has no rootfs (bare guest).
static void pts_node_path(int n, char *buf, size_t bn) { snprintf(buf, bn, "%s/dev/pts/%d", g_rootfs_canon, n); }
static void pts_publish(int n) {
    if (!g_rootfs_canon[0] || n < 0 || n >= DEVPTS_MAX) return;
    char p[4200];
    pts_node_path(n, p, sizeof p);
    int fd = open(p, O_CREAT | O_WRONLY, 0620);
    if (fd >= 0) close(fd);
}
static void pts_unpublish(int n) {
    if (!g_rootfs_canon[0] || n < 0 || n >= DEVPTS_MAX) return;
    char p[4200];
    pts_node_path(n, p, sizeof p);
    unlink(p);
}
// Allocate the lowest free pts index for a new host master fd. Index 0 is reserved for the controlling
// terminal whenever the container has one (matching devpts, where the ctty grabbed 0 first).
static int pts_alloc(int masterfd) {
    int start = (ctty_anchor() >= 0) ? 1 : 0;
    for (int n = start; n < DEVPTS_MAX; n++) {
        if (!g_pts_master[n]) {
            g_pts_master[n] = masterfd + 1;
            if (masterfd >= 0 && masterfd < 1024) { g_fd_ptsn[masterfd] = n + 1; g_fd_ptsmaster[masterfd] = 1; }
            // #420: cache the host slave device path now, while the master is open, so /dev/pts/N still
            // resolves after a forked child closes its master (the parent keeps the pty alive).
            g_pts_slavename[n][0] = 0;
            char *sn = ptsname(masterfd);
            if (sn) { strncpy(g_pts_slavename[n], sn, sizeof g_pts_slavename[n] - 1); g_pts_slavename[n][sizeof g_pts_slavename[n] - 1] = 0; }
            return n;
        }
    }
    return -1;
}
static int pts_master_fd(int n) { return (n >= 0 && n < DEVPTS_MAX && g_pts_master[n]) ? g_pts_master[n] - 1 : -1; }
static int pts_index_of_master(int fd) { return (fd >= 0 && fd < 1024 && g_fd_ptsmaster[fd]) ? g_fd_ptsn[fd] - 1 : -1; }
static int pts_index_of_fd(int fd) { return (fd >= 0 && fd < 1024 && g_fd_ptsn[fd]) ? g_fd_ptsn[fd] - 1 : -1; }
static int pts_fd_is_master(int fd) { return fd >= 0 && fd < 1024 && g_fd_ptsmaster[fd]; }
// #420: the cached host slave device path for index N (empty string -> NULL). Used to resolve /dev/pts/N
// when this process no longer holds the master fd (a forked child closed it) but the pty is still alive.
static const char *pts_slave_name(int n) {
    return (n >= 0 && n < DEVPTS_MAX && g_pts_slavename[n][0]) ? g_pts_slavename[n] : NULL;
}
// Record a freshly-opened slave fd's pts index and publish its /dev/pts/N node.
static void pts_note_slave(int slavefd, int n) {
    if (slavefd >= 0 && slavefd < 1024) { g_fd_ptsn[slavefd] = n + 1; g_fd_ptsmaster[slavefd] = 0; }
    pts_publish(n);
}
// close(2) / CLOEXEC-sweep teardown: a master frees its index (and its /dev/pts/N node); a slave clears
// only its own entry (other slaves / the master keep the pty alive).
static void pts_on_close(int fd) {
    if (fd < 0 || fd >= 1024 || !g_fd_ptsn[fd]) return;
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
    if (!sn) sn = pts_slave_name(n); // #420: master closed in this (forked) process; use the cached path
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
    return !strcmp(gp, "/dev/null")     ? "/dev/null"
           : !strcmp(gp, "/dev/zero")    ? "/dev/zero"
           : !strcmp(gp, "/dev/full")    ? "/dev/zero" // /dev/full reads return zeros (writes ENOSPC, gated by fd flag)
           : !strcmp(gp, "/dev/random")  ? "/dev/random"
           : !strcmp(gp, "/dev/urandom") ? "/dev/urandom"
           : !strcmp(gp, "/dev/tty")     ? "/dev/tty"
           : !strcmp(gp, "/dev/console") ? "/dev/null" // no host console in the jail -> back it with /dev/null
                                         : NULL;
}
// Populate the container's /dev at start-up. dd flattens the image into one rootfs (no per-container
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
    mkdir(base, 0755); // ensure /dev exists (image /dev contents were excluded at unpack)
    // helper: build <rootfs>/dev/<leaf> into a scratch buffer
#define DEVP(leaf) (snprintf(base + bl, sizeof base - bl, "/%s", (leaf)), base)
    // /dev/fd + the std stream aliases: the standard Linux symlinks into /proc/self/fd (which the engine
    // already synthesizes). readlink/ls see the symlink; open("/dev/fd/N") is caught by procfd_num().
    symlink("/proc/self/fd", DEVP("fd"));
    symlink("/proc/self/fd/0", DEVP("stdin"));
    symlink("/proc/self/fd/1", DEVP("stdout"));
    symlink("/proc/self/fd/2", DEVP("stderr"));
    // char-device placeholders so they list in /dev; open()/stat() are intercepted by the fs.c synth
    // (dev_node_hostpath), so the empty file is never actually read/written.
    static const char *const chr[] = {"null", "zero", "full", "random", "urandom", "tty", "console", "ptmx"};
    for (size_t i = 0; i < sizeof chr / sizeof *chr; i++) {
        int fd = open(DEVP(chr[i]), O_CREAT | O_WRONLY, 0666);
        if (fd >= 0) close(fd);
    }
    mkdir(DEVP("pts"), 0755);  // devpts mount point; /dev/pts/N slaves resolve via ptsname in fs.c
    // devpts publishes a /dev/pts/ptmx multiplexer node (docker mounts it with ptmxmode=0666); `ls /dev/pts`
    // lists it, and open("/dev/pts/ptmx") is intercepted like /dev/ptmx in fs.c.
    { int fd = open(DEVP("pts/ptmx"), O_CREAT | O_WRONLY, 0666); if (fd >= 0) close(fd); }
    // When the container was handed a controlling terminal (docker run -t: the daemon's login_tty made fd
    // 0/1/2 the pty slave), Linux/devpts names it /dev/pts/0. Materialize that entry so `ls /dev/pts` lists
    // it; stat()/open()/readlink of /dev/pts/0 are intercepted (synth_stat_raw + fs.c) and routed to the
    // real controlling tty, so ttyname(3)/`tty`/`ps` resolve it instead of leaking the host pty device name.
    if (isatty(0) || isatty(1) || isatty(2)) {
        int fd = open(DEVP("pts/0"), O_CREAT | O_WRONLY, 0620);
        if (fd >= 0) close(fd);
    }
    mkdir(DEVP("shm"), 01777); // POSIX shm dir (shm_open names get redirected to a host tmp file in fs.c)
    mkdir(DEVP("mqueue"), 01777);
#undef DEVP
}
// #348: materialize /etc/machine-id (32 lowercase hex + newline) so libdbus/systemd/journald/gnome find a
// stable machine identity that AGREES with /proc/sys/kernel/random/boot_id (both derive from the same
// per-container boot bytes). Only written when the image ships no machine-id (missing or empty) -- an
// image/user-provisioned id is left untouched. Written straight into the writable upper (a real file), so
// reads need no interception. /var/lib/dbus/machine-id (the legacy dbus path) is filled the same way when
// its directory exists. Idempotent.
static void container_populate_machine_id(void) {
    if (!g_rootfs_canon[0]) return;
    uint8_t b[16];
    boot_id_bytes(b);
    char id[40];
    int idn = 0;
    for (int i = 0; i < 16; i++) idn += snprintf(id + idn, sizeof id - (size_t)idn, "%02x", b[i]);
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
            if (write(fd, id, (size_t)idn) < 0) { /* best-effort */ }
            close(fd);
        }
    }
}
// -> macOS struct stat for a synth file
static int synth_stat_raw(const char *gp, struct stat *s) {
    if (!gp) return 0; // NULL (bad) guest path: not a synthetic node; let the caller's host stat EFAULT
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
        static const struct { const char *p; int maj, min; unsigned mode; } D[] = {
            {"/dev/null", 1, 3, 0666},   {"/dev/zero", 1, 5, 0666},    {"/dev/full", 1, 7, 0666},
            {"/dev/random", 1, 8, 0666}, {"/dev/urandom", 1, 9, 0666}, {"/dev/tty", 5, 0, 0666},
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
    // /sys/class/net (#289): the class dir + per-iface dirs are directories; attribute files are regular.
    if (gp && !strncmp(gp, "/sys/class/net", 14)) {
        const char *r = gp + 14;
        int isdir = (r[0] == 0 || (r[0] == '/' && r[1] == 0) || // /sys/class/net
                     (r[0] == '/' && (!strcmp(r + 1, "lo") || !strcmp(r + 1, "eth0")))); // iface dir
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
    // #412: the CPU-topology sysfs tree must stat as PRESENT so tools that stat a path BEFORE opening it
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
                for (; *d >= '0' && *d <= '9'; d++) n = n * 10 + (*d - '0');
                if (*d == 0 && n < container_online_cpus()) {
                    hit = 1;
                    isdir = 1; // a cpuN directory we advertise
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
        const char *lf = proc_any_leaf(gp, &pid);
        if (lf && pid > 0) {
            int host;
            if (pid == (int)getpid() || pid == container_pid() || pid == 1 || proc_pid_member(pid, &host)) {
                int istaskdir = !strcmp(lf, "task");
                if (!istaskdir && !strncmp(lf, "task/", 5) && lf[5]) {
                    istaskdir = 1;
                    for (const char *t = lf + 5; *t; t++)
                        if (*t < '0' || *t > '9') istaskdir = 0; // task/<tid> only (not task/<tid>/<leaf>)
                }
                if (istaskdir) {
                    memset(s, 0, sizeof *s);
                    s->st_mode = S_IFDIR | 0555;
                    s->st_nlink = 3;
                    return 1;
                }
            }
        }
    }
    // /proc/<pid>/fd is a directory and /proc/<pid>/fd/N is a symlink -- answer these directly so stat()
    // sees the right type WITHOUT proc_open() materializing a temp dir as a stat side effect.
    const char *leaf = proc_self_leaf(gp);
    if (leaf) {
        if (!strcmp(leaf, "fd")) {
            memset(s, 0, sizeof *s);
            s->st_mode = S_IFDIR | 0555;
            s->st_nlink = 2;
            return 1;
        }
        if (!strncmp(leaf, "fd/", 3) && leaf[3]) {
            int isnum = 1;
            for (const char *t = leaf + 3; *t; t++)
                if (*t < '0' || *t > '9') isnum = 0;
            if (isnum) {
                memset(s, 0, sizeof *s);
                s->st_mode = S_IFLNK | 0777;
                s->st_nlink = 1;
                s->st_size = 64; // Linux reports a fixed 64 for /proc/<pid>/fd/N links
                return 1;
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
    s->st_mode = S_IFREG | 0444;
    // present as a readable regular file
    s->st_nlink = 1;
    return 1;
}
// -> Linux struct stat buffer
static int synth_stat(const char *gp, uint8_t *out) {
    struct stat s;
    if (!synth_stat_raw(gp, &s)) return 0;
    fill_linux_stat(out, &s, NULL, -1); // synth /proc /sys file: no host backing -> no guest-chown xattr
    return 1;
}
