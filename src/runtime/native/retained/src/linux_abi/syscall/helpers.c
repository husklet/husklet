// Extracted service() helper block: file-scope globals + static helper functions used by service().
// Not standalone -- #included by ../service.c after its system headers and before static void service().
// ---- guest RLIMIT_NOFILE enforcement -------------------------------------------------------------
// hl shares the host descriptor table, whose real cap is far larger than the guest's (engine-private fds
// are hoisted above 1<<20, see engine_fd_hoist), so the guest's soft RLIMIT_NOFILE is purely EMULATED
// (svc_fill_rlimit reports 20480, or the docker --ulimit / guest setrlimit override in g_limits). The
// host therefore never enforces it: a raw dup/open/dup2 at or past the guest cap wrongly succeeds. These
// helpers restore the Linux contract -- Linux allocates the lowest free fd and fails EMFILE if it would be
// >= the soft limit; an explicit dup2/dup3 newfd >= the limit is EBADF. (LTP dup03/dup201.)
static int guest_nofile_cur(void) {
    uint64_t cur = 20480; // hl default soft RLIMIT_NOFILE (mirror svc_fill_rlimit, defined later in proc.c)
    hl_limit_table_get(&g_limits, 7, &cur, NULL); // docker --ulimit / guest setrlimit(RLIMIT_NOFILE)
    return cur > 0x7fffffff ? 0x7fffffff : (int)cur;
}

