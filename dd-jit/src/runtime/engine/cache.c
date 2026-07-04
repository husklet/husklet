// dd/runtime/jit -- the code cache, the gpc->host block map, and lazy inter-block chaining.
// One 64MB W^X MAP_JIT arena; blocks appended + chained (b/bl backpatch). Host-ISA engine state.

// ---------------- JIT code cache ----------------
#define CACHE_SZ (64u << 20)
// base, bump pointer
static uint8_t *g_cache, *g_cp;
// start of current translation (for icache flush)
static uint8_t *g_emit_start;

// ---- dual-mapped (W^X-toggle-free) code cache ----
// g_cache/g_cp are the RW (writer) alias; the engine EXECUTES through an RX alias of the
// SAME physical pages at g_cache + g_rw2rx (created by vm_remap'ing to a second address,
// the Apple-Silicon dual-map JIT technique). All PC-relative emission/back-patching is a
// difference of two cache addresses, so it is alias-invariant and needs no conversion;
// only the few ABSOLUTE handoffs (run_block target, IBTC/IC body literals, icache flush)
// convert RW<->RX. g_rw2rx == 0 selects the single-MAP_JIT fallback that toggles the whole
// region's W^X per translation/IC-fill (NODUALMAP=1).
static ptrdiff_t g_rw2rx;     // RX_addr - RW_addr (0 in fallback)
static int g_dualmap;         // 1 when the RW/RX dual mapping is active
static uint64_t g_wx_toggles; // # of pthread_jit_write_protect_np() calls actually made (PROF)
#define J_RX(p) ((void *)((uint8_t *)(p) + g_rw2rx)) // RW alias addr -> RX alias addr
#define J_RW(p) ((void *)((uint8_t *)(p) - g_rw2rx)) // RX alias addr -> RW alias addr
// The single W^X gate. Under dual mapping it is a no-op: writes land on the RW alias and
// execution reads the RX alias, so no per-region permission flip (and no peer-thread race).
static inline void jit_wprot(int enable_exec) {
    if (g_dualmap) return;
    g_wx_toggles++;
    pthread_jit_write_protect_np(enable_exec);
}
// Allocate one dual-mapped code cache: a plain anon RW region + an RX alias of the SAME physical
// pages at a second VA (vm_remap). Returns 0 and fills *rw / *delta(=RX-RW) on success.
#include <mach/mach.h>
#include <mach/mach_vm.h>
static int dualmap_alloc(uint8_t **rw_out, ptrdiff_t *delta_out) {
    uint8_t *rw = mmap(NULL, CACHE_SZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (rw == MAP_FAILED) return -1;
    mach_vm_address_t rx = 0;
    vm_prot_t cur = 0, max = 0;
    // #371: the RX alias is created VM_INHERIT_NONE, so a fork child gets a HOLE at the RX VA instead of
    // an independently-COW'd (silently diverged) second copy. jit_after_fork() then re-remaps a fresh RX
    // alias of the child's OWN (COW-inherited) RW pages at that same VA -- ~1us -- which re-couples the
    // two views, so the child keeps every inherited translation AND its later writes propagate to RX
    // (empirically verified, incl. 4 nested fork generations). Every fork path runs jit_after_fork()
    // before any guest code executes, so nothing ever fetches from the hole.
    kern_return_t kr = mach_vm_remap(mach_task_self(), &rx, CACHE_SZ, 0, VM_FLAGS_ANYWHERE, mach_task_self(),
                                     (mach_vm_address_t)rw, FALSE, &cur, &max, VM_INHERIT_NONE);
    if (kr == KERN_SUCCESS) kr = mach_vm_protect(mach_task_self(), rx, CACHE_SZ, FALSE, VM_PROT_READ | VM_PROT_EXECUTE);
    if (kr != KERN_SUCCESS) {
        munmap(rw, CACHE_SZ);
        return -1;
    }
    *rw_out = rw;
    *delta_out = (uint8_t *)rx - rw;
    return 0;
}
// PROF-only: accumulated wall time spent in the translate region (the part the W^X toggles bracket).
static uint64_t g_xlate_ns;
static inline uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + t.tv_nsec;
}

// Threads: each guest thread runs run_guest on its OWN struct cpu, stored in a
// pthread TSD slot so emitted block-exit code can recover it from host TLS.
static pthread_key_t g_cpu_key;
// serializes translation
static pthread_mutex_t g_jit_lock = PTHREAD_MUTEX_INITIALIZER;
// guards the FS-metadata cache under threads
static pthread_mutex_t g_cache_lock = PTHREAD_MUTEX_INITIALIZER;
#define CLK                                                                                                            \
    int _th = g_threaded;                                                                                              \
    if (_th) pthread_mutex_lock(&g_cache_lock)
#define CUL                                                                                                            \
    do {                                                                                                               \
        if (_th) pthread_mutex_unlock(&g_cache_lock);                                                                  \
    } while (0)
// >0 once a guest thread is spawned
static int g_threaded;

