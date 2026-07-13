// Extracted service() helper block: file-scope globals + static helper functions used by service().
// Not standalone -- #included by ../service.c after its system headers and before static void service().
// Emulated pipe-buffer sizes for F_SETPIPE_SZ/F_GETPIPE_SZ (macOS has no pipe-size fcntl): we record
// the requested (page-rounded) size per-fd and report it back, so size-probing programs see it stick.
static int g_pipesz[DD_NFD];

// ---- guest RLIMIT_NOFILE enforcement -------------------------------------------------------------
// dd shares the host descriptor table, whose real cap is far larger than the guest's (engine-private fds
// are hoisted above 1<<20, see engine_fd_hoist), so the guest's soft RLIMIT_NOFILE is purely EMULATED
// (svc_fill_rlimit reports 20480, or the docker --ulimit / guest setrlimit override in g_ulimit[7]). The
// host therefore never enforces it: a raw dup/open/dup2 at or past the guest cap wrongly succeeds. These
// helpers restore the Linux contract -- Linux allocates the lowest free fd and fails EMFILE if it would be
// >= the soft limit; an explicit dup2/dup3 newfd >= the limit is EBADF. (LTP dup03/dup201.)
static int guest_nofile_cur(void) {
    uint64_t cur = 20480; // dd default soft RLIMIT_NOFILE (mirror svc_fill_rlimit, defined later in proc.c)
    if (g_ulimit[7].set) cur = g_ulimit[7].cur; // docker --ulimit / guest setrlimit(RLIMIT_NOFILE)
    return cur > 0x7fffffff ? 0x7fffffff : (int)cur;
}

// Gate a freshly-allocated host fd (from a lowest-free allocator: dup, open, pipe, socket, ...) against the
// guest cap. A number at/above the cap is one Linux would never have handed out -> close it and report
// EMFILE. Returns r unchanged when in range (or already an error). Pass-through when no cap applies.
static int nofile_gate(int r) {
    if (r >= 0 && r >= guest_nofile_cur()) {
        int e = errno;
        close(r);
        (void)e;
        errno = EMFILE;
        return -1;
    }
    return r;
}

// ---- guest PROT_NONE region registry -------------------------------------------------------------
// The SINGLE source of truth is g_gna (gna_add/gna_clear/gna_hit/gna_reset) in os/linux/thread.c, which the
// target TU #includes BEFORE this file. dd force-maps guest anon memory host-RW (case 222) so its no-op
// mprotect model works, which means a host syscall writing into a guest PROT_NONE buffer does NOT get the
// EFAULT a real Linux copy_to_user would (the host page is really RW). g_gna tracks the guest's requested
// PROT_NONE ranges; the check lives INSIDE host_range_mapped (thread.c), so every guest-buffer validator
// (guest_bad_ptr in mem.c -> io/net/poll) gets the EFAULT for free, and the read/write family (io.c) queries
// gna_hit directly. Fed by mmap(222)/mprotect(226)/munmap(215) in mem.c; reset on execve (gna_reset).
// (removed the duplicate pn_add/pn_del/pn_hit registry that used to live here.)

// ---- flock(2) emulation on a private companion file (BSD whole-file advisory locks) ----------------
// On Linux, flock() (BSD, whole-file, per-open-file-description) and fcntl() POSIX record locks are
// INDEPENDENT lock spaces: one process can hold both on the same fd at once. macOS instead routes BOTH
// through the same per-vnode lock list, so a process holding a flock spuriously conflicts with its OWN
// fcntl F_SETLK on the same file (and vice-versa) -> EAGAIN. To restore Linux independence we service
// flock() NOT on the guest's real fd but on a private COMPANION file -- a distinct vnode, keyed by the
// target's (device,inode) -- using host fcntl locks there. flock therefore never touches the real file's
// fcntl record locks. Cross-process/cross-fork flock exclusion is preserved because every process that
// flock()s the same underlying file contends on the same companion (same dev/ino -> same companion path);
// fcntl record locks stay on the real fd, disjoint from the companion. LOCK_SH->F_RDLCK, LOCK_EX->F_WRLCK,
// LOCK_NB->F_SETLK (else F_SETLKW), LOCK_UN->F_UNLCK. Per-fd state (fd<DD_NFD) drives release-on-close so a
// held flock is dropped when its last fd closes, matching flock's "released on last close" semantics.
#define FLOCK_DIR "/tmp/.ddflock"
static uint8_t g_flock_type[DD_NFD]; // per guest fd: 0 none, else LOCK_SH / LOCK_EX currently held via companion

static struct {
    dev_t dev;
    ino_t ino;
    int fd;   // host fd of the companion lock file (kept open for the process lifetime)
    int refs; // guest fds in THIS process currently holding a flock on this file (for release-on-close)
} g_flkcomp[256];

static int g_nflkcomp;

// Companion index for the file underlying guest `fd` (opening/caching it on first use). -1 (errno set) on
// failure. The companion is pushed to a high descriptor so it never collides with the guest's low fds
// (mirrors engine_fd_reloc); CLOEXEC keeps it out of any real host exec while still inheriting across fork.
static int flock_companion(int fd) {
    struct stat st;
    if (fstat(fd, &st) < 0) return -1;
    for (int i = 0; i < g_nflkcomp; i++)
        if (g_flkcomp[i].dev == st.st_dev && g_flkcomp[i].ino == st.st_ino) return i;
    if (g_nflkcomp >= (int)(sizeof g_flkcomp / sizeof g_flkcomp[0])) {
        errno = ENOLCK;
        return -1;
    }
    mkdir(FLOCK_DIR, 0777);
    char p[80];
    snprintf(p, sizeof p, FLOCK_DIR "/%llx.%llx", (unsigned long long)st.st_dev, (unsigned long long)st.st_ino);
    int c = open(p, O_RDWR | O_CREAT | O_CLOEXEC, 0666);
    if (c < 0) return -1;
    int hi = fcntl(c, F_DUPFD_CLOEXEC, 1 << 20);
    if (hi < 0) hi = fcntl(c, F_DUPFD_CLOEXEC, 64);
    if (hi >= 0) {
        close(c);
        c = hi;
    }
    g_flkcomp[g_nflkcomp].dev = st.st_dev;
    g_flkcomp[g_nflkcomp].ino = st.st_ino;
    g_flkcomp[g_nflkcomp].fd = c;
    g_flkcomp[g_nflkcomp].refs = 0;
    return g_nflkcomp++;
}

// flock(2): whole-file advisory lock delegated to the companion. Returns 0 or -1 (host errno set); the
// caller applies the normal macOS->Linux errno translation.
static int dd_flock(int fd, int op) {
    int idx = flock_companion(fd);
    if (idx < 0) return -1;
    int comp = g_flkcomp[idx].fd, base = op & ~LOCK_NB;
    struct flock fl = {.l_whence = SEEK_SET, .l_start = 0, .l_len = 0}; // whole file
    if (base == LOCK_UN) {
        fl.l_type = F_UNLCK;
        int r = fcntl(comp, F_SETLK, &fl);
        if (r == 0 && fd >= 0 && fd < DD_NFD && g_flock_type[fd]) {
            g_flock_type[fd] = 0;
            if (--g_flkcomp[idx].refs < 0) g_flkcomp[idx].refs = 0;
        }
        return r;
    }
    fl.l_type = base == LOCK_SH ? F_RDLCK : F_WRLCK;
    int r = fcntl(comp, (op & LOCK_NB) ? F_SETLK : F_SETLKW, &fl);
    if (r == 0 && fd >= 0 && fd < DD_NFD) {
        if (!g_flock_type[fd]) g_flkcomp[idx].refs++;
        g_flock_type[fd] = (uint8_t)base;
    }
    return r;
}

