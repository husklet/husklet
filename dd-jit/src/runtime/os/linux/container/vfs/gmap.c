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

// Guest-visible length of the mapping that starts at addr (0 if untracked) -- for /proc maps synthesis.
static uint64_t gmap_glen(uint64_t addr) {
    for (int i = 0; i < g_ngmap; i++)
        if (g_gmap[i].addr == addr) return g_gmap[i].glen;
    return 0;
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
static int g_mlock_all; // mlockall(MCL_CURRENT|MCL_FUTURE) set; munlockall clears it + every range

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
} // execve replaces the address space

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