// gpc->host block map capacity. Sized so the 64MB CACHE_SZ arena fills (-> the dispatcher's wholesale
// flush) LONG before this open-addressed table does: even all-minimum-size blocks (prologue + a one-insn
// exit, ~90 host words ~360B) cap at ~186K live blocks in a full cache, so 2^19 slots keeps the load
// factor under ~40% (short linear-probe chains) and guarantees map_put never silently fails mid-run. A
// FULL table made map_put a no-op -> map_body() then returned NULL for a freshly-translated block, and
// patch_links_to() back-patched a `b (NULL - slot)` wild branch (mongod, ~65K blocks of C++ static init,
// crashed with SIGILL/SIGSEGV here). NOT the leaked container-state MAP_N (that one is unrelated, 64K).
#define JIT_MAP_N (1u << 19)
static struct {
    uint64_t gpc;
    void *host;
    void *body;
} g_map[JIT_MAP_N];
// ---- SMC precise gate: the set of guest 4KB pages we have translated ANY block from ----
// A code-generating guest (V8, a JIT) issues `ic ivau` (icache invalidate by VA) after writing each
// freshly-generated cache line. The old smc_icflush() responded to EVERY such flush by nuking the whole
// translation map + the whole IBTC -- so a `node -e 1+1` paid ~80K re-translations and a tight JS loop
// paid ~37M (60s of pure re-translation), because V8 flushes thousands of times while it grows its code
// space. But almost every flush targets a BRAND-NEW page that was never translated, so there is provably
// nothing stale to drop. This open-addressed set records which guest pages have a live translation; an
// `ic ivau` to a page NOT in the set is a no-op (skip the wholesale drop). A page that WAS translated
// still triggers the full conservative invalidation -> correctness for genuine in-place self-modification
// is unchanged. Reset whenever g_map is wholesale-cleared (the set then re-fills as blocks re-translate).
#define TXPG_N (1u << 18) // 256K slots * 8B = 2MB; guest code spans at most a few thousand pages
static uint64_t g_txpg[TXPG_N]; // value = guest page (addr>>12); 0 = empty (page 0 never holds guest code)
static void txpg_put(uint64_t p) { // insert one guest page (addr>>12) into the set
    uint32_t h = (uint32_t)(p * 2654435761u) & (TXPG_N - 1);
    for (uint32_t i = 0; i < TXPG_N; i++) {
        uint32_t j = (h + i) & (TXPG_N - 1);
        if (g_txpg[j] == p) break;                    // already present
        if (g_txpg[j] == 0) { g_txpg[j] = p; break; } // insert into the first empty slot
    }
}
// ---- SMC precise gate, CACHE-LINE granularity (64B = the unit `ic ivau, Xt` actually invalidates) ----
// The page-granular set below over-approximates badly for a guest whose code arena packs many functions per
// 4KB page (BeamAsm): appending function F2 onto a page that already holds a translated F1 makes txpg_has()
// true for F2's `ic ivau`, forcing a wholesale drop even though NO translated byte changed. This finer set
// records the exact 64B source lines a live block was translated from, so the gate fires only when the
// invalidated line genuinely overlaps translated code (real in-place self-modification), not mere same-page
// appends. Sized 2^21 slots (16MB) so even a large JIT working set (~1M lines = 64MB of guest code) keeps
// the open-addressed load factor low; saturation degrades conservatively (assume present -> wholesale drop).
#define TXLN_N (1u << 21)
static uint64_t g_txln[TXLN_N]; // value = guest line (addr>>6); 0 = empty
static void txln_put(uint64_t l) {
    uint32_t h = (uint32_t)(l * 2654435761u) & (TXLN_N - 1);
    for (uint32_t i = 0; i < TXLN_N; i++) {
        uint32_t j = (h + i) & (TXLN_N - 1);
        if (g_txln[j] == l) break;
        if (g_txln[j] == 0) { g_txln[j] = l; break; }
    }
}
static int txln_has(uint64_t addr) { // is the 64B line at addr the source of any live translation?
    uint64_t l = addr >> 6;
    uint32_t h = (uint32_t)(l * 2654435761u) & (TXLN_N - 1);
    for (uint32_t i = 0; i < TXLN_N; i++) {
        uint32_t j = (h + i) & (TXLN_N - 1);
        if (g_txln[j] == l) return 1;
        if (g_txln[j] == 0) return 0;
    }
    return 1; // saturated -> conservative
}
static void txln_clear(void) { memset(g_txln, 0, sizeof g_txln); }
static void txpg_mark(uint64_t lo, uint64_t hi) {
    if (hi <= lo) hi = lo + 1;
    for (uint64_t p = lo >> 12; p <= ((hi - 1) >> 12); p++)
        txpg_put(p);
    for (uint64_t l = lo >> 6; l <= ((hi - 1) >> 6); l++) // finer line-granular set (see txln_has)
        txln_put(l);
}
static int txpg_has(uint64_t addr) {
    uint64_t p = addr >> 12;
    uint32_t h = (uint32_t)(p * 2654435761u) & (TXPG_N - 1);
    for (uint32_t i = 0; i < TXPG_N; i++) {
        uint32_t j = (h + i) & (TXPG_N - 1);
        if (g_txpg[j] == p) return 1;
        if (g_txpg[j] == 0) return 0; // hit an empty slot before the page -> not present
    }
    return 1; // table saturated -> conservatively assume present (forces a full invalidation)
}
static void txpg_clear(void) { memset(g_txpg, 0, sizeof g_txpg); }
static int map_idx(uint64_t gpc) {
    // hash shift is per-arch (frontend/<arch>/abi.h G_GPC_HASH_SHIFT): aarch64 PCs are 4-byte aligned
    // (>>2 spreads), x86 PCs are byte-granular (>>0). Pure tuning constant; aarch64 value is 2 (unchanged).
    uint32_t h = (uint32_t)((gpc >> G_GPC_HASH_SHIFT) * 2654435761u) & (JIT_MAP_N - 1);
    for (int i = 0; i < JIT_MAP_N; i++) {
        uint32_t j = (h + i) & (JIT_MAP_N - 1);
        if (g_map[j].host && g_map[j].gpc == gpc) return j;
        if (!g_map[j].host) return -1;
    }
    return -1;
}
static void *map_host(uint64_t gpc) {
    int i = map_idx(gpc);
    return i < 0 ? NULL : g_map[i].host;
}
static void *map_body(uint64_t gpc) {
    int i = map_idx(gpc);
    return i < 0 ? NULL : g_map[i].body;
}
static void map_put(uint64_t gpc, void *host, void *body) {
    uint32_t h = (uint32_t)((gpc >> G_GPC_HASH_SHIFT) * 2654435761u) & (JIT_MAP_N - 1);
    for (int i = 0; i < JIT_MAP_N; i++) {
        uint32_t j = (h + i) & (JIT_MAP_N - 1);
        if (!g_map[j].host) {
            g_map[j].gpc = gpc;
            g_map[j].host = host;
            g_map[j].body = body;
            return;
        }
    }
}

// IBTC: a shared, direct-mapped hash table {guest target -> host body_ind} probed
// inline by indirect branches. Handles polymorphic dispatch (interpreters) that a
// per-site 1-entry cache can't. Plain data (no W^X); zeroed at start and on flush.
//
// Sized at 64Ki entries (1 MiB). A direct-mapped IBTC keyed on the guest target takes a
// conflict miss whenever two hot targets alias one slot; with multiple guest threads (V8
// worker threads, Go) running the SAME translated code, each thread's distinct hot targets
// evict the others' from a shared slot -- a cross-thread thrash whose miss bounces through
// the C dispatcher (lock + map_host) every time. A 64Ki table (vs the former 8Ki) cuts the
// aliasing pressure ~8x, so far more indirect branches hit inline and never reach the
// dispatcher. The reader's hash width (engine/stubs.c) and both fills (the per-arch
// G_IBTC_FILL, which key on `(target>>2) & (IBTC_N-1)`) follow this constant.
#define IBTC_N 65536
// 16-byte aligned so each {target,body} entry sits in a single 16-byte granule -> a
// naturally-aligned 128-bit ldp/stp is single-copy atomic under FEAT_LSE2 (all Apple
// Silicon). That atomicity is what lets a lock-free reader observe {target,body} as an
// indivisible pair: it can never see new-target/old-body or old-target/new-body (the
// torn-dispatch hazard). See G_IBTC_FILL (writer) + emit_ibranch (reader).
typedef struct {
    uint64_t target;
    void *body;
} ibtc_ent;
_Alignas(16) static ibtc_ent g_ibtc[IBTC_N];

// ---- W5C: race-free threaded IBTC fill ----
// g_mtibtc: enable threaded shared-hash IBTC fill (NOMTIBTC=1 disables -> revert to the
// locked-dispatcher path where threaded indirect branches always miss to the C dispatcher).
// g_mtfill: PROF count of threaded shared-hash publishes. g_futexq: per-address futex
// wait queues (NOFUTEXQ=1 -> the legacy single global mutex + broadcast in thread.c).
static int g_mtibtc = 1;
static int g_futexq = 1;
static uint64_t g_mtfill;
// Atomic 128-bit RELEASE publish of a {target, body} pair into a 16-byte-aligned IBTC slot.
// Single writer (the dispatcher holds g_jit_lock across every fill); many lock-free readers.
// `dmb ish` orders all prior stores (incl. the body block's translation + its IC IVAU, both
// already DSB-complete before this point) before the pair becomes observable; the `stp` of two
// X regs to a 16-byte-aligned address is single-copy atomic under FEAT_LSE2 (all Apple Silicon),
// so it is mutually atomic with the reader's plain `ldp`. We use explicit asm rather than a
// 16-byte __atomic (which could lower to a lock-based libatomic call that would NOT be atomic
// against the lock-free ldp reader). Layout: target at +0, body at +8 (matches struct ibtc_ent).
static inline void ibtc_publish(ibtc_ent *e, uint64_t target, void *body) {
    __asm__ volatile("dmb ish\n\t"
                     "stp %1, %2, [%0]\n\t"
                     :
                     : "r"(e), "r"(target), "r"(body)
                     : "memory");
}
static uint64_t g_prof_cross, g_prof_miss, g_prof_xlate, g_prof_sys, g_lse_n;
// PROF=1: dispatcher crossings / IBTC misses / translations
static int g_prof;
// A3 §B instrumentation (PROF=1). Runtime: shadow pushes executed, predicted-return FAST hits (host
// ret, RAS), and returns that fell through emit_shadow_ret to the IBTC fallback. Translate-time:
// how many guest `bl` sites the depth-gate steered to §B (shadow push) vs the cheap leaf Stage-B path.
static uint64_t g_prof_shpush, g_prof_shret_hit, g_prof_shret_fb;
static uint64_t g_prof_bl_shadow, g_prof_bl_leaf;

