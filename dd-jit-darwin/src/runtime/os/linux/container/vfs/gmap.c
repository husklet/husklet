// Extracted from ../vfs.c: guest address-space mapping registry (g_gmap + gmap_add/del/find_len/reset_all)
// Not standalone -- #included by ../vfs.c at the original position (verbatim move, identical
// preprocessed TU). Relies on ../vfs.c's preceding globals/headers; see vfs.c for context.
// Guest address-space registry. Every guest mapping (ELF image, interp, heap, stack, anon/file mmap) is
// tracked so execve() can tear the inherited space down before loading the new image. Without this a
// post-fork exec keeps the PARENT's dense layout, and load_elf must bias a non-PIE ET_EXEC off its fixed
// vaddr (macOS __PAGEZERO reserves the low 4 GB) -> the image's baked absolute refs collide with the
// densely-packed inherited maps -> SIGSEGV. Resetting reproduces the clean fresh-exec layout that works.
#define GMAP_N 8192 // was 1024 -- a heavy guest overflowed it, leaking the untracked mappings at execve teardown

// `len` is the FULL tracked extent (incl. the 64 KB guard tail dd reserves past a guest anon mapping,
// so glibc's vectorized over-reads stay mapped) -- used for execve teardown / munmap / mremap. `glen`
// is the guest-VISIBLE logical length (== len for guard-less mappings); /proc/[pid]/{,s}maps reports it
// so a mapping's Size/Rss matches what the guest asked for, not dd's over-reservation (LTP mlock05 Rss).
static struct {
    uint64_t addr, len, glen;
} g_gmap[GMAP_N];

static int g_ngmap;

static void gmap_add(uint64_t addr, uint64_t len) {
    if (!addr || addr == (uint64_t)-1 || len == 0 || g_ngmap >= GMAP_N) return;
    g_gmap[g_ngmap].addr = addr;
    g_gmap[g_ngmap].len = len;
    g_gmap[g_ngmap].glen = len; // default: no guard tail; the anon-map paths override via gmap_set_glen
    g_ngmap++;
}

// Record a mapping's guest-visible logical length (< its full tracked extent when a guard tail rides
// along). Called right after gmap_add on the guard-reserving anon mmap/mremap paths.
static void gmap_set_glen(uint64_t addr, uint64_t glen) {
    for (int i = 0; i < g_ngmap; i++)
        if (g_gmap[i].addr == addr) {
            g_gmap[i].glen = glen;
            return;
        }
}

static void gmap_del(uint64_t addr) {
    for (int i = 0; i < g_ngmap; i++)
        if (g_gmap[i].addr == addr) {
            g_gmap[i] = g_gmap[--g_ngmap];
            return;
        }
}

// The tracked extent (incl. any guard tail) of a mapping that starts at addr, or 0 if untracked.
static uint64_t gmap_find_len(uint64_t addr) {
    for (int i = 0; i < g_ngmap; i++)
        if (g_gmap[i].addr == addr) return g_gmap[i].len;
    return 0;
}

static void gmap_reset_all(void) { // munmap every tracked guest mapping; the caller reloads fresh
    for (int i = 0; i < g_ngmap; i++)
        munmap((void *)g_gmap[i].addr, (size_t)g_gmap[i].len);
    g_ngmap = 0;
}

// ---- mlock accounting (LTP mlock05 / munlockall01) -----------------------------------------------
// macOS has no mlock/mlockall equivalent, so "keep resident" is a pure no-op AT THE HOST -- but Linux
// software observes the lock STATE back through /proc/self/smaps ("Locked:") and /proc/self/status
// ("VmLck:"). Track the guest's locked byte ranges (non-overlapping, page-aligned) so those files
// report the truth. mlockall(MCL_CURRENT|MCL_FUTURE) locks everything -> a whole-process flag.
// mlock/munlock maintained by mem.c (228/229); mlockall/munlockall by rare.c (230/231).
#define MLK_N 1024

static struct {
    uint64_t lo, hi;
} g_mlk[MLK_N];

static int g_nmlk;
static int g_mlock_all;    // mlockall(MCL_CURRENT|MCL_FUTURE) set; munlockall clears it + every range
static int g_mlock_future; // MCL_FUTURE armed: a fresh mmap (mem.c case 222) is wired resident on creation

