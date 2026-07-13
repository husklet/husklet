// dd/runtime/frontend/x86_64 -- opt8 persistent translated-code cache (DDJIT_PCACHE=1; default OFF).
//
// Idea: cold start of a short-lived container is dominated by translating the dynamic linker (musl
// ld.so) + the program's startup path -- ~1000 blocks for `busybox echo`. That work is identical on
// every launch of the same binary. We persist the translated arena + block map to a file and mmap it
// back on the next run of the same binary, skipping translation entirely (~40% of internal cold start).
//
// What makes the bytes reusable across *processes*:
//   * The guest image + interp are mapped at FIXED addresses (PC_IMG_BASE / PC_INTERP_BASE), so guest
//     PCs (the block-map keys) and any guest address baked into host code are stable, and arena-internal
//     absolute pointers (g_map host/body, g_pend slots) + PC-relative chaining are valid as-is on reload.
//   * The host addresses baked into emitted blocks (block_return, &g_ibtc, &g_fast_count, &g_pending,
//     &g_sig_inline_count, &g_yield_inline_count, &dd_rep_movs/stos, ...) ALL live in this one PIE binary,
//     which dyld slides as a unit; each baked site is emitted as a fixed 4-insn movz/movk slot and recorded
//     in g_reloc, and rewritten on load by the single image slide. if the g_reloc table ever fills we
//     poison the arena (g_pcache_poison) so save() refuses -- a persisted arena has EVERY baked site
//     recorded by construction, so a reload can never keep a stale absolute host address (was: silent drop
//     -> intermittent, ASLR-slide-dependent SIGSEGV once the arena grew past the old 1<<16 cap).
//   * The JIT arena does NOT need fixing (MAP_JIT can't be MAP_FIXED anyway): g_map/g_pend are persisted
//     as arena OFFSETS and rebuilt against the live g_cache.
//   * LIBRARY blocks. The dynamic linker's library maps land wherever the host kernel places them,
//     which used to make every library block's gpc key non-reusable (dead weight in the restored map --
//     and worse, a LIVE hazard: a later run mapping a DIFFERENT lib over a cached gpc range would HIT a
//     stale translation of the old bytes -> intermittent warm-only SIGSEGV). Now, when the cache is on,
//     the guest's file-backed non-fixed mmaps get DETERMINISTIC base hints from a per-process bump
//     allocator at PC_LIB_BASE (os/linux/syscall/mem.c hook), and each hinted map that lands on its hint
//     is recorded in a MANIFEST {base, len, file-identity}. Library blocks are persisted with the
//     manifest, and on a warm run they are NOT inserted into the block map at load -- they are DEFERRED
//     and only activated when the guest actually maps a file with the SAME identity at the SAME base
//     (pcache_note_libmap). Identity mismatch / different layout -> those blocks are dropped. So a
//     restored block can never shadow different guest bytes, by construction.
//
// Invalidation: the cache file is keyed by (engine version, cpu-struct size, map/IBTC sizes, both fixed
// bases, entry PC, argv[0] basename, and the identity -- dev/ino/size/mtime -- of the guest binary AND
// its interpreter). Any mismatch / truncation / corruption -> graceful MISS: ignore the file and
// translate fresh, re-save.
//
// EXEC RE-KEY: an in-process execve (proc.c case 221) flushes the arena and re-loads a NEW image;
// both the save AND load side re-key at that boundary (pcache_exec_reload). g_pc_binid/g_pc_entry are
// ONLY ever assigned (a) at initial load, before any translation, and (b) inside pcache_exec_reload,
// immediately after the exec flushed the arena -- so the key can never describe a different image than
// the arena content. (Pre-#373 the x86 engine never re-keyed: a `sh -c tar` run persisted tar's arena
// under busybox-sh's key -- an accidental win for that chain, silent poison for every other exec.)
//
// WARM-STAT SELF-TUNING: a sidecar "<file>.warm" records how much of the restored map a warm run
// actually reused (waste = restored blocks that never became usable -- deferred library entries whose
// image never re-mapped with the matching identity). When waste*2 >= restored, the restore is dead
// weight (layout drifted / lib changed) -> later runs SKIP the restore entirely (header-only read, no
// arena I/O, no map pollution) instead of paying for it. Fresh translations of NEW code never count
// against the restore (a bigger workload under the same key must not poison the policy). The sidecar is
// advisory only: it never gates correctness, only the restore/skip decision, and it is written with the
// same atomic temp+rename protocol (the .pcache file itself is NEVER modified after publication).

#define PC_MAGIC 0x31304350544a4444ull // "DDJPCT01" (LE)
#define PC_VERSION                                                                                                     \
    6 // BUMP on ANY codegen change (different emitted bytes -> stale). v5: IRQSLIM layout.
      // v6: exec re-key, argv0 key, full-width (JIT_MAP_N) map persistence,
      // library manifest + deferred activation, warm-stat sidecar.