// ============================================================================
// ARM-B1 FEASIBILITY INSTRUMENTATION (IBPROF) -- gated, measurement-only.
// Forces EVERY guest indirect transfer (br/blr/ret) through the C dispatcher
// (reason R_IBLOG) so we can record per-(site) traffic + the executed target
// stream. The normal inline IBTC is bypassed (no fills) while IBPROF is on, so
// the cost numbers it reports are SHAPE-only (counts), never timing.
//   - per-site histogram: which guest indirect branch dominates traffic
//   - last-value (monomorphic) hit rate per site  (= per-site IC effectiveness)
//   - 1st-order Markov hit rate per (site, prev-target)  (= the B1 thread guard
//     hit rate: given we just ran handler A, is the next handler always B?)
// ============================================================================
#ifndef R_IBLOG
#define R_IBLOG 3
#endif
static int g_ibprof;    // IBPROF=1
static int g_vdbetrace; // VDBETRACE=1 (ARM-B1 prototype gate; defined here, used in translate.c)
// IBSLIM (perf lever #4): indirect-dispatch/call-path slimming (aarch64; defined here -- shared TU --
// used in translate/aarch64). NOIBSLIM=1 reverts every piece (steal-aware emit_set_x30 + the dead
// per-site-IC skip at recognized interpreter-dispatch `br`s) for A/B. CTXDISP=1 additionally enables
// the experimental history-keyed context dispatch at recognized dispatch sites (measurement gate).
static int g_noibslim; // NOIBSLIM=1
static int g_ctxdisp;  // CTXDISP=1
#define IBSITE_N 8192
static struct ibsite {
    uint64_t site, count, last_tgt, mono; // mono = #times target==previous target at this site
    uint64_t h1, h2, h3;                  // most-recent 3 targets (h1=newest) for the order-k predictors
} g_ibsite[IBSITE_N];
// One open-addressed predictor table per Markov order. Each entry is a (key)->last-observed-next
// "last value" predictor; hit = #times the actual next equalled the prediction. The order-k key
// folds the site with the last k targets, so order-k models "given the last k opcode handlers, is
// the next handler fixed?" -- exactly the guard a depth-k thread of the VDBE loop would carry.
#define IBTRANS_N (1u << 19)
static struct ibtrans {
    uint64_t site, pred, count, hit;
} g_ibtrans1[IBTRANS_N], g_ibtrans2[IBTRANS_N], g_ibtrans3[IBTRANS_N];
static uint64_t g_ib_total;
static inline uint64_t ibmix(uint64_t a, uint64_t b) {
    uint64_t h = a * 0x9E3779B97F4A7C15ull ^ (b + 0x7F4A7C15ull);
    h ^= h >> 29;
    h *= 0xBF58476D1CE4E5B9ull;
    h ^= h >> 32;
    return h;
}
static struct ibsite *ib_find_site(uint64_t site) {
    uint32_t h = (uint32_t)(ibmix(site, 0) & (IBSITE_N - 1));
    for (int i = 0; i < IBSITE_N; i++) {
        uint32_t j = (h + i) & (IBSITE_N - 1);
        if (g_ibsite[j].count == 0 && g_ibsite[j].site == 0) {
            g_ibsite[j].site = site;
            return &g_ibsite[j];
        }
        if (g_ibsite[j].site == site) return &g_ibsite[j];
    }
    return &g_ibsite[0]; // table full: degrade, don't crash
}
// Keyed predictor slot: the fold key lives in a side array `keys`; `tab[j].site` carries the real
// site (for per-site aggregation in the dump). NULL on probe overflow (rare).
static uint64_t g_k1[IBTRANS_N], g_k2[IBTRANS_N], g_k3[IBTRANS_N];
static struct ibtrans *ib_slot(struct ibtrans *tab, uint64_t *keys, uint64_t key, uint64_t site) {
    if (key == 0) key = 1;
    uint32_t h = (uint32_t)(key & (IBTRANS_N - 1));
    for (int i = 0; i < 64; i++) {
        uint32_t j = (h + i) & (IBTRANS_N - 1);
        if (keys[j] == 0) {
            keys[j] = key;
            tab[j].site = site;
            return &tab[j];
        }
        if (keys[j] == key) return &tab[j];
    }
    return NULL;
}
static inline void ib_pred(struct ibtrans *t, uint64_t target) {
    if (!t) return;
    if (t->count == 0)
        t->pred = target; // seed (uncounted)
    else if (t->pred == target)
        t->hit++;
    else
        t->pred = target; // last-value, adapt
    t->count++;
}
static void ib_log(uint64_t site, uint64_t target) {
    g_ib_total++;
    struct ibsite *s = ib_find_site(site);
    int n = (s->count != 0) + (s->h2 != 0 || s->count > 1) + (s->h3 != 0 || s->count > 2);
    (void)n;
    if (s->count >= 1) { // have h1
        if (target == s->h1) s->mono++;
        ib_pred(ib_slot(g_ibtrans1, g_k1, ibmix(site, s->h1), site), target);
    }
    if (s->count >= 2) // have h1,h2
        ib_pred(ib_slot(g_ibtrans2, g_k2, ibmix(ibmix(site, s->h1), s->h2), site), target);
    if (s->count >= 3) // have h1,h2,h3
        ib_pred(ib_slot(g_ibtrans3, g_k3, ibmix(ibmix(ibmix(site, s->h1), s->h2), s->h3), site), target);
    s->h3 = s->h2;
    s->h2 = s->h1;
    s->h1 = target;
    s->last_tgt = target;
    s->count++;
}
// ARM-B1 VDBETRACE prototype counters/state.
static uint64_t g_vt_inline;    // # of speculative-direct-chain (SDC) JT-dispatch sites emitted
static uint64_t g_vt_fills;     // # of SDC (re)specializations performed by the dispatcher
static uint64_t g_vt_force_inl; // # of dispatch blocks force-inlined into a predecessor (path-specialize)
static int g_vt_hitcount;       // VTHITCOUNT=1: emit an inline SDC guard-hit counter (perturbs timing)
static uint64_t g_vt_hit;       // # of SDC guard hits (threaded direct chains taken)
// ARM-B1 SDC fill: the speculative direct-chain at a JT dispatch missed to the dispatcher (genuinely-new
// target). (Re)specialize the site to this target: write the guard literal and back-patch the in-cache
// `b` slot to a DIRECT branch into the target's body (regs-live entry). `rec_rx` is the RX address of the
// 2-quad record { [0]=guard target literal, [1]=RW addr of the `b` slot } (low tag bit already stripped).
static void sdc_fill(uint64_t rec_rx, uint64_t target) {
    uint64_t *rec = (uint64_t *)J_RW((void *)rec_rx);
    void *body = map_body(target); // RW body (post-prologue, regs-live entry)
    if (!body) return;             // not translated (shouldn't happen: dispatcher just translated pc)
    uint32_t *bslot = (uint32_t *)rec[1]; // RW addr of the direct-branch slot
    jit_wprot(0);
    int64_t d = ((uint8_t *)body - (uint8_t *)bslot) / 4; // RW-RW delta == RX-RX delta (alias-invariant)
    *bslot = 0x14000000u | ((uint32_t)d & 0x3FFFFFFu);     // b body
    rec[0] = target;                                       // arm the guard
    jit_wprot(1);
    sys_icache_invalidate(J_RX(bslot), 4); // the patched `b` executes from the RX alias
    g_vt_fills++;
}
static void vt_dump(void) {
    if (!g_vdbetrace) return;
    fprintf(stderr, "[vdbetrace] sdc_sites=%llu sdc_fills=%llu force_inlined_dispatch=%llu\n",
            (unsigned long long)g_vt_inline, (unsigned long long)g_vt_fills, (unsigned long long)g_vt_force_inl);
    if (g_vt_hitcount)
        fprintf(stderr, "[vdbetrace] guard_hits=%llu (threaded direct chains taken; VTHITCOUNT perturbs timing)\n",
                (unsigned long long)g_vt_hit);
}

