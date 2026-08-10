// translator/guest/aarch64 -- persistent cross-process translated-code cache for the aarch64
// engine. Mirror of the x86 cache (guest/x86_64/cache.c), adapted to the two ways the
// aarch64 engine differs from the x86 frontend:
//
//   1. NO reloc centralization. The x86 emitter routes every baked host pointer through one
//      emit_host_ptr() into a g_reloc table; the aarch64 emitter bakes host addresses inline
//      (block_return via e_movconst, &g_ibtc / &g_t2cnt[] via adrp+add) with no record. So this file
//      provides the recorded emitters (emit_blockret / emit_ibtcptr / emit_t2cntptr / pc_record_icsite)
//      that stubs.c + translate.c call. When g_pcache is OFF they fall back to
//      the exact original emitters, so the default correctness matrix stays byte-identical.
//
//   2. DUAL-MAPPED W^X arena. The engine writes g_cache (RW alias) and executes g_cache+g_rw2rx (RX
//      alias). We memcpy the persisted bytes through the RW alias inside the single jit_wprot() write
//      window (a no-op under dual mapping) and publish the RX alias through host memory services.
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
//     and bars the inherited epoch from saving. An in-process execve flushes the arena, re-keys, and resets
//     every recording table, creating a clean cache-production epoch for the new image.
//   * DYNAMIC LIBRARIES use deterministic mmap hints and a persisted {base,len,file-identity} manifest.
//     Their restored maps and instruction provenance remain deferred until the same file identity maps at
//     the same range. Unclassified translations are never persisted, and identity drift cannot activate
//     stale host code over different guest bytes.
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
// Keyed by (engine build id, cpu-struct size, map/ibtc sizes, both fixed bases,
// entry PC, argv[0] basename, and the identity -- dev/ino/size/mtime(ns) -- of the guest binary AND its
// interpreter). Opt in via HL_PCACHE=1.

#define PC_MAGIC 0x34414350544a4c48ull // "HLJTPCA4" (LE tag)
#define PC_VERSION 11                  // v11 adds the identity-validated dynamic-library manifest.
#define PC_VERSION_EFF PC_VERSION
#define PC_TRANSLATOR_ABI HL_PCACHE_ABI_AARCH64
#define PC_IMG_BASE 0x0000040000000000ull    // 4 TB -- fixed guest image base (probed free on Apple silicon)
#define PC_INTERP_BASE 0x0000048000000000ull // 4.5 TB -- fixed interp (ld.so) base
#define PC_LIB_BASE 0x0000050000000000ull    // 5 TB -- deterministic dynamic-library window
#define PC_LIB_SPAN (1ull << 38)             // 256 GB; beyond it mappings use ordinary placement
#define PC_LIB_MAX 512                       // bounded persisted identity manifest
#define PC_RELOC_CAP (1u << 20)              // recorded baked-host-pointer slots (poison if exceeded)

#include "../../cache_abi.h"
#include "../../reloc.h"
#include "../../digest.h"
#include "../../identity.h"
#include "../../window.h"
#include "../../persist.h"

// reloc kinds (packed into pc_reloc.info: kind<<0 | rd<<8 | slot<<16)
#define RK_BLOCKRET 1   // 4-insn movz/movk of block_return into reg `rd`
#define RK_IBTC 2       // 4-insn movz/movk of &g_ibtc into reg `rd`
#define RK_T2CNT 3      // 4-insn movz/movk of &g_t2cnt[slot] into reg `rd`
#define RK_ICSITE 4     // 16-byte per-site IC {target,body} literal pair -> zero on load (neutralize)
#define RK_BUSFAULT 5   // 4-insn pointer to the generic translated-memory BUS query
#define RK_GUEST_ADRP 6 // one-insn ADRP of a fixed guest page; re-encode for the live arena RX base

// ---- engine state (defined here; used by the recorded emitters + load/save) ----
static int g_pcache;   // persistent cache active (HL_PCACHE=1)
static int g_coldprof; // Internal cache timing diagnostics; production entry keeps this disabled.
static hl_persist_directory g_pc_directory;
static char g_pc_directory_path[1024];
static uint64_t g_force_base;   // if nonzero, load_elf() maps the NEXT image MAP_FIXED here (one-shot; elf.c)
static int g_force_base_failed; // a fixed-VA map fell back to a kernel base -> this image can't hit OR save
static uint64_t g_pc_binid;     // identity of the guest binary+interp+argv0+engine+mode (cache file key)
static uint64_t g_pc_entry;     // initial guest pc (sanity key)
static int g_pcache_poison;     // an unrecorded baked host pointer may exist -> save() refuses (correctness)
static int g_pcache_loaded;     // this run restored from cache -> never re-save (arena would snowball)
static int g_pcache_forked;     // this process is a fork child -> never save (stale-bookkeeping guard)
static hl_reloc g_reloc_storage[PC_RELOC_CAP];
static hl_reloc_table g_reloc_table = {g_reloc_storage, 0, (int)PC_RELOC_CAP};
#define g_reloc (g_reloc_table.records)
#define g_nreloc (g_reloc_table.count)

#define PC_PROV_CAP (1u << 16)