// close() hook: drop the flock this fd contributed; release the companion lock once its last holder in
// this process is gone (flock is released on the last close of the file).
static void flock_on_close(int fd) {
    if (fd < 0 || fd >= DD_NFD || !g_flock_type[fd]) return;
    g_flock_type[fd] = 0;
    int idx = flock_companion(fd);
    if (idx < 0) return;
    if (--g_flkcomp[idx].refs <= 0) {
        g_flkcomp[idx].refs = 0;
        struct flock fl = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
        fcntl(g_flkcomp[idx].fd, F_SETLK, &fl);
    }
}

// ---- fcntl(2) POSIX advisory byte-range locks -- IN-ENGINE cross-process manager --------------
// SQLite/postgres take ~2 fcntl(F_SETLK/F_GETLK) byte-range locks PER query on the DB file; routing each to
// a real macOS fcntl() was ~57% of a file-backed point-select's runtime (~20% the host round-trip itself +
// ~30% dispatch and the Linux<->macOS `struct flock` translation). We service these ops from a SHARED-MEMORY
// lock table INSIDE the engine instead, eliminating the host syscall entirely (mirrors getpid, which the
// profiler measured FASTER than native).
//
// CORRECTNESS -- POSIX record locks coordinate ACROSS the container's guest processes (multi-connection
// SQLite, postgres backends), which are SEPARATE host processes all descended from container init by fork().
// The table lives in a MAP_SHARED|MAP_ANON region created at engine startup (constructor, before any guest
// fork), so every forked worker inherits the SAME physical table -- identical to the cross-process futex
// table in thread.c. dd's guest execve reloads the image IN-PROCESS, so the region (and a process's locks)
// survive exec, exactly as POSIX requires. Records are keyed by host-file identity (dev,ino) + absolute byte
// range + type, owned PER-PROCESS by host pid (threads of a process share the pid, hence share their locks;
// a fork child gets a NEW pid so it does NOT inherit the parent's locks -- POSIX). This is fully INDEPENDENT
// of flock(2) (serviced on a companion file, above), so the flock<->fcntl independence is preserved.
// A crashed owner's stale records are reclaimed lazily on conflict (kill(pid,0)==ESRCH) and swept when the
// table fills; they are released eagerly on close (poslk_on_close, POSIX "any close drops all this file's
// locks") and on process exit (poslk_on_exit). The table lock is an atomic spinlock (macOS has no robust
// pthread mutex) that STEALS the lock from a crashed holder, so a fault mid-update can't wedge the table.
#include <signal.h>
#include <sched.h>
#include <stdint.h>
#define POSLK_MAX 8192

struct poslk_rec {
    dev_t dev;
    ino_t ino;
    int64_t lo, hi; // absolute byte range [lo,hi); hi==INT64_MAX => to-EOF (Linux l_len 0)
    int32_t type;   // F_RDLCK / F_WRLCK (F_UNLCK is never stored)
    int32_t owner;  // owning process = host pid (0 == free slot)
};

struct poslk_shm {
    _Atomic int32_t lockword; // 0 = free, else host pid of the holder (spinlock; stolen from a dead holder)
    int32_t hi;               // high-water slot count (bounds the linear scan)
    struct poslk_rec rec[POSLK_MAX];
};
static struct poslk_shm *g_poslk;
// PERF: the hot path must issue ZERO host syscalls (the point of). Two process-local caches make that
// so: (a) g_lk{dev,ino,val}[fd] memoises a guest fd's host (dev,ino) so a repeated F_SETLK on the same DB fd
// never re-fstat()s (cleared on close in fd_reset_emul); (b) g_mypid caches getpid(). Both are per-process
// (NOT in the shared region); fork inherits a valid COW copy (same fds/files) and the clone child resets
// g_mypid/g_i_locked via poslk_after_fork.
static dev_t g_lkdev[DD_NFD];
static ino_t g_lkino[DD_NFD];
static uint8_t g_lkval[DD_NFD]; // 1 == g_lk{dev,ino}[fd] valid
static int32_t g_mypid;       // cached getpid() (0 = not cached yet)
static int g_i_locked;        // this process has held >=1 fcntl record lock (gates the close-time fstat)

static inline int32_t poslk_mypid(void) {
    if (!g_mypid) g_mypid = getpid();
    return g_mypid;
}

static void poslk_after_fork(void) {
    g_mypid = 0;    // child has a new host pid -> re-cache lazily
    g_i_locked = 0; // POSIX: a fork child inherits NONE of the parent's record locks
}