// Every env var that changes the EMITTED BYTES must also key the persistent cache, or a warm load with
// DDJIT_PCACHE=1 can dispatch translations produced under a DIFFERENT codegen mode (silently making the
// env-driven compat/debug switch ineffective, or worse, dispatching a mismatched block layout). IRQSLIM
// (g_fwdskip) changes the block-entry layout; the rest gate distinct emitted sequences. Read the env vars
// directly (they are process-global + immutable for the run) with the SAME truthiness each codegen site
// uses. Bits are stable/disjoint so a cache written in one mode can never be reloaded into another.
static uint64_t pcache_env_nz(const char *n) { // "any non-0 value" gate (NOLAZY/NOX*DIRECT style)
    const char *s = getenv(n);
    return (s && strcmp(s, "0")) ? 1 : 0;
}
// "first char present and not '0'" gate: matches the exact truthiness several codegen sites use
// (`s && *s && *s != '0'`) -- e.g. NOFASTSYS/NOSIGINLINE (emit.c), NOXBLOCKFLAGS/NOXALUFLAGELIDE
// (trace.c), NOPFAFELIM (translate.c), NOREP (repstr.c). Distinct from pcache_env_nz, which treats
// "" as set and "01" as set; those sites treat "" and any '0'-prefixed value as UNSET, so a cache-id
// built with this predicate flips at exactly the same values the emitter changes bytes at.
static uint64_t pcache_env_fcnz(const char *n) {
    const char *s = getenv(n);
    return (s && *s && *s != '0') ? 1 : 0;
}
static uint64_t pcache_codegen_mode_bits(void) {
    uint64_t b = 0;
    if (g_fwdskip) b |= 1ull << 8; // IRQSLIM block-entry layout (kept at bit 8 for continuity)
    if (pcache_env_nz("NOLAZY")) b |= 1ull << 9;
    if (pcache_env_nz("NOXALUDIRECT")) b |= 1ull << 10;
    if (pcache_env_nz("NOXSHIFTDIRECT")) b |= 1ull << 11;
    if (getenv("NOSTITCH")) b |= 1ull << 12;
    if (getenv("NOREPCMP")) b |= 1ull << 13;
    if (getenv("NOSSEOPT")) b |= 1ull << 14;
    if (getenv("NOEAOPT")) b |= 1ull << 15;
    if (getenv("NOGUESTFOLD")) b |= 1ull << 16;
    if (getenv("NOIRQCHECK")) b |= 1ull << 17;
    // ---- C16 audit gap-close: 13 A/B flags that CHANGE the emitted x86 bytes but were NOT keyed. ----
    // Each owns a NEW, UNIQUE bit in the 18..30 range (disjoint from the version int in bits 0..2 and
    // from the 8..17 flags above). The predicate below mirrors, char-for-char, the SAME getenv truthiness
    // the corresponding x86 emitter reads -- presence (`!= NULL`) or "first char non-'0'" (pcache_env_fcnz)
    // -- so two runs that would emit different bytes always get different cache-ids. Reading getenv here
    // (not the codegen's lazily-cached `-1` statics, which may be uninitialized this early and live in
    // other translation units) is the file's own established doctrine (see the header comment) and is
    // exact because every one of those statics is a pure deterministic function of exactly this getenv.
    if (getenv("DDJIT_NOSLIMSYS")) b |= 1ull << 18;      // emit.c: `getenv?0:1` (presence) -> spill layout
    if (pcache_env_fcnz("DDJIT_NOFASTSYS")) b |= 1ull << 19;  // emit.c s1_calibrate: fast-syscall inline arms
    if (pcache_env_fcnz("DDJIT_NOSIGINLINE")) b |= 1ull << 20; // emit.c: W4F signal-inline arms
    if (getenv("NOTIER2X")) b |= 1ull << 21;             // engine_glue/linux_x86_64: presence -> tier-2 codegen
    if (getenv("NOFLAGELIDE")) b |= 1ull << 22;          // master flag-elide gate (shift/xblock/loop): presence
    if (getenv("NOSHIFTFLAGELIDE")) b |= 1ull << 23;     // shift.c: presence -> shift flag synthesis elision
    if (pcache_env_fcnz("NOXBLOCKFLAGS")) b |= 1ull << 24;   // trace.c xblkflags_on: block-scan NZCV elision
    if (pcache_env_fcnz("NOXALUFLAGELIDE")) b |= 1ull << 25; // trace.c xblkalu_elide_on: ALU dead-flag elision
    if (pcache_env_fcnz("NOPFAFELIM")) b |= 1ull << 26;     // translate.c pfaf_elim_on: PF/AF substrate elim
    if (getenv("NOX87OPT")) b |= 1ull << 27;             // x87.c x87opt_on: presence (`==NULL`) -> x87 shadow-top opt
    if (getenv("NOXFPDNAN")) b |= 1ull << 28;            // translate.c g_fpdnan: presence (`==NULL`) -> SSE dNaN path
    if (pcache_env_fcnz("NOREP")) b |= 1ull << 29;       // repstr.c norep_disabled: rep movs/stos fast path
    if (getenv("IBTC1WAY")) b |= 1ull << 30;             // engine_glue g_ibtc1way: presence -> IBTC probe codegen
    // SAFETY: pcache_codegen_mode_bits() is consulted ONLY when the persistent pcache is ACTIVE
    // (DDJIT_PCACHE=1, DEFAULT-OFF). With it off, no cache is loaded or stored, so these bits are never
    // read -> BYTE-IDENTICAL default behavior. With it on, adding bits only makes cache-ids MORE distinct:
    // worst case is an extra cache miss + in-memory recompile (safe), never a wrong load. This change can
    // only FIX the stale-warm-load bug (a warm arena built under a flag reloaded without it), never add one.
    return b;
}
#define PC_VERSION_EFF (PC_VERSION | pcache_codegen_mode_bits())
// Fixed guest VA bases (high, reliably free above the kernel-chosen heap/stack and below the dyld shared
// cache). Probed stable on Apple silicon; PIE images so we choose the base.
#define PC_IMG_BASE 0x0000040000000000ull    // 4 TB
#define PC_INTERP_BASE 0x0000048000000000ull // 4.5 TB
// deterministic library window (file-backed non-fixed guest mmaps get bump-allocated hints here).
#define PC_LIB_BASE 0x0000050000000000ull // 5 TB
#define PC_LIB_SPAN (1ull << 38)          // 256 GB window (beyond it: no hint, kernel placement)
#define PC_LIB_MAX 512                    // manifest entries persisted (beyond: unhinted, not cached)

