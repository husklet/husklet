// dd/runtime/translate/aarch64 -- persistent cross-process translated-code cache for the aarch64
// engine. Mirror of the x86 opt8 pcache (translate/x86_64/pcache.c), adapted to the two ways the
// aarch64 engine differs from the x86 frontend:
//
//   1. NO reloc centralization. The x86 emitter routes every baked host pointer through one
//      emit_host_ptr() into a g_reloc table; the aarch64 emitter bakes host addresses inline
//      (block_return via e_movconst, &g_ibtc / &g_t2cnt[] via adrp+add) with no record. So this file
//      provides the recorded emitters (emit_blockret / emit_ibtcptr / emit_t2cntptr / pc_record_icsite)
//      that engine/stubs.c + translate/aarch64/translate.c call. When g_pcache is OFF they fall back to
//      the exact original emitters, so the default correctness matrix stays byte-identical.
//
//   2. DUAL-MAPPED W^X arena. The engine writes g_cache (RW alias) and executes g_cache+g_rw2rx (RX
//      alias). We memcpy the persisted bytes through the RW alias inside the single jit_wprot() write
//      window (a no-op under dual mapping) and sys_icache_invalidate() the RX alias.
//
// WHAT MAKES THE BYTES REUSABLE ACROSS PROCESSES (esp. the go-build fork+execve storm, where each
// forked child re-loads a toolchain binary IN-PROCESS -- proc.c case 221 -- from a COLD, freshly
// jit_after_fork()'d arena):
//   * The guest image + interp are mapped at FIXED VAs (g_force_base = PC_IMG_BASE / PC_INTERP_BASE), so
//     every guest PC (block-map key) and every guest address baked into host code (pcrel_base literals,
//     non-PIE ranges) is stable across runs -- the emitted BYTES are identical. If either fixed mapping
//     ever fails (g_force_base_failed), the run neither loads nor saves: a mixed-base arena must not mix
//     with fixed-base files.
//   * The only ABSOLUTE HOST addresses baked into a block live in this engine binary and are recorded in
//     g_reloc: block_return (RK_BLOCKRET), &g_ibtc (RK_IBTC), &g_t2cnt[slot] (RK_T2CNT). On load we
//     RE-EMIT each with THIS process's live symbol (a fixed 4-insn movz/movk), so ASLR of the engine is
//     irrelevant -- no slide, no fixed engine base needed. Arena-internal absolute pointers are avoided:
//     block chaining + per-site IC guards are PC-relative (alias/position-invariant); the per-site IC's
//     cached {target,body} literals are NEUTRALIZED on load (RK_ICSITE zeroes the pair so the guard never
//     matches a stale body -- the site refills at runtime), and the shared g_ibtc data table is zeroed.
//   * The tier-2 back-edge counters (&g_t2cnt[slot]) are per-process BSS; we persist the g_t2gpc/g_t2cnt
//     slot arrays so a restored counter still promotes the RIGHT loop, and RK_T2CNT re-points the bake.
//   * The SMC precise-gate page set (g_txpg) is persisted and re-inserted on load: a warm run's guest
//     `ic ivau` against a page we restored blocks from MUST still take the conservative wholesale drop,
//     or the run would keep executing a stale translation of code the guest just rewrote.
//
// THE DISCIPLINE (x86 pcache hardening, replicated + extended here):
//   * POISON-ON-OVERFLOW: a persisted arena MUST have EVERY baked host pointer recorded, or a reload
//     keeps a stale absolute address -> intermittent ASLR-dependent SIGSEGV. We poison (refuse to save)
//     when the g_reloc table overflows, and when a NON-default codegen mode that bakes an unrecorded
//     host pointer is active (PROF).
//   * NEVER RE-SAVE AFTER LOAD: a warm run keeps translating (tier-2 recompiles, on-demand blocks), so
//     re-persisting would snowball the file past CACHE_SZ across runs (the x86 overflow-SIGSEGV).
//     We persist exactly once, on the cold miss.
//   * NEVER SAVE FROM A FORK CHILD (the concurrent-crash root cause, new): jit_after_fork gives
//     the child a FRESH EMPTY arena but the reloc/map bookkeeping here survived the fork -- a child save
//     would persist the PARENT's reloc offsets against the child's re-translated arena, and the next
//     load's relocation pass would then stomp 16-byte movz/movk sequences over live code at those stale
//     offsets -> SIGSEGV/hang on the next hit. PCACHE_FORK_HOOK resets the recording state in the child
//     and bars it from saving; an in-process execve (pcache_exec_reload) fully re-keys + resets state,
//     which makes saving safe again -- that is exactly the go-build fork+execve case we want cached.
//   * NEVER SAVE ACROSS A WHOLESALE FLUSH without resetting the records: the dispatcher's cache-full
//     flush (in-place or stop-the-world) drops/renews the arena, so PCACHE_FLUSH_HOOK zeroes g_nreloc;
//     everything re-emitted afterwards re-records, keeping "every baked pointer recorded" by construction.
//   * NEVER SAVE AFTER GUEST SMC (new): a guest that generated/patched code at runtime (g_smc_seen) has
//     translations of NON-file bytes in the arena; the binary-identity key cannot validate those.
//   * THREAD-SAFE SNAPSHOT (new): exit_group in a threaded guest (go compile) saves while peer threads
//     run; the snapshot is taken under g_jit_lock -- the same lock the dispatcher holds for every arena/
//     map/pend/IC mutation -- so a torn arena can never be persisted.
//
// LOAD SAFETY: the whole payload is FNV-1a checksummed, every section is bounds-checked, and every
// record is validated (reloc offsets in-arena + aligned, t2 slots in-range, map/pend offsets in-arena)
// BEFORE any of it is trusted; the cache file is opened O_NOFOLLOW and must be a regular file owned by
// us. ANY mismatch / truncation / corruption -> graceful MISS: ignore the file, translate fresh, and
// re-save (the fresh save's atomic rename self-heals the bad file). Publication is always write-temp +
// rename -- a reader never observes a partially-written file, and concurrent savers can never interleave.
//
// Keyed by (engine build id, codegen-mode env flags, cpu-struct size, map/ibtc sizes, both fixed bases,
// entry PC, argv[0] basename, and the identity -- dev/ino/size/mtime(ns) -- of the guest binary AND its
// interpreter). OPT-IN via DDJIT_PCACHE=1 (byte-identical matrix when off, exactly like the x86 pcache);
// DDJIT_NOPCACHE=1 is the kill-switch that wins over everything.