// Remove [addr,addr+len) from the locked set, splitting any straddled range (mirrors pn_del).
static void mlk_del(uint64_t addr, uint64_t len) {
    if (!len || !g_nmlk) return;
    uint64_t rlo = addr & ~(uint64_t)0xfff, rhi = (addr + len + 0xfff) & ~(uint64_t)0xfff;
    for (int i = 0; i < g_nmlk;) {
        uint64_t lo = g_mlk[i].lo, hi = g_mlk[i].hi;
        if (rhi <= lo || rlo >= hi) {
            i++;
            continue;
        } // no overlap
        int keep_head = lo < rlo, keep_tail = rhi < hi;
        if (!keep_head && !keep_tail) {
            g_mlk[i] = g_mlk[--g_nmlk];
            continue;
        } // fully covered -> drop
        if (keep_head)
            g_mlk[i].hi = rlo;
        else
            g_mlk[i].lo = rhi;
        if (keep_head && keep_tail && g_nmlk < MLK_N) {
            g_mlk[g_nmlk].lo = rhi;
            g_mlk[g_nmlk].hi = hi;
            g_nmlk++;
        }
        i++;
    }
}

// Add [addr,addr+len) to the locked set. Delete-then-insert keeps the ranges non-overlapping so the
// byte-coverage sums below can never double-count.
static void mlk_add(uint64_t addr, uint64_t len) {
    if (!len) return;
    uint64_t lo = addr & ~(uint64_t)0xfff, hi = (addr + len + 0xfff) & ~(uint64_t)0xfff;
    if (hi <= lo) return;
    mlk_del(lo, hi - lo);
    if (g_nmlk >= MLK_N) return; // registry full -> best effort (report under-counts, never faults)
    g_mlk[g_nmlk].lo = lo;
    g_mlk[g_nmlk].hi = hi;
    g_nmlk++;
}

static void mlk_reset(void) {
    g_nmlk = 0;
    g_mlock_all = 0;
    g_mlock_future = 0;
} // execve replaces the address space

// mlockall(MCL_CURRENT): actually WIRE every currently-mapped guest range resident via the host mlock(2)
// (macOS has mlock, same as mlock(2)/case 228 uses). Best-effort: a range the host refuses (RLIMIT_MEMLOCK
// exhausted) is left pageable rather than aborting the whole call -- Linux would ENOMEM, but dd keeps the
// call succeeding with honest /proc state (the wired ranges are real; see the residual note in
// syscall-compat.md). Returns the number of ranges the host declined (0 = fully wired).
static int mlk_wire_current(void) {
    int failed = 0;
    for (int i = 0; i < g_ngmap; i++) {
        if (!g_gmap[i].addr || !g_gmap[i].len) continue;
        if (mlock((void *)g_gmap[i].addr, (size_t)g_gmap[i].len) != 0) failed++;
    }
    return failed;
}

// munlockall(): drop the host wiring on every tracked guest range (mirrors mlk_wire_current). Failures are
// ignored -- an unwired/never-wired range simply stays unwired.
static void mlk_unwire_all(void) {
    for (int i = 0; i < g_ngmap; i++) {
        if (!g_gmap[i].addr || !g_gmap[i].len) continue;
        munlock((void *)g_gmap[i].addr, (size_t)g_gmap[i].len);
    }
}

// Locked bytes within [lo,hi) -- for a single /proc/.../smaps region's "Locked:" field.
static uint64_t mlk_region_locked(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return 0;
    if (g_mlock_all) return hi - lo;
    uint64_t sum = 0;
    for (int i = 0; i < g_nmlk; i++) {
        uint64_t a = g_mlk[i].lo > lo ? g_mlk[i].lo : lo;
        uint64_t b = g_mlk[i].hi < hi ? g_mlk[i].hi : hi;
        if (b > a) sum += b - a;
    }
    return sum;
}

// Total locked bytes -- for /proc/.../status "VmLck:". With MCL_CURRENT|MCL_FUTURE every mapping is
// locked, so report the whole tracked guest address space (sum of visible mapping lengths; always
// non-zero once the image is loaded -- g_mem_charged is only maintained under a cgroup cap, so it can't
// be used here). Otherwise sum the explicitly mlock'd ranges.
static uint64_t mlk_total_locked(void) {
    uint64_t sum = 0;
    if (g_mlock_all) {
        for (int i = 0; i < g_ngmap; i++)
            sum += g_gmap[i].glen;
        return sum;
    }
    for (int i = 0; i < g_nmlk; i++)
        sum += g_mlk[i].hi - g_mlk[i].lo;
    return sum;
}