static void poslk_init(void) {
    if (g_poslk) return;
    void *m = mmap(NULL, sizeof(struct poslk_shm), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (m == MAP_FAILED) return;     // no shared table -> every op falls back to the host fcntl path
    g_poslk = (struct poslk_shm *)m; // MAP_ANON is zero-filled: lockword=0, hi=0, all owner=0
}

__attribute__((constructor)) static void poslk_ctor(void) {
    poslk_init();
}

// Is host pid `p` still a live process? A dead owner's records/locks are reclaimable.
static inline int poslk_alive(int32_t p) {
    if (p <= 0) return 0;
    return !(kill(p, 0) < 0 && errno == ESRCH);
}

static void poslk_lock(void) {
    int32_t me = poslk_mypid();
    for (int spin = 0;; spin++) {
        int32_t expect = 0;
        if (atomic_compare_exchange_weak_explicit(&g_poslk->lockword, &expect, me, memory_order_acquire,
                                                  memory_order_relaxed))
            return;
        if ((spin & 1023) == 1023) {
            int32_t owner = atomic_load_explicit(&g_poslk->lockword, memory_order_relaxed);
            if (owner && !poslk_alive(owner)) // holder crashed mid-update -> steal the spinlock
                atomic_compare_exchange_strong_explicit(&g_poslk->lockword, &owner, me, memory_order_acquire,
                                                        memory_order_relaxed);
            sched_yield();
        }
    }
}

static void poslk_unlock(void) {
    atomic_store_explicit(&g_poslk->lockword, 0, memory_order_release);
}

// A free slot (reuse a vacated one, else grow the high-water). NULL when the table is full.
static struct poslk_rec *poslk_slot(void) {
    for (int i = 0; i < g_poslk->hi; i++)
        if (!g_poslk->rec[i].owner) return &g_poslk->rec[i];
    if (g_poslk->hi < POSLK_MAX) return &g_poslk->rec[g_poslk->hi++];
    return NULL;
}

static void poslk_sweep_dead(void) {
    for (int i = 0; i < g_poslk->hi; i++)
        if (g_poslk->rec[i].owner && !poslk_alive(g_poslk->rec[i].owner)) g_poslk->rec[i].owner = 0;
}

// Ranges overlap AND at least one is a write lock (two read locks never conflict).
static inline int poslk_conflict(const struct poslk_rec *r, int64_t lo, int64_t hi, int type) {
    if (r->hi <= lo || hi <= r->lo) return 0;     // disjoint
    return r->type == F_WRLCK || type == F_WRLCK; // a writer on either side conflicts
}

// Remove owner `own`'s locks over [lo,hi) on (dev,ino), SPLITTING any record that straddles a boundary so the
// non-overlapping fragments survive with their original type (POSIX unlock / replace-on-set). Held under the
// spinlock. New fragments land in reused slots and, being disjoint from [lo,hi), are skipped by this scan.
static void poslk_clear_own(dev_t dev, ino_t ino, int32_t own, int64_t lo, int64_t hi) {
    int n = g_poslk->hi;
    for (int i = 0; i < n; i++) {
        struct poslk_rec *r = &g_poslk->rec[i];
        if (r->owner != own || r->dev != dev || r->ino != ino) continue;
        if (r->hi <= lo || hi <= r->lo) continue; // disjoint -> keep
        int64_t olo = r->lo, ohi = r->hi;
        int32_t ot = r->type;
        r->owner = 0; // drop the straddling original; re-add surviving fragments below
        if (olo < lo) {
            struct poslk_rec *f = poslk_slot();
            if (f) {
                f->dev = dev;
                f->ino = ino;
                f->lo = olo;
                f->hi = lo;
                f->type = ot;
                f->owner = own;
            }
        }
        if (ohi > hi) {
            struct poslk_rec *f = poslk_slot();
            if (f) {
                f->dev = dev;
                f->ino = ino;
                f->lo = hi;
                f->hi = ohi;
                f->type = ot;
                f->owner = own;
            }
        }
    }
}

// Resolve a Linux `struct flock` at `lf` on guest `fd` to (dev,ino,[lo,hi),type). Returns 0 on success,
// -2 if the fd is not a regular file (caller must use the host fcntl path), or -1 with errno set on error.
static int poslk_resolve(int fd, const uint8_t *lf, dev_t *dev, ino_t *ino, int64_t *lo, int64_t *hi, int *type) {
    short lt = *(const short *)(lf + 0);
    *type = lt == 0 ? F_RDLCK : lt == 1 ? F_WRLCK : F_UNLCK; // Linux RDLCK=0/WRLCK=1/UNLCK=2
    short whence = *(const short *)(lf + 2);
    int64_t start = *(const int64_t *)(lf + 8);
    int64_t len = *(const int64_t *)(lf + 16);
    int cached = fd >= 0 && fd < DD_NFD && g_lkval[fd];
    int64_t base;
    // Hot path: SEEK_SET on a cached fd -> (dev,ino) from the cache, base 0, and NOT A SINGLE host syscall.
    if (cached && whence == SEEK_SET) {
        *dev = g_lkdev[fd];
        *ino = g_lkino[fd];
        base = 0;
    } else {
        struct stat st; // cache miss, or SEEK_CUR/SEEK_END need the live offset/size -> one fstat
        if (fstat(fd, &st) < 0) return -1;
        if (!S_ISREG(st.st_mode)) return -2; // POSIX record locks are only meaningful on regular files
        *dev = st.st_dev;
        *ino = st.st_ino;
        if (fd >= 0 && fd < DD_NFD) {
            g_lkdev[fd] = st.st_dev;
            g_lkino[fd] = st.st_ino;
            g_lkval[fd] = 1;
        }
        if (whence == SEEK_SET)
            base = 0;
        else if (whence == SEEK_CUR) {
            off_t cur = lseek(fd, 0, SEEK_CUR);
            if (cur == (off_t)-1) return -1;
            base = cur;
        } else if (whence == SEEK_END)
            base = st.st_size;
        else {
            errno = EINVAL;
            return -1;
        }
    }
    int64_t s = base + start, e;
    if (len == 0)
        e = INT64_MAX; // to EOF and any future growth
    else if (len > 0)
        e = s + len;
    else { // negative len: the range ends at s and starts len bytes before it
        e = s;
        s = s + len;
    }
    if (s < 0) {
        errno = EINVAL;
        return -1;
    }
    *lo = s;
    *hi = e;
    return 0;
}

// Service one fcntl POSIX-lock op in-engine: lcmd 5=F_GETLK, 6=F_SETLK, 7=F_SETLKW. Returns 1 when serviced
// (*out = the value to return to the guest: 0 / -EAGAIN / -EBADF / -EINVAL / -ENOLCK) or 0 when the fd is not
// a regular file (the caller must fall back to a real host fcntl). F_SETLKW is a single non-blocking attempt
// here (returns -EAGAIN on conflict); io.c wraps it in a signal-aware poll-retry loop.
static int poslk_op(int fd, int lcmd, uint8_t *lf, int *out) {
    if (!g_poslk) return 0; // shared table unavailable -> host path
    dev_t dev;
    ino_t ino;
    int64_t lo, hi;
    int type;
    int rr = poslk_resolve(fd, lf, &dev, &ino, &lo, &hi, &type);
    if (rr == -2) return 0;
    if (rr < 0) {
        *out = -(errno ? errno : EINVAL);
        return 1;
    }
    int32_t me = poslk_mypid();
    poslk_lock();
    if (lcmd == 5) { // F_GETLK: report the first conflicting lock held by ANOTHER process, else F_UNLCK
        struct poslk_rec *hit = NULL;
        for (int i = 0; i < g_poslk->hi; i++) {
            struct poslk_rec *r = &g_poslk->rec[i];
            if (!r->owner || r->owner == me || r->dev != dev || r->ino != ino) continue;
            if (!poslk_conflict(r, lo, hi, type)) continue;
            if (!poslk_alive(r->owner)) {
                r->owner = 0;
                continue;
            } // reclaim a dead holder
            hit = r;
            break;
        }
        if (hit) {
            *(short *)(lf + 0) = hit->type == F_RDLCK ? 0 : 1;
            *(short *)(lf + 2) = SEEK_SET;
            *(int64_t *)(lf + 8) = hit->lo;
            *(int64_t *)(lf + 16) = hit->hi == INT64_MAX ? 0 : (hit->hi - hit->lo);
            // report the holder's GUEST pid (container init's host pid shows through as guest pid 1)
            *(int32_t *)(lf + 24) = (g_init_hostpid && hit->owner == g_init_hostpid) ? 1 : hit->owner;
        } else {
            *(short *)(lf + 0) = 2; // F_UNLCK -> Linux 2: the requested lock could be placed
        }
        *out = 0;
        poslk_unlock();
        return 1;
    }
    if (type == F_UNLCK) { // F_SETLK/W unlock: drop this process's locks over the range (with splitting)
        poslk_clear_own(dev, ino, me, lo, hi);
        *out = 0;
        poslk_unlock();
        return 1;
    }
    for (int i = 0; i < g_poslk->hi; i++) { // conflict scan vs OTHER owners
        struct poslk_rec *r = &g_poslk->rec[i];
        if (!r->owner || r->owner == me || r->dev != dev || r->ino != ino) continue;
        if (!poslk_conflict(r, lo, hi, type)) continue;
        if (!poslk_alive(r->owner)) {
            r->owner = 0;
            continue;
        }
        *out = -EAGAIN; // blocked (F_SETLK: EAGAIN; F_SETLKW: caller retries)
        poslk_unlock();
        return 1;
    }
    poslk_clear_own(dev, ino, me, lo, hi); // replace/upgrade/downgrade: drop own overlap, then insert
    struct poslk_rec *slot = poslk_slot();
    if (!slot) { // table full -> reclaim dead owners and retry once
        poslk_sweep_dead();
        slot = poslk_slot();
    }
    if (!slot) {
        *out = -ENOLCK;
        poslk_unlock();
        return 1;
    }
    slot->dev = dev;
    slot->ino = ino;
    slot->lo = lo;
    slot->hi = hi;
    slot->type = type;
    slot->owner = me;
    g_i_locked = 1; // this process now holds a record lock -> its regular-file closes must check for release
    *out = 0;
    poslk_unlock();
    return 1;
}

// close() hook: POSIX releases ALL a process's locks on a file when ANY fd for it is closed. Called from
// fd_reset_emul BEFORE the real close(). Fast path: the fd's (dev,ino) is cached (it took a lock op) -> no
// fstat. A process that never held any fcntl lock does nothing. Only the residual case (a never-locked fd
// aliasing a file this process HAS locked via another fd) needs the fstat -- and only for lock-holders.
static void poslk_on_close(int fd) {
    if (!g_poslk) return;
    int32_t me = poslk_mypid();
    if (fd >= 0 && fd < DD_NFD && g_lkval[fd]) { // fast: release by the cached identity, then forget the fd
        poslk_lock();
        poslk_clear_own(g_lkdev[fd], g_lkino[fd], me, 0, INT64_MAX);
        poslk_unlock();
        g_lkval[fd] = 0;
        return;
    }
    if (!g_i_locked) return; // never took a lock -> nothing of ours to release (the common close: no syscall)
    struct stat st;
    if (fstat(fd, &st) < 0 || !S_ISREG(st.st_mode)) return;
    poslk_lock();
    poslk_clear_own(st.st_dev, st.st_ino, me, 0, INT64_MAX);
    poslk_unlock();
}

// process-exit hook (exit_group): drop every lock this process holds. Belt-and-suspenders over the lazy
// dead-owner reclaim, keeping the shared table tidy for long-lived containers.
static void poslk_on_exit(void) {
    if (!g_poslk) return;
    int32_t me = poslk_mypid();
    poslk_lock();
    for (int i = 0; i < g_poslk->hi; i++)
        if (g_poslk->rec[i].owner == me) g_poslk->rec[i].owner = 0;
    poslk_unlock();
}

// Configurable fsync durability policy (S3DB_DURABILITY=none|fast|strict), read once and cached.
//   0 = fast   (DEFAULT, and when env unset): plain fsync() -- the macOS fast path, unchanged legacy
//               behavior. NOTE: a plain fsync() only reaches the drive's write cache on macOS; this is
//               survives-process-crash, NOT survives-host-power-loss. We deliberately do NOT map
//               fsync->F_FULLFSYNC by default: a single F_FULLFSYNC is ~3 ms (160x a plain fsync) and
//               collapses sqlite commit throughput ~35-100x. Default must stay `fast`.
//   1 = none   no-op barrier (return success without syncing) -- for ephemeral/CI containers, which are
//               page-cache-coherent for any reader but NOT host-crash-durable. ~2.9x sqlite insert tps.
//   2 = strict fcntl(fd, F_FULLFSYNC) for real on-platter durability (falls back to fsync on non-reg fds).
static int s3db_durability(void) {
    static int mode = -1;
    if (__builtin_expect(mode < 0, 0)) {
        const char *e = getenv("S3DB_DURABILITY");
        int m = 0; // default == fast == legacy
        if (e) {
            if (!strcmp(e, "none"))
                m = 1;
            else if (!strcmp(e, "strict"))
                m = 2;
            else
                m = 0; // "fast" or anything unrecognized -> fast (safe default)
        }
        mode = m;
    }
    return mode;
}

// Route fsync/fdatasync/sync_file_range (82/83/84) through the durability policy. Returns 0 on success
// or -errno (Linux ABI convention used by the caller's G_RET). `fast`/default is byte-identical to the
// legacy `fsync((int)fd) < 0 ? -errno : 0` path.
static uint64_t s3db_sync_fd(int fd) {
    switch (s3db_durability()) {
    case 1: return 0; // none: no-op barrier
    case 2:           // strict: real host-crash durability; fall back to fsync if F_FULLFSYNC unsupported
        if (fcntl(fd, F_FULLFSYNC, 0) == 0) return 0;
        // EINVAL/ENOTSUP on non-regular fds (pipes, sockets, etc.) -> plain fsync
        return fsync(fd) < 0 ? (uint64_t)(-errno) : 0;
    default: // 0 fast (== legacy default)
        return fsync(fd) < 0 ? (uint64_t)(-errno) : 0;
    }
}

// list a directory's entries (minus . / ..) as a newline-joined, NUL-terminated malloc'd string (for the
// inotify-on-a-directory diff). NULL on error.
static char *dir_snapshot(const char *path) {
    DIR *d = opendir(path);
    if (!d) return NULL;
    size_t cap = 256, len = 0;
    char *s = malloc(cap);
    if (!s) {
        closedir(d);
        return NULL;
    }
    s[0] = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
        size_t nl = strlen(e->d_name);
        if (len + nl + 2 > cap) {
            cap = (len + nl + 2) * 2;
            char *n = realloc(s, cap);
            if (!n) break;
            s = n;
        }
        memcpy(s + len, e->d_name, nl);
        len += nl;
        s[len++] = '\n';
        s[len] = 0;
    }
    closedir(d);
    return s;
}