#define PC_MAGIC 0x3436414350544a44ull // "DDJTPCA4" (LE tag)
#define PC_VERSION 3 // BUMP on ANY aarch64 codegen/layout change. v3: IRQSLIM/IBSLIM layout + pend.fwd.
// IRQSLIM changes the block-entry layout (fixed 2-insn poll header + poll-free forward-chain entry at
// body+8), and it is env-toggleable (NOIRQSLIM/NOIRQCHECK/NOSTEAL1617) -- mix the LIVE mode into the
// version (mirrors the x86 pcache's PC_VERSION_EFF) so a cache written in one layout can never be
// reloaded into the other: a +8 forward chain into a legacy-layout block would land mid-instruction,
// and a legacy chain into an IRQSLIM block would double-run the poll header's cbnz target.
#define PC_VERSION_EFF (PC_VERSION | ((uint64_t)(g_fwdskip ? 0x100 : 0)))
#define PC_IMG_BASE 0x0000040000000000ull    // 4 TB -- fixed guest image base (probed free on Apple silicon)
#define PC_INTERP_BASE 0x0000048000000000ull // 4.5 TB -- fixed interp (ld.so) base
#define PC_RELOC_CAP (1u << 20)              // recorded baked-host-pointer slots (poison if exceeded)

// reloc kinds (packed into pc_reloc.info: kind<<0 | rd<<8 | slot<<16)
#define RK_BLOCKRET 1 // 4-insn movz/movk of block_return into reg `rd`
#define RK_IBTC 2     // 4-insn movz/movk of &g_ibtc into reg `rd`
#define RK_T2CNT 3    // 4-insn movz/movk of &g_t2cnt[slot] into reg `rd`
#define RK_ICSITE 4   // 16-byte per-site IC {target,body} literal pair -> zero on load (neutralize)

struct pc_reloc {
    uint32_t off;  // arena byte offset of the first word (movz/movk kinds) or the literal pair (ICSITE)
    uint32_t info; // kind | (rd<<8) | (slot<<16)
};