struct pc_hdr {
    uint64_t magic, version;
    uint64_t cpu_sz, map_n, ibtc_n;
    uint64_t img_base, interp_base;
    uint64_t bin_id;     // identity of guest binary + interp + argv0 + engine build
    uint64_t entry_jump; // initial rip (sanity)
    uint64_t arena_used; // bytes of translated code
    uint64_t n_mapent, n_pend, n_reloc, n_lib;
    uint64_t csum;            // v6: FNV-1a over every byte after this header (parity with the aarch64 pcache)
    uint64_t block_return_at; // block_return's host addr at save time -> the image-slide anchor on load
    uint64_t ibtc_at;         // g_ibtc host addr at save time (diagnostic)
};

// word-wise FNV-1a (8 bytes/step): runs over multi-MB arenas on every load AND save; same detection
// strength as byte-wise for the threat model (accidental truncation/corruption, torn writers).
static uint64_t pc_fnv(uint64_t h, const void *buf, size_t n) {
    const uint8_t *p = (const uint8_t *)buf;
    for (; n >= 8; p += 8, n -= 8) {
        uint64_t w;
        memcpy(&w, p, 8);
        h ^= w;
        h *= 1099511628211ull;
    }
    for (; n; p++, n--) {
        h ^= *p;
        h *= 1099511628211ull;
    }
    return h;
}

struct pc_mapent {
    uint64_t gpc, host_off, body_off;
}; // host/body as arena offsets

struct pc_pend {
    uint64_t slot_off, target;
    uint32_t is_bl;
};

struct pc_lib {
    uint64_t base, len, id; // manifest: a deterministic-hinted file map (id = fstat identity hash)
};

// warm-stat sidecar ("<cachefile>.warm"): written by warm runs, read by the next load's
// restore-or-skip decision. Advisory only -- validated against the .pcache generation via arena_used/
// restored, ignored (and the restore performed as usual) on any mismatch.
struct pc_warm {
    uint64_t magic, arena_used, restored, waste; // waste = restored blocks that never became usable
};

#define PC_WARM_MAGIC 0x314d525750434444ull // "DDCPWRM1"

static int g_pcache_forked;          // set in a fork child (fresh arena, inherited bookkeeping) -> never save
static int g_pcache_skip;            // this run intentionally skipped a dead-weight restore -> don't churn-resave
static int g_pc_flushed;             // a wholesale cache flush ran this epoch (restored blocks are gone)
static uint64_t g_pc_restored_n;     // mapents the last load restored (0 = no load this epoch)
static uint64_t g_pc_restored_arena; // arena_used of the loaded file (sidecar generation tie)
// The two fixed images' live guest spans, recorded by load_elf when it consumes g_force_base. Everything
// outside these spans (and outside the manifest) is unrevivable by key construction and is neither
// persisted nor restored into the block map.
static uint64_t g_pc_img_lo, g_pc_img_hi, g_pc_interp_lo, g_pc_interp_hi;
// #210: latched by load_elf (elf.c) when a fixed-VA (MAP_FIXED) image map collides and falls back to a
// kernel-chosen base. This run's arena then mixes bases, so the pcache must neither RESTORE a fixed-base
// file over it (block-map keys + baked guest addresses belong to PC_IMG_BASE, not the fallback base ->
// wild fault) nor PERSIST one (a later fixed-base run could HIT a file whose baked bytes name a random
// base). Parity with the aarch64 loader's g_force_base_failed (translate/aarch64/pcache.c).
static int g_force_base_failed;
// runtime state: the deterministic-hint bump pointer, the manifest this run RECORDS (cold path),
// the manifest the load RESTORED (warm path), and the deferred (not-yet-activated) restored map entries.
static uint64_t g_pc_lib_next = PC_LIB_BASE;
static struct pc_lib g_pc_libs[PC_LIB_MAX]; // cold: recorded as the guest maps; warm: the file's manifest
static int g_pc_nlib;
static struct pc_mapent *g_pc_defer; // deferred restored entries (library gpc ranges), until activation
static uint64_t g_pc_ndefer;
static uint64_t g_pc_live_n;    // restored entries that went live at load (fixed-image ranges)
static uint64_t g_pc_activated; // deferred entries activated by identity-matching library maps

static uint64_t pcache_id_of(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) return 0;
    uint64_t h = 1469598103934665603ull; // FNV-1a over identity fields + path
    uint64_t fields[4] = {(uint64_t)st.st_dev, (uint64_t)st.st_ino, (uint64_t)st.st_size,
                          (uint64_t)st.st_mtimespec.tv_sec};
    for (int i = 0; i < 4; i++) {
        h ^= fields[i];
        h *= 1099511628211ull;
    }
    for (const char *p = path; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 1099511628211ull;
    }
    return h;
}

// A per-engine-build tag mixed into every cache id so the cache self-invalidates across dd versions:
// host code emitted by a DIFFERENT engine build is never loaded (loading it would crash). __DATE__/
// __TIME__ change on every (re)build, so a user who updates dd transparently gets a fresh cache --
// they never need to clear ~/.dd/pcache by hand. (Old files just go unreferenced; harmless cruft.)
static uint64_t pcache_engine_id(void) {
    uint64_t h = 1469598103934665603ull;
    for (const char *p = __DATE__ " " __TIME__; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 1099511628211ull;
    }
    return h;
}