// is `name` present as a line in the newline-joined snapshot?
static int snap_has(const char *snap, const char *name, size_t nl) {
    if (!snap) return 0;
    for (const char *p = snap; *p;) {
        const char *e = strchr(p, '\n');
        size_t l = e ? (size_t)(e - p) : strlen(p);
        if (l == nl && !memcmp(p, name, nl)) return 1;
        p = e ? e + 1 : p + l;
    }
    return 0;
}

// inotify rename events (lsys-inotify-moves): the snapshot diff only sees entry NAMES, so it cannot pair a
// rename into IN_MOVED_FROM/IN_MOVED_TO with a shared cookie. rename(2) calls inotify_notify_move(), which
// queues the paired events against every watch on the source / destination directory; inotify read() then
// drains the queue (inomv_drain) alongside the usual create/delete diff.
static struct {
    int wd;
    uint32_t mask, cookie;
    char name[256];
} g_inomv[64];

static int g_inomv_n;
static uint32_t g_inomv_cookie;

static void inomv_push(int wd, uint32_t mask, uint32_t cookie, const char *name) {
    if (g_inomv_n >= (int)(sizeof g_inomv / sizeof g_inomv[0])) return;
    g_inomv[g_inomv_n].wd = wd;
    g_inomv[g_inomv_n].mask = mask;
    g_inomv[g_inomv_n].cookie = cookie;
    snprintf(g_inomv[g_inomv_n].name, sizeof g_inomv[0].name, "%s", name);
    g_inomv_n++;
}