// ---- engine state (defined here; used by the recorded emitters + load/save) ----
static int g_pcache;            // persistent cache active (DDJIT_PCACHE=1; DDJIT_NOPCACHE=1 kills)
static int g_coldprof;          // COLDPROF=1: print pcache hit/miss + save timing
static uint64_t g_force_base;   // if nonzero, load_elf() maps the NEXT image MAP_FIXED here (one-shot; elf.c)
static int g_force_base_failed; // a fixed-VA map fell back to a kernel base -> this image can't hit OR save
static uint64_t g_pc_binid;     // identity of the guest binary+interp+argv0+engine+mode (cache file key)
static uint64_t g_pc_entry;     // initial guest pc (sanity key)
static int g_pcache_poison;     // an unrecorded baked host pointer may exist -> save() refuses (correctness)
static int g_pcache_loaded;     // this run restored from cache -> never re-save (arena would snowball)
static int g_pcache_forked;     // this process is a fork child -> never save (stale-bookkeeping guard);
                                // cleared by pcache_exec_reload (execve fully re-keys + resets the records)
static struct pc_reloc g_reloc[PC_RELOC_CAP];
static int g_nreloc;

static void block_return(void); // engine trampoline (also forward-declared in engine/stubs.c)

static void pc_reloc_add(uint32_t off, uint8_t kind, uint8_t rd, uint16_t slot) {
    if (g_nreloc < (int)PC_RELOC_CAP) {
        g_reloc[g_nreloc].off = off;
        g_reloc[g_nreloc].info = (uint32_t)kind | ((uint32_t)rd << 8) | ((uint32_t)slot << 16);
        g_nreloc++;
    } else {
        g_pcache_poison = 1; // table full -> a baked pointer would go unrecorded; refuse to persist
    }
}

// Fixed 4-insn absolute materialization (movz + 3*movk). Fixed length so a reload can re-emit into the
// SAME reserved bytes regardless of the value's high-lane sparsity (unlike e_movconst, which is variable).
static void emit_hostptr48(int rd, uint64_t v) {
    e_movz(rd, (uint32_t)(v & 0xffff), 0);
    e_movk(rd, (uint32_t)((v >> 16) & 0xffff), 1);
    e_movk(rd, (uint32_t)((v >> 32) & 0xffff), 2);
    e_movk(rd, (uint32_t)((v >> 48) & 0xffff), 3);
}

// Recorded emitters. When the cache is OFF they fall back to the exact original emitter (matrix stays
// byte-identical); when ON they use the fixed 4-insn form + record the slot for relocation on load.
static void emit_blockret(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_BLOCKRET, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)block_return);
    } else {
        e_movconst(rd, (uint64_t)block_return);
    }
}

static void emit_ibtcptr(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_IBTC, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)g_ibtc);
    } else {
        e_adrp_add(rd, (uint64_t)g_ibtc);
    }
}

static void emit_t2cntptr(int rd, int slot) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_T2CNT, (uint8_t)rd, (uint16_t)slot);
        emit_hostptr48(rd, (uint64_t)&g_t2cnt[slot]);
    } else {
        e_adrp_add(rd, (uint64_t)&g_t2cnt[slot]);
    }
}

// Record a per-site IC's 16-byte cached {target,body} literal pair so a reload can zero it (the cached
// body pointer is an arena address that would be stale in a fresh process; a zeroed guard never matches
// -> the site harmlessly re-resolves through the dispatcher, which rewrites both literals).
static void pc_record_icsite(uint8_t *lt) {
    if (g_pcache) pc_reloc_add((uint32_t)(lt - g_cache), RK_ICSITE, 0, 0);
}

// ---- persisted layout ----
// [pc_hdr][n_reloc pc_reloc][n_mapent pc_mapent][n_pend pc_pend][n_t2 pc_t2][n_txpg u64][arena bytes]
struct pc_hdr {
    uint64_t magic, version;
    uint64_t cpu_sz, jit_map_n, ibtc_n;
    uint64_t img_base, interp_base;
    uint64_t bin_id, entry_jump;
    uint64_t arena_used;
    uint64_t n_reloc, n_mapent, n_pend, n_t2, n_txpg;
    uint64_t csum;                     // FNV-1a over every byte after this header
    uint64_t block_return_at, ibtc_at; // diagnostics only (we re-emit from live symbols)
};

struct pc_mapent {
    uint64_t gpc, host_off, body_off;
};

struct pc_pend {
    uint64_t slot_off, target;
    uint32_t is_bl, fwd; // fwd: IRQSLIM forward edge -> patch_links_to targets body+g_fwdskip, not body+0
};