// hash the BASENAME of argv[0]. A multicall binary (busybox, toolchain drivers) runs a DIFFERENT
// code path per applet, and with the exec re-key each epoch persists its own arena -- so the key must
// separate `sh` from `tar` or one applet's arena would shadow the other's. Basename (not full argv) so a
// single-purpose binary invoked with varying flags keeps ONE cache (mirrors the aarch64 pcache).
static uint64_t pcache_argv0_id(const char *argv0) {
    if (!argv0) return 0x1357ull;
    const char *base = argv0;
    for (const char *p = argv0; *p; p++)
        if (*p == '/') base = p + 1;
    uint64_t h = 1469598103934665603ull;
    for (const char *p = base; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 1099511628211ull;
    }
    return h;
}

static uint64_t pcache_make_id(const char *prog_host, const char *interp_host, const char *argv0) {
    uint64_t a = pcache_id_of(prog_host);
    uint64_t b = interp_host ? pcache_id_of(interp_host) : 0xABCDEFull;
    return (a ^ (b * 1099511628211ull)) ^ pcache_engine_id() ^ (pcache_argv0_id(argv0) * 0x100000001B3ull);
}

static void pcache_file(char *out, size_t n) {
    const char *dir = getenv("DDJIT_PCACHE_DIR");
    if (!dir || !dir[0]) dir = "/tmp/ddjit-pcache";
    mkdir(dir, 0700);
    snprintf(out, n, "%s/%016llx.pcache", dir, (unsigned long long)g_pc_binid);
}

// ---- deterministic library-map hints + the identity manifest ----
// load_elf (translate/x86_64/elf.c) records the two fixed images' spans when it consumes g_force_base;
// everything else revivable must come from a manifest-validated library map.
static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    if (base >= PC_INTERP_BASE) {
        g_pc_interp_lo = base;
        g_pc_interp_hi = base + span;
    } else if (base >= PC_IMG_BASE) {
        g_pc_img_lo = base;
        g_pc_img_hi = base + span;
    }
}

static int pc_gpc_fixed(uint64_t gpc) {
    return (gpc >= g_pc_img_lo && gpc < g_pc_img_hi) || (gpc >= g_pc_interp_lo && gpc < g_pc_interp_hi);
}

static int pc_gpc_in_lib(uint64_t gpc) { // in the RECORDED (cold) / RESTORED (warm) manifest?
    for (int i = 0; i < g_pc_nlib; i++)
        if (gpc >= g_pc_libs[i].base && gpc < g_pc_libs[i].base + g_pc_libs[i].len) return 1;
    return 0;
}

// Bump-allocated deterministic hint for a file-backed non-fixed guest mmap. 2 MB-aligned spans with a
// 2 MB hole between neighbours; deterministic because the SAME binary issues the SAME ordered sequence
// of library maps. Beyond the window (or when the cache is off): 0 = no hint, kernel placement.
static uint64_t pcache_mmap_hint(uint64_t len) {
    if (!g_pcache) return 0;
    uint64_t span = ((len + 0x1fffffull) & ~0x1fffffull) + 0x200000ull;
    uint64_t a = __atomic_fetch_add(&g_pc_lib_next, span, __ATOMIC_RELAXED);
    if (a + span > PC_LIB_BASE + PC_LIB_SPAN) return 0;
    return a;
}

#define PCACHE_MMAP_HINT 1

static uint64_t pc_fd_id(int fd) { // file identity for the manifest (dev/ino/size/mtime; no path needed)
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) return 0;
    uint64_t h = 1469598103934665603ull;
    uint64_t fields[5] = {(uint64_t)st.st_dev, (uint64_t)st.st_ino, (uint64_t)st.st_size,
                          (uint64_t)st.st_mtimespec.tv_sec, (uint64_t)st.st_mtimespec.tv_nsec};
    for (int i = 0; i < 5; i++) {
        h ^= fields[i];
        h *= 1099511628211ull;
    }
    return h;
}

// Called by mem.c after a hinted file-backed mmap SUCCEEDED AT ITS HINT (r == hint). Cold epoch: record
// the mapping in the manifest so its blocks persist. Warm epoch: this is the activation gate -- if the
// mapped file's identity matches the manifest entry restored for this base, the deferred blocks in
// [base, base+len) become live (map_put); otherwise they are dropped (different lib/layout -> a restored
// translation must never shadow different guest bytes).
static void pcache_note_libmap(uint64_t base, uint64_t len, int fd) {
    if (!g_pcache) return;
    uint64_t id = pc_fd_id(fd);
    if (!id) return;
    if (!g_pcache_loaded) { // cold epoch: record for save
        if (g_pc_nlib < PC_LIB_MAX) {
            g_pc_libs[g_pc_nlib].base = base;
            g_pc_libs[g_pc_nlib].len = len;
            g_pc_libs[g_pc_nlib].id = id;
            g_pc_nlib++;
        }
        return;
    }
    if (!g_pc_ndefer) return; // warm epoch, nothing deferred (or all activated/dropped already)
    for (int i = 0; i < g_pc_nlib; i++) {
        if (g_pc_libs[i].base != base) continue;
        if (g_pc_libs[i].id != id || g_pc_libs[i].len != len)
            return; // identity drifted: leave deferred (never activates)
        // Activate every deferred block in this range. map_put is a shared-map mutation: take the
        // translation lock when guest threads exist (same discipline as the dispatcher).
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        for (uint64_t j = 0; j < g_pc_ndefer; j++) {
            uint64_t gpc = g_pc_defer[j].gpc;
            if (gpc >= base && gpc < base + len && g_pc_defer[j].host_off) {
                map_put(gpc, g_cache + g_pc_defer[j].host_off, g_cache + g_pc_defer[j].body_off);
                g_pc_defer[j].host_off = 0; // consumed
                g_pc_activated++;
            }
        }
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        return;
    }
}