// ---- RLIMIT_MEMLOCK enforcement (LTP mlock05 rlimit half) -----------------------------------------
// The container runs UNPRIVILEGED (no CAP_IPC_LOCK -- see proc.c sched_setscheduler, which EPERMs RT
// scheduling for the same reason), so mlock/mlock2/mlockall must honor RLIMIT_MEMLOCK exactly as Linux
// mm/mlock.c does, rather than only relying on the macOS host mlock (which is bounded by the HOST's limit,
// not the guest's):
//   * can_do_mlock(): a soft limit of 0 (unprivileged) refuses the op outright with EPERM.
//   * mlock/mlock2: (already-locked + newly-locked) bytes over the soft limit -> ENOMEM, nothing wired.
//   * mlockall(MCL_CURRENT): total mapped bytes over the soft limit -> ENOMEM.
//   * mmap under MCL_FUTURE: a new mapping that would push locked bytes over the limit is left unwired and
//     uncounted (the mmap still succeeds), so the tracked locked total never exceeds the limit.
// The soft limit is the guest's RLIMIT_MEMLOCK (resource 8): a docker --ulimit / guest setrlimit override
// in g_ulimit[8], else RLIM_INFINITY -- unset means "not enforced", preserving the legacy best-effort path.
#define DD_RLIMIT_MEMLOCK 8

static uint64_t mlk_memlock_limit(void) {
    return g_ulimit[DD_RLIMIT_MEMLOCK].set ? g_ulimit[DD_RLIMIT_MEMLOCK].cur : ~0ull;
}

// Explicitly mlock()'d bytes (sum of the tracked ranges) -- the accounting base for the rlimit check.
// Distinct from mlk_total_locked(), which reports the WHOLE address space under mlockall for /proc.
static uint64_t mlk_locked_bytes(void) {
    uint64_t s = 0;
    for (int i = 0; i < g_nmlk; i++)
        s += g_mlk[i].hi - g_mlk[i].lo;
    return s;
}

// Page-rounded bytes of [addr,addr+len) NOT already counted as locked (so re-locking an overlapping range
// does not double-charge, matching Linux's per-page locked_vm accounting).
static uint64_t mlk_uncounted_bytes(uint64_t addr, uint64_t len) {
    if (!len) return 0;
    uint64_t lo = addr & ~(uint64_t)0xfff, hi = (addr + len + 0xfff) & ~(uint64_t)0xfff;
    if (hi <= lo) return 0;
    uint64_t already = 0;
    for (int i = 0; i < g_nmlk; i++) {
        uint64_t a = g_mlk[i].lo > lo ? g_mlk[i].lo : lo;
        uint64_t b = g_mlk[i].hi < hi ? g_mlk[i].hi : hi;
        if (b > a) already += b - a;
    }
    return (hi - lo) - already;
}

// mlock/mlock2 rlimit gate. Returns 0 if the lock may proceed, else -EPERM (soft limit 0) / -ENOMEM
// (would exceed). RLIM_INFINITY / unset -> always 0 (legacy best-effort, no enforcement).
static int mlk_rlimit_gate(uint64_t addr, uint64_t len) {
    uint64_t lim = mlk_memlock_limit();
    if (lim == ~0ull) return 0;  // unlimited / unset -> not enforced
    if (lim == 0) return -EPERM; // can_do_mlock(): no locking allowed at all
    if (!len) return 0;
    if (mlk_locked_bytes() + mlk_uncounted_bytes(addr, len) > lim) return -ENOMEM;
    return 0;
}

// mlockall(MCL_CURRENT) rlimit gate: the whole mapped address space is about to be wired. Returns 0 /
// -EPERM (soft limit 0) / -ENOMEM (total mapped bytes exceed the soft limit).
static int mlk_rlimit_gate_all(void) {
    uint64_t lim = mlk_memlock_limit();
    if (lim == ~0ull) return 0;
    if (lim == 0) return -EPERM;
    uint64_t total = 0;
    for (int i = 0; i < g_ngmap; i++)
        total += g_gmap[i].glen;
    if (total > lim) return -ENOMEM;
    return 0;
}