struct pc_prov {
    uint64_t host_off, guest;
    uint32_t size, reserved;
};
static struct pc_prov g_pc_prov[PC_PROV_CAP];
static uint32_t g_pc_nprov;
static uint64_t g_pc_img_lo, g_pc_img_hi, g_pc_interp_lo, g_pc_interp_hi;
static uint64_t g_pc_lib_next = PC_LIB_BASE;

struct pc_lib {
    uint64_t base, len, id;
};

static struct pc_lib g_pc_libs[PC_LIB_MAX];
static int g_pc_nlib;
static struct pc_mapent *g_pc_defer;
static uint64_t g_pc_ndefer;
static struct pc_prov *g_pc_prov_defer;
static uint64_t g_pc_nprov_defer;

static void pcache_record_provenance(uint64_t host, uint64_t end, uint64_t guest) {
    jit_instruction_map_put(host, end, guest);
    if (!g_pcache || host < (uint64_t)g_cache || end < host || end - host > UINT32_MAX || g_pc_nprov >= PC_PROV_CAP) {
        if (g_pcache && g_pc_nprov >= PC_PROV_CAP) g_pcache_poison = 1;
        return;
    }
    g_pc_prov[g_pc_nprov++] = (struct pc_prov){host - (uint64_t)g_cache, guest, (uint32_t)(end - host), 0};
}

#if defined(__GNUC__) && !defined(__clang__) && defined(__aarch64__)
extern void block_return(void) __attribute__((visibility("hidden")));
#else
static void block_return(void);
#endif

static void pc_reloc_add(uint32_t off, uint8_t kind, uint8_t rd, uint16_t slot) {
    if (!hl_reloc_add(&g_reloc_table, off, (uint32_t)kind | ((uint32_t)rd << 8) | ((uint32_t)slot << 16))) {
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

static void emit_busfaultptr(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_BUSFAULT, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)jit_guest_bus_fault);
    } else {
        e_movconst(rd, (uint64_t)jit_guest_bus_fault);
    }
}

// Materialize a page-aligned GUEST address with one host ADRP when it is reachable from the RX alias.
// The instruction PC is the executable alias, not g_cp's writable alias.  Persistent-cache reload may
// put that alias at a different VA, so record the instruction: the loader recovers its fixed guest target
// from the saved instruction + saved RX base and re-encodes it relative to the live RX base.
//
// Returns 1 after emitting, or 0 if the target is outside ADRP's signed 21-page (±4 GiB) range.  Callers
// must use their existing e_movconst fallback on 0.
static int emit_guest_adrp_page(int rd, uint64_t target) {
    uint64_t target_page = target & ~UINT64_C(0xfff);
    uint64_t pc_page = (uint64_t)J_RX(g_cp) & ~UINT64_C(0xfff);
    int64_t delta = (int64_t)(target_page - pc_page);
    if ((delta & 0xfff) || delta < -INT64_C(0x100000000) || delta > INT64_C(0xfffff000)) return 0;
    int64_t pages = delta >> 12;
    uint32_t imm21 = (uint32_t)pages & 0x1fffff;
    if (g_pcache) pc_reloc_add((uint32_t)(g_cp - g_cache), RK_GUEST_ADRP, (uint8_t)rd, 0);
    emit32(0x90000000u | ((imm21 & 3) << 29) | (((imm21 >> 2) & 0x7ffff) << 5) | (uint32_t)rd);
    return 1;
}

// Record a per-site IC's 16-byte cached {target,body} literal pair so a reload can zero it (the cached
// body pointer is an arena address that would be stale in a fresh process; a zeroed guard never matches
// -> the site harmlessly re-resolves through the dispatcher, which rewrites both literals).
static void pc_record_icsite(uint8_t *lt) {
    if (g_pcache) pc_reloc_add((uint32_t)(lt - g_cache), RK_ICSITE, 0, 0);
}

// ---- persisted layout ----
// [pc_hdr][reloc][map][pend][t2][txpg][provenance][library manifest][arena bytes]
struct pc_hdr {
    uint64_t magic, version;
    uint64_t translator_abi;
    uint64_t cpu_sz, jit_map_n, ibtc_n;
    uint64_t img_base, interp_base;
    uint64_t bin_id, entry_jump;
    uint64_t arena_used;
    uint64_t n_reloc, n_mapent, n_pend, n_t2, n_txpg, n_prov, n_lib;
    uint64_t csum;                     // FNV-1a over every byte after this header
    uint64_t block_return_at, ibtc_at; // diagnostics only (we re-emit from live symbols)
    uint64_t arena_rx_at;              // v6: RX base used to encode RK_GUEST_ADRP instructions
};

struct pc_mapent {
    uint64_t gpc, guest_start, guest_end, host_off, body_off;
};

struct pc_pend {
    uint64_t slot_off, target, source_gpc;
    uint32_t kind, fwd, orig, reserved;
};

struct pc_t2 {
    uint64_t gpc, cnt;
};

// Only fixed images and identity-validated, deterministically placed file mappings are revivable.
static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    if (base > UINT64_MAX - span) return;
    if (base >= PC_INTERP_BASE) {
        g_pc_interp_lo = base;
        g_pc_interp_hi = base + span;
    } else if (base >= PC_IMG_BASE) {
        g_pc_img_lo = base;
        g_pc_img_hi = base + span;
    }
}