struct pc_t2 {
    uint64_t gpc, cnt;
};

static uint64_t pc_fnv(uint64_t h, const void *buf, size_t n) {
    const uint8_t *p = (const uint8_t *)buf;
    // word-wise FNV-1a (8 bytes per step): the payload checksum runs over multi-MB arenas on every load
    // AND save, and the byte-wise loop was a measurable slice of the warm start it exists to protect.
    // Same detection strength for our threat model (accidental truncation/corruption, torn writers).
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

static uint64_t pcache_id_of(const char *path) {
    struct stat st;
    if (!path || stat(path, &st) != 0) return 0;
    uint64_t fields[5] = {(uint64_t)st.st_dev, (uint64_t)st.st_ino, (uint64_t)st.st_size,
                          (uint64_t)st.st_mtimespec.tv_sec, (uint64_t)st.st_mtimespec.tv_nsec};
    uint64_t h = pc_fnv(1469598103934665603ull, fields, sizeof fields);
    return pc_fnv(h, path, strlen(path));
}

// Per-engine-build tag so the cache self-invalidates across dd rebuilds (host bytes from a different build
// must never load). __DATE__/__TIME__ change every (re)build -> a user who updates dd gets a fresh cache
// transparently (old files just go unreferenced; harmless cruft in the cache dir).
static uint64_t pcache_engine_id(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    return pc_fnv(1469598103934665603ull, tag, sizeof tag - 1);
}

// Codegen-mode env flags that change the EMITTED BYTES (A/B kill-switches). Folded into the cache id so
// an arena saved under one mode is never loaded under another (each mode gets its own cache file).
static uint64_t pcache_mode_id(void) {
    static const char *envs[] = {"NOSTEAL1617", "NOSTEALFAST", "NOGUESTFOLD", "NOSHADOWTUNE", "SHADOWGATE",
                                 "NOSTITCH", "NOLSE", "NOTIER2",
                                 // perf-wave-2 codegen toggles: IRQSLIM (2-insn poll header + body+8
                                 // forward entries; also mixed into PC_VERSION_EFF via the LIVE
                                 // g_fwdskip), IBSLIM (set_x30 + hash_tail shapes).
                                 "NOIRQSLIM", "NOIRQCHECK", "NOIBSLIM"};
    uint64_t h = 1469598103934665603ull;
    for (size_t i = 0; i < sizeof envs / sizeof envs[0]; i++) {
        const char *v = getenv(envs[i]);
        h = pc_fnv(h, envs[i], strlen(envs[i]));
        h = pc_fnv(h, v ? v : "\x01", v ? strlen(v) : 1);
    }
    return h;
}

// Hash the BASENAME of argv[0]. A multicall binary (busybox, toolchain drivers) runs DIFFERENT code
// paths per argv[0]; the translated arena is therefore per-applet, so the cache MUST be keyed by argv[0]
// too or one applet loads another's arena. Basename (not full argv) so a single-purpose binary invoked
// with varying flags -- e.g. go's `compile -o pkgN.a -p pkgN ...` -- keeps ONE cache reused across all
// its invocations (the go-build fork-storm win).
static uint64_t pcache_argv0_id(const char *argv0) {
    if (!argv0) return 0x1357ull;
    const char *base = argv0;
    for (const char *p = argv0; *p; p++)
        if (*p == '/') base = p + 1;
    return pc_fnv(1469598103934665603ull, base, strlen(base));
}

static uint64_t pcache_make_id(const char *prog_host, const char *interp_host, const char *argv0) {
    uint64_t a = pcache_id_of(prog_host);
    uint64_t b = interp_host ? pcache_id_of(interp_host) : 0xABCDEFull;
    return (a ^ (b * 1099511628211ull)) ^ pcache_engine_id() ^ (pcache_argv0_id(argv0) * 0x100000001B3ull) ^
           (pcache_mode_id() * 0x9E3779B97F4A7C15ull);
}

static void pcache_file(char *out, size_t n) {
    const char *dir = getenv("DDJIT_PCACHE_DIR");
    if (!dir || !dir[0]) dir = "/tmp/ddjit-pcache-arm64";
    mkdir(dir, 0700);
    snprintf(out, n, "%s/%016llx.pcache", dir, (unsigned long long)g_pc_binid);
}

// Re-emit / neutralize every recorded slot for THIS process. Runs inside the jit_wprot() write window,
// against the RW alias (g_cache + off). Offsets/slots were validated by pcache_load before we get here.
static void pcache_relocate(void) {
    for (int i = 0; i < g_nreloc; i++) {
        uint32_t off = g_reloc[i].off, info = g_reloc[i].info;
        int kind = info & 0xff, rd = (info >> 8) & 0xff, slot = (info >> 16) & 0xffff;
        uint32_t *w = (uint32_t *)(g_cache + off);
        uint64_t v;
        switch (kind) {
        case RK_BLOCKRET: v = (uint64_t)block_return; break;
        case RK_IBTC: v = (uint64_t)g_ibtc; break;
        case RK_T2CNT: v = (uint64_t)&g_t2cnt[slot]; break;
        case RK_ICSITE:
            // zero both cached literals (target at +0, body at +8): the body is a stale arena pointer, and
            // a zeroed target makes the site's equality guard miss so the dispatcher re-resolves + refills.
            *(uint64_t *)(g_cache + off) = 0;
            *(uint64_t *)(g_cache + off + 8) = 0;
            continue;
        default: continue;
        }
        w[0] = 0xD2800000u | (((uint32_t)(v) & 0xffff) << 5) | rd;                    // movz rd, #v[0:16]
        w[1] = 0xF2800000u | (1u << 21) | (((uint32_t)(v >> 16) & 0xffff) << 5) | rd; // movk #16
        w[2] = 0xF2800000u | (2u << 21) | (((uint32_t)(v >> 32) & 0xffff) << 5) | rd; // movk #32
        w[3] = 0xF2800000u | (3u << 21) | (((uint32_t)(v >> 48) & 0xffff) << 5) | rd; // movk #48
    }
}

// Validate one reloc record against the arena bounds. 16 bytes are rewritten for every kind (4 insns or
// the literal pair), so the whole window must be inside the restored arena and naturally aligned.
static int pc_reloc_ok(struct pc_reloc r, uint64_t arena_used) {
    int kind = r.info & 0xff, slot = (r.info >> 16) & 0xffff;
    if ((uint64_t)r.off + 16 > arena_used) return 0;
    if (kind == RK_ICSITE) return (r.off & 7) == 0;
    if (r.off & 3) return 0;
    if (((r.info >> 8) & 0xff) > 30) return 0; // rd must be a real GPR (we never bake into sp/xzr)
    if (kind == RK_T2CNT) return slot < T2_MAX;
    return kind == RK_BLOCKRET || kind == RK_IBTC;
}

// Returns 1 on HIT (arena + maps restored -> translation of the startup path is skipped). ANY mismatch /
// truncation / checksum failure / out-of-bounds record -> 0 (graceful MISS; the caller translates fresh
// and the exit-time save atomically replaces the bad file).
static int pcache_load(uint64_t entry_jump) {
    if (!g_pcache || !g_pc_binid || g_force_base_failed) return 0;
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024];
    pcache_file(path, sizeof path);
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return 0;
    // Trust gate: a regular file, owned by us, not group/world-writable (the cache dir may live in /tmp).
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
    if (h.magic != PC_MAGIC || h.version != PC_VERSION_EFF || h.cpu_sz != sizeof(struct cpu) ||
        h.jit_map_n != JIT_MAP_N || h.ibtc_n != IBTC_N || h.img_base != PC_IMG_BASE ||
        h.interp_base != PC_INTERP_BASE || h.bin_id != g_pc_binid || h.entry_jump != entry_jump ||
        h.arena_used > CACHE_SZ || (h.arena_used & 3) || h.n_reloc > PC_RELOC_CAP || h.n_mapent > JIT_MAP_N ||
        h.n_pend > (1u << 16) || h.n_t2 > T2_MAX || h.n_txpg > TXPG_N) {
        close(fd);
        return 0;
    }
    struct pc_reloc *re = h.n_reloc ? malloc(h.n_reloc * sizeof *re) : NULL;
    struct pc_mapent *me = h.n_mapent ? malloc(h.n_mapent * sizeof *me) : NULL;
    struct pc_pend *pe = h.n_pend ? malloc(h.n_pend * sizeof *pe) : NULL;
    struct pc_t2 *te = h.n_t2 ? malloc(h.n_t2 * sizeof *te) : NULL;
    uint64_t *tx = h.n_txpg ? malloc(h.n_txpg * sizeof *tx) : NULL;
    uint8_t *abuf = h.arena_used ? malloc(h.arena_used) : NULL;
    int ok = (h.n_reloc == 0 || re) && (h.n_mapent == 0 || me) && (h.n_pend == 0 || pe) && (h.n_t2 == 0 || te) &&
             (h.n_txpg == 0 || tx) && (h.arena_used == 0 || abuf);
#define PC_RD(buf, nbytes) (ok && ((nbytes) == 0 || (ok = read(fd, (buf), (nbytes)) == (ssize_t)(nbytes))))
    PC_RD(re, h.n_reloc * sizeof *re);
    PC_RD(me, h.n_mapent * sizeof *me);
    PC_RD(pe, h.n_pend * sizeof *pe);
    PC_RD(te, h.n_t2 * sizeof *te);
    PC_RD(tx, h.n_txpg * sizeof *tx);
    for (uint64_t got = 0; ok && got < h.arena_used;) {
        ssize_t r = read(fd, abuf + got, h.arena_used - got);
        if (r <= 0) {
            ok = 0;
            break;
        }
        got += (uint64_t)r;
    }
#undef PC_RD
    close(fd);
    // Whole-payload checksum BEFORE trusting any record (bit rot / short file / foreign writer).
    if (ok) {
        uint64_t cs = 1469598103934665603ull;
        cs = pc_fnv(cs, re, h.n_reloc * sizeof *re);
        cs = pc_fnv(cs, me, h.n_mapent * sizeof *me);
        cs = pc_fnv(cs, pe, h.n_pend * sizeof *pe);
        cs = pc_fnv(cs, te, h.n_t2 * sizeof *te);
        cs = pc_fnv(cs, tx, h.n_txpg * sizeof *tx);
        cs = pc_fnv(cs, abuf, h.arena_used);
        ok = cs == h.csum;
    }
    // Per-record bounds: every offset a later pass will WRITE or BRANCH through must be inside the arena.
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = pc_reloc_ok(re[i], h.arena_used);
    for (uint64_t i = 0; ok && i < h.n_mapent; i++)
        ok = me[i].host_off < h.arena_used && me[i].body_off < h.arena_used && !(me[i].host_off & 3) &&
             !(me[i].body_off & 3);
    for (uint64_t i = 0; ok && i < h.n_pend; i++)
        ok = pe[i].slot_off + 4 <= h.arena_used && !(pe[i].slot_off & 3);
    if (!ok) {
        free(re);
        free(me);
        free(pe);
        free(te);
        free(tx);
        free(abuf);
        return 0;
    }

    // Rebuild engine state from the offset-relative records.
    g_nreloc = (int)h.n_reloc;
    memcpy(g_reloc, re, h.n_reloc * sizeof *re);
    for (uint64_t i = 0; i < h.n_mapent; i++)
        map_put(me[i].gpc, g_cache + me[i].host_off, g_cache + me[i].body_off);
    g_npend = 0;
    for (uint64_t i = 0; i < h.n_pend; i++) // fwd restored too: a forward pend must patch to body+8 (IRQSLIM)
        add_pend3((uint32_t *)(g_cache + pe[i].slot_off), pe[i].target, (int)pe[i].is_bl, (int)pe[i].fwd);
    g_t2n = (int)h.n_t2;
    for (uint64_t i = 0; i < h.n_t2; i++) {
        g_t2gpc[i] = te[i].gpc;
        g_t2cnt[i] = te[i].cnt ? te[i].cnt : 1; // 0 = promotion was pending; a 0 counter would wrap, never fire
    }
    // SMC precise gate: re-mark every guest page the restored blocks were translated from, so a warm-run
    // `ic ivau` against restored code still takes the conservative wholesale drop.
    txpg_clear();
    txln_clear(); // restored blocks carry page info only; the line set stays empty ->
                  // smc_icflush's coarse page fallback (g_pcache_loaded) covers restored code
    for (uint64_t i = 0; i < h.n_txpg; i++)
        if (tx[i]) txpg_put(tx[i]);
    g_cp = g_cache + h.arena_used;
    free(re);
    free(me);
    free(pe);
    free(te);
    free(tx);

    // Commit the arena bytes + re-emit every baked host pointer, then publish to the I-cache.
    jit_wprot(0);
    memcpy(g_cache, abuf, h.arena_used);
    pcache_relocate();
    jit_wprot(1);
    sys_icache_invalidate(J_RX(g_cache), h.arena_used);
    memset(g_ibtc, 0, sizeof g_ibtc); // shared IBTC data table: refills lazily
    free(abuf);
    g_pcache_loaded = 1;
    if (g_coldprof)
        fprintf(stderr, "[pcache] load %llu B arena, %llu blocks, %llu reloc in %.3f ms\n",
                (unsigned long long)h.arena_used, (unsigned long long)h.n_mapent, (unsigned long long)h.n_reloc,
                (now_ns() - t0) / 1e6);
    return 1;
}