// Queue IN_MOVED_FROM(src basename) + IN_MOVED_TO(dst basename), sharing one cookie, against any inotify
// watch whose directory is the source / destination parent. host paths are resolved exactly as add_watch
// resolved g_inotify_wpath (atpath), so their parent-dir prefixes compare equal. No-op when nothing watches.
static void inotify_notify_move(int sdirfd, const char *spath, int ddirfd, const char *dpath) {
    if (!spath || !dpath) return;
    char sb[4300], db[4300];
    const char *sh = atpath(sdirfd, spath, sb, sizeof sb, 1);
    const char *dh = atpath(ddirfd, dpath, db, sizeof db, 1);
    if (!sh || !dh) return;
    uint32_t cookie = ++g_inomv_cookie;
    if (!cookie) cookie = ++g_inomv_cookie; // a rename cookie is always nonzero on Linux
    const char *full[2] = {sh, dh};
    uint32_t mask[2] = {0x00000040u, 0x00000080u}; // IN_MOVED_FROM / IN_MOVED_TO
    for (int side = 0; side < 2; side++) {
        const char *slash = strrchr(full[side], '/');
        if (!slash || slash == full[side]) continue;
        size_t dlen = (size_t)(slash - full[side]);
        const char *base = slash + 1;
        for (int wd = 0; wd < 1024; wd++) {
            if (!g_inotify_wpath[wd][0]) continue;
            if (strlen(g_inotify_wpath[wd]) == dlen && !strncmp(g_inotify_wpath[wd], full[side], dlen))
                inomv_push(wd, mask[side], cookie, base);
        }
    }
}

// Emit any queued rename events belonging to inotify instance `instfd` into the read() buffer; returns the
// bytes written and removes the drained entries.
static size_t inomv_drain(int instfd, uint8_t *out, size_t cap) {
    size_t off = 0;
    for (int i = 0; i < g_inomv_n;) {
        int wd = g_inomv[i].wd;
        if (wd < 0 || wd >= 1024 || g_inotify_owner[wd] != instfd) {
            i++;
            continue;
        }
        size_t l = strlen(g_inomv[i].name);
        size_t nlen = (l + 1 + 15) & ~(size_t)15; // padded name field
        if (off + 16 + nlen > cap) break;
        *(int32_t *)(out + off) = wd;
        *(uint32_t *)(out + off + 4) = g_inomv[i].mask;
        *(uint32_t *)(out + off + 8) = g_inomv[i].cookie;
        *(uint32_t *)(out + off + 12) = (uint32_t)nlen;
        memcpy(out + off + 16, g_inomv[i].name, l);
        memset(out + off + 16 + l, 0, nlen - l);
        off += 16 + nlen;
        g_inomv[i] = g_inomv[--g_inomv_n]; // remove drained entry (order among distinct events is unspecified)
    }
    return off;
}

// pipe read-pushback (tee(2)): consume up to `cap` bytes from fd's pushback into buf (removing them).
static size_t pipe_pushback_take(int fd, void *buf, size_t cap) {
    if (fd < 0 || fd >= DD_NFD || g_fd_pb_len[fd] == 0 || cap == 0) return 0;
    size_t k = g_fd_pb_len[fd] < cap ? g_fd_pb_len[fd] : cap;
    memcpy(buf, g_fd_pushback[fd], k);
    if (k < g_fd_pb_len[fd]) {
        memmove(g_fd_pushback[fd], g_fd_pushback[fd] + k, g_fd_pb_len[fd] - k);
        g_fd_pb_len[fd] -= k;
    } else {
        free(g_fd_pushback[fd]);
        g_fd_pushback[fd] = NULL;
        g_fd_pb_len[fd] = 0;
    }
    return k;
}

// Replace fd's pushback with `len` bytes of `data` (tee restores the source it peeked out of the pipe).
static void pipe_pushback_set(int fd, const void *data, size_t len) {
    if (fd < 0 || fd >= DD_NFD) return;
    free(g_fd_pushback[fd]);
    g_fd_pushback[fd] = NULL;
    g_fd_pb_len[fd] = 0;
    if (len == 0) return;
    g_fd_pushback[fd] = malloc(len);
    if (!g_fd_pushback[fd]) return;
    memcpy(g_fd_pushback[fd], data, len);
    g_fd_pb_len[fd] = len;
}

static char g_procname[16]; // prctl PR_SET_NAME / PR_GET_NAME (the 15-char process/thread name)

// getdents directory-stream cache (guest fd -> host DIR*). MUST be invalidated on close(), else a reused
// fd gets a stale DIR* already at EOF (a second opendir of the same path then reads nothing -- broke glob).
static struct {
    int fd;
    DIR *d;
} g_dirs[64];

static int g_ndirs;

static void dirs_drop(int fd) {
    for (int i = 0; i < g_ndirs; i++)
        if (g_dirs[i].fd == fd) {
            closedir(g_dirs[i].d);
            g_dirs[i] = g_dirs[--g_ndirs];
            return;
        }
}

// Parse a "#!" shebang line. `host_path` is the RESOLVED HOST path of a candidate program. If the file
// begins with "#!", fills `interp` (size ni) with the interpreter path and `arg` (size na) with the
// optional single argument (arg[0]==0 when there is none) and returns 1. Returns 0 when it is not a
// shebang script, -1 when the file can't be opened/read. Shared by execve (case 221) and the initial
// program loader (dd_run): both then rewrite argv to [interp, (arg), scriptpath, args...] and load the
// INTERPRETER instead of the script. load_elf has no ELF-magic/#! check, so the script bytes would
// otherwise be parsed as a bogus ELF and fault.
static int parse_shebang(const char *host_path, char *interp, size_t ni, char *arg, size_t na) {
    int fd = open(host_path, O_RDONLY);
    char hdr[258];
    ssize_t k = fd >= 0 ? read(fd, hdr, sizeof hdr - 1) : -1;
    if (fd >= 0) close(fd);
    if (k < 0) return -1;
    if (k <= 3 || hdr[0] != '#' || hdr[1] != '!') return 0;
    hdr[k] = 0;
    char *nl = strchr(hdr, '\n');
    if (nl) *nl = 0;
    char *s = hdr + 2;
    while (*s == ' ' || *s == '\t')
        // interpreter path
        s++;
    char *e = s;
    while (*e && *e != ' ' && *e != '\t')
        e++;
    char *a = NULL;
    if (*e) {
        *e = 0;
        a = e + 1;
        while (*a == ' ' || *a == '\t')
            a++;
        if (!*a) a = NULL;
    }
    snprintf(interp, ni, "%s", s);
    if (a)
        snprintf(arg, na, "%s", a);
    else
        arg[0] = 0;
    return 1;
}

// Max "#!" nesting Linux binfmt_script resolves before ELOOP (BINPRM_MAX_RECURSION == 4).
#define SHEBANG_MAX 4