static int pc_range_fixed(uint64_t start, uint64_t end) {
    return start < end &&
           ((start >= g_pc_img_lo && end <= g_pc_img_hi) || (start >= g_pc_interp_lo && end <= g_pc_interp_hi));
}

static int pc_range_in_lib(uint64_t start, uint64_t end) {
    if (start >= end) return 0;
    for (int i = 0; i < g_pc_nlib; i++) {
        uint64_t limit = g_pc_libs[i].base + g_pc_libs[i].len;
        if (limit >= g_pc_libs[i].base && start >= g_pc_libs[i].base && end <= limit) return 1;
    }
    return 0;
}

static uint64_t pcache_mmap_hint(uint64_t len) {
    if (!g_pcache || len > UINT64_MAX - UINT64_C(0x1fffff)) return 0;
    uint64_t rounded = (len + UINT64_C(0x1fffff)) & ~UINT64_C(0x1fffff);
    if (rounded > UINT64_MAX - UINT64_C(0x200000)) return 0;
    uint64_t span = rounded + UINT64_C(0x200000);
    uint64_t base = __atomic_fetch_add(&g_pc_lib_next, span, __ATOMIC_RELAXED);
    if (base < PC_LIB_BASE || base > PC_LIB_BASE + PC_LIB_SPAN || span > PC_LIB_BASE + PC_LIB_SPAN - base) return 0;
    return base;
}

#define PCACHE_MMAP_HINT 1

static void pcache_note_libmap(uint64_t base, uint64_t len, const hl_host_file_metadata *metadata) {
    if (!g_pcache || !metadata || !len || base > UINT64_MAX - len) return;
    uint64_t id = hl_identity_file(metadata);
    if (!id) return;
    if (!g_pcache_loaded) {
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        if (g_pc_nlib < PC_LIB_MAX) g_pc_libs[g_pc_nlib++] = (struct pc_lib){base, len, id};
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        return;
    }
    for (int i = 0; i < g_pc_nlib; i++) {
        if (g_pc_libs[i].base != base) continue;
        if (g_pc_libs[i].len != len || g_pc_libs[i].id != id) return;
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        for (uint64_t j = 0; j < g_pc_ndefer; j++) {
            struct pc_mapent *entry = &g_pc_defer[j];
            if (entry->host_off && entry->gpc >= base && entry->gpc < base + len) {
                map_put(entry->gpc, entry->guest_start, entry->guest_end, g_cache + entry->host_off,
                        g_cache + entry->body_off);
                entry->host_off = 0;
            }
        }
        for (uint64_t j = 0; j < g_pc_nprov_defer; j++) {
            struct pc_prov *entry = &g_pc_prov_defer[j];
            if (entry->size && entry->guest >= base && entry->guest < base + len) {
                jit_instruction_map_put((uint64_t)g_cache + entry->host_off,
                                        (uint64_t)g_cache + entry->host_off + entry->size, entry->guest);
                entry->size = 0;
            }
        }
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        return;
    }
}

static uint64_t pcache_id_of(const char *path) {
    return hl_identity_source(&g_jit_services, path);
}

// Per-engine-build tag so the cache self-invalidates across engine rebuilds: host bytes emitted by another
// build must never be loaded, because they are instructions the current translator would not have emitted.
//
// The explicit translator ABI is authoritative for embedded engines, where g_self_path identifies the
// outer application rather than the translator archive. The executable identity and compile-time tag remain
// useful mix-ins, but neither replaces bumping PC_TRANSLATOR_ABI after an incompatible codegen change.
static uint64_t pcache_engine_id(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    uint64_t build = hl_digest_bytes(HL_DIGEST_SEED, tag, sizeof tag - 1);
    build = hl_digest_bytes(build, &(uint64_t){pcache_id_of(g_self_path)}, sizeof(uint64_t));
    build = hl_digest_bytes(build, &(uint64_t){PC_TRANSLATOR_ABI}, sizeof(uint64_t));
    uint64_t modes = (uint64_t)(g_guestfold != 0) | ((uint64_t)(g_steal1617 != 0) << 1) |
                     ((uint64_t)(g_noibslim != 0) << 2) | ((uint64_t)(g_mtibtc != 0) << 3) |
                     ((uint64_t)(g_no_stw_reclaim != 0) << 4) | ((uint64_t)(g_prof != 0) << 5) |
                     ((uint64_t)(uint32_t)g_fwdskip << 32);
    return hl_identity_configuration(build, 1, 1, modes);
}

// Hash the BASENAME of argv[0]. A multicall binary (busybox, toolchain drivers) runs DIFFERENT code
// paths per argv[0]; the translated arena is therefore per-applet, so the cache MUST be keyed by argv[0]
// too or one applet loads another's arena. Basename (not full argv) so a single-purpose binary invoked
// with varying flags -- e.g. go's `compile -o pkgN.a -p pkgN ...` -- keeps ONE cache reused across all
// its invocations (the go-build fork-storm win).
static uint64_t pcache_argv0_id(const char *argv0) {
    return hl_identity_name(argv0);
}