// ---- CTXDISP (lever #4 experiment): history-keyed context dispatch at recognized interpreter-
// dispatch sites (computed-goto / jump-table `br xN`). Each site gets an in-cache array of CTX_N
// 64-byte stubs, one per rolling-history hash bucket: {16B atomic {target,body} pair | guard code}.
// The main site indexes the array by a hash of the last ~3 guest targets ONLY (never the current
// one), so each history context owns a PRIVATE `br` whose target the host BTB predicts per-address
// (the order-3 Markov signal: 83% at the CPython-shaped megamorphic site). The guard is an exact
// 64-bit compare of the REAL computed target against the stub's filled target before the jump, so a
// stale/colliding stub can never land wrong -- it falls into the ordinary shared-hash IBTC probe.
// Stubs are (re)filled by the dispatcher (ctx_fill), throttled by a per-site countdown so steady-
// state refills cost ~1/CTX_REFILL of ctx misses. Gated CTXDISP=1 (measurement; default OFF).
#define CTX_N 256      // history buckets (stubs) per site; 64B each -> 16KB in-cache per site
#define CTX_REFILL 64  // ctx misses between dispatcher refills (throttle)
#define CTXSITE_MAX 256
static struct {
    uint64_t gpc;  // dispatch-site guest pc (dedup on re-translate)
    uint64_t hist; // rolling target history (h = (h<<16) ^ (target>>2)); emitted code reads+writes
    uint64_t ctr;  // refill throttle countdown (emitted code decrements; dispatcher resets)
} g_ctxsite[CTXSITE_MAX];
static int g_nctxsite;
static uint64_t g_ctx_fills; // dispatcher stub (re)specializations
static int ctxsite_slot(uint64_t gpc) {
    for (int i = 0; i < g_nctxsite; i++)
        if (g_ctxsite[i].gpc == gpc) return i;
    if (g_nctxsite >= CTXSITE_MAX) return -1;
    int i = g_nctxsite++;
    g_ctxsite[i].gpc = gpc;
    g_ctxsite[i].hist = 0;
    g_ctxsite[i].ctr = CTX_REFILL;
    return i;
}
// A ctx stub missed to the dispatcher (throttled): (re)specialize the stub whose 16B pair sits at
// pair_rx (RX alias; tag bits already stripped) to the just-resolved target. Same publish contract
// as the shared hash: single 128-bit release store (atomic under LSE2), so a lock-free reader can
// never observe a torn {target,body} -- and the emitted guard re-verifies target equality anyway.
static void ctx_fill(uint64_t pair_rx, uint64_t target) {
    void *bd = map_body(target);
    if (!bd) return; // not translated (dispatcher translates target first, so this is defensive)
    ibtc_publish((ibtc_ent *)J_RW((void *)pair_rx), target, J_RX(bd));
    g_ctx_fills++;
}
static void ctx_dump(void) {
    if (!g_ctxdisp) return;
    fprintf(stderr, "[ctxdisp] sites=%d fills=%llu\n", g_nctxsite, (unsigned long long)g_ctx_fills);
}
// MAPDUMP=<prefix> (profiling-only, this worktree): at exit_group, dump the translation map + the raw
// code cache so an offline tool can attribute sampled host PCs to guest blocks/instructions.
// <prefix>.map is text: "CACHE <rw> <rx> <cp>" then one "MAP <gpc> <host> <body>" per live block.
// <prefix>.bin is the raw [g_cache, g_cp) bytes. Zero cost unless the env is set.
static void md_dump(void) {
    const char *pfx = getenv("MAPDUMP");
    if (!pfx || !g_cache) return;
    char p[1024];
    snprintf(p, sizeof p, "%s.map", pfx);
    FILE *f = fopen(p, "w");
    if (!f) return;
    fprintf(f, "CACHE %p %p %p\n", (void *)g_cache, J_RX(g_cache), (void *)g_cp);
    for (uint32_t i = 0; i < JIT_MAP_N; i++)
        if (g_map[i].host)
            fprintf(f, "MAP %llx %p %p\n", (unsigned long long)g_map[i].gpc, g_map[i].host, g_map[i].body);
    fclose(f);
    snprintf(p, sizeof p, "%s.bin", pfx);
    f = fopen(p, "wb");
    if (!f) return;
    fwrite(g_cache, 1, (size_t)(g_cp - g_cache), f);
    fclose(f);
}
// ARM-B1: recognize a clang jump-table switch dispatch at a guest `br xN`. The compiler emits
//   ldrh wM,[xB,wI,uxtw #1] ; adr xA,. ; add xN,xA,wM,sxth #2 ; br xN
// (an indexed 16-bit offset table). Bit-exact opcode match on the 3 predecessors + Rd==br.Rn.
static int is_jt_dispatch_br(uint64_t gpc) {
    uint32_t a = *(uint32_t *)(gpc - 12), b = *(uint32_t *)(gpc - 8), c = *(uint32_t *)(gpc - 4),
             br = *(uint32_t *)gpc;
    int brn = (int)((br >> 5) & 31);
    return (a & 0xFFE0FC00u) == 0x78605800u   // ldrh wM,[xB,wI,uxtw #1]
           && (b & 0x9F000000u) == 0x10000000u // adr xA, .
           && (c & 0xFFE0FC00u) == 0x8B20A800u // add xd,xa,wm,sxth #2
           && (int)(c & 31) == brn;            // add Rd feeds the br
}
// True if the block at `tgt` is a JT-dispatch block (reaches a JT-dispatch `br` with no intervening
// call/return/svc/unconditional-b that would end the block first). Used to force-inline the shared
// dispatch into each predecessor so the resulting `br` is PATH-SPECIFIC (the B1 specialization).
static int is_jt_dispatch_block(uint64_t tgt) {
    for (int i = 3; i < 28; i++) {
        uint64_t p = tgt + 4u * (unsigned)i;
        uint32_t in = *(uint32_t *)p;
        if ((in & 0xFFFFFC1Fu) == 0xD61F0000u) return is_jt_dispatch_br(p); // br
        if ((in & 0xFC000000u) == 0x94000000u) return 0;                    // bl
        if ((in & 0xFFFFFC1Fu) == 0xD63F0000u) return 0;                    // blr
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u) return 0;                    // ret
        if (in == 0xD4000001u) return 0;                                    // svc
        if ((in & 0xFC000000u) == 0x14000000u) return 0; // unconditional b: block ends before any br
    }
    return 0;
}
// IBSLIM: recognize an interpreter-dispatch indirect `br xN` -- a jump through a table of CODE
// POINTERS: `ldr xN, [xB, {w|x}M, {uxtw|lsl|sxtw} #3]` feeding `br xN` (gcc/clang computed goto --
// CPython's eval loop, sqlite's VDBE -- and any switch over a pointer table), or clang's
// 16-bit-offset jump table (is_jt_dispatch_br). Such a site is megamorphic by construction, so its
// per-site monomorphic IC is dead weight (measured 5.4% hit at the CPython-shaped bench site).
// Pure heuristic: a false negative keeps the ordinary emit_ibranch; a false positive merely skips
// a per-site IC that would have hit. Both are correct.
static int is_ptrtable_ldr(uint32_t in, int rt) {
    if ((in & 0xFFE00C00u) != 0xF8600800u) return 0; // LDR Xt, [Xn, Rm, ext/lsl {#3}] (64-bit)
    if ((int)(in & 31) != rt) return 0;              // must define the branch register
    unsigned opt = (in >> 13) & 7;                   // uxtw(2) / lsl(3) / sxtw(6)
    if (opt != 2 && opt != 3 && opt != 6) return 0;
    return (int)((in >> 12) & 1);                    // S=1: scaled #3 (an 8-byte pointer table)
}
static int is_interp_dispatch_br(uint64_t gpc, int brn) {
    if ((gpc & 0xFFFu) < 12) return 0; // never scan backwards across a page boundary
    uint32_t p1 = *(uint32_t *)(gpc - 4);
    if (is_ptrtable_ldr(p1, brn)) return 1;
    // allow ONE scheduled insn between the table load and the br, provided it does not redefine
    // the branch register (Rd is bits 4:0 for the data-processing forms gcc schedules here).
    if (is_ptrtable_ldr(*(uint32_t *)(gpc - 8), brn) && (int)(p1 & 31) != brn) return 1;
    return is_jt_dispatch_br(gpc);
}
static int ibsite_cmp(const void *a, const void *b) {
    uint64_t ca = ((const struct ibsite *)a)->count, cb = ((const struct ibsite *)b)->count;
    return ca < cb ? 1 : ca > cb ? -1 : 0;
}
static void ib_dump(void) {
    if (!g_ibprof) return;
    // copy + sort sites by traffic
    static struct ibsite snap[IBSITE_N];
    int n = 0;
    for (int i = 0; i < IBSITE_N; i++)
        if (g_ibsite[i].count) snap[n++] = g_ibsite[i];
    qsort(snap, (size_t)n, sizeof snap[0], ibsite_cmp);
    fprintf(stderr, "[ibprof] total_indirect=%llu distinct_sites=%d\n", (unsigned long long)g_ib_total, n);
    fprintf(stderr, "[ibprof]  rank        site      count    %%traffic  mono%%   o1%%    o2%%    o3%%   dt\n");
    for (int r = 0; r < n && r < 24; r++) {
        // aggregate order-k hit rates for this site (hits / counted-transitions, excluding seeds)
        uint64_t h1 = 0, c1 = 0, h2 = 0, c2 = 0, h3 = 0, c3 = 0;
        int dt = 0;
        for (uint32_t k = 0; k < IBTRANS_N; k++) {
            if (g_ibtrans1[k].count && g_ibtrans1[k].site == snap[r].site) {
                h1 += g_ibtrans1[k].hit;
                c1 += g_ibtrans1[k].count - 1;
                dt++;
            }
            if (g_ibtrans2[k].count && g_ibtrans2[k].site == snap[r].site) {
                h2 += g_ibtrans2[k].hit;
                c2 += g_ibtrans2[k].count - 1;
            }
            if (g_ibtrans3[k].count && g_ibtrans3[k].site == snap[r].site) {
                h3 += g_ibtrans3[k].hit;
                c3 += g_ibtrans3[k].count - 1;
            }
        }
        double pct = 100.0 * (double)snap[r].count / (double)(g_ib_total ? g_ib_total : 1);
        double monop = 100.0 * (double)snap[r].mono / (double)(snap[r].count ? snap[r].count : 1);
        double o1 = c1 ? 100.0 * (double)h1 / (double)c1 : 0.0;
        double o2 = c2 ? 100.0 * (double)h2 / (double)c2 : 0.0;
        double o3 = c3 ? 100.0 * (double)h3 / (double)c3 : 0.0;
        fprintf(stderr, "[ibprof]  %3d  %12llx  %11llu  %7.2f  %5.1f  %5.2f  %5.2f  %5.2f  %4d\n", r,
                (unsigned long long)snap[r].site, (unsigned long long)snap[r].count, pct, monop, o1, o2, o3, dt);
    }
}