// Resolve a possibly-NESTED "#!" shebang chain the way Linux binfmt_script does, recursing up to
// SHEBANG_MAX levels. On entry argv[0] is the guest program path and `host0` its already-resolved host
// path; `argv` is a NULL-terminated array of capacity `cap` (>= argc + SHEBANG_MAX*2 + 1). While argv[0]
// resolves to a shebang script "#!I [opt]", argv is rewritten IN PLACE, Linux-style, to [I, (opt), argv...]
// -- the old argv[0] stays on as the script-path argument -- and the loop repeats on I. This is what makes
// an interpreter that is ITSELF a "#!" script work: mysql:8's docker-entrypoint.sh is "#!/usr/bin/env bash",
// and /usr/bin/env is "#!/usr/bin/coreutils --coreutils-prog-shebang=env" (a script, not an ELF). The old
// single-level code followed only the first "#!", then tried to load /usr/bin/env's SCRIPT text as an ELF
// (e_machine garbage -> "wrong engine"). Synthesized I/opt strings are copied into `store` (caller-owned
// char[SHEBANG_MAX*2][256], stable for as long as argv is used). On return *phost -> the FINAL interpreter's
// host path (in `hostbuf`, size nh) which the caller loads as an ELF, and the NEW argc is returned. A
// non-shebang / unreadable argv[0] is the base case (argc unchanged, *phost = its host path). Returns -1 on
// ELOOP (chain deeper than SHEBANG_MAX) or when argv has no room -- the caller must then error out.
static int resolve_shebang_chain(char **argv, int argc, int cap, const char *host0, char store[][256], char *hostbuf,
                                 size_t nh, const char **phost) {
    char curhost[4200];
    snprintf(curhost, sizeof curhost, "%s", host0);
    int nstore = 0;
    for (int level = 0;; level++) {
        char interp[256], opt[256];
        if (parse_shebang(curhost, interp, sizeof interp, opt, sizeof opt) != 1) {
            // base case: argv[0] is an ELF (or unreadable -> caller's load_elf/access reports it)
            snprintf(hostbuf, nh, "%s", curhost);
            *phost = hostbuf;
            return argc;
        }
        if (level >= SHEBANG_MAX) return -1; // too many nested "#!" -> ELOOP
        int ins = opt[0] ? 2 : 1;            // interp, plus the optional single arg (Linux: everything after
        if (argc + ins >= cap) return -1;    // the first space is ONE arg)
        char *si = store[nstore++];
        snprintf(si, 256, "%s", interp);
        char *so = NULL;
        if (opt[0]) {
            so = store[nstore++];
            snprintf(so, 256, "%s", opt);
        }
        // shift argv (incl. its NULL terminator) right by `ins`, then prepend [I, (opt)]
        for (int i = argc; i >= 0; i--)
            argv[i + ins] = argv[i];
        argv[0] = si;
        if (so) argv[1] = so;
        argc += ins;
        // resolve the new interpreter's host path (overlay-aware) for the next round
        char nb[4200];
        snprintf(curhost, sizeof curhost, "%s", xresolve_overlay(si, nb, sizeof nb));
    }
}

// Anonymous PRIVATE mmap ranges (MAP_ANON|MAP_PRIVATE) tracked so that madvise(MADV_DONTNEED) can
// give real Linux semantics -- re-mmap fresh zero pages over the range -- WITHOUT ever disturbing a
// file-backed or shared mapping (re-mmapping those with MAP_ANON would discard file data / break
// sharing). DONTNEED only acts when the advised range is fully contained in a tracked private-anon
// region; otherwise it falls back to the safe advisory passthrough. Lock-free like g_gmap; a race can
// only forget an entry (-> safe no-op), and the containment check gates every destructive remap.
static struct {
    uint64_t addr, len;
    int prot;
} g_anonmap[2048];

static int g_nanonmap;

static void anon_track(uint64_t addr, uint64_t len, int prot) {
    if (!addr || g_nanonmap >= (int)(sizeof g_anonmap / sizeof g_anonmap[0])) return;
    g_anonmap[g_nanonmap].addr = addr;
    g_anonmap[g_nanonmap].len = len;
    g_anonmap[g_nanonmap].prot = prot;
    g_nanonmap++;
}

// Forget any tracked anon coverage overlapping [addr,addr+len) -- on munmap, or when a non-anon
// mapping is laid over the range. Err toward forgetting (whole-entry drop) so a stale entry can never
// cause a wrong anon-remap of what is now a file mapping.
static void anon_untrack(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_nanonmap;) {
        uint64_t a = g_anonmap[i].addr, e = a + g_anonmap[i].len;
        if (a < end && addr < e)
            g_anonmap[i] = g_anonmap[--g_nanonmap];
        else
            i++;
    }
}

// prot of the tracked private-anon region fully containing [addr,addr+len), else -1 (unknown -> do
// not remap). Full containment guarantees the range is anon, so the remap cannot corrupt a file map.
static int anon_prot_if_contained(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_nanonmap; i++)
        if (g_anonmap[i].addr <= addr && end <= g_anonmap[i].addr + g_anonmap[i].len) return g_anonmap[i].prot;
    return -1;
}

// MADV_WIPEONFORK(18) ranges: PRIVATE-ANON regions the guest asked to be presented ZERO-FILLED to a
// child after fork(2) (Linux 4.14+). Tracked here; fork_child_hooks() memsets each range to 0 in the
// child, so the child (and, since it inherits this registry across fork, any grandchild) sees zeros
// while the parent keeps its data. MADV_KEEPONFORK(19) undoes it by dropping the range. Only private-
// anon pages are valid (the madvise handler EINVALs anything else, exactly like Linux).
static struct {
    uint64_t addr, len;
} g_wipefork[256];

static int g_nwipefork;

static void wipefork_add(uint64_t addr, uint64_t len) {
    if (!addr || !len) return;
    for (int i = 0; i < g_nwipefork; i++)
        if (g_wipefork[i].addr == addr && g_wipefork[i].len == len) return; // already tracked
    if (g_nwipefork >= (int)(sizeof g_wipefork / sizeof g_wipefork[0])) return;
    g_wipefork[g_nwipefork].addr = addr;
    g_wipefork[g_nwipefork].len = len;
    g_nwipefork++;
}

static void wipefork_del(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_nwipefork;) {
        uint64_t a = g_wipefork[i].addr, e = a + g_wipefork[i].len;
        if (a < end && addr < e)
            g_wipefork[i] = g_wipefork[--g_nwipefork]; // overlaps the KEEPONFORK range -> forget it
        else
            i++;
    }
}

// Called in the fork CHILD (from fork_child_hooks): present each MADV_WIPEONFORK range as zero-filled.
static void wipefork_apply_child(void) {
    for (int i = 0; i < g_nwipefork; i++)
        memset((void *)g_wipefork[i].addr, 0, (size_t)g_wipefork[i].len);
}