// Persist the current arena + maps (atomic temp+rename). Refuses after a load (snowball), from a
// fork child (stale bookkeeping), when poisoned (unrecorded baked pointer), after guest SMC
// (non-file code in the arena), or when a fixed-VA map fell back (mixed-base arena). The snapshot is
// taken under g_jit_lock so a live peer thread (threaded exit_group) can never tear it.
static void pcache_save(void) {
    if (!g_pcache || !g_pc_binid || g_cp == g_cache) return;
    if (g_pcache_poison || g_pcache_loaded || g_pcache_forked || g_force_base_failed || g_smc_seen) return;
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024], tmp[1120];
    pcache_file(path, sizeof path);
    snprintf(tmp, sizeof tmp, "%s.%d.tmp", path, (int)getpid());
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return;
    // ---- consistent snapshot under the translation lock (peers may still be running/translating) ----
    pthread_mutex_lock(&g_jit_lock);
    uint64_t nmap = 0;
    for (uint32_t i = 0; i < JIT_MAP_N; i++)
        if (g_map[i].host) nmap++;
    uint64_t ntxpg = 0;
    for (uint32_t i = 0; i < TXPG_N; i++)
        if (g_txpg[i]) ntxpg++;
    uint64_t arena_used = (uint64_t)(g_cp - g_cache);
    struct pc_hdr h;
    memset(&h, 0, sizeof h);
    h.magic = PC_MAGIC;
    h.version = PC_VERSION_EFF; // layout-mode-mixed (IRQSLIM on/off) -- cross-mode loads are guaranteed MISSes
    h.cpu_sz = sizeof(struct cpu);
    h.jit_map_n = JIT_MAP_N;
    h.ibtc_n = IBTC_N;
    h.img_base = PC_IMG_BASE;
    h.interp_base = PC_INTERP_BASE;
    h.bin_id = g_pc_binid;
    h.entry_jump = g_pc_entry;
    h.arena_used = arena_used;
    h.n_reloc = (uint64_t)g_nreloc;
    h.n_mapent = nmap;
    h.n_pend = (uint64_t)g_npend;
    h.n_t2 = (uint64_t)g_t2n;
    h.n_txpg = ntxpg;
    h.block_return_at = (uint64_t)block_return;
    h.ibtc_at = (uint64_t)g_ibtc;
    // Build the whole image in one heap buffer -> one write() (per-record writes dominated the save cost).
    size_t total = sizeof h + (size_t)g_nreloc * sizeof(struct pc_reloc) + (size_t)nmap * sizeof(struct pc_mapent) +
                   (size_t)g_npend * sizeof(struct pc_pend) + (size_t)g_t2n * sizeof(struct pc_t2) +
                   (size_t)ntxpg * sizeof(uint64_t) + arena_used;
    uint8_t *buf = malloc(total);
    int ok = buf != NULL;
    if (ok) {
        uint8_t *w = buf + sizeof h; // header written last (its csum covers everything after it)
        memcpy(w, g_reloc, (size_t)g_nreloc * sizeof(struct pc_reloc));
        w += (size_t)g_nreloc * sizeof(struct pc_reloc);
        for (uint32_t i = 0; i < JIT_MAP_N; i++) {
            if (!g_map[i].host) continue;
            struct pc_mapent e = {g_map[i].gpc, (uint64_t)((uint8_t *)g_map[i].host - g_cache),
                                  (uint64_t)((uint8_t *)g_map[i].body - g_cache)};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_npend; i++) {
            struct pc_pend e = {(uint64_t)((uint8_t *)g_pend[i].slot - g_cache), g_pend[i].target,
                                (uint32_t)g_pend[i].is_bl, (uint32_t)g_pend[i].fwd};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_t2n; i++) {
            struct pc_t2 e = {g_t2gpc[i], g_t2cnt[i]};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (uint32_t i = 0; i < TXPG_N; i++)
            if (g_txpg[i]) {
                memcpy(w, &g_txpg[i], 8);
                w += 8;
            }
        memcpy(w, g_cache, arena_used); // read from the RW alias is always permitted
        h.csum = pc_fnv(1469598103934665603ull, buf + sizeof h, total - sizeof h);
        memcpy(buf, &h, sizeof h);
    }
    pthread_mutex_unlock(&g_jit_lock);
    if (ok) ok = write(fd, buf, total) == (ssize_t)total;
    free(buf);
    close(fd);
    if (ok)
        rename(tmp, path); // atomic publication: readers see the old complete file or this one, never a mix
    else
        unlink(tmp);
    if (g_coldprof)
        fprintf(stderr, "[pcache] save %s (%llu B arena, %llu blocks, %d reloc) in %.3f ms\n", ok ? "ok" : "FAILED",
                (unsigned long long)arena_used, (unsigned long long)nmap, g_nreloc, (now_ns() - t0) / 1e6);
}