static uint64_t pcache_make_id(const char *prog_host, const char *interp_host, const char *argv0) {
    uint64_t a = pcache_id_of(prog_host);
    uint64_t b = interp_host ? pcache_id_of(interp_host) : 0xABCDEFull;
    return hl_identity_mix(a, b, pcache_engine_id(), pcache_argv0_id(argv0));
}

static int pcache_file(char *out, size_t n) {
    const char *dir = hl_option_get("HL_PCACHE_DIR");
    if (!dir || !dir[0]) dir = "/tmp/hl-engine-pcache-aarch64";
    if (g_pc_directory.handle != HL_HOST_HANDLE_INVALID && strcmp(g_pc_directory_path, dir) != 0) {
        (void)hl_persist_directory_close(&g_pc_directory);
        g_pc_directory_path[0] = 0;
    }
    if (g_pc_directory.handle == HL_HOST_HANDLE_INVALID &&
        !hl_persist_directory_open(&g_pc_directory, &g_jit_services, dir, 1))
        return 0;
    if (!g_pc_directory_path[0]) {
        int copied = snprintf(g_pc_directory_path, sizeof g_pc_directory_path, "%s", dir);
        if (copied <= 0 || (size_t)copied >= sizeof g_pc_directory_path) {
            (void)hl_persist_directory_close(&g_pc_directory);
            g_pc_directory_path[0] = 0;
            return 0;
        }
    }
    int written = snprintf(out, n, "%016llx.pcache", (unsigned long long)g_pc_binid);
    return written > 0 && (size_t)written < n;
}

static void pcache_directory_close(void) {
    if (g_pc_directory.handle != HL_HOST_HANDLE_INVALID) (void)hl_persist_directory_close(&g_pc_directory);
    g_pc_directory_path[0] = 0;
}