// ---------------- W4E adaptive tier-2 ----------------
// W4E tier-2: a hot self-loop's in-cache back-edge counter reached threshold -> the dispatcher
// recompiles (promotes) the block with the optimized codegen, then resumes (pc already = block start).
// (The reason code normally lives next to R_BRANCH/R_SYSCALL in include/cpu_aarch64.h; it is defined here
// because this engine integration is confined to the jit/ + frontend/aarch64/ translate units.)
// W5B: the x86 engine reuses this substrate but its reason-code space already uses 2 for R_CPUID, so it
// pre-defines R_TIER2=7 in include/cpu_x86_64.h. Guard the aarch64 default so the x86 value wins in the
// x86 unity build; aarch64 (whose cpu_aarch64.h does not define it) still gets 2. No aarch64 change.
#ifndef R_TIER2
#define R_TIER2 2
#endif
//
// A same-ISA aarch64->aarch64 transliterator already keeps every guest GPR in its host reg and flags
// native, so tier-1 hot loops are near-native EXCEPT the conditional back-edge: a self-loop `b.cond` is
// laid as `b.cond Ltaken; b body` -- TWO taken host branches per iteration. Tier-2 recompiles a hot
// self-loop folding that into a single `b.cond body` (native-equivalent).
//
// Hotness must be measured IN-CACHE: a chained hot loop never returns to the dispatcher, so a
// dispatcher-side counter is blind to it. Each translated single-block self-loop therefore carries a
// cheap, flag-free, decrementing back-edge counter (initialized to the threshold). When it hits zero the
// back-edge exits R_TIER2; the dispatcher promotes the block (recompile + swap the map entry + repoint
// pending chains/IBTC) and resumes -- the remaining iterations run folded in-cache. The counter is
// removed by the recompile, so the promoted steady state has ZERO tier-2 overhead.
#define T2_MAX 8192
// per self-loop iteration counter (plain RW data -- NOT in the W^X cache, which is RX while executing;
// emitted code stores to it via an adrp+add absolute address)
static uint64_t g_t2cnt[T2_MAX];
static uint64_t g_t2gpc[T2_MAX]; // the loop-start gpc owning each slot (dedup on re-translate)
static int g_t2n;                // slots allocated
static int g_notier2;            // NOTIER2=1 kill switch (pure tier-1 baseline)
static uint64_t g_t2thresh = 1000; // back-edge iterations before promotion (TIER2_THRESHOLD env)
static uint64_t g_prof_t2;       // PROF: blocks promoted to tier-2
static int g_tier2_build;        // set while recompiling a block as tier-2 (fold, no counter, no map_put)
static void *g_last_body;        // body pointer of the most recent translate_block (for the promoter)
// Kill-switch + threshold env, read ONCE (idempotent static guard; the W4E diff read these in the target
// main(), relocated here to keep the integration inside the allowed jit/ + frontend/aarch64/ units).
static int g_t2_envdone;
static void tier2_env_init(void) {
    if (g_t2_envdone) return;
    g_t2_envdone = 1;
    g_notier2 = getenv("NOTIER2") != NULL;
    const char *t = getenv("TIER2_THRESHOLD");
    if (t && atoll(t) > 0) g_t2thresh = (uint64_t)atoll(t);
}
// Find (or allocate) the counter slot for a self-loop whose body starts at gpc. Re-translation of the
// same loop reuses its slot so the count is not reset (and a re-translated promoted loop won't re-arm a
// fresh counter). Returns -1 if the table is full (-> emit plain tier-1, no counter).
static int t2_slot(uint64_t gpc) {
    for (int i = 0; i < g_t2n; i++)
        if (g_t2gpc[i] == gpc) return i;
    if (g_t2n >= T2_MAX) return -1;
    int i = g_t2n++;
    g_t2gpc[i] = gpc;
    g_t2cnt[i] = g_t2thresh;
    return i;
}