// /proc/self/fd/N (and /proc/<pid>/fd/N for our own pid -- host pid, container pid, or init's "1")
// names an already-open fd. macOS has no /proc, so detect this form and recover the fd number; the
// caller then resolves it via F_GETPATH (readlinkat) or dup()/reopen (openat). Returns N>=0 on an
// EXACT /proc/.../fd/<N> match, else -1 (anything with a trailing component falls through to normal
// resolution). NOTE: <pid>/fd accepts only this process's pid; foreign pids are not introspectable.
static int procfd_num(const char *p) {
    if (!p) return -1;
    // /dev/fd/N is the standard Linux symlink into /proc/self/fd (bash process substitution opens
    // /dev/fd/63; postgres initdb reads the password from /dev/fd/63). macOS has no /dev, and the
    // on-disk /dev/fd -> /proc/self/fd symlink can't be followed into the (synthetic) /proc by the host
    // resolver, so recover the fd number here -- the caller reopens it by F_GETPATH/dup, exactly as it
    // does for /proc/self/fd/N. Numeric leaf only, so /dev/fd itself (the bare dir) falls through to the
    // on-disk symlink (readlink -> "/proc/self/fd"). The /dev/std{in,out,err} aliases are deliberately
    // NOT matched here: they must readlink to their symlink text ("/proc/self/fd/0") for `ls -l /dev`,
    // so their open() is handled at the openat call site instead (dev_std_fd), not via readlink.
    const char *rest = NULL;
    if (!strncmp(p, "/dev/fd/", 8))
        rest = p + 8;
    else if (!strncmp(p, "/proc/self/fd/", 14))
        rest = p + 14;
    else if (!strncmp(p, "/proc/", 6)) {
        const char *q = p + 6;
        int i = 0;
        char num[16];
        while (q[i] >= '0' && q[i] <= '9' && i < 15) {
            num[i] = q[i];
            i++;
        }
        if (i == 0 || strncmp(q + i, "/fd/", 4)) return -1;
        num[i] = 0;
        int pid = atoi(num);
        if (pid != (int)getpid() && pid != container_pid()) return -1;
        rest = q + i + 4;
    }
    if (!rest || rest[0] < '0' || rest[0] > '9') return -1;
    for (const char *s = rest; *s; s++)
        if (*s < '0' || *s > '9') return -1; // trailing path component -> not a bare fd link
    return atoi(rest);
}

// The /dev/std{in,out,err} aliases -> fd 0/1/2 for the OPEN path only (readlink keeps its on-disk
// symlink text, so `ls -l /dev` doesn't F_GETPATH a pipe fd and get EBADF). Returns the fd, else -1.
static int dev_std_fd(const char *p) {
    if (!p) return -1;
    if (!strcmp(p, "/dev/stdin")) return 0;
    if (!strcmp(p, "/dev/stdout")) return 1;
    if (!strcmp(p, "/dev/stderr")) return 2;
    return -1;
}

// ===== w3e EPOLL FAST PATH (gate: NOEPOLLOPT=1 reverts to the original per-ctl kevent path) =====
// The baseline emulates epoll over a per-epoll-fd kqueue, but issues a *separate* kevent() syscall
// for every epoll_ctl (and one per filter), then another for epoll_wait. For server event loops that
// rearm (EPOLLONESHOT) or toggle EPOLLOUT every request, that is one extra kevent round-trip per
// request. This path instead BUFFERS the epoll_ctl changes per epoll-fd and submits them as the
// *changelist* argument of the next epoll_wait kevent() — folding all pending registration changes
// into the single wait syscall (the classic libevent/libev kqueue batching). It also tracks the
// armed read/write filter per guest fd so EPOLL_CTL_MOD correctly removes a dropped filter (the
// baseline leaves a stale EVFILT_WRITE armed on a MOD from IN|OUT->IN), without emitting spurious
// EV_DELETEs that would error.  Tables are indexed by fd<DD_NFD (matches every other fd table here);
// epfd/fd >= DD_NFD fall back to the immediate path.
static int g_epopt = -1;              // -1 unknown, 0 off, 1 on
static struct kevent *g_ep_chg[DD_NFD]; // deferred changelist per epoll fd
static int g_ep_chgn[DD_NFD], g_ep_chgcap[DD_NFD];
static uint8_t g_ep_rd[DD_NFD], g_ep_wr[DD_NFD]; // per guest fd: read/write filter currently armed
static uint8_t g_ep_os[DD_NFD];                // per guest fd: EPOLLONESHOT requested (kernel auto-removes on fire)
static uint8_t g_epoll[DD_NFD]; // per fd: an epoll instance (backed by kqueue) -- rebuilt across fork (macOS kqueue() is
                              // not inherited)
// per fd: this epoll instance has a dup alias. A dup() shares the SAME kqueue, so both aliases must submit
// interest DIRECTLY to the kernel (the deferred per-epfd changelist would strand a change on one alias);
// forces epoll_ctl/epoll_wait onto the immediate path for a dup'd instance.
static uint8_t g_ep_dupd[DD_NFD];
static unsigned long long g_ep_kevent_calls; // PROF: kevent() syscalls issued by the epoll path
static int g_epprof = -1;

// ---- per-instance epoll interest table (fd -> owning instance + events + udata) --------------------
// Linux keeps an interest list per epoll instance and ties each registration to the underlying OPEN FILE
// DESCRIPTION, not the fd number. dd emulates epoll with a macOS kqueue whose knotes are keyed by fd
// number and vanish when that fd number closes -- so two OFD-semantics that dd previously got wrong:
//   (1) a fork child inherits the parent's interest list, but macOS does NOT inherit kqueue()s, so the
//       rebuilt child kqueue was EMPTY (a child that epoll_waits without re-registering saw nothing);
//   (2) closing a watched fd whose OFD stays alive via a dup should KEEP the registration (readiness
//       persists), but the kqueue knote died with the fd number.
// The table below records, per WATCHED fd, its owning epoll instance + the registered events + udata, so
// the fork child can re-arm every inherited registration on its rebuilt kqueue, and a close-with-surviving
// -dup can re-home the knote onto the surviving alias. Like the g_ep_rd/wr armed maps this is a SINGLE
// owner per watched fd (dd already assumes a fd is watched by at most one epoll instance); the writes are
// a couple of fd-indexed stores on the epoll_ctl path, so the hot path cost matches the existing armed map.
static int g_ep_owner[DD_NFD];    // watched fd -> owning epoll instance fd + 1 (0 = not watched)
static uint32_t g_ep_events[DD_NFD]; // watched fd -> the epoll events mask registered (EPOLLIN/OUT/ET/ONESHOT)
static uint64_t g_ep_udata[DD_NFD];  // watched fd -> the epoll_event.data registered for it

// ---- open-file-description identity for fd-number aliases (dup) -------------------------------------
// dd shares the host descriptor table with the guest, so two guest fds that refer to the same OFD are two
// host fds dup'd from each other. There is no portable "same OFD?" query, so track it ourselves: every
// dup(2)/dup2/dup3/F_DUPFD tags both fds with a shared group id (see fd_carry_virt). close() clears the id.
// Used by the epoll close path to find a surviving alias of a just-closed watched fd (finding: epoll
// readiness must persist while a dup keeps the OFD open).
static uint32_t g_ofd_id[DD_NFD]; // 0 = no known alias; else a group id shared by every dup of this OFD
static uint32_t g_ofd_next = 1;

// Assign (or propagate) a shared OFD group id from oldfd to newfd on dup.
static void ofd_link_dup(int newfd, int oldfd) {
    if (oldfd < 0 || oldfd >= DD_NFD || newfd < 0 || newfd >= DD_NFD || oldfd == newfd) return;
    if (!g_ofd_id[oldfd]) g_ofd_id[oldfd] = g_ofd_next++;
    g_ofd_id[newfd] = g_ofd_id[oldfd];
}