// Poison the cache if a non-default codegen mode that bakes an UNRECORDED host pointer is active (their
// counters/logs are emitted via raw e_movconst/adrp of BSS addresses with no reloc record). Called once
// at engine init, after the mode flags are read.
static void pcache_poison_check(void) {
    if (g_prof) g_pcache_poison = 1;
}

// ---- guest fork hook (proc.c, both clone/fork sites, in the child, right after jit_after_fork) ----
// The child either KEPT the parent's warm arena (preserved-arena fork: single-threaded parent /
// MAP_JIT fallback) or got a fresh empty one (threaded rebuild); either way its arena from here on is a
// fork-private slice whose inherited g_pc_binid/g_pc_entry identity belongs to the PARENT's complete
// image -- so drop the inherited reloc records and bar this process from saving. An in-process execve
// re-keys everything and lifts the bar (pcache_exec_reload).
static void pcache_after_fork(void) {
    g_nreloc = 0;
    g_pcache_forked = 1;
}

#define PCACHE_FORK_HOOK pcache_after_fork()

// ---- wholesale-flush hook (engine/dispatch.c, after the cache-full in-place or stop-the-world flush) --
// The arena content the records described is gone (bump pointer reset / fresh arena). Everything emitted
// from here on re-records against the new arena, so the "every baked pointer recorded" invariant holds by
// construction after a plain reset. (A restored-then-flushed run is already barred from saving by
// g_pcache_loaded; this keeps the cold-run bookkeeping correct too.)
static void pcache_after_wholesale_flush(void) {
    g_nreloc = 0;
}