// Direct-branch edges whose target wasn't translated yet: remembered so the branch
// can be back-patched into a direct `b target.body` once the target is translated.
//
// IRQSLIM (aarch64, lever #4): when the #292 async-signal poll is emitted as a fixed 2-insn block
// header (ldr+cbnz, see emit_irq_check), a FORWARD direct chain may land at body+8 and skip the
// poll: a cycle of direct branches must contain a backward edge (code addresses strictly increase
// along forward-only paths), and every indirect entry (IBTC/IC/ctx/SDC) still lands on body+0 --
// so every possible in-cache loop keeps polling, while straight-line chains (the common case in
// branchy interpreter code) stop paying a load+branch per block. g_fwdskip is 8 when that layout
// is active (aarch64 default), 0 otherwise (x86 engine, NOIRQSLIM/NOIRQCHECK/NOSTEAL1617).
static int g_fwdskip;
static struct {
    uint32_t *slot;
    uint64_t target;
    int is_bl;
    // is_bl: §B host bl, patch as bl
    int fwd; // IRQSLIM: forward direct edge -> patch to body+g_fwdskip (skip the entry poll)
} g_pend[1 << 16];
static int g_npend;
static void add_pend3(uint32_t *slot, uint64_t target, int is_bl, int fwd) {
    if (g_npend < (1 << 16)) {
        g_pend[g_npend].slot = slot;
        g_pend[g_npend].target = target;
        g_pend[g_npend].is_bl = is_bl;
        g_pend[g_npend].fwd = fwd;
        g_npend++;
    }
}
static void add_pend2(uint32_t *slot, uint64_t target, int is_bl) { add_pend3(slot, target, is_bl, 0); }
static void add_pend(uint32_t *slot, uint64_t target) { add_pend3(slot, target, 0, 0); }
static void patch_links_to(uint64_t gpc, void *body) {
    // body == NULL means gpc has no live translation (e.g. map_put silently failed on a full map).
    // Patching `b (body - slot)` would then bake a wild branch; leave the pends unresolved so they keep
    // taking the safe dispatcher round-trip until gpc is (re)registered with a real body.
    if (!body) return;
    for (int i = 0; i < g_npend;) {
        if (g_pend[i].target == gpc) {
            uint8_t *entry = (uint8_t *)body + (g_pend[i].fwd ? g_fwdskip : 0);
            int64_t d = (entry - (uint8_t *)g_pend[i].slot) / 4;
            *g_pend[i].slot =
                // bl / b target.body (+8: forward edge skips the entry poll under IRQSLIM)
                (g_pend[i].is_bl ? 0x94000000u : 0x14000000u) | ((uint32_t)d & 0x3FFFFFFu);
            sys_icache_invalidate(g_pend[i].slot, 4);
            // swap-remove
            g_pend[i] = g_pend[--g_npend];
        } else
            i++;
    }
}

// ============================================================================
// Stop-the-world code-cache flush (multi-threaded).
// ============================================================================
// The single-threaded wholesale flush (dispatch.c) reuses the 64MB arena in place: it resets the bump
// pointer and the block map, then re-translates over the old bytes. That is unsafe once a SECOND guest
// thread is live -- a peer may be executing a translated block we would overwrite. Rather than bail (the
// old `code cache full with threads (unsupported)` _exit(70)), we stop the world: every OTHER guest
// thread is parked at a safepoint (in a host signal handler, on its host stack, OFF the code cache),
// then we switch to a FRESH cache and release them. Each peer re-translates on demand. The OLD cache is
// retained and never modified, so a peer parked mid-block resumes into valid code and drifts onto the
// fresh cache at its next dispatcher round-trip.
//
// The common single-thread path never reaches here (dispatch.c gates on a live peer count), so this adds
// ZERO overhead to single-threaded execution.

// A host signal the guest signal map never targets (os/linux/signal.c sig_l2m()'s range omits 7/EMT and
// 29/INFO), so installing a process-wide handler for it cannot collide with an emulated guest signal.
#define STW_SIG SIGEMT
#define STW_MAXTHREAD 4096
// Registry of live guest threads: every thread that runs run_guest registers on entry and unregisters on
// exit, so a flusher can enumerate the peers to quiesce. `used` is atomic so peers_live()/the flusher see
// a consistent snapshot; the reg lock serializes slot allocation. `exec_gen` is the generation of the code
// cache this thread is currently executing in (published once per block by the dispatcher); the reclaimer
// uses it to free a retired cache only once no thread is still running in it. See reclaim_retired().
static struct {
    _Atomic int used;
    pthread_t th;
    _Atomic uint64_t exec_gen;
} g_stw_threads[STW_MAXTHREAD];
static pthread_mutex_t g_stw_reg_lock = PTHREAD_MUTEX_INITIALIZER;
static _Atomic int g_stw_active; // 1 while a flush is in progress -> parked peers spin until cleared
static _Atomic int g_stw_parked; // # of peers currently parked at the safepoint
static uint64_t g_stw_flushes;   // PROF: stop-the-world flushes performed

// ---- peer-refcounted retired-cache reclamation ----
// Each stop-the-world flush switches to a FRESH cache and RETIRES the old one. A retired cache of
// generation G must stay mapped until no guest thread can still execute in it: a peer parked mid-block (in
// the STW signal handler) resumes into the cache that was current when it parked, and only drifts onto the
// fresh cache at its next dispatcher round-trip. We give every cache a generation number (g_cache_gen,
// bumped on each flush-to-fresh) and have each thread publish the generation it is executing
// (g_stw_threads[].exec_gen, one relaxed store per block in the dispatcher, threaded-only). A retired
// cache is reclaimed (unmapped) once no live thread's exec_gen still names its generation. This bounds
// retained VA (no per-flush 64MB leak) AND removes the old unsafe reuse-in-place-on-alloc-failure path
// that corrupted parked peers.
static uint64_t g_cache_gen;                          // generation of the CURRENT cache (g_cache)
static __thread _Atomic uint64_t *g_my_exec_gen;      // this thread's exec_gen slot (NULL until registered)
#define STW_RETIRED_MAX (STW_MAXTHREAD + 8)
static struct {
    uint8_t *rw;     // RW base of the retired mapping
    ptrdiff_t rw2rx; // RX-RW delta (0 for the single-mapping MAP_JIT fallback)
    uint64_t gen;    // generation this cache served
} g_retired[STW_RETIRED_MAX];
static int g_nretired;

// Park safepoint handler -- async-signal-safe (atomics + nanosleep only). A peer caught here is, by
// definition, no longer executing a translated block (it is on its host stack in this handler), so the
// flusher may safely retire the cache while we spin.
static void stw_park_handler(int sig) {
    (void)sig;
    atomic_fetch_add_explicit(&g_stw_parked, 1, memory_order_seq_cst);
    while (atomic_load_explicit(&g_stw_active, memory_order_seq_cst)) {
        struct timespec ts = {0, 200000}; // 0.2ms
        nanosleep(&ts, NULL);
    }
    atomic_fetch_sub_explicit(&g_stw_parked, 1, memory_order_seq_cst);
}
static pthread_once_t g_stw_once = PTHREAD_ONCE_INIT;
static void stw_install(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = stw_park_handler;
    sa.sa_flags = SA_RESTART; // auto-restart interrupted host syscalls so a flush never perturbs a peer
    sigemptyset(&sa.sa_mask);
    sigaction(STW_SIG, &sa, NULL);
}
static void stw_register(void) {
    pthread_once(&g_stw_once, stw_install);
    // Guarantee the park signal is deliverable on this thread (a blocked STW_SIG would stall a flush).
    sigset_t unb;
    sigemptyset(&unb);
    sigaddset(&unb, STW_SIG);
    pthread_sigmask(SIG_UNBLOCK, &unb, NULL);
    // A flush holds g_stw_reg_lock for its whole duration, so while we hold it g_cache_gen is stable and
    // this thread will next execute the CURRENT cache -> seed exec_gen to that generation.
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (!atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed)) {
            g_stw_threads[i].th = pthread_self();
            atomic_store_explicit(&g_stw_threads[i].exec_gen, g_cache_gen, memory_order_relaxed);
            g_my_exec_gen = &g_stw_threads[i].exec_gen;
            atomic_store_explicit(&g_stw_threads[i].used, 1, memory_order_release);
            break;
        }
    pthread_mutex_unlock(&g_stw_reg_lock);
}
static void stw_unregister(void) {
    pthread_t me = pthread_self();
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) &&
            pthread_equal(g_stw_threads[i].th, me)) {
            atomic_store_explicit(&g_stw_threads[i].used, 0, memory_order_release);
            break;
        }
    pthread_mutex_unlock(&g_stw_reg_lock);
}
// # of OTHER live guest threads (excludes the caller). 0 -> the cheap in-place flush is safe.
static int stw_peers_live(void) {
    pthread_t me = pthread_self();
    int n = 0;
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) &&
            !pthread_equal(g_stw_threads[i].th, me))
            n++;
    pthread_mutex_unlock(&g_stw_reg_lock);
    return n;
}