// Guest soft RLIMIT_FSIZE (max file size, bytes). ~0 (RLIM_INFINITY) means no limit -- the common case,
// where the write path pays nothing. Set by the guest via setrlimit/prlimit64 (stored in g_limits, res 1).
static uint64_t guest_fsize_cur(void) {
    uint64_t cur = ~UINT64_C(0);
    hl_limit_table_get(&g_limits, 1, &cur, NULL); // RLIMIT_FSIZE
    return cur;
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
// target TU #includes BEFORE this file. hl force-maps guest anon memory host-RW (case 222) so its no-op
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
// LOCK_NB->F_SETLK (else F_SETLKW), LOCK_UN->F_UNLCK. Per-fd state (fd<HL_NFD) drives release-on-close so a
// held flock is dropped when its last fd closes, matching flock's "released on last close" semantics.
#include <sys/file.h> // flock(2) prototype (LOCK_* constants + the host flock() delegate on Linux)
#define FLOCK_DIR "/tmp/.hl-flock"
static uint8_t g_flock_type[HL_NFD]; // per guest fd: 0 none, else LOCK_SH / LOCK_EX currently held via companion

static struct {
    dev_t dev;
    ino_t ino;
    int fd;   // host fd of the companion lock file (kept open for the process lifetime)
    int refs; // guest fds in THIS process currently holding a flock on this file (for release-on-close)
} g_flkcomp[256];

static int g_nflkcomp;
static int flock_broker_apply(const hl_linux_fd_snapshot *source, uint64_t device, uint64_t object, int operation);

// Companion index for the file underlying guest `fd` (opening/caching it on first use). -1 (errno set) on
// failure. The companion is pushed to a high descriptor so it never collides with the guest's low fds
// (mirrors engine_fd_reloc); CLOEXEC keeps it out of any real host exec while still inheriting across fork.
static int flock_companion_identity(uint64_t device, uint64_t object) {
    for (int i = 0; i < g_nflkcomp; i++)
        if ((uint64_t)g_flkcomp[i].dev == device && (uint64_t)g_flkcomp[i].ino == object) return i;
    if (g_nflkcomp >= (int)(sizeof g_flkcomp / sizeof g_flkcomp[0])) {
        errno = ENOLCK;
        return -1;
    }
    hl_compat_mkdir(FLOCK_DIR, 0777);
    char p[80];
    snprintf(p, sizeof p, FLOCK_DIR "/%llx.%llx", (unsigned long long)device, (unsigned long long)object);
    int c = open(p, O_RDWR | O_CREAT | O_CLOEXEC, 0666);
    if (c < 0) return -1;
    int hi = fcntl(c, F_DUPFD_CLOEXEC, 1 << 20);
    if (hi < 0) hi = fcntl(c, F_DUPFD_CLOEXEC, 64);
    if (hi >= 0) {
        close(c);
        c = hi;
    }
    g_flkcomp[g_nflkcomp].dev = (dev_t)device;
    g_flkcomp[g_nflkcomp].ino = (ino_t)object;
    g_flkcomp[g_nflkcomp].fd = c;
    g_flkcomp[g_nflkcomp].refs = 0;
    return g_nflkcomp++;
}

static int flock_companion(int fd) {
    struct stat st;
    if (fstat(fd, &st) < 0) return -1;
    return flock_companion_identity((uint64_t)st.st_dev, (uint64_t)st.st_ino);
}

// flock(2): whole-file advisory lock delegated to the companion. Returns 0 or -1 (host errno set); the
// caller applies the normal macOS->Linux errno translation.
static int hl_flock(int fd, int op) {
#if defined(__linux__)
    // On a Linux host, flock(2) on the guest's real descriptor already carries exact flock semantics: the lock
    // is owned by the OPEN FILE DESCRIPTION, so two separate open()s of one file contend even inside a single
    // process while dup/fork siblings share it, and it is independent of the guest's fcntl POSIX record locks
    // (which this engine services in-engine and never places on the host fd). The companion-fd emulation below
    // exists only for a non-Linux (macOS) host, where flock and fcntl share one per-vnode lock list; it routes
    // both descriptors to a single process-local fcntl lock and so cannot observe an intra-process cross-fd
    // flock conflict. Delegate straight to the host on Linux, where the kernel enforces the correct model.
    return flock(fd, op);
#else
    int idx = flock_companion(fd);
    if (idx < 0) return -1;
    int comp = g_flkcomp[idx].fd, base = op & ~LOCK_NB;
    struct flock fl = {.l_whence = SEEK_SET, .l_start = 0, .l_len = 0}; // whole file
    if (base == LOCK_UN) {
        fl.l_type = F_UNLCK;
        int r = fcntl(comp, F_SETLK, &fl);
        if (r == 0 && fd >= 0 && fd < HL_NFD && g_flock_type[fd]) {
            g_flock_type[fd] = 0;
            if (--g_flkcomp[idx].refs < 0) g_flkcomp[idx].refs = 0;
        }
        return r;
    }
    fl.l_type = base == LOCK_SH ? F_RDLCK : F_WRLCK;
    int r = fcntl(comp, (op & LOCK_NB) ? F_SETLK : F_SETLKW, &fl);
    if (r == 0 && fd >= 0 && fd < HL_NFD) {
        if (!g_flock_type[fd]) g_flkcomp[idx].refs++;
        g_flock_type[fd] = (uint8_t)base;
    }
    return r;
#endif
}

// Take/drop the whole-file advisory lock on the COMPANION fd (a distinct vnode keyed by the target's
// dev/ino). On Linux we use flock(2) on the companion, NOT fcntl POSIX record locks: flock ownership is
// scoped to the OPEN FILE DESCRIPTION, so a fork child inheriting the (CLOEXEC) companion fd SHARES the
// parent's lock exactly as the real flock semantics require -- whereas a per-process fcntl record lock is
// NOT inherited across fork and would make the child's flock through an inherited descriptor spuriously
// conflict with the still-holding parent (fork-shares). Independent open()s of the target still contend
// because each process caches its OWN companion fd (a distinct OFD). On a non-Linux (macOS) host flock and
// fcntl share one per-vnode lock list, so we keep the fcntl-on-companion emulation there.
static int flock_companion_set(int comp, int base, int nonblock) {
#if defined(__linux__)
    return flock(comp, base == LOCK_UN ? LOCK_UN : base | (nonblock ? LOCK_NB : 0));
#else
    struct flock fl = {.l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    fl.l_type = base == LOCK_UN ? F_UNLCK : base == LOCK_SH ? F_RDLCK : F_WRLCK;
    return fcntl(comp, base == LOCK_UN || nonblock ? F_SETLK : F_SETLKW, &fl);
#endif
}

static int hl_flock_identity(const hl_linux_fd_snapshot *source, uint64_t device, uint64_t object, int op) {
    if (flock_broker_apply(source, device, object, op) != 0) return -1;
    int fd = (int)source->fd;
    int idx = flock_companion_identity(device, object);
    if (idx < 0) return -1;
    int comp = g_flkcomp[idx].fd, base = op & ~LOCK_NB;
    if (base != LOCK_SH && base != LOCK_EX && base != LOCK_UN) {
        errno = EINVAL;
        return -1;
    }
    int r = flock_companion_set(comp, base, op & LOCK_NB);
    if (r == 0 && fd >= 0 && fd < HL_NFD) {
        if (base == LOCK_UN) {
            if (g_flock_type[fd] && --g_flkcomp[idx].refs < 0) g_flkcomp[idx].refs = 0;
            g_flock_type[fd] = 0;
        } else {
            if (!g_flock_type[fd]) g_flkcomp[idx].refs++;
            g_flock_type[fd] = (uint8_t)base;
        }
    }
    return r;
}

static void flock_on_close_identity(int fd, uint64_t device, uint64_t object) {
    if (fd < 0 || fd >= HL_NFD || !g_flock_type[fd]) return;
    g_flock_type[fd] = 0;
    int idx = flock_companion_identity(device, object);
    if (idx < 0) return;
    if (--g_flkcomp[idx].refs <= 0) {
        g_flkcomp[idx].refs = 0;
        (void)flock_companion_set(g_flkcomp[idx].fd, LOCK_UN, 1);
    }
}

// close() hook: drop the flock this fd contributed; release the companion lock once its last holder in
// this process is gone (flock is released on the last close of the file).
static void flock_on_close(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_flock_type[fd]) return;
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
// table in thread.c. hl's guest execve reloads the image IN-PROCESS, so the region (and a process's locks)
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
#define FLOCK_BROKER_MAX 512
#define FLOCK_HOLDERS_MAX 32

struct flock_broker_record {
    uint64_t device, object, token;
    int32_t holders[FLOCK_HOLDERS_MAX];
    uint8_t mode;
    uint8_t active;
};

struct poslk_rec {
    uint64_t device;
    uint64_t object;
    int64_t lo, hi; // absolute byte range [lo,hi); hi==INT64_MAX => to-EOF (Linux l_len 0)
    int32_t type;   // F_RDLCK / F_WRLCK (F_UNLCK is never stored)
    int32_t owner;  // owning process = host pid (0 == free slot)
};

struct poslk_shm {
    _Atomic int32_t lockword; // 0 = free, else host pid of the holder (spinlock; stolen from a dead holder)
    int32_t hi;               // high-water slot count (bounds the linear scan)
    struct poslk_rec rec[POSLK_MAX];
    _Atomic uint64_t flock_generation;
    struct flock_broker_record flock[FLOCK_BROKER_MAX];
};
static struct poslk_shm *g_poslk;
// PERF: the hot path must issue ZERO host syscalls (the point of). Two process-local caches make that
// so: (a) g_lk{dev,ino,val}[fd] memoises a guest fd's host (dev,ino) so a repeated F_SETLK on the same DB fd
// never re-fstat()s (cleared on close in fd_reset_emul); (b) g_mypid caches getpid(). Both are per-process
// (NOT in the shared region); fork inherits a valid COW copy (same fds/files) and the clone child resets
// g_mypid/g_i_locked via poslk_after_fork.
static dev_t g_lkdev[HL_NFD];
static ino_t g_lkino[HL_NFD];
static uint8_t g_lkval[HL_NFD]; // 1 == g_lk{dev,ino}[fd] valid
static int32_t g_mypid;         // cached getpid() (0 = not cached yet)
static int g_i_locked;          // this process has held >=1 fcntl record lock (gates the close-time fstat)

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

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void poslk_ctor(void) {
    poslk_init();
}
#endif

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
            if (owner && !poslk_alive(owner)) { // holder crashed mid-update -> steal the spinlock
                if (atomic_compare_exchange_strong_explicit(&g_poslk->lockword, &owner, me, memory_order_acquire,
                                                            memory_order_relaxed))
                    return;
            }
            sched_yield();
        }
    }
}