// Find an OPEN guest fd (other than `fd`) that shares fd's OFD group id, or -1 if none survives.
static int ofd_surviving_alias(int fd) {
    if (fd < 0 || fd >= DD_NFD || !g_ofd_id[fd]) return -1;
    uint32_t id = g_ofd_id[fd];
    for (int i = 0; i < DD_NFD; i++)
        if (i != fd && g_ofd_id[i] == id && fcntl(i, F_GETFD) != -1) return i;
    return -1;
}

static int epopt_on(void) {
    if (g_epopt < 0) {
        const char *e = getenv("NOEPOLLOPT");
        g_epopt = (e && e[0] == '1') ? 0 : 1;
    }
    return g_epopt;
}

static void ep_prof_dump(void) {
    if (g_epprof == 1) fprintf(stderr, "[ddepollprof] epoll_kevent_syscalls=%llu\n", g_ep_kevent_calls);
}

static void ep_count(void) {
    if (g_epprof < 0) {
        const char *e = getenv("DDEPOLLPROF");
        g_epprof = (e && e[0] == '1') ? 1 : 0;
        if (g_epprof) atexit(ep_prof_dump);
    }
    if (g_epprof == 1) g_ep_kevent_calls++;
}

// append a change to epfd's buffer, coalescing on (ident,filter) so repeated ctls collapse.
static void ep_push(int ep, uintptr_t ident, int16_t filt, uint16_t flags, void *udata) {
    if (ep < 0 || ep >= DD_NFD) return;
    struct kevent *a = g_ep_chg[ep];
    for (int i = 0; i < g_ep_chgn[ep]; i++)
        if (a[i].ident == ident && a[i].filter == filt) {
            EV_SET(&a[i], ident, filt, flags, 0, 0, udata);
            return;
        }
    if (g_ep_chgn[ep] >= g_ep_chgcap[ep]) {
        int nc = g_ep_chgcap[ep] ? g_ep_chgcap[ep] * 2 : 16;
        struct kevent *na = realloc(a, (size_t)nc * sizeof *na);
        if (!na) return;
        g_ep_chg[ep] = na;
        g_ep_chgcap[ep] = nc;
        a = na;
    }
    EV_SET(&a[g_ep_chgn[ep]++], ident, filt, flags, 0, 0, udata);
}

// reset epoll armed-state for a guest fd (called from close(): kqueue auto-removes a closed fd, so the
// armed map must follow to avoid a later stale EV_DELETE on a reused fd number).
static void ep_fd_reset(int fd) {
    if (fd < 0 || fd >= DD_NFD) return;
    g_ep_rd[fd] = g_ep_wr[fd] = g_ep_os[fd] = 0;
    if (g_ep_chg[fd]) {
        free(g_ep_chg[fd]);
        g_ep_chg[fd] = NULL;
    } // if fd was an epoll fd, drop its pending changelist
    g_ep_chgn[fd] = g_ep_chgcap[fd] = 0;
    // Closing an epoll INSTANCE removes every registration it held (Linux). Drop the interest ownership of
    // each fd it watched so a reused epoll fd number can't inherit stale registrations / mis-rehome later.
    if (g_epoll[fd]) {
        for (int w = 0; w < DD_NFD; w++)
            if (g_ep_owner[w] == fd + 1) {
                g_ep_owner[w] = 0;
                g_ep_events[w] = 0;
                g_ep_udata[w] = 0;
            }
    }
    g_epoll[fd] = 0;    // a reused fd number is no longer an epoll instance
    g_ep_dupd[fd] = 0;  // ...nor a dup alias of one
    // drop this fd's own interest-table entry + OFD group id (any surviving-dup re-home already ran in
    // ep_close_rehome). A reused fd number must start with no owner/events/udata and no stale alias link.
    g_ep_owner[fd] = 0;
    g_ep_events[fd] = 0;
    g_ep_udata[fd] = 0;
    g_ofd_id[fd] = 0;
}

// close() hook for the inotify family (event.c cases 26/27/28). dd emulates inotify with a kqueue: the
// INSTANCE fd carries g_inotify[fd]=1, while each WATCH descriptor (wd) is itself a host O_EVTONLY fd with
// g_inotify_wpath/_snap/_owner keyed by that wd NUMBER. Both roles live in the shared fd table, so on close
// a reused fd number must shed whichever role it held or a later read()/epoll_wait/inotify read misroutes to
// a dead watch. fd_reset_emul used to clear only g_inotify_owner[fd], leaving g_inotify[fd] stamped --
// a recycled inotify-instance number then still routed read() into the inotify drain (io.c) against a stale
// kqueue. Called from fd_reset_emul BEFORE the real close(); idempotent and a no-op on a plain fd.
static void inotify_fd_reset(int fd) {
    if (fd < 0 || fd >= DD_NFD) return;
    if (g_inotify[fd]) {                  // this fd is an inotify INSTANCE -> Linux drops every watch it owned
        for (int i = 0; i < g_inomv_n;) { // discard the instance's pending move-queue entries (keyed by owner)
            int w = g_inomv[i].wd;
            if (w >= 0 && w < 1024 && g_inotify_owner[w] == fd)
                g_inomv[i] = g_inomv[--g_inomv_n];
            else
                i++;
        }
        for (int w = 0; w < 1024; w++) { // tear down every directory-watch this instance owned
            if (g_inotify_owner[w] == fd && (g_inotify_wpath[w][0] || g_inotify_snap[w])) {
                free(g_inotify_snap[w]);
                g_inotify_snap[w] = NULL;
                g_inotify_wpath[w][0] = 0;
                g_inotify_owner[w] = 0;
                close(w); // the watch's own host O_EVTONLY fd; kqueue auto-removes its EVFILT_VNODE registration
            }
        }
        g_inotify[fd] = 0;
    }
    // this fd may itself be a WATCH descriptor (freed via inotify_rm_watch or a stray close) -> drop its
    // cached snapshot/path/owner so a reused number can't inherit a stale directory diff.
    if (g_inotify_wpath[fd][0] || g_inotify_snap[fd]) {
        free(g_inotify_snap[fd]);
        g_inotify_snap[fd] = NULL;
        g_inotify_wpath[fd][0] = 0;
    }
    g_inotify_owner[fd] = 0;
}

// Shared boundary errno translation for every svc_<family>() module tail. Each family early-returns from
// service_local (before its trailing m2l_errno), so each must map G_RET's host(macOS) errno to the Linux
// errno the guest expects (e.g. macOS EAGAIN=35 = Linux EDEADLK). Skip when c->redirect is set: a redirect
// (execve / sigreturn) leaves an already-Linux value in G_RET that must not be re-translated -- a no-op for
// the families that never set redirect, so collapsing all tails onto this one helper is byte-identical.
// Returns 1 so a handler can `return svc_done(c);`.
static int svc_done(struct cpu *c) {
    if (!c->redirect) {
        int64_t rv = (int64_t)G_RET(c);
        if (rv < 0 && rv >= -4095) G_RET(c) = (uint64_t)(-(int64_t)m2l_errno((int)(-rv)));
    }
    return 1;
}