// Unmap a retired cache's mapping(s): the RW base, plus the RX alias when dual-mapped (delta != 0).
static void cache_unmap(uint8_t *rw, ptrdiff_t rw2rx) {
    munmap(rw, CACHE_SZ);
    if (rw2rx) munmap(rw + rw2rx, CACHE_SZ);
}
// True if some live guest thread is still executing in generation `gen`. Caller holds g_stw_reg_lock;
// during a flush all peers are quiesced at the safepoint, so the exec_gen snapshot is stable.
static int gen_in_use(uint64_t gen) {
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) &&
            atomic_load_explicit(&g_stw_threads[i].exec_gen, memory_order_relaxed) == gen)
            return 1;
    return 0;
}
// Reclaim (unmap) every retired cache no live thread is still executing in. Caller holds BOTH g_jit_lock
// (so no peer can transition into a new block) and g_stw_reg_lock (so the registry is stable). Called from
// jit_flush_to_fresh before the fresh allocation, so freed VA is available to it.
static void reclaim_retired(void) {
    for (int i = 0; i < g_nretired;) {
        if (!gen_in_use(g_retired[i].gen)) {
            cache_unmap(g_retired[i].rw, g_retired[i].rw2rx);
            g_retired[i] = g_retired[--g_nretired]; // swap-remove
        } else
            i++;
    }
}
// Record the CURRENT cache as retired (its blocks may still be reached by parked peers / baked-in chains)
// so a later reclaim_retired() frees it once every thread has drifted off its generation.
static void retire_current(void) {
    if (g_nretired < STW_RETIRED_MAX) {
        g_retired[g_nretired].rw = g_cache;
        g_retired[g_nretired].rw2rx = g_rw2rx;
        g_retired[g_nretired].gen = g_cache_gen;
        g_nretired++;
    }
}
// A fresh cache could not be allocated and the peers are quiesced IN / parked ON the current cache, so
// reusing it in place would corrupt them on resume. Reclamation has already freed everything safe to free,
// so we cannot proceed -- abort cleanly rather than corrupt guest state.
static void cache_oom_abort(void) {
    static const char msg[] = "dd: JIT code cache exhausted (out of VA for a fresh cache under threads)\n";
    write(2, msg, sizeof msg - 1);
    _exit(70);
}

// Retire the current cache, switch to a brand-new one, and drop every cross-block link (map / IBTC /
// pending chains). The OLD cache is left mapped and UNMODIFIED (its blocks may still be reached by parked
// peers and by baked-in chains/inline ICs); reclaim_retired() unmaps it once no thread is in its
// generation, so retained VA stays bounded (no per-flush leak). MUST run with all peers quiesced
// (stw_flush) and the dispatcher holding g_jit_lock.
static void jit_flush_to_fresh(void) {
    reclaim_retired(); // free retired caches no peer is still in -> bound VA + free space for the new alloc
    if (g_dualmap) {
        uint8_t *rw;
        ptrdiff_t d;
        if (dualmap_alloc(&rw, &d) != 0) cache_oom_abort();
        retire_current();
        g_cache = g_cp = rw;
        g_rw2rx = d;
    } else {
        uint8_t *nc = mmap(NULL, CACHE_SZ, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
        if (nc == MAP_FAILED) cache_oom_abort();
        retire_current(); // rw2rx == 0 in this fallback -> reclaim unmaps the single RWX region
        g_cache = g_cp = nc;
    }
    g_cache_gen++; // peers still on the just-retired generation pin it until they round-trip
    memset(g_map, 0, sizeof g_map);
    memset(g_ibtc, 0, sizeof g_ibtc);
    g_npend = 0;
}

// Stop-the-world flush. Called from the dispatcher (holding g_jit_lock) when the cache is full and a peer
// guest thread is live: quiesce every peer at the park safepoint, switch to a fresh cache, then release.
static void stw_flush(void) {
    g_stw_flushes++;
    atomic_store_explicit(&g_stw_active, 1, memory_order_seq_cst);
    pthread_t me = pthread_self();
    int target = 0;
    // Hold g_stw_reg_lock for the WHOLE flush (not just the enumeration). stw_unregister() -- the only
    // place a guest thread clears its `used` slot and then terminates -- also takes this lock, so while we
    // hold it an exiting peer is pinned in stw_unregister and cannot terminate. That closes a lost-signal
    // hang: if the lock were dropped right after enumeration, a peer we just pthread_kill'd could unregister
    // and exit before its STW_SIG was ever delivered (the kernel discards a directed signal posted to a
    // thread that has already terminated). Its park would then never happen, g_stw_parked would never reach
    // `target`, and this flusher -- holding g_jit_lock -- would spin forever, stalling every guest thread
    // (the rustc/Go "blocks at exit, 0% CPU" hang). Pinned in stw_unregister, the peer instead takes the
    // pending STW_SIG (the park handler runs on top of the blocked pthread_mutex_lock), parks, and is
    // counted. Lock order is always g_jit_lock -> g_stw_reg_lock (matches stw_peers_live), so no deadlock;
    // the park handler itself takes no lock, so parking peers never need this lock to make progress.
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) &&
            !pthread_equal(g_stw_threads[i].th, me))
            if (pthread_kill(g_stw_threads[i].th, STW_SIG) == 0) target++;
    // Wait until every signaled peer has reached the safepoint (so none is executing in the cache).
    while (atomic_load_explicit(&g_stw_parked, memory_order_seq_cst) < target) {
        struct timespec ts = {0, 50000};
        nanosleep(&ts, NULL);
    }
    jit_flush_to_fresh();
    atomic_store_explicit(&g_stw_active, 0, memory_order_seq_cst); // release the world
    // Wait for all peers to leave the handler so the counters are clean for the next flush.
    while (atomic_load_explicit(&g_stw_parked, memory_order_seq_cst) > 0) {
        struct timespec ts = {0, 50000};
        nanosleep(&ts, NULL);
    }
    pthread_mutex_unlock(&g_stw_reg_lock);
}
// #267 SMC coherence: the guest overwrote already-translated code -> drop the cross-block link tables so the
// modified bytes re-translate on next dispatch. Only ever called with NO other guest thread live (single-
// threaded, or the caller holds g_jit_lock and stw_peers_live()==0); a wholesale drop cannot be made
// coherent while peers execute (see smc_icflush).
// NOTE: deliberately does NOT txln_clear(). The single-threaded in-place SMC soak (soak_smc / smc2) fires
// this drop on EVERY iteration (200k+); memset'ing the 16MB line-set each time added ~12s and timed the soak
// out. g_txln is kept MONOTONIC instead -- it only ever marks lines a translation WAS emitted from, so it
// never yields a stale "no-op" for a genuine in-place rewrite (the rewritten line stays marked -> the gate
// keeps firing the drop -> re-translation -> correct). Not un-marking a line whose block was dropped only
// ever causes an EXTRA (safe) drop later, never a missed one. (txpg_clear stays: 8x smaller, prior behaviour;
// the pcache paths still txln_clear on a new image, off the hot path.)
static void smc_inplace_drop(void) {
    memset(g_map, 0, sizeof g_map);
    memset(g_ibtc, 0, sizeof g_ibtc);
    g_npend = 0;
    txpg_clear();
}
// fork(): drop the inherited (parent-only) thread registry -- host fork() duplicates only the calling
// thread -- so a later flush in the child never signals a dead handle. Re-register the child's own thread.
static void stw_after_fork(void) {
    atomic_store_explicit(&g_stw_active, 0, memory_order_relaxed);
    atomic_store_explicit(&g_stw_parked, 0, memory_order_relaxed);
    pthread_mutex_init(&g_stw_reg_lock, NULL);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        atomic_store_explicit(&g_stw_threads[i].used, 0, memory_order_relaxed);
    g_stw_threads[0].th = pthread_self();
    atomic_store_explicit(&g_stw_threads[0].exec_gen, g_cache_gen, memory_order_relaxed);
    atomic_store_explicit(&g_stw_threads[0].used, 1, memory_order_relaxed);
    g_my_exec_gen = &g_stw_threads[0].exec_gen;
}