static void poslk_unlock(void) {
    atomic_store_explicit(&g_poslk->lockword, 0, memory_order_release);
}

static int flock_holder_find(struct flock_broker_record *record, int32_t pid) {
    for (int index = 0; index < FLOCK_HOLDERS_MAX; ++index)
        if (record->holders[index] == pid) return index;
    return -1;
}

static int flock_holder_add(struct flock_broker_record *record, int32_t pid) {
    if (flock_holder_find(record, pid) >= 0) return 0;
    for (int index = 0; index < FLOCK_HOLDERS_MAX; ++index)
        if (record->holders[index] == 0) {
            record->holders[index] = pid;
            return 0;
        }
    errno = ENOLCK;
    return -1;
}

static int flock_has_holders(const struct flock_broker_record *record) {
    for (int index = 0; index < FLOCK_HOLDERS_MAX; ++index)
        if (record->holders[index] != 0) return 1;
    return 0;
}

static int flock_broker_apply(const hl_linux_fd_snapshot *source, uint64_t device, uint64_t object, int operation) {
    if (g_poslk == NULL || source->flock_token == 0) {
        errno = ENOLCK;
        return -1;
    }
    int base = operation & ~LOCK_NB;
    if (base != LOCK_SH && base != LOCK_EX && base != LOCK_UN) {
        errno = EINVAL;
        return -1;
    }
    for (;;) {
        struct flock_broker_record *own = NULL, *free_record = NULL;
        int conflict = 0;
        uint64_t before = atomic_load_explicit(&g_poslk->flock_generation, memory_order_acquire);
        poslk_lock();
        for (int index = 0; index < FLOCK_BROKER_MAX; ++index) {
            struct flock_broker_record *record = &g_poslk->flock[index];
            if (!record->active) {
                if (free_record == NULL) free_record = record;
                continue;
            }
            for (int holder = 0; holder < FLOCK_HOLDERS_MAX; ++holder)
                if (record->holders[holder] > 0 && kill(record->holders[holder], 0) < 0 && errno == ESRCH)
                    record->holders[holder] = 0;
            if (!flock_has_holders(record)) {
                memset(record, 0, sizeof(*record));
                if (free_record == NULL) free_record = record;
                continue;
            }
            if (record->token == source->flock_token) {
                own = record;
                continue;
            }
            if (record->device == device && record->object == object && record->mode != 0 && base != LOCK_UN &&
                (base == LOCK_EX || record->mode == LOCK_EX))
                conflict = 1;
        }
        if (!conflict && base == LOCK_UN) {
            if (own != NULL) own->mode = 0;
        } else if (!conflict) {
            if (own == NULL) {
                if (free_record == NULL) {
                    poslk_unlock();
                    errno = ENOLCK;
                    return -1;
                }
                own = free_record;
                memset(own, 0, sizeof(*own));
                own->device = device;
                own->object = object;
                own->token = source->flock_token;
                own->active = 1;
            }
            if (flock_holder_add(own, getpid()) != 0) {
                poslk_unlock();
                return -1;
            }
            own->mode = (uint8_t)base;
        }
        if (!conflict) atomic_fetch_add_explicit(&g_poslk->flock_generation, 1, memory_order_release);
        poslk_unlock();
        if (!conflict) return 0;
        if (operation & LOCK_NB) {
            errno = EWOULDBLOCK;
            return -1;
        }
        do {
            struct timespec pause = {0, 1000000};
            nanosleep(&pause, NULL);
        } while (atomic_load_explicit(&g_poslk->flock_generation, memory_order_acquire) == before);
    }
}

static void flock_broker_detach(const hl_linux_fd_snapshot *source) {
    if (g_poslk == NULL || source->flock_token == 0 || source->descriptor_references != 1) return;
    poslk_lock();
    for (int index = 0; index < FLOCK_BROKER_MAX; ++index) {
        struct flock_broker_record *record = &g_poslk->flock[index];
        if (!record->active || record->token != source->flock_token) continue;
        int holder = flock_holder_find(record, getpid());
        if (holder >= 0) record->holders[holder] = 0;
        if (!flock_has_holders(record)) memset(record, 0, sizeof(*record));
        atomic_fetch_add_explicit(&g_poslk->flock_generation, 1, memory_order_release);
        break;
    }
    poslk_unlock();
}