// ---- warm-stat sidecar ----
static void pcache_warm_file(char *out, size_t n) {
    char p[1024];
    pcache_file(p, sizeof p);
    snprintf(out, n, "%s.warm", p);
}

// Should this load be skipped as dead weight? Only when a PREVIOUS warm run of the SAME file generation
// reported that it re-translated at least half of what the restore brought in.
static int pcache_warm_should_skip(const struct pc_hdr *h) {
    char wp[1200];
    pcache_warm_file(wp, sizeof wp);
    int fd = open(wp, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return 0;
    struct pc_warm w;
    struct stat st;
    int ok = fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == geteuid() && !(st.st_mode & 022) &&
             st.st_size == (off_t)sizeof w && read(fd, &w, sizeof w) == (ssize_t)sizeof w;
    close(fd);
    if (!ok || w.magic != PC_WARM_MAGIC) return 0;
    if (w.arena_used != h->arena_used || w.restored != h->n_mapent) return 0; // different file generation
    if (w.waste > JIT_MAP_N || w.restored > JIT_MAP_N) return 0;              // implausible: ignore
    return w.restored && w.waste * 2 >= w.restored;
}

// Record this warm run's revival stats (called instead of a save when the epoch was loaded). waste =
// restored entries that never became usable: deferred library blocks whose image never mapped with the
// matching identity at the recorded base (layout drift / changed lib). Fresh translations of NEW code do
// NOT count against the restore (a bigger workload under the same key must not poison the policy). A
// wholesale flush mid-run dropped the restored blocks -> the restore was oversized for this guest anyway,
// report it fully dead so later runs skip.
static void pcache_warm_note(void) {
    if (!g_pc_binid || !g_pc_restored_n || g_pcache_forked) return;
    uint64_t used = g_pc_live_n + g_pc_activated;
    uint64_t waste = (g_pc_flushed || used > g_pc_restored_n) ? g_pc_restored_n : g_pc_restored_n - used;
    struct pc_warm w = {PC_WARM_MAGIC, g_pc_restored_arena, g_pc_restored_n, waste};
    char wp[1200], tmp[1280];
    pcache_warm_file(wp, sizeof wp);
    snprintf(tmp, sizeof tmp, "%s.%d.tmp", wp, (int)getpid());
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return;
    int ok = write(fd, &w, sizeof w) == (ssize_t)sizeof w;
    close(fd);
    if (ok)
        rename(tmp, wp);
    else
        unlink(tmp);
    if (g_coldprof)
        fprintf(stderr, "[pcache] warm-note restored=%llu waste=%llu%s\n", (unsigned long long)g_pc_restored_n,
                (unsigned long long)waste, waste * 2 >= g_pc_restored_n ? " (dead-weight: next run skips)" : "");
}

// Rewrite every recorded host-pointer slot for THIS process. Every baked pointer lives in this PIE
// binary's image, which dyld slides as one unit, so a single delta -- (block_return now) minus
// (block_return at save time) -- relocates them ALL. We reconstruct each slot's saved value (and its
// destination register) from the existing movz/movk encoding, add the slide, and re-emit, so we don't
// need to remember which global each slot held.
static void pcache_relocate(uint64_t saved_block_return) {
    uint64_t slide = (uint64_t)block_return - saved_block_return;
    for (int i = 0; i < g_nreloc; i++) {
        uint32_t *p = (uint32_t *)(g_cache + g_reloc[i].off);
        int rd = (int)(p[0] & 0x1f); // movz/movk encode rd in bits[4:0] (same in all 4 words)
        uint64_t old = (uint64_t)((p[0] >> 5) & 0xffff) | ((uint64_t)((p[1] >> 5) & 0xffff) << 16) |
                       ((uint64_t)((p[2] >> 5) & 0xffff) << 32) | ((uint64_t)((p[3] >> 5) & 0xffff) << 48);
        uint64_t v = old + slide;
        p[0] = 0xD2800000u | (0 << 21) | (((uint32_t)(v) & 0xffff) << 5) | rd;       // movz
        p[1] = 0xF2800000u | (1 << 21) | (((uint32_t)(v >> 16) & 0xffff) << 5) | rd; // movk #16
        p[2] = 0xF2800000u | (2 << 21) | (((uint32_t)(v >> 32) & 0xffff) << 5) | rd; // movk #32
        p[3] = 0xF2800000u | (3 << 21) | (((uint32_t)(v >> 48) & 0xffff) << 5) | rd; // movk #48
    }
}

// Returns 1 on a cache hit (arena + maps restored, translation can be skipped). On ANY mismatch,
// truncation, short read, or allocation failure it returns 0 (graceful MISS -> caller translates fresh).
static int pcache_load(uint64_t entry_jump) {
    if (g_force_base_failed) return 0; // #210: fixed-base map fell back -> live layout != file's baked base
    char path[1024];
    pcache_file(path, sizeof path);
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return 0;
    // Trust gate (parity with the aarch64 pcache): a regular file, owned by us, not group/world-writable
    // (the cache dir may live in /tmp).
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 022)) {
        close(fd);
        return 0;
    }
    struct pc_hdr h;
    if (read(fd, &h, sizeof h) != (ssize_t)sizeof h) {
        close(fd);
        return 0;
    }
    if (h.magic != PC_MAGIC || h.version != PC_VERSION_EFF || h.cpu_sz != sizeof(struct cpu) || h.map_n != JIT_MAP_N ||
        h.ibtc_n != IBTC_N || h.img_base != PC_IMG_BASE || h.interp_base != PC_INTERP_BASE || h.bin_id != g_pc_binid ||
        h.entry_jump != entry_jump || h.arena_used > CACHE_SZ || h.n_mapent > JIT_MAP_N || h.n_pend > (1u << 16) ||
        h.n_reloc > PC_RELOC_CAP || h.n_lib > PC_LIB_MAX) { // n_reloc bound tracks the g_reloc cap
        close(fd);
        return 0;
    }
    // a previous warm run of this same generation proved the restore is dead weight -> skip it
    // (header-only read: no arena I/O, no map pollution, no resave churn). The file stays for the day
    // the layout becomes deterministic again (engine update re-keys everything anyway).
    if (pcache_warm_should_skip(&h)) {
        close(fd);
        g_pcache_skip = 1;
        if (g_coldprof) fprintf(stderr, "[pcache] SKIP (previous warm run re-translated >=1/2 of restore)\n");
        return 0;
    }
    // pull the variable-size sections
    struct pc_mapent *me = h.n_mapent ? malloc(h.n_mapent * sizeof *me) : NULL;
    struct pc_pend *pe = h.n_pend ? malloc(h.n_pend * sizeof *pe) : NULL;
    uint8_t *abuf = h.arena_used ? malloc(h.arena_used) : NULL;
    int ok = (h.n_mapent == 0 || me) && (h.n_pend == 0 || pe) && (h.arena_used == 0 || abuf);
    if (ok && h.n_reloc)
        ok = read(fd, g_reloc, h.n_reloc * sizeof g_reloc[0]) == (ssize_t)(h.n_reloc * sizeof g_reloc[0]);
    if (ok && h.n_mapent) ok = read(fd, me, h.n_mapent * sizeof *me) == (ssize_t)(h.n_mapent * sizeof *me);
    if (ok && h.n_pend) ok = read(fd, pe, h.n_pend * sizeof *pe) == (ssize_t)(h.n_pend * sizeof *pe);
    if (ok && h.n_lib)
        ok = read(fd, g_pc_libs, h.n_lib * sizeof g_pc_libs[0]) == (ssize_t)(h.n_lib * sizeof g_pc_libs[0]);
    // Arena bytes: read into a heap buffer -- checksummed below BEFORE anything is trusted, and only then
    // memcpy'd into the W^X arena under write mode. (read()'s kernel copyout cannot target a MAP_JIT page
    // gated by the thread's W^X state; a userspace memcpy can, once the write window is open.)
    for (uint64_t got = 0; ok && got < h.arena_used;) {
        ssize_t r = read(fd, abuf + got, h.arena_used - got);
        if (r <= 0) {
            ok = 0;
            break;
        }
        got += (uint64_t)r;
    }
    close(fd);
    // v6: whole-payload checksum BEFORE trusting any record (bit rot / short file / foreign writer) --
    // the same validation-before-trust discipline as the aarch64 pcache.
    if (ok) {
        uint64_t cs = 1469598103934665603ull;
        cs = pc_fnv(cs, g_reloc, h.n_reloc * sizeof g_reloc[0]);
        cs = pc_fnv(cs, me, h.n_mapent * sizeof *me);
        cs = pc_fnv(cs, pe, h.n_pend * sizeof *pe);
        cs = pc_fnv(cs, g_pc_libs, h.n_lib * sizeof g_pc_libs[0]);
        cs = pc_fnv(cs, abuf, h.arena_used);
        ok = cs == h.csum;
    }
    // Bounds-validate every record whose offset a later pass writes or branches through.
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = (uint64_t)g_reloc[i].off + 16 <= h.arena_used;
    for (uint64_t i = 0; ok && i < h.n_mapent; i++)
        ok = me[i].host_off < h.arena_used && me[i].body_off < h.arena_used;
    for (uint64_t i = 0; ok && i < h.n_pend; i++)
        ok = pe[i].slot_off + 4 <= h.arena_used;
    if (!ok) {
        free(me);
        free(pe);
        free(abuf);
        g_pc_nlib = 0;
        return 0;
    }
    pthread_jit_write_protect_np(0);
    memcpy(g_cache, abuf, h.arena_used);
    pthread_jit_write_protect_np(1);
    free(abuf);
    // rebuild the engine state from the offset-relative records. fixed-image blocks (main+interp,
    // identity-validated by the cache key itself) go live NOW; manifest (library) blocks are DEFERRED
    // until the guest maps the same file identity at the same base (pcache_note_libmap); anything else
    // is unrevivable and dropped (belt-and-braces: the save side never persists such entries).
    g_nreloc = (int)h.n_reloc;
    g_pc_nlib = (int)h.n_lib;
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    uint64_t nlive = 0, ndefer = 0;
    for (uint64_t i = 0; i < h.n_mapent; i++) {
        if (pc_gpc_fixed(me[i].gpc)) {
            map_put(me[i].gpc, g_cache + me[i].host_off, g_cache + me[i].body_off);
            nlive++;
        } else if (pc_gpc_in_lib(me[i].gpc)) {
            ndefer++;
        }
    }
    if (ndefer) {
        g_pc_defer = malloc(ndefer * sizeof *g_pc_defer);
        if (g_pc_defer) {
            for (uint64_t i = 0; i < h.n_mapent; i++)
                if (!pc_gpc_fixed(me[i].gpc) && pc_gpc_in_lib(me[i].gpc)) g_pc_defer[g_pc_ndefer++] = me[i];
        }
    }
    g_npend = 0;
    for (uint64_t i = 0; i < h.n_pend; i++)
        add_pend2((uint32_t *)(g_cache + pe[i].slot_off), pe[i].target, (int)pe[i].is_bl);
    g_cp = g_cache + h.arena_used;
    free(me);
    free(pe);
    // re-slide every baked PIE host pointer for THIS process + publish the restored code to the i-cache
    pthread_jit_write_protect_np(0);
    pcache_relocate(h.block_return_at);
    pthread_jit_write_protect_np(1);
    sys_icache_invalidate(g_cache, h.arena_used);
    memset(g_ibtc, 0, sizeof g_ibtc); // runtime cache: repopulates lazily
    g_pcache_loaded = 1;
    g_pc_restored_n = nlive + g_pc_ndefer; // what the warm-stat measures waste against
    g_pc_restored_arena = h.arena_used;
    g_pc_live_n = nlive;
    g_pc_activated = 0;
    g_pc_flushed = 0;
    if (g_coldprof)
        fprintf(stderr, "[pcache] restore live=%llu deferred-lib=%llu dropped=%llu\n", (unsigned long long)nlive,
                (unsigned long long)g_pc_ndefer, (unsigned long long)(h.n_mapent - nlive - g_pc_ndefer));
    return 1;
}