#define PCACHE_FLUSH_HOOK pcache_after_wholesale_flush()

// ---- guest execve (proc.c case 221) hooks ----
// The go-build fork+execve storm re-loads a toolchain binary (compile/asm/link) IN-PROCESS from a COLD,
// freshly jit_after_fork()'d arena; these let that reload restore the binary's warm arena from the cache.
// Gated behind PCACHE_EXEC_HOOKS so the SHARED proc.c compiles unchanged for the x86 engine.
static void pcache_exec_force_main(void) {
    if (g_pcache) {
        g_force_base = PC_IMG_BASE;
        g_force_base_failed = 0; // fresh image, fresh verdict
    }
}

static void pcache_exec_force_interp(void) {
    if (g_pcache) g_force_base = PC_INTERP_BASE;
}

static void pcache_exec_reload(const char *prog_host, const char *interp_host, const char *argv0, uint64_t jump) {
    if (!g_pcache) return;
    // execve is a full identity + arena reset (thread_exit_others ran; gmap/arena/map/ibtc flushed by
    // case 221), so the recording state resets with it and saving becomes safe again -- including for a
    // fork child (this is exactly the fork+execve toolchain case the cache exists for).
    g_nreloc = 0;
    g_t2n = 0;    // fresh tier-2 slot set for the new image (no cross-image alias)
    txpg_clear(); // nothing is translated now; the set re-fills (or is restored by the load below)
    txln_clear();
    g_pcache_loaded = 0; // allow a cold-miss save of the NEW binary
    g_pcache_forked = 0;
    g_pc_binid = pcache_make_id(prog_host, interp_host, argv0);
    g_pc_entry = jump;
    int hit = pcache_load(jump);
    if (g_coldprof) fprintf(stderr, "[pcache] exec %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
}

#define PCACHE_EXEC_HOOKS 1

#define PCACHE_SAVE_HOOK pcache_save()
