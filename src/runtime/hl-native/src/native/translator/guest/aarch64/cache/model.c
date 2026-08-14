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
#define PC_VERSION 13                  // v13 disables persistence for mutable file-backed library mappings.
#define PC_VERSION_EFF PC_VERSION
#define PC_TRANSLATOR_ABI HL_PCACHE_ABI_AARCH64
#define PC_IMG_BASE 0x0000040000000000ull    // 4 TB -- fixed guest image base (probed free on Apple silicon)
#define PC_INTERP_BASE 0x0000048000000000ull // 4.5 TB -- fixed interp (ld.so) base
#define PC_LIB_BASE 0x0000050000000000ull    // 5 TB -- deterministic dynamic-library window
#define PC_LIB_SPAN (1ull << 38)             // 256 GB; beyond it mappings use ordinary placement
#define PC_LIB_MAX 512                       // bounded persisted identity manifest
#define PC_RELOC_CAP (1u << 20)              // recorded baked-host-pointer slots (poison if exceeded)

#include "../../../cache_abi.h"
#include "../../../reloc.h"
#include "../../../digest.h"
#include "../../../identity.h"
#include "../../../persist.h"

static int pc_window_contains(uint64_t extent, uint64_t offset, uint64_t width, uint64_t alignment) {
    if (alignment == 0 || offset % alignment != 0) return 0;
    return offset <= extent && width <= extent - offset;
}

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
static hl_identity_digest g_pc_binid; // full identity of guest+interp+argv0+engine+mode
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