// Persist the current arena + maps (atomic temp+rename). Called at guest exit AND at execve,
// right before the exec flushes the arena -- each image epoch persists under its OWN key exactly once.
static void pcache_save(void) {
    if (!g_pcache || !g_pc_binid || g_cp == g_cache) return;
    if (g_force_base_failed) return; // #210: mixed-base arena (a fixed-VA image map fell back) -> not revivable
    if (g_pcache_poison) return; // arena has un-recorded baked host pointers -> not safely relocatable
    // NEVER save from a fork child. jit_after_fork rebuilt a FRESH EMPTY arena in the child, but
    // the g_reloc table (and binid/entry identity) survived the fork -- a child save would persist the
    // PARENT's reloc offsets against the child's re-translated arena, and the next load's relocation pass
    // would stomp fixed-slot rewrites over live code at those stale offsets (intermittent SIGSEGV/hang,
    // the aarch64 concurrent-hit crash). The child's arena is only the post-fork slice anyway.
    // (An in-process execve fully re-keys + resets this state -- pcache_exec_reload -- so a forked child
    // that exec'd a new image saves that image normally: the go-build storm case.)
    if (g_pcache_forked) return;
    // NEVER re-save after a load. A warm run keeps translating -- notably tier-2 hot-block recompiles,
    // which re-emit into the arena WITHOUT a map entry (g_tier2_build) -- so g_cp (arena_used) grows every
    // run. Re-persisting that snowballs the on-disk arena across sequential `--rm` runs of the same image
    // (~5 MB/run here) until it overflows CACHE_SZ; the next load's `arena_used > CACHE_SZ` check then makes
    // it graceful-miss, but before that the in-memory arena has already run past its end -> silent guest
    // SIGSEGV (docker rc 255, empty output, no daemon-log error). The cold (miss) arena already covers the
    // startup/common path; any block missing from it is simply re-translated in-memory on each warm run
    // (correct, just not cached). So we persist exactly once, on the cold miss, and never grow the file.
    // instead of a no-op, a loaded epoch records its revival stats for the restore-or-skip policy.
    if (g_pcache_loaded) {
        pcache_warm_note();
        return;
    }
    if (g_pcache_skip) return; // we intentionally skipped the restore; re-saving would churn the file
    uint64_t _t0 = g_coldprof ? coldprof_now_ns() : 0;
    // count occupied map slots that are REVIVABLE (fixed images + manifest libs). Anything else is keyed
    // by a kernel-chosen address that the next run won't reproduce -- persisting it would be dead weight
    // at best, and a stale-translation hazard if the next run reuses the address range for other code.
    uint64_t nmap = 0;
    for (uint32_t i = 0; i < JIT_MAP_N; i++)
        if (g_map[i].host && (pc_gpc_fixed(g_map[i].gpc) || pc_gpc_in_lib(g_map[i].gpc))) nmap++;
    char path[1024], tmp[1056];
    pcache_file(path, sizeof path);
    snprintf(tmp, sizeof tmp, "%s.%d.tmp", path, (int)getpid());
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return;
    struct pc_hdr h;
    memset(&h, 0, sizeof h);
    h.magic = PC_MAGIC;
    h.version = PC_VERSION_EFF;
    h.cpu_sz = sizeof(struct cpu);
    h.map_n = JIT_MAP_N;
    h.ibtc_n = IBTC_N;
    h.img_base = PC_IMG_BASE;
    h.interp_base = PC_INTERP_BASE;
    h.bin_id = g_pc_binid;
    h.entry_jump = g_pc_entry;
    h.arena_used = (uint64_t)(g_cp - g_cache);
    h.n_mapent = nmap;
    h.n_pend = (uint64_t)g_npend;
    h.n_reloc = (uint64_t)g_nreloc;
    h.n_lib = (uint64_t)g_pc_nlib;
    h.block_return_at = (uint64_t)block_return;
    h.ibtc_at = (uint64_t)g_ibtc;
    // Build the whole image in one heap buffer and write it with a single syscall (per-record write()s
    // were ~1300 syscalls and dominated the one-time save cost).
    size_t total = sizeof h + (size_t)g_nreloc * sizeof g_reloc[0] + (size_t)nmap * sizeof(struct pc_mapent) +
                   (size_t)g_npend * sizeof(struct pc_pend) + (size_t)g_pc_nlib * sizeof(struct pc_lib) + h.arena_used;
    uint8_t *buf = malloc(total), *w = buf;
    int ok = buf != NULL;
    if (ok) {
        w += sizeof h; // header written last: its csum covers everything after it
        memcpy(w, g_reloc, (size_t)g_nreloc * sizeof g_reloc[0]);
        w += (size_t)g_nreloc * sizeof g_reloc[0];
        for (uint32_t i = 0; i < JIT_MAP_N; i++) {
            if (!g_map[i].host || !(pc_gpc_fixed(g_map[i].gpc) || pc_gpc_in_lib(g_map[i].gpc))) continue;
            struct pc_mapent e = {g_map[i].gpc, (uint64_t)((uint8_t *)g_map[i].host - g_cache),
                                  (uint64_t)((uint8_t *)g_map[i].body - g_cache)};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_npend; i++) {
            struct pc_pend e = {(uint64_t)((uint8_t *)g_pend[i].slot - g_cache), g_pend[i].target,
                                (uint32_t)g_pend[i].is_bl};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        memcpy(w, g_pc_libs, (size_t)g_pc_nlib * sizeof(struct pc_lib));
        w += (size_t)g_pc_nlib * sizeof(struct pc_lib);
        memcpy(w, g_cache, h.arena_used); // read from W^X arena is always permitted
        w += h.arena_used;
        h.csum = pc_fnv(1469598103934665603ull, buf + sizeof h, total - sizeof h);
        memcpy(buf, &h, sizeof h);
        ok = write(fd, buf, total) == (ssize_t)total;
    }
    free(buf);
    close(fd);
    if (ok) {
        rename(tmp, path);
        char wp[1200];
        pcache_warm_file(wp, sizeof wp);
        unlink(wp); // fresh generation: any old warm-stat sidecar no longer describes this file
    } else
        unlink(tmp);
    if (g_coldprof)
        fprintf(stderr, "[pcache] save %s (%llu B arena, %llu blocks, %d libs) in %.3f ms\n", ok ? "ok" : "FAILED",
                (unsigned long long)h.arena_used, (unsigned long long)nmap, g_pc_nlib, (coldprof_now_ns() - _t0) / 1e6);
}

// hygiene hooks (shared proc.c / engine/dispatch.c call these on both engines):
//  - fork child: its arena is now a fork-private slice (kept warm by the preserved-arena fork, or
//    fresh from the threaded rebuild) under the PARENT's identity -> drop the inherited reloc records and
//    bar this process from saving (see the g_pcache_forked comment in pcache_save).
//  - wholesale cache-full flush: the arena content the records described is gone -> reset so records stay
//    in lockstep with what is re-emitted (every baked pointer recorded, by construction).
static void pcache_after_fork(void) {
    g_nreloc = 0;
    g_pcache_forked = 1;
}

#define PCACHE_FORK_HOOK pcache_after_fork()

static void pcache_after_wholesale_flush(void) {
    g_nreloc = 0;
    g_pc_flushed = 1; // any restored blocks are gone; the warm-stat must report the restore dead
    g_pc_ndefer = 0;  // deferred entries pointed into the dropped arena content
}

#define PCACHE_FLUSH_HOOK pcache_after_wholesale_flush()

// ---- guest execve (proc.c case 221) hooks ----
// An in-process exec re-loads a NEW image after flushing the arena; the cache identity re-keys with it.
// proc.c calls PCACHE_SAVE_HOOK (above) BEFORE the flush -- persisting the outgoing image under the
// OUTGOING key -- then these force the new image onto the fixed bases and re-key + reload. This is what
// makes a wrong-key save impossible: g_pc_binid is only ever assigned in lockstep with an arena reset.
static void pcache_exec_force_main(void) {
    if (g_pcache) {
        g_force_base = PC_IMG_BASE;
        g_pc_img_lo = g_pc_img_hi = g_pc_interp_lo = g_pc_interp_hi = 0; // re-recorded by load_elf
    }
}

static void pcache_exec_force_interp(void) {
    if (g_pcache) g_force_base = PC_INTERP_BASE;
}

static void pcache_exec_reload(const char *prog_host, const char *interp_host, const char *argv0, uint64_t jump) {
    if (!g_pcache) return;
    // execve is a full identity + arena reset (thread_exit_others ran; the old image was unmapped and the
    // arena/map/ibtc flushed by case 221), so the recording state resets with it and saving becomes safe
    // again -- including for a fork child (the fork+execve toolchain storm is exactly the case we cache).
    g_nreloc = 0;
    g_pcache_loaded = 0;
    g_pcache_forked = 0;
    g_pcache_skip = 0;
    g_pc_flushed = 0;
    g_pc_restored_n = g_pc_restored_arena = 0;
    g_pc_live_n = g_pc_activated = 0;
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    g_pc_nlib = 0;
    __atomic_store_n(&g_pc_lib_next, PC_LIB_BASE, __ATOMIC_RELAXED); // fresh image, fresh hint sequence
    g_pc_binid = pcache_make_id(prog_host, interp_host, argv0);
    g_pc_entry = jump;
    int hit = pcache_load(jump);
    if (g_coldprof) fprintf(stderr, "[pcache] exec %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
}

#define PCACHE_EXEC_HOOKS 1

#define PCACHE_SAVE_HOOK pcache_save()