// Re-emit / neutralize every recorded slot for THIS process. Runs inside the jit_wprot() write window,
// against the RW alias (g_cache + off). Offsets/slots were validated by pcache_load before we get here.
static void pcache_relocate(uint64_t saved_rx) {
    for (int i = 0; i < g_nreloc; i++) {
        uint32_t off = g_reloc[i].off, info = g_reloc[i].info;
        int kind = info & 0xff, rd = (info >> 8) & 0xff, slot = (info >> 16) & 0xffff;
        uint32_t *w = (uint32_t *)(g_cache + off);
        uint64_t v;
        switch (kind) {
        case RK_BLOCKRET: v = (uint64_t)block_return; break;
        case RK_IBTC: v = (uint64_t)g_ibtc; break;
        case RK_T2CNT: v = (uint64_t)&g_t2cnt[slot]; break;
        case RK_BUSFAULT: v = (uint64_t)jit_guest_bus_fault; break;
        case RK_ICSITE:
            // Zero the guard target literal (+0): a zeroed target makes the site's equality guard miss so
            // the dispatcher re-resolves + refills. Under the x16/x17 steal the +8 slot (Lb) is not a stale
            // arena pointer but the hit branch's arena OFFSET (stable across reload) -> keep it so the refill
            // can re-find the branch; the stale direct branch stays unreachable (guard misses) until then.
            // Legacy (non-steal) still stores a stale body pointer at +8 -> zero it.
            *(uint64_t *)(g_cache + off) = 0;
            if (!g_steal1617) *(uint64_t *)(g_cache + off + 8) = 0;
            continue;
        case RK_GUEST_ADRP: {
            // Validation already proved this target remains reachable from the live RX alias.
            uint32_t in = w[0];
            uint32_t imm21 = ((in >> 29) & 3) | (((in >> 5) & 0x7ffff) << 2);
            int64_t pages = (int32_t)(imm21 << 11) >> 11;
            uint64_t saved_pc_page = (saved_rx + off) & ~UINT64_C(0xfff);
            uint64_t target_page = saved_pc_page + (uint64_t)(pages * INT64_C(4096));
            uint64_t live_pc_page = ((uint64_t)J_RX(g_cache) + off) & ~UINT64_C(0xfff);
            int64_t live_pages = (int64_t)(target_page - live_pc_page) >> 12;
            uint32_t live_imm21 = (uint32_t)live_pages & 0x1fffff;
            w[0] = 0x90000000u | ((live_imm21 & 3) << 29) | (((live_imm21 >> 2) & 0x7ffff) << 5) | rd;
            continue;
        }
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
static int pc_reloc_ok(hl_reloc r, uint64_t arena_used) {
    int kind = r.info & 0xff, slot = (r.info >> 16) & 0xffff;
    uint64_t width = kind == RK_GUEST_ADRP ? 4 : 16;
    if (!hl_window_contains(arena_used, r.off, width, kind == RK_ICSITE ? 8 : 4)) return 0;
    if (kind == RK_ICSITE) return 1;
    if (((r.info >> 8) & 0xff) > 30) return 0; // rd must be a real GPR (we never bake into sp/xzr)
    if (kind == RK_T2CNT) return slot < T2_MAX;
    return kind == RK_BLOCKRET || kind == RK_IBTC || kind == RK_BUSFAULT || kind == RK_GUEST_ADRP;
}

// Validate the opcode and prove that an ADRP target recovered relative to the saved RX base remains
// encodable relative to this process's RX base.  Do this before rebuilding any live map state, so a cache
// created with an unusually distant arena base degrades to a clean miss instead of leaving partial state.
static int pc_guest_adrp_ok(hl_reloc r, const uint8_t *arena, uint64_t saved_rx) {
    if ((r.info & 0xff) != RK_GUEST_ADRP) return 1;
    uint32_t in;
    memcpy(&in, arena + r.off, sizeof in);
    int rd = (r.info >> 8) & 0xff;
    if ((in & 0x9f000000u) != 0x90000000u || (in & 31) != (uint32_t)rd) return 0;
    uint32_t imm21 = ((in >> 29) & 3) | (((in >> 5) & 0x7ffff) << 2);
    int64_t pages = (int32_t)(imm21 << 11) >> 11;
    uint64_t saved_pc_page = (saved_rx + r.off) & ~UINT64_C(0xfff);
    uint64_t target_page = saved_pc_page + (uint64_t)(pages * INT64_C(4096));
    uint64_t live_pc_page = ((uint64_t)J_RX(g_cache) + r.off) & ~UINT64_C(0xfff);
    int64_t delta = (int64_t)(target_page - live_pc_page);
    return !(delta & 0xfff) && delta >= -INT64_C(0x100000000) && delta <= INT64_C(0xfffff000);
}

// Returns 1 on HIT (arena + maps restored -> translation of the startup path is skipped). ANY mismatch /
// truncation / checksum failure / out-of-bounds record -> 0 (graceful MISS; the caller translates fresh
// and the exit-time save atomically replaces the bad file).
static int pcache_load(uint64_t entry_jump) {
    if (!g_pcache || !g_pc_binid || g_force_base_failed) return 0;
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024];
    if (!pcache_file(path, sizeof path)) return 0;
    void *image = NULL;
    size_t image_size = 0;
    if (!hl_persist_load_at(&g_pc_directory, path, CACHE_SZ + UINT64_C(134217728), &image, &image_size)) return 0;
    hl_persist_cursor cursor = {image, image_size, 0};
    struct pc_hdr h;
    if (!hl_persist_take(&cursor, &h, sizeof h)) {
        free(image);
        return 0;
    }
    if (h.magic != PC_MAGIC || !hl_pcache_compatible(h.version, h.translator_abi, PC_VERSION_EFF, PC_TRANSLATOR_ABI) ||
        h.cpu_sz != sizeof(struct cpu) || h.jit_map_n != JIT_MAP_N || h.ibtc_n != IBTC_N || h.img_base != PC_IMG_BASE ||
        h.interp_base != PC_INTERP_BASE || h.bin_id != g_pc_binid || h.entry_jump != entry_jump ||
        h.arena_used > CACHE_SZ || (h.arena_used & 3) || h.n_reloc > PC_RELOC_CAP || h.n_mapent > JIT_MAP_N ||
        h.n_pend > (1u << 16) || h.n_t2 > T2_MAX || h.n_txpg > TXPG_N || h.n_prov > PC_PROV_CAP ||
        h.n_lib > PC_LIB_MAX) {
        free(image);
        return 0;
    }
    hl_reloc *re = h.n_reloc ? malloc(h.n_reloc * sizeof *re) : NULL;
    struct pc_mapent *me = h.n_mapent ? malloc(h.n_mapent * sizeof *me) : NULL;
    struct pc_pend *pe = h.n_pend ? malloc(h.n_pend * sizeof *pe) : NULL;
    struct pc_t2 *te = h.n_t2 ? malloc(h.n_t2 * sizeof *te) : NULL;
    uint64_t *tx = h.n_txpg ? malloc(h.n_txpg * sizeof *tx) : NULL;
    struct pc_prov *pv = h.n_prov ? malloc(h.n_prov * sizeof *pv) : NULL;
    struct pc_lib *libs = h.n_lib ? malloc(h.n_lib * sizeof *libs) : NULL;
    uint8_t *abuf = h.arena_used ? malloc(h.arena_used) : NULL;
    int ok = (h.n_reloc == 0 || re) && (h.n_mapent == 0 || me) && (h.n_pend == 0 || pe) && (h.n_t2 == 0 || te) &&
             (h.n_txpg == 0 || tx) && (h.n_prov == 0 || pv) && (h.n_lib == 0 || libs) && (h.arena_used == 0 || abuf);
#define PC_RD(buf, nbytes) (ok && (ok = hl_persist_take(&cursor, (buf), (size_t)(nbytes))))
    PC_RD(re, h.n_reloc * sizeof *re);
    PC_RD(me, h.n_mapent * sizeof *me);
    PC_RD(pe, h.n_pend * sizeof *pe);
    PC_RD(te, h.n_t2 * sizeof *te);
    PC_RD(tx, h.n_txpg * sizeof *tx);
    PC_RD(pv, h.n_prov * sizeof *pv);
    PC_RD(libs, h.n_lib * sizeof *libs);
    if (ok) ok = hl_persist_take(&cursor, abuf, (size_t)h.arena_used) && cursor.offset == cursor.size;
#undef PC_RD
    free(image);
    // Whole-payload checksum BEFORE trusting any record (bit rot / short file / foreign writer).
    if (ok) {
        hl_digest digest;
        hl_digest_init(&digest, HL_DIGEST_SEED);
        hl_digest_update(&digest, re, h.n_reloc * sizeof *re);
        hl_digest_update(&digest, me, h.n_mapent * sizeof *me);
        hl_digest_update(&digest, pe, h.n_pend * sizeof *pe);
        hl_digest_update(&digest, te, h.n_t2 * sizeof *te);
        hl_digest_update(&digest, tx, h.n_txpg * sizeof *tx);
        hl_digest_update(&digest, pv, h.n_prov * sizeof *pv);
        hl_digest_update(&digest, libs, h.n_lib * sizeof *libs);
        hl_digest_update(&digest, abuf, h.arena_used);
        ok = hl_digest_value(&digest) == h.csum;
    }
    // Per-record bounds: every offset a later pass will WRITE or BRANCH through must be inside the arena.
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = pc_reloc_ok(re[i], h.arena_used);
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = pc_guest_adrp_ok(re[i], abuf, h.arena_rx_at);
    for (uint64_t i = 0; ok && i < h.n_mapent; i++)
        ok = hl_window_contains(h.arena_used, me[i].host_off, 1, 4) &&
             hl_window_contains(h.arena_used, me[i].body_off, 1, 4) && me[i].guest_start <= me[i].gpc &&
             me[i].gpc < me[i].guest_end;
    for (uint64_t i = 0; ok && i < h.n_pend; i++) {
        ok = hl_window_contains(h.arena_used, pe[i].slot_off, 4, 4) && pe[i].kind <= 2 && pe[i].fwd <= 1;
        if (ok && pe[i].kind == 2) {
            uint32_t in = pe[i].orig;
            int valid = (in & 0xff000010u) == 0x54000000u || (in & 0x7e000000u) == 0x34000000u ||
                        (in & 0x7e000000u) == 0x36000000u;
            ok = valid && !(pe[i].source_gpc & 3) &&
                 pe[i].fwd == (uint32_t)(g_fwdskip && pe[i].target > pe[i].source_gpc);
        }
    }
    for (uint64_t i = 0; ok && i < h.n_prov; i++)
        ok = pv[i].reserved == 0 && pv[i].size != 0 && hl_window_contains(h.arena_used, pv[i].host_off, pv[i].size, 4);
    for (uint64_t i = 0; ok && i < h.n_lib; i++) {
        uint64_t end = libs[i].base + libs[i].len;
        ok = libs[i].id != 0 && libs[i].len != 0 && !(libs[i].base & UINT64_C(0x1fffff)) && end >= libs[i].base &&
             libs[i].base >= PC_LIB_BASE && end <= PC_LIB_BASE + PC_LIB_SPAN;
        for (uint64_t j = 0; ok && j < i; j++) {
            uint64_t other_end = libs[j].base + libs[j].len;
            ok = end <= libs[j].base || libs[i].base >= other_end;
        }
    }
    if (!ok) {
        free(re);
        free(me);
        free(pe);
        free(te);
        free(tx);
        free(pv);
        free(libs);
        free(abuf);
        return 0;
    }

    // Rebuild engine state from the offset-relative records.
    if (!hl_reloc_import(&g_reloc_table, re, (size_t)h.n_reloc)) {
        free(re);
        free(me);
        free(pe);
        free(te);
        free(tx);
        free(pv);
        free(libs);
        free(abuf);
        return 0;
    }
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
    memcpy(g_pc_libs, libs, (size_t)h.n_lib * sizeof *libs);
    g_pc_nlib = (int)h.n_lib;
    uint64_t map_deferred = 0;
    for (uint64_t i = 0; i < h.n_mapent; i++) {
        if (pc_range_fixed(me[i].guest_start, me[i].guest_end))
            map_put(me[i].gpc, me[i].guest_start, me[i].guest_end, g_cache + me[i].host_off, g_cache + me[i].body_off);
        else if (pc_range_in_lib(me[i].guest_start, me[i].guest_end))
            map_deferred++;
    }
    if (map_deferred) {
        g_pc_defer = malloc((size_t)map_deferred * sizeof *g_pc_defer);
        if (g_pc_defer)
            for (uint64_t i = 0; i < h.n_mapent; i++)
                if (!pc_range_fixed(me[i].guest_start, me[i].guest_end) &&
                    pc_range_in_lib(me[i].guest_start, me[i].guest_end))
                    g_pc_defer[g_pc_ndefer++] = me[i];
    }
    pend_reset();
    for (uint64_t i = 0; i < h.n_pend; i++) {
        uint32_t *slot = (uint32_t *)(g_cache + pe[i].slot_off);
        if (pe[i].kind == 2)
            add_pend_cond(slot, pe[i].target, pe[i].orig, pe[i].source_gpc, (int)pe[i].fwd);
        else
            add_pend3(slot, pe[i].target, (int)pe[i].kind, (int)pe[i].fwd);
    }
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
    g_pc_nprov = 0;
    uint64_t prov_deferred = 0;
    for (uint64_t i = 0; i < h.n_prov; i++) {
        if (pc_range_fixed(pv[i].guest, pv[i].guest + 4)) {
            jit_instruction_map_put((uint64_t)g_cache + pv[i].host_off, (uint64_t)g_cache + pv[i].host_off + pv[i].size,
                                    pv[i].guest);
            g_pc_prov[g_pc_nprov++] = pv[i];
        } else if (pc_range_in_lib(pv[i].guest, pv[i].guest + 4)) {
            prov_deferred++;
        }
    }
    if (prov_deferred) {
        g_pc_prov_defer = malloc((size_t)prov_deferred * sizeof *g_pc_prov_defer);
        if (g_pc_prov_defer)
            for (uint64_t i = 0; i < h.n_prov; i++)
                if (!pc_range_fixed(pv[i].guest, pv[i].guest + 4) && pc_range_in_lib(pv[i].guest, pv[i].guest + 4))
                    g_pc_prov_defer[g_pc_nprov_defer++] = pv[i];
    }
    g_cp = g_cache + h.arena_used;
    free(re);
    free(me);
    free(pe);
    free(te);
    free(tx);
    free(pv);
    free(libs);

    // Commit the arena bytes + re-emit every baked host pointer, then publish to the I-cache.
    if (!jit_wprot(0)) {
        free(abuf);
        return 0;
    }
    memcpy(g_cache, abuf, h.arena_used);
    pcache_relocate(h.arena_rx_at);
    if (!jit_wprot(1) || !jit_publish_code(J_RX(g_cache), h.arena_used)) {
        free(abuf);
        return 0;
    }
    memset(g_ibtc, 0, sizeof g_ibtc); // shared IBTC data table: refills lazily
    free(abuf);
    g_pcache_loaded = 1;
    // Restored blocks carry page info only (their lines were not decoded here); keep the historical
    // page-fallback path for them by arming eager line recording for every block translated from now on,
    // matching the pre-lazy behaviour for a warm-loaded arena.
    g_txln_active = 1;
    /*
     * Restored blocks predate the live line-content table and can contain
     * direct caller ingress.  The first warm-image SMC must therefore perform
     * the normal activation prime before later events become precisely
     * source-targeted.
     */
    g_txln_prime = 1;
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
    if (g_pcache_poison || g_pcache_loaded || g_pcache_forked || g_force_base_failed || smc_seen()) return;
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024];
    if (!pcache_file(path, sizeof path)) return;
    // ---- consistent snapshot under the translation lock (peers may still be running/translating) ----
    pthread_mutex_lock(&g_jit_lock);
    uint64_t nmap = 0;
    for (uint32_t i = 0; i < JIT_MAP_N; i++)
        if (map_live(i) && (pc_range_fixed(g_map_guest_start[i], g_map_guest_end[i]) ||
                            pc_range_in_lib(g_map_guest_start[i], g_map_guest_end[i])))
            nmap++;
    uint64_t ntxpg = 0;
    for (uint32_t i = 0; i < TXPG_N; i++)
        if (g_txpg[i] && (pc_range_fixed(g_txpg[i], g_txpg[i] + UINT64_C(4096)) ||
                          pc_range_in_lib(g_txpg[i], g_txpg[i] + UINT64_C(4096))))
            ntxpg++;
    uint64_t nprov = 0;
    for (uint32_t i = 0; i < g_pc_nprov; i++)
        if (pc_range_fixed(g_pc_prov[i].guest, g_pc_prov[i].guest + 4) ||
            pc_range_in_lib(g_pc_prov[i].guest, g_pc_prov[i].guest + 4))
            nprov++;
    uint64_t arena_used = (uint64_t)(g_cp - g_cache);
    struct pc_hdr h;
    memset(&h, 0, sizeof h);
    h.magic = PC_MAGIC;
    h.version = PC_VERSION_EFF;
    h.translator_abi = PC_TRANSLATOR_ABI;
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
    h.n_prov = nprov;
    h.n_lib = (uint64_t)g_pc_nlib;
    h.block_return_at = (uint64_t)block_return;
    h.ibtc_at = (uint64_t)g_ibtc;
    h.arena_rx_at = (uint64_t)J_RX(g_cache);
    // Build the whole image in one heap buffer -> one write() (per-record writes dominated the save cost).
    size_t total = sizeof h + (size_t)g_nreloc * sizeof(hl_reloc) + (size_t)nmap * sizeof(struct pc_mapent) +
                   (size_t)g_npend * sizeof(struct pc_pend) + (size_t)g_t2n * sizeof(struct pc_t2) +
                   (size_t)ntxpg * sizeof(uint64_t) + (size_t)nprov * sizeof(struct pc_prov) +
                   (size_t)g_pc_nlib * sizeof(struct pc_lib) + arena_used;
    uint8_t *buf = malloc(total);
    int ok = buf != NULL;
    if (ok) {
        uint8_t *w = buf + sizeof h; // header written last (its csum covers everything after it)
        memcpy(w, g_reloc, (size_t)g_nreloc * sizeof(hl_reloc));
        w += (size_t)g_nreloc * sizeof(hl_reloc);
        for (uint32_t i = 0; i < JIT_MAP_N; i++) {
            if (!map_live(i) || !(pc_range_fixed(g_map_guest_start[i], g_map_guest_end[i]) ||
                                  pc_range_in_lib(g_map_guest_start[i], g_map_guest_end[i])))
                continue;
            // The hot map entry stays 32 bytes; cold source bounds live in parallel arrays and are persisted
            // so a warm-loaded block remains individually invalidatable after guest code rewrites.
            struct pc_mapent e = {g_map[i].gpc, g_map_guest_start[i], g_map_guest_end[i],
                                  (uint64_t)((uint8_t *)g_map[i].host - g_cache),
                                  (uint64_t)((uint8_t *)g_map[i].body - g_cache)};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_npend; i++) {
            struct pc_pend e = {(uint64_t)((uint8_t *)g_pend[i].slot - g_cache),
                                g_pend[i].target,
                                g_pend[i].source_gpc,
                                (uint32_t)g_pend[i].is_bl,
                                (uint32_t)g_pend[i].fwd,
                                g_pend[i].orig,
                                0};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_t2n; i++) {
            struct pc_t2 e = {g_t2gpc[i], g_t2cnt[i]};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (uint32_t i = 0; i < TXPG_N; i++)
            if (g_txpg[i] && (pc_range_fixed(g_txpg[i], g_txpg[i] + UINT64_C(4096)) ||
                              pc_range_in_lib(g_txpg[i], g_txpg[i] + UINT64_C(4096)))) {
                memcpy(w, &g_txpg[i], 8);
                w += 8;
            }
        for (uint32_t i = 0; i < g_pc_nprov; i++)
            if (pc_range_fixed(g_pc_prov[i].guest, g_pc_prov[i].guest + 4) ||
                pc_range_in_lib(g_pc_prov[i].guest, g_pc_prov[i].guest + 4)) {
                memcpy(w, &g_pc_prov[i], sizeof g_pc_prov[i]);
                w += sizeof g_pc_prov[i];
            }
        memcpy(w, g_pc_libs, (size_t)g_pc_nlib * sizeof(struct pc_lib));
        w += (size_t)g_pc_nlib * sizeof(struct pc_lib);
        memcpy(w, g_cache, arena_used); // read from the RW alias is always permitted
        h.csum = hl_digest_bytes(HL_DIGEST_SEED, buf + sizeof h, total - sizeof h);
        memcpy(buf, &h, sizeof h);
    }
    pthread_mutex_unlock(&g_jit_lock);
    if (ok) ok = hl_persist_store_at(&g_pc_directory, path, buf, total);
    free(buf);
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
// image -- so drop the inherited reloc records and bar this process from saving. Execve may safely LOAD
// an independently-published cache after re-keying, but it must not publish the child epoch: the translated
// arena still descends from the fork snapshot. A later exec may publish only after its complete
// identity/arena/relocation/provenance/library reset in pcache_exec_reload.
static void pcache_after_fork(void) {
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    // Do not call the allocator in the immediate fork hook. The inherited buffers remain owned by this
    // process and are released at the next exec reset; zero counts make them unreachable in this epoch.
    g_pc_ndefer = 0;
    g_pc_nprov_defer = 0;
    g_pcache_forked = 1;
}

#define PCACHE_FORK_HOOK pcache_after_fork()

// ---- wholesale-flush hook (engine/dispatch.c, after the cache-full in-place or stop-the-world flush) --
// The arena content the records described is gone (bump pointer reset / fresh arena). Everything emitted
// from here on re-records against the new arena, so the "every baked pointer recorded" invariant holds by
// construction after a plain reset. (A restored-then-flushed run is already barred from saving by
// g_pcache_loaded; this keeps the cold-run bookkeeping correct too.)
static void pcache_after_wholesale_flush(void) {
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
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
        g_pc_img_lo = g_pc_img_hi = g_pc_interp_lo = g_pc_interp_hi = 0;
    }
}

static void pcache_exec_force_interp(void) {
    if (g_pcache) g_force_base = PC_INTERP_BASE;
}

static void pcache_exec_reload(const char *prog_host, const char *interp_host, const char *argv0, uint64_t jump) {
    if (!g_pcache) return;
    // Execve has flushed the old arena and installed a new fixed-base image. Reset every cache-production
    // datum in lockstep with that identity boundary. The new epoch may publish even when its process was
    // originally forked: no inherited relocation, provenance, map, or library state survives this reset.
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    g_t2n = 0;    // fresh tier-2 slot set for the new image (no cross-image alias)
    txpg_clear(); // nothing is translated now; the set re-fills (or is restored by the load below)
    txln_clear();
    g_pcache_loaded = 0; // allow a cold-miss save of the NEW binary
    g_pcache_forked = 0;
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
    g_pc_nlib = 0;
    __atomic_store_n(&g_pc_lib_next, PC_LIB_BASE, __ATOMIC_RELAXED);
    g_pc_binid = pcache_make_id(prog_host, interp_host, argv0);
    g_pc_entry = jump;
    int hit = pcache_load(jump);
    if (g_coldprof) fprintf(stderr, "[pcache] exec %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
}

#define PCACHE_EXEC_HOOKS 1

#define PCACHE_SAVE_HOOK pcache_save()