// fork() and the dual-mapped cache. Left alone, fork() would COW the RW and RX aliases independently and
// the child's two views of the SAME cache would silently diverge (writes through RW never reach the COW'd
// RX -> the child executes stale/zero pages). dualmap_alloc therefore marks the RX alias VM_INHERIT_NONE
// (the child gets a hole at the RX VA), and here -- in the child, before its next run_block -- we re-remap
// a fresh RX alias of the child's OWN COW-inherited RW pages at that SAME VA. That re-couples the aliases
// (child RW writes are visible through child RX again; verified empirically incl. nested forks) at the
// SAME addresses, so EVERY inherited translation, g_map/g_ibtc entry, cross-block chain and IC stays
// valid: a fork child resumes on the parent's warm code with ~zero rebuild cost (#371; was a full 64MB
// dual-map rebuild + ~13MB of map memsets = ~0.7ms per fork, plus a full re-translate of everything).
//
// Preserving translations across fork is exactly what the single-mapping MAP_JIT fallback has always done
// (its page-table execute permission and content are inherited correctly), so the preserved-arena
// semantics are the long-proven fallback semantics -- the dual map now just matches them.
//
// THREADED parent: a peer M may be mid-translate at the fork instant (holding g_jit_lock), so the
// inherited arena/g_map can be a torn snapshot. The single surviving thread cannot tell, so in that case
// we keep the conservative pre-#371 behaviour: build a FRESH dual map and drop the inherited translations
// (the child re-translates on demand). g_fork_preserved tells proc.c whether the per-arch caches keyed on
// cache VAs (x86 g_xibtc) survived (1) or must be dropped (0).
static int g_fork_preserved;
static void jit_after_fork(void) {
    g_fork_preserved = 1; // MAP_JIT fallback + successful re-remap keep the arena; rebuild paths clear it
    stw_after_fork(); // single-threaded child: shed the inherited thread registry (also for the MAP_JIT path)
    // fork() only clones the CALLING thread. If a peer M was translating (holding g_jit_lock, and g_cache_lock
    // under it in map_put) at the instant the guest forked, the child inherits those mutexes LOCKED with no
    // owner thread left to release them -- so the child's very first dispatcher iteration deadlocks forever in
    // run_guest's `pthread_mutex_lock(&g_jit_lock)` (0% CPU) while its parent blocks reaping it. This is THE
    // go/npm/cargo build hang (#285/#175): a heavily-threaded driver (Go compiler, node) forks a child while
    // sibling Ms are mid-translate. The child is single-threaded now, so reinitialising both locks to a clean
    // unlocked state is always correct (no surviving peer can hold or want them; the calling thread never holds
    // an engine lock across a guest syscall). Must run before the !g_dualmap early return so the MAP_JIT path
    // is covered too.
    pthread_mutex_init(&g_jit_lock, NULL);
    pthread_mutex_init(&g_cache_lock, NULL);
    // The child inherited the parent's retired-cache list as COW copies but will never resume a parent peer
    // into them; drop the bookkeeping and unmap them so the child does not carry the parent's retired VA.
    for (int i = 0; i < g_nretired; i++)
        cache_unmap(g_retired[i].rw, g_retired[i].rw2rx);
    g_nretired = 0;
    if (!g_dualmap) return;
    if (!g_threaded) {
        // Single-threaded parent (shells, make, configure -- the fork-storm case): the inherited RW arena
        // is a consistent snapshot (no peer could be mid-emit; the forking thread never forks while
        // translating). Re-alias RX from the child's RW at the SAME VA the parent used -- the RX range is
        // a guaranteed hole here (VM_INHERIT_NONE in dualmap_alloc). ~1us, preserves everything.
        mach_vm_address_t rx = (mach_vm_address_t)(g_cache + g_rw2rx);
        vm_prot_t cur = 0, max = 0;
        kern_return_t kr = mach_vm_remap(mach_task_self(), &rx, CACHE_SZ, 0, VM_FLAGS_FIXED, mach_task_self(),
                                         (mach_vm_address_t)g_cache, FALSE, &cur, &max, VM_INHERIT_NONE);
        if (kr == KERN_SUCCESS) {
            if (mach_vm_protect(mach_task_self(), rx, CACHE_SZ, FALSE, VM_PROT_READ | VM_PROT_EXECUTE) ==
                KERN_SUCCESS)
                return; // arena + g_map/g_ibtc/chains all still valid -- nothing to drop
            // we own the new mapping but could not make it executable: scrub OUR mapping, then rebuild.
            mach_vm_deallocate(mach_task_self(), rx, CACHE_SZ);
        }
        // Defensive fall-through (the FIXED remap can only fail if something else occupied the RX hole,
        // which nothing between fork() and this hook should do): leave whatever occupies the range alone
        // -- it is not ours to deallocate -- and take the full rebuild below, which allocates elsewhere.
    }
    // Threaded parent (or the defensive fall-through): conservative rebuild -- fresh dual map, drop the
    // (possibly torn) inherited translations; the child re-translates on demand.
    //
    // NB: do NOT munmap the old RX range here. In this child it is a hole (VM_INHERIT_NONE at
    // dualmap_alloc; or scrubbed/foreign after the defensive fall-through above), and dualmap_alloc below
    // is free to place the FRESH arena exactly inside that 64MB gap -- a post-alloc munmap of the old RX
    // range would then silently unmap the brand-new cache and the child's first emission faults
    // (SIGSEGV at new-RW+0x10; hit by every fork from a threaded parent until this note existed).
    g_fork_preserved = 0;
    uint8_t *old_rw = g_cache;
    uint8_t *rw;
    ptrdiff_t d;
    if (dualmap_alloc(&rw, &d) != 0) { // alloc failure (extremely rare): leave map as-is. The RX range is
        // a hole in the child (INHERIT_NONE), so re-alias it in place as a last resort; if even that
        // fails the child aborts on its first fetch, which is still better than executing stale bytes.
        mach_vm_address_t rx = (mach_vm_address_t)(g_cache + g_rw2rx);
        vm_prot_t cur = 0, max = 0;
        if (mach_vm_remap(mach_task_self(), &rx, CACHE_SZ, 0, VM_FLAGS_FIXED, mach_task_self(),
                          (mach_vm_address_t)g_cache, FALSE, &cur, &max, VM_INHERIT_NONE) == KERN_SUCCESS)
            mach_vm_protect(mach_task_self(), rx, CACHE_SZ, FALSE, VM_PROT_READ | VM_PROT_EXECUTE);
        return;
    }
    g_cache = g_cp = rw;
    g_rw2rx = d;
    memset(g_map, 0, sizeof g_map);
    memset(g_ibtc, 0, sizeof g_ibtc);
    g_npend = 0;
    munmap(old_rw, CACHE_SZ); // the old RW was still mapped through the alloc, so it cannot overlap the
                              // fresh arena; the old RX range is a pre-existing hole (see the NB above)
}