static void flock_broker_after_fork(void) {
    if (g_poslk == NULL || g_linux_box == NULL) return;
    poslk_lock();
    // The per-fd scan only ever calls flock_holder_add on ACTIVE broker records: with no active record
    // anywhere in the container it is a pure no-op. Skip the full fd_capacity walk (an inherited flock is
    // rare, the table is large) after a cheap FLOCK_BROKER_MAX (512) probe; the generation bump is
    // unconditional exactly as before so any observer's re-read semantics are unchanged.
    int any_active = 0;
    for (int index = 0; index < FLOCK_BROKER_MAX; ++index)
        if (g_poslk->flock[index].active) {
            any_active = 1;
            break;
        }
    if (any_active)
        for (uint32_t fd = 0; fd < g_linux_box->fd_capacity; ++fd) {
            hl_linux_fd_snapshot source;
            if (hl_linux_fd_snapshot_get(g_linux_box, fd, &source) != HL_STATUS_OK || source.flock_token == 0) continue;
            for (int index = 0; index < FLOCK_BROKER_MAX; ++index) {
                struct flock_broker_record *record = &g_poslk->flock[index];
                if (record->active && record->token == source.flock_token) (void)flock_holder_add(record, getpid());
            }
        }
    atomic_fetch_add_explicit(&g_poslk->flock_generation, 1, memory_order_release);
    poslk_unlock();
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
static void poslk_clear_own(uint64_t device, uint64_t object, int32_t own, int64_t lo, int64_t hi) {
    int n = g_poslk->hi;
    for (int i = 0; i < n; i++) {
        struct poslk_rec *r = &g_poslk->rec[i];
        if (r->owner != own || r->device != device || r->object != object) continue;
        if (r->hi <= lo || hi <= r->lo) continue; // disjoint -> keep
        int64_t olo = r->lo, ohi = r->hi;
        int32_t ot = r->type;
        r->owner = 0; // drop the straddling original; re-add surviving fragments below
        if (olo < lo) {
            struct poslk_rec *f = poslk_slot();
            if (f) {
                f->device = device;
                f->object = object;
                f->lo = olo;
                f->hi = lo;
                f->type = ot;
                f->owner = own;
            }
        }
        if (ohi > hi) {
            struct poslk_rec *f = poslk_slot();
            if (f) {
                f->device = device;
                f->object = object;
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
    int cached = fd >= 0 && fd < HL_NFD && g_lkval[fd];
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
        if (fd >= 0 && fd < HL_NFD) {
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
static int poslk_apply(uint64_t device, uint64_t object, int lcmd, uint8_t *lf, int64_t lo, int64_t hi, int type,
                       int *out) {
    int32_t me = poslk_mypid();
    poslk_lock();
    if (lcmd == 5) { // F_GETLK: report the first conflicting lock held by ANOTHER process, else F_UNLCK
        struct poslk_rec *hit = NULL;
        for (int i = 0; i < g_poslk->hi; i++) {
            struct poslk_rec *r = &g_poslk->rec[i];
            if (!r->owner || r->owner == me || r->device != device || r->object != object) continue;
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
        poslk_clear_own(device, object, me, lo, hi);
        *out = 0;
        poslk_unlock();
        return 1;
    }
    for (int i = 0; i < g_poslk->hi; i++) { // conflict scan vs OTHER owners
        struct poslk_rec *r = &g_poslk->rec[i];
        if (!r->owner || r->owner == me || r->device != device || r->object != object) continue;
        if (!poslk_conflict(r, lo, hi, type)) continue;
        if (!poslk_alive(r->owner)) {
            r->owner = 0;
            continue;
        }
        *out = -EAGAIN; // blocked (F_SETLK: EAGAIN; F_SETLKW: caller retries)
        poslk_unlock();
        return 1;
    }
    poslk_clear_own(device, object, me, lo, hi); // replace/upgrade/downgrade: drop own overlap, then insert
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
    slot->device = device;
    slot->object = object;
    slot->lo = lo;
    slot->hi = hi;
    slot->type = type;
    slot->owner = me;
    g_i_locked = 1; // this process now holds a record lock -> its regular-file closes must check for release
    *out = 0;
    poslk_unlock();
    return 1;
}

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
    return poslk_apply((uint64_t)dev, (uint64_t)ino, lcmd, lf, lo, hi, type, out);
}

/* Host-neutral typed-file entry: the caller resolves stable identity, current
   offset, and size through host services, never by treating an opaque handle
   as a native descriptor. */
static int poslk_op_identity(uint64_t device, uint64_t object, int64_t current, uint64_t size, int lcmd, uint8_t *lf,
                             int *out) {
    short linux_type = *(const short *)(lf + 0);
    short whence = *(const short *)(lf + 2);
    int64_t start = *(const int64_t *)(lf + 8);
    int64_t length = *(const int64_t *)(lf + 16);
    int type = linux_type == 0 ? F_RDLCK : linux_type == 1 ? F_WRLCK : F_UNLCK;
    int64_t base;
    if (!g_poslk) {
        *out = -ENOLCK;
        return 1;
    }
    if (linux_type < 0 || linux_type > 2) {
        *out = -EINVAL;
        return 1;
    }
    if (whence == SEEK_SET)
        base = 0;
    else if (whence == SEEK_CUR)
        base = current;
    else if (whence == SEEK_END) {
        if (size > INT64_MAX) {
            *out = -EOVERFLOW;
            return 1;
        }
        base = (int64_t)size;
    } else {
        *out = -EINVAL;
        return 1;
    }
    int64_t begin;
    if (__builtin_add_overflow(base, start, &begin) || begin < 0) {
        *out = -EINVAL;
        return 1;
    }
    int64_t end;
    if (length == 0)
        end = INT64_MAX;
    else if (length > 0) {
        if (__builtin_add_overflow(begin, length, &end)) {
            *out = -EOVERFLOW;
            return 1;
        }
    } else {
        end = begin;
        if (__builtin_add_overflow(begin, length, &begin) || begin < 0) {
            *out = -EINVAL;
            return 1;
        }
    }
    return poslk_apply(device, object, lcmd, lf, begin, end, type, out);
}

static void poslk_on_close_identity(uint64_t device, uint64_t object) {
    if (!g_poslk || !g_i_locked) return;
    poslk_lock();
    poslk_clear_own(device, object, poslk_mypid(), 0, INT64_MAX);
    poslk_unlock();
}

// close() hook: POSIX releases ALL a process's locks on a file when ANY fd for it is closed. Called from
// fd_reset_emul BEFORE the real close(). Fast path: the fd's (dev,ino) is cached (it took a lock op) -> no
// fstat. A process that never held any fcntl lock does nothing. Only the residual case (a never-locked fd
// aliasing a file this process HAS locked via another fd) needs the fstat -- and only for lock-holders.
static void poslk_on_close(int fd) {
    if (!g_poslk) return;
    int32_t me = poslk_mypid();
    if (fd >= 0 && fd < HL_NFD && g_lkval[fd]) { // fast: release by the cached identity, then forget the fd
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
// Route fsync/fdatasync/sync_file_range (82/83/84) through the durability policy. Returns 0 on success
// or -errno (Linux ABI convention used by the caller's G_RET). `fast`/default is byte-identical to the
// legacy `fsync((int)fd) < 0 ? -errno : 0` path.
static uint64_t s3db_sync_fd(int fd) {
    return fsync(fd) < 0 ? (uint64_t)(-errno) : 0;
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
    if (fd < 0 || fd >= HL_NFD || g_fd_pb_len[fd] == 0 || cap == 0) return 0;
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
    if (fd < 0 || fd >= HL_NFD) return;
    free(g_fd_pushback[fd]);
    g_fd_pushback[fd] = NULL;
    g_fd_pb_len[fd] = 0;
    if (len == 0) return;
    g_fd_pushback[fd] = malloc(len);
    if (!g_fd_pushback[fd]) return;
    memcpy(g_fd_pushback[fd], data, len);
    g_fd_pb_len[fd] = len;
}

// prctl PR_SET_NAME / PR_GET_NAME name (15 chars). PER-THREAD on Linux: every task has its own comm, and
// glibc's pthread_setname_np/getname_np short-circuit to prctl for the calling thread. A single shared slot
// made concurrent workers overwrite each other's name. Empty means "never set on this thread" -> readers
// fall back to the process comm (proc_comm), which is what a fresh task inherits.
static __thread char g_procname[16];

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
// program loader: both then rewrite argv to [interp, (arg), scriptpath, args...] and load the
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
// region; otherwise it falls back to the safe advisory passthrough. Capacity grows with the mapping
// population like g_gmap so ordinary valid mappings never silently lose their Linux policy metadata.
struct anon_mapping {
    uint64_t addr, len;
    int prot;
};

static struct anon_mapping *g_anonmap;
static int g_nanonmap;
static int g_anonmap_capacity;

// The registry grows/reallocs and is mutated by mmap/mprotect/munmap/madvise from every guest thread
// concurrently, so a lock is mandatory: a lock-free reader can see a torn entry, and a realloc racing a
// reader can free the array out from under it (use-after-free). One mutex serializes every g_anonmap
// access. It is a fork-unsafe mutex (a peer thread can hold it at fork) -- fork_child_hooks() calls
// anon_after_fork() to re-init it in the child, mirroring eventfd_after_fork/sysv_after_fork.
static pthread_mutex_t g_anonmap_lock = PTHREAD_MUTEX_INITIALIZER;

static void anon_lock(void) {
    pthread_mutex_lock(&g_anonmap_lock);
}

static void anon_unlock(void) {
    pthread_mutex_unlock(&g_anonmap_lock);
}

// Called in the fork CHILD (from fork_child_hooks): a peer thread may have held g_anonmap_lock at the
// instant of fork, leaving the inherited copy permanently locked. Re-init to a fresh unlocked mutex.
static void anon_after_fork(void) {
    pthread_mutex_init(&g_anonmap_lock, NULL);
}

static void anon_reserve_one(void) {
    if (g_nanonmap < g_anonmap_capacity) return;
    int capacity = g_anonmap_capacity ? g_anonmap_capacity * 2 : 256;
    if (capacity < g_anonmap_capacity || (size_t)capacity > SIZE_MAX / sizeof(*g_anonmap)) abort();
    struct anon_mapping *mappings = realloc(g_anonmap, (size_t)capacity * sizeof(*mappings));
    if (!mappings) abort();
    g_anonmap = mappings;
    g_anonmap_capacity = capacity;
}

// Core append; caller MUST hold g_anonmap_lock (used both by the public wrapper and by
// anon_update_prot_locked, which appends head/tail remainders while already holding the lock).
static void anon_track_locked(uint64_t addr, uint64_t len, int prot) {
    if (!addr) return;
    anon_reserve_one();
    g_anonmap[g_nanonmap].addr = addr;
    g_anonmap[g_nanonmap].len = len;
    g_anonmap[g_nanonmap].prot = prot;
    g_nanonmap++;
}

static void anon_track(uint64_t addr, uint64_t len, int prot) {
    anon_lock();
    anon_track_locked(addr, len, prot);
    anon_unlock();
}

// Forget any tracked anon coverage overlapping [addr,addr+len) -- on munmap, or when a non-anon
// mapping is laid over the range. Err toward forgetting (whole-entry drop) so a stale entry can never
// cause a wrong anon-remap of what is now a file mapping. Caller MUST hold g_anonmap_lock.
static void anon_untrack_locked(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_nanonmap;) {
        uint64_t a = g_anonmap[i].addr, e = a + g_anonmap[i].len;
        if (a < end && addr < e)
            g_anonmap[i] = g_anonmap[--g_nanonmap];
        else
            i++;
    }
}

static void anon_untrack(uint64_t addr, uint64_t len) {
    anon_lock();
    anon_untrack_locked(addr, len);
    anon_unlock();
}

// A successful guest mprotect changes the CURRENT protection of tracked private-anon pages, and a
// later MADV_DONTNEED must re-establish the range with THAT protection -- re-mapping with the stale
// mmap-time prot (PROT_NONE for a reserve-then-commit arena, the mozjs/V8 GC-chunk pattern) turns the
// committed pages back into an inaccessible reservation and the guest's next store faults. Split any
// tracked record around [addr,addr+len) so the subrange records the new prot while the head/tail keep
// theirs. The whole split (rewrite + head/tail appends) runs under g_anonmap_lock so a concurrent
// reader never sees a partially-split registry. Caller MUST hold the lock.
static void anon_update_prot_locked(uint64_t addr, uint64_t len, int prot) {
    uint64_t end = addr + len;
    if (!addr || end <= addr) return;
    int n = g_nanonmap;
    for (int i = 0; i < n; i++) {
        uint64_t a = g_anonmap[i].addr, e = a + g_anonmap[i].len;
        int old = g_anonmap[i].prot;
        if (a >= end || addr >= e || old == prot) continue;
        uint64_t lo = addr > a ? addr : a, hi = end < e ? end : e;
        // Rewrite this record to the updated subrange, then append head/tail remainders with the old
        // prot. anon_prot_if_contained scans first-match, so the record itself must carry the new prot.
        g_anonmap[i].addr = lo;
        g_anonmap[i].len = hi - lo;
        g_anonmap[i].prot = prot;
        if (a < lo) anon_track_locked(a, lo - a, old);
        if (hi < e) anon_track_locked(hi, e - hi, old);
    }
}

static void anon_update_prot(uint64_t addr, uint64_t len, int prot) {
    anon_lock();
    anon_update_prot_locked(addr, len, prot);
    anon_unlock();
}

// prot of the tracked private-anon region fully containing [addr,addr+len), else -1 (unknown -> do
// not remap). Full containment guarantees the range is anon, so the remap cannot corrupt a file map.
// Read the verdict into a local under the lock, then unlock and return it (never hold across return).
static int anon_prot_if_contained(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    int prot = -1;
    anon_lock();
    for (int i = 0; i < g_nanonmap; i++)
        if (g_anonmap[i].addr <= addr && end <= g_anonmap[i].addr + g_anonmap[i].len) {
            prot = g_anonmap[i].prot;
            break;
        }
    anon_unlock();
    return prot;
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

// MADV_DONTFORK(14) ranges: PRIVATE-ANON regions the guest asked NOT to be inherited by a fork(2) child
// (Linux VM_DONTCOPY). Tracked here (mirrors g_wipefork); fork_child_hooks() unmaps each range in the
// child so a child access FAULTS exactly as Linux would, while the parent keeps its mapping.
// MADV_DOFORK(15) undoes it by dropping the range. The registry is inherited across fork.
static struct {
    uint64_t addr, len;
} g_dontfork[256];

static int g_ndontfork;

static void dontfork_add(uint64_t addr, uint64_t len) {
    if (!addr || !len) return;
    for (int i = 0; i < g_ndontfork; i++)
        if (g_dontfork[i].addr == addr && g_dontfork[i].len == len) return; // already tracked
    if (g_ndontfork >= (int)(sizeof g_dontfork / sizeof g_dontfork[0])) return;
    g_dontfork[g_ndontfork].addr = addr;
    g_dontfork[g_ndontfork].len = len;
    g_ndontfork++;
}

static void dontfork_del(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_ndontfork;) {
        uint64_t a = g_dontfork[i].addr, e = a + g_dontfork[i].len;
        if (a < end && addr < e)
            g_dontfork[i] = g_dontfork[--g_ndontfork]; // overlaps the DOFORK/unmapped range -> forget it
        else
            i++;
    }
}

// Called in the fork CHILD (from fork_child_hooks): drop each MADV_DONTFORK range so the child faults on
// access (Linux presents the range as unmapped in the child).
static void dontfork_apply_child(void) {
    for (int i = 0; i < g_ndontfork; i++) {
        uint64_t a = g_dontfork[i].addr, l = g_dontfork[i].len;
        munmap((void *)a, (size_t)l);
        // Retire the range from the private-anon registry and mark it PROT_NONE so a child access faults
        // (SIGSEGV) exactly as Linux VM_DONTCOPY presents it -- otherwise the x86 lazy anon-page grower,
        // seeing the address still inside a tracked anon region, would silently re-serve a zero page.
        anon_untrack(a, l);
        gna_add(a & ~(uint64_t)0xfff, (a + l + 0xfff) & ~(uint64_t)0xfff);
    }
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

// Normalize the standard /dev/fd/N alias into the single synthetic procfs namespace used by every
// path operation. Returning the original path for non-fd entries keeps ordinary /dev resolution unchanged.
static const char *procfd_namespace_path(const char *path, char *storage, size_t capacity) {
    int fd = procfd_num(path);
    if (fd < 0 || path == NULL || strncmp(path, "/dev/fd/", 8) != 0) return path;
    if (snprintf(storage, capacity, "/proc/self/fd/%d", fd) >= (int)capacity) return path;
    return storage;
}

static int procfd_directory_path(const char *path) {
    return path && (!strcmp(path, "/proc/self/fd") || !strcmp(path, "/proc/self/fd/") ||
                    !strcmp(path, "/proc/thread-self/fd") || !strcmp(path, "/proc/thread-self/fd/"));
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
// EV_DELETEs that would error.  Tables are indexed by fd<HL_NFD (matches every other fd table here);
// epfd/fd >= HL_NFD fall back to the immediate path.
static int g_epopt = -1;                // -1 unknown, 0 off, 1 on
static struct kevent *g_ep_chg[HL_NFD]; // deferred changelist per epoll fd
static int g_ep_chgn[HL_NFD], g_ep_chgcap[HL_NFD];
static uint8_t g_ep_rd[HL_NFD], g_ep_wr[HL_NFD]; // per guest fd: read/write filter currently armed
static uint8_t g_ep_os[HL_NFD];                  // per guest fd: EPOLLONESHOT requested (kernel auto-removes on fire)
static uint8_t g_epoll[HL_NFD]; // per fd: an epoll instance (backed by kqueue) -- rebuilt across fork (macOS kqueue()
                                // is not inherited)
// per fd: this epoll instance has a dup alias. A dup() shares the SAME kqueue, so both aliases must submit
// interest DIRECTLY to the kernel (the deferred per-epfd changelist would strand a change on one alias);
// forces epoll_ctl/epoll_wait onto the immediate path for a dup'd instance.
static uint8_t g_ep_dupd[HL_NFD];
static uint16_t g_ep_cslot[HL_NFD]; // epoll aliases -> canonical registry slot + 1

static int epoll_slot(int fd) {
    return fd >= 0 && fd < HL_NFD && g_ep_cslot[fd] ? (int)g_ep_cslot[fd] - 1 : fd;
}

// ---- per-instance epoll interest table (fd -> owning instance + events + udata) --------------------
// Linux keeps an interest list per epoll instance and ties each registration to the underlying OPEN FILE
// DESCRIPTION, not the fd number. hl emulates epoll with a macOS kqueue whose knotes are keyed by fd
// number and vanish when that fd number closes -- so two OFD-semantics that hl previously got wrong:
//   (1) a fork child inherits the parent's interest list, but macOS does NOT inherit kqueue()s, so the
//       rebuilt child kqueue was EMPTY (a child that epoll_waits without re-registering saw nothing);
//   (2) closing a watched fd whose OFD stays alive via a dup should KEEP the registration (readiness
//       persists), but the kqueue knote died with the fd number.
// The table below records, per WATCHED fd, its owning epoll instance + the registered events + udata, so
// the fork child can re-arm every inherited registration on its rebuilt kqueue, and a close-with-surviving
// -dup can re-home the knote onto the surviving alias. Like the g_ep_rd/wr armed maps this is a SINGLE
// owner per watched fd (hl already assumes a fd is watched by at most one epoll instance); the writes are
// a couple of fd-indexed stores on the epoll_ctl path, so the hot path cost matches the existing armed map.
static int g_ep_owner[HL_NFD];            // watched fd -> owning epoll instance fd + 1 (0 = not watched)
static uint32_t g_ep_events[HL_NFD];      // watched fd -> the epoll events mask registered (EPOLLIN/OUT/ET/ONESHOT)
static uint64_t g_ep_udata[HL_NFD];       // watched fd -> the epoll_event.data registered for it
static void ep_mem_close(int ep, int fd); // implemented beside the membership table in event.c
static int ep_mem_test(int ep, int fd);
static void ep_mem_set(int ep, int fd, int on);
static void ep_mem_clear(int ep);

// ---- open-file-description identity for fd-number aliases (dup) -------------------------------------
// hl shares the host descriptor table with the guest, so two guest fds that refer to the same OFD are two
// host fds dup'd from each other. There is no portable "same OFD?" query, so track it ourselves: every
// dup(2)/dup2/dup3/F_DUPFD tags both fds with a shared group id (see fd_carry_virt). close() clears the id.
// Used by the epoll close path to find a surviving alias of a just-closed watched fd (finding: epoll
// readiness must persist while a dup keeps the OFD open).
// Assign (or propagate) a shared OFD group id from oldfd to newfd on dup.
static void ofd_link_dup(int newfd, int oldfd) {
    if (oldfd < 0 || oldfd >= HL_NFD || newfd < 0 || newfd >= HL_NFD || oldfd == newfd) return;
    if (!g_ofd_id[oldfd]) g_ofd_id[oldfd] = ofd_identity_new();
    g_ofd_id[newfd] = g_ofd_id[oldfd];
}

// Find an OPEN guest fd (other than `fd`) that shares fd's OFD group id, or -1 if none survives.
static int ofd_surviving_alias(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_ofd_id[fd]) return -1;
    uint64_t id = g_ofd_id[fd];
    for (int i = 0; i < HL_NFD; i++)
        if (i != fd && g_ofd_id[i] == id && fcntl(i, F_GETFD) != -1) return i;
    return -1;
}

static int epopt_on(void) {
    if (g_epopt < 0) g_epopt = 1;
    return g_epopt;
}

// append a change to epfd's buffer, coalescing on (ident,filter) so repeated ctls collapse.
static void ep_push(int ep, uintptr_t ident, int16_t filt, uint16_t flags, void *udata) {
    if (ep < 0 || ep >= HL_NFD) return;
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

static void ep_rehome_instance(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_epoll[fd] || epoll_slot(fd) != fd) return;
    int replacement = -1;
    for (int candidate = 0; candidate < HL_NFD; ++candidate)
        if (candidate != fd && g_epoll[candidate] && epoll_slot(candidate) == fd && fcntl(candidate, F_GETFD) >= 0) {
            replacement = candidate;
            break;
        }
    if (replacement < 0) return;
    g_ep_provider_generations[replacement] = g_ep_provider_generations[fd];
    for (int candidate = 0; candidate < HL_NFD; ++candidate) {
        if (g_ep_cslot[candidate] == (uint16_t)(fd + 1)) g_ep_cslot[candidate] = (uint16_t)(replacement + 1);
        if (g_ep_owner[candidate] == fd + 1) g_ep_owner[candidate] = replacement + 1;
        if (ep_mem_test(fd, candidate)) ep_mem_set(replacement, candidate, 1);
    }
    ep_mem_clear(fd);
    for (uint32_t index = 0; index < EP_PROVIDER_WATCH_LIMIT; ++index) {
        ep_provider_watch *watch = &g_ep_provider_watches[index];
        if (atomic_load_explicit(&watch->state, memory_order_acquire) == EP_PROVIDER_ACTIVE && watch->epoll == fd) {
            watch->epoll = replacement;
            watch->epoll_generation = g_ep_provider_generations[replacement];
        }
    }
    for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
        ep_object_watch *watch = &g_ep_object_watches[index];
        if (atomic_load_explicit(&watch->active, memory_order_acquire) != 0 && watch->epoll == fd) {
            watch->epoll = replacement;
            watch->epoll_generation = g_ep_provider_generations[replacement];
        }
    }
    g_ep_object_count[replacement] = g_ep_object_count[fd];
    g_ep_object_count[fd] = 0;
}

// reset epoll armed-state for a guest fd (called from close(): kqueue auto-removes a closed fd, so the
// armed map must follow to avoid a later stale EV_DELETE on a reused fd number).
static void ep_fd_reset(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    ep_rehome_instance(fd);
    // Closing the final descriptor for a watched OFD removes its registration from Linux epoll.  kqueue
    // removes the knote itself, but our Linux EEXIST/ENOENT membership mirror must be cleared explicitly;
    // otherwise immediate reuse of this descriptor number makes a fresh EPOLL_CTL_ADD fail with EEXIST.
    // ep_close_rehome has already moved a surviving dup and cleared this bit, so this is idempotent there.
    if (g_ep_owner[fd]) ep_mem_close(g_ep_owner[fd] - 1, fd);
    ep_provider_retire_endpoint(fd);
    ep_object_retire_endpoint(fd);
    g_ep_provider_generations[fd] = ep_provider_next(g_ep_provider_generations[fd]);
    g_ep_rd[fd] = g_ep_wr[fd] = g_ep_os[fd] = 0;
    if (g_ep_chg[fd]) {
        free(g_ep_chg[fd]);
        g_ep_chg[fd] = NULL;
    } // if fd was an epoll fd, drop its pending changelist
    g_ep_chgn[fd] = g_ep_chgcap[fd] = 0;
    // Closing an epoll INSTANCE removes every registration it held (Linux). Drop the interest ownership of
    // each fd it watched so a reused epoll fd number can't inherit stale registrations / mis-rehome later.
    if (g_epoll[fd]) {
        for (int w = 0; w < HL_NFD; w++)
            if (g_ep_owner[w] == fd + 1) {
                g_ep_owner[w] = 0;
                g_ep_events[w] = 0;
                g_ep_udata[w] = 0;
            }
    }
    g_epoll[fd] = 0;   // a reused fd number is no longer an epoll instance
    g_ep_dupd[fd] = 0; // ...nor a dup alias of one
    g_ep_cslot[fd] = 0;
    // drop this fd's own interest-table entry + OFD group id (any surviving-dup re-home already ran in
    // ep_close_rehome). A reused fd number must start with no owner/events/udata and no stale alias link.
    g_ep_owner[fd] = 0;
    g_ep_events[fd] = 0;
    g_ep_udata[fd] = 0;
    g_ofd_id[fd] = 0;
}

// close() hook for the inotify family (event.c cases 26/27/28). hl emulates inotify with a kqueue: the
// INSTANCE fd carries g_inotify[fd]=1, while each WATCH descriptor (wd) is itself a host O_EVTONLY fd with
// g_inotify_wpath/_snap/_owner keyed by that wd NUMBER. Both roles live in the shared fd table, so on close
// a reused fd number must shed whichever role it held or a later read()/epoll_wait/inotify read misroutes to
// a dead watch. fd_reset_emul used to clear only g_inotify_owner[fd], leaving g_inotify[fd] stamped --
// a recycled inotify-instance number then still routed read() into the inotify drain (io.c) against a stale
// kqueue. Called from fd_reset_emul BEFORE the real close(); idempotent and a no-op on a plain fd.
static void inotify_fd_reset(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
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
                g_inotify_mask[w] = 0;
                g_inotify_pending[w] = 0;
                g_inotify_isdir[w] = 0;
#if !defined(__linux__)
                close(w); // the watch's own host O_EVTONLY fd; kqueue auto-removes its EVFILT_VNODE registration
#endif
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
    g_inotify_mask[fd] = 0;
    g_inotify_pending[fd] = 0;
    g_inotify_isdir[fd] = 0;
    free(g_inotify_raw[fd]);
    g_inotify_raw[fd] = NULL;
    g_inotify_raw_len[fd] = g_inotify_raw_pos[fd] = 0;
}

// Shared boundary errno translation for every svc_<family>() module tail. Each family early-returns from
// service_local (before its trailing errno translation), so each must map G_RET's host macOS errno to Linux.
// errno the guest expects (e.g. macOS EAGAIN=35 = Linux EDEADLK). Skip when c->redirect is set: a redirect
// (execve / sigreturn) leaves an already-Linux value in G_RET that must not be re-translated -- a no-op for
// the families that never set redirect, so collapsing all tails onto this one helper is byte-identical.
// Returns 1 so a handler can `return svc_done(c);`.
static int svc_done(struct cpu *c) {
    if (!c->redirect) {
        int64_t rv = (int64_t)G_RET(c);
        if (rv < 0 && rv >= -4095) G_RET(c) = (uint64_t)(-(int64_t)hl_linux_errno_from_macos((int)(-rv)));
    }
    return 1;
}
