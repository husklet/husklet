#include "../engine/fatal_diagnostic.h"

// translator -- the code cache, the gpc->host block map, and lazy inter-block chaining.
// One W^X MAP_JIT arena; blocks appended + chained (b/bl backpatch). Host-ISA engine state.

// ---------------- JIT code cache ----------------
#include "../linux_abi/host_mman.h"
#include "../include/hl/log.h"
#include "../host/clock.h"
#include "../host/cpu.h"
#include "../host/range.h"
#include "../engine/fatal.h"
#include "arena.h"
#include "emit.h"
#include "guest_fetch.h"

#include <stddef.h>

#define CACHE_SZ (64u << 20)
_Static_assert(CACHE_SZ <= INT32_MAX,
               "every source and target in one immutable cache generation must be rel32-reachable");
/* A stitched AArch64 region may contain 4096 guest instructions.  When a
   file-backed BUS ledger is active, one memory instruction expands into a
   validated guard plus the original operation.  Reserve enough space for the
   largest translated region before entering an emitter; the former 64 KiB
   reserve was smaller than a legal guarded region and allowed g_cp to reach
   the adjacent executable alias. */
#define CACHE_EMIT_HEADROOM (4u << 20)
// Emission-arena state stays TU-local. The aliases preserve existing unity callsites while making the
// ownership boundary explicit for the future inline assembler context.
static hl_emit_state g_emit;
#define g_cache g_emit.base
#define g_cp g_emit.cursor
#define g_emit_start g_emit.start
#define g_rw2rx g_emit.rx_delta
#define g_dualmap g_emit.dual_alias
#define g_wx_toggles g_emit.wx_toggles
#define g_code_mapping g_emit.mapping

// ---- dual-mapped (W^X-toggle-free) code cache ----
// g_cache/g_cp are the RW (writer) alias; the engine EXECUTES through an RX alias of the
// SAME physical pages at g_cache + g_rw2rx (created by vm_remap'ing to a second address,
// the Apple-Silicon dual-map JIT technique). All PC-relative emission/back-patching is a
// difference of two cache addresses, so it is alias-invariant and needs no conversion;
// only the few ABSOLUTE handoffs (run_block target, IBTC/IC body literals, icache flush)
// convert RW<->RX. g_rw2rx == 0 selects the single-MAP_JIT fallback that toggles the whole
// region's W^X per translation/IC-fill (NODUALMAP=1).
static hl_log_context g_jit_log;
static hl_fatal_context g_jit_fatal;
static int cache_oom_fail(void);
#define J_RX(p) hl_emit_rx(&g_emit, (const void *)(uintptr_t)(p)) // RW alias addr -> RX alias addr
#define J_RW(p) hl_emit_rw(&g_emit, (const void *)(uintptr_t)(p)) // RX alias addr -> RW alias addr

// DIAGNOSTIC predicate (elf.c fatal-fault guard): is a host PC inside the CURRENT RX code cache arena?
int jit_pc_in_cache(uint64_t pc, uint64_t *base) {
    uint64_t lo = (uint64_t)g_cache + g_rw2rx, hi = lo + CACHE_SZ;
    if (base) *base = lo;
    return g_cache && pc >= lo && pc < hi;
}

// The single W^X gate. Under dual mapping it is a no-op: writes land on the RW alias and
// execution reads the RX alias, so no per-region permission flip (and no peer-thread race).
static int jit_fail(hl_status status, const char *message, size_t size) {
    (void)hl_fatal_report(&g_jit_fatal, status, HL_LOG_TAG_JIT, message, size);
    return 0;
}

static inline int jit_wprot(int enable_exec) {
    hl_host_result result;
    if (g_dualmap) return 1;
    g_wx_toggles++;
    result = enable_exec ? g_jit_services.memory->end_code_write(g_jit_services.context)
                         : g_jit_services.memory->begin_code_write(g_jit_services.context);
    if (result.status != HL_STATUS_OK)
        return jit_fail(result.status, "unable to change JIT write protection",
                        sizeof("unable to change JIT write protection") - 1u);
    return 1;
}

static int jit_publish_code(const void *address, size_t size) {
    uintptr_t current = (uintptr_t)address;
    uintptr_t writable = (uintptr_t)g_cache;
    uintptr_t executable = (uintptr_t)J_RX(g_cache);
    uint64_t offset;
    hl_host_result result;
    if (current >= writable && current - writable <= CACHE_SZ && size <= CACHE_SZ - (current - writable))
        offset = (uint64_t)(current - writable);
    else if (current >= executable && current - executable <= CACHE_SZ && size <= CACHE_SZ - (current - executable))
        offset = (uint64_t)(current - executable);
    else {
        return jit_fail(HL_STATUS_INVALID_ARGUMENT, "code publication outside JIT mapping",
                        sizeof("code publication outside JIT mapping") - 1u);
    }
    result = g_jit_services.memory->publish_code(g_jit_services.context, g_code_mapping.handle, offset, size);
    if (result.status != HL_STATUS_OK)
        return jit_fail(result.status, "unable to publish translated code",
                        sizeof("unable to publish translated code") - 1u);
    return 1;
}

static int code_mapping_reserve(hl_host_code_mapping *mapping, int dual_alias) {
    uint64_t alignment;
    if (hl_host_services_validate(&g_jit_services, HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_CODE_MAPPING) !=
        HL_STATUS_OK)
        return -1;
    if (g_jit_log.host == NULL) (void)hl_log_context_init(&g_jit_log, &g_jit_services, hl_option_get("HL_LOG"));
    hl_fatal_diagnostic_init(&g_jit_services, hl_option_get("HL_FATAL_DIAGNOSTICS"));
    /* Must start on a host page boundary: reserve_code rejects anything smaller, and the dual-alias path
       maps the object twice at that granularity so RW and RX differ by whole pages (making g_rw2rx a constant
       delta).  16384 is the fallback for a host that cannot answer. */
    alignment = (uint64_t)hl_host_page_size();
    if (alignment == 0 || (alignment & (alignment - 1)) != 0) alignment = 16384u;
    return hl_arena_reserve(&g_jit_services, CACHE_SZ, alignment, dual_alias, mapping);
}

/* A dual alias is an optimization, not an allocation invariant.  Apply the same fallback at every
   arena allocation site: a long-running threaded guest reaches this path again at rollover, where
   address-space fragmentation can reject the second alias even though one executable mapping still fits. */
typedef int (*hl_code_mapping_reserve_fn)(hl_host_code_mapping *, int, void *);

static int code_mapping_reserve_adapter(hl_host_code_mapping *mapping, int dual_alias, void *opaque) {
    (void)opaque;
    return code_mapping_reserve(mapping, dual_alias);
}

static int code_mapping_reserve_preferred_with(hl_host_code_mapping *mapping, int dual_alias,
                                                hl_code_mapping_reserve_fn reserve, void *opaque) {
    if (reserve(mapping, dual_alias, opaque) == 0) return 0;
    return dual_alias ? reserve(mapping, 0, opaque) : -1;
}

static int code_mapping_reserve_preferred(hl_host_code_mapping *mapping, int dual_alias) {
    return code_mapping_reserve_preferred_with(mapping, dual_alias, code_mapping_reserve_adapter, NULL);
}

#if HL_NATIVE_TEST_HOOKS
typedef struct {
    int attempts;
} hl_rollover_mapping_test_state;

static int rollover_mapping_test_entry(void) { return 42; }

static int rollover_mapping_test_reserve(hl_host_code_mapping *mapping, int dual_alias, void *opaque) {
    hl_rollover_mapping_test_state *state = opaque;
    state->attempts++;
    if (dual_alias) return -1;
    memset(mapping, 0, sizeof *mapping);
    mapping->abi = 1;
    mapping->size = sizeof *mapping;
    mapping->writable_address = (uint64_t)(uintptr_t)rollover_mapping_test_entry;
    mapping->executable_address = mapping->writable_address;
    return 0;
}

/* Exercises the exact allocator decision used by threaded rollover.  The injected first attempt
   deterministically rejects the dual alias; the fallback must return a single executable target. */
int HL_TARGET_LOCAL(jit_rollover_mapping_test)(uint64_t *result) {
    hl_host_code_mapping mapping;
    hl_rollover_mapping_test_state state = {0};
    if (code_mapping_reserve_preferred_with(&mapping, 1, rollover_mapping_test_reserve, &state) != 0) return -ENOMEM;
    if (state.attempts != 2 || mapping.writable_address != mapping.executable_address ||
        mapping.executable_address != (uint64_t)(uintptr_t)rollover_mapping_test_entry)
        return -EUCLEAN;
    *result = (uint64_t)rollover_mapping_test_entry();
    return *result == 42 ? 0 : -EUCLEAN;
}
#endif

static int jit_cache_init(void) {
    hl_fatal_context_init(&g_jit_fatal, &g_jit_services);
    // Dual aliases avoid global W^X flips. Hosts that cannot create them still have a correct MAP_JIT
    // path; this is a capability fallback, not a user-facing mode switch.
#if defined(__APPLE__)
    // Apple hardened runtime: the dual-alias RX mapping is *non*-MAP_JIT executable memory
    // (mach_vm_remap + VM_PROT_EXECUTE). Under strict AMFI that alias is only tolerated with the
    // restricted `com.apple.security.cs.disable-executable-page-protection` entitlement, which is NOT
    // honored for an ad-hoc signature — the kernel SIGKILLs the engine the moment it executes the
    // alias (fine on SIP-disabled/VM macs, lethal on stock macos-26 CI runners). The single-MAP_JIT
    // W^X path (dual_alias=0, g_rw2rx==0, pthread_jit_write_protect flips) needs only `allow-jit` and
    // works on every mac, so select it by default here. Reserving with dual_alias=0 keeps g_dualmap
    // consistent for later flush re-reserves (code_mapping_reserve(&mapping, g_dualmap)) and matches
    // the historical fork-preserving path (see notes at the !g_dualmap fork sites).
    if (code_mapping_reserve(&g_code_mapping, 0) != 0) {
        (void)cache_oom_fail();
        return -1;
    }
#else
    if (code_mapping_reserve_preferred(&g_code_mapping, 1) != 0) {
        (void)cache_oom_fail();
        return -1;
    }
#endif
    hl_arena_bind(&g_emit, &g_code_mapping);
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "cache reserve rw=%p rx=%p bytes=%u dual=%d", (void *)g_cache, J_RX(g_cache),
            CACHE_SZ, g_dualmap);
    return 0;
}

#include "../engine/profile.h"

// Dispatcher profiling is one state object. Compatibility aliases keep the wider profiling/reporting
// code source-compatible while that ownership is progressively narrowed.
static hl_dispatch_profile g_dispatch_profile;
static uint64_t g_service_ns;
#define g_prof (g_dispatch_profile.enabled)
#define g_prof_cross (g_dispatch_profile.crossings)
#define g_prof_xlate (g_dispatch_profile.translations)
#define g_xlate_ns (g_dispatch_profile.translation_ns)

static inline uint64_t now_ns(void) {
    hl_host_result result = g_jit_services.clock->monotonic_ns(g_jit_services.context);
    return result.status == HL_STATUS_OK ? result.value : 0;
}

static inline void jit_backoff_ns(uint64_t interval_ns) {
    (void)g_jit_services.clock->backoff_ns(g_jit_services.context, interval_ns);
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
// Monotonic synchronization authority for decisions which must not race the
// interval between authorizing a guest peer and registering its host thread.
static _Atomic int g_ever_threaded;

// gpc->host block map capacity. Sized so the CACHE_SZ arena fills (-> the dispatcher's wholesale
// flush) LONG before this open-addressed table does: even all-minimum-size blocks (prologue + a one-insn
// exit, ~90 host words ~360B) cap at ~186K live blocks in a full cache, so 2^19 slots keeps the load
// factor under ~40% (short linear-probe chains) and guarantees map_put never silently fails mid-run. A
// FULL table made map_put a no-op -> map_body() then returned NULL for a freshly-translated block, and
// patch_links_to() back-patched a `b (NULL - slot)` wild branch (mongod, ~65K blocks of C++ static init,
// crashed with SIGILL/SIGSEGV here). NOT the leaked container-state MAP_N (that one is unrelated, 64K).
#define JIT_MAP_N (1u << 19)
#define TXPG_N (1u << 18)
#define TXLN_N (1u << 21)

typedef struct {
    uint64_t gpc;
    // Liveness generation folded INTO the entry (was a parallel g_map_generation[] array). map_idx/map_body
    // and map_put previously touched TWO large sparse arrays per probe -- the 20MB entry table AND a
    // separate 2MB generation array -- costing two independent cache misses per lookup step. gpc and
    // generation now share the entry's first 16 bytes (one cache line), halving the per-probe miss count on
    // the hot lookup (map_body runs inside the decode loop's stitch checks) and on map_put. Semantics are
    // unchanged: an entry is live iff generation == g_map_epoch.
    uint32_t generation;
    /*
     * Open-address deletion marker, and the reason this entry is still 32 bytes: {uint64_t; uint32_t;
     * pointer; pointer} leaves four bytes of padding after `generation`, so the marker rides along for
     * free.  It belongs here on merit as well -- `generation` and `tombstone_epoch` are the two halves of
     * one question, "is this slot live, dead-but-chained, or empty", and map_idx/map_put ask both on every
     * probe step that lands on a non-live slot.  Held in its own JIT_MAP_N array it made that step read a
     * second multi-megabyte table, which on a cold or freshly-forked address space is a second page fault.
     *
     * An invalidated slot cannot become an ordinary empty slot, because a colliding live key may follow it
     * in the probe cluster.  Tagging the tombstone with the epoch preserves that lookup chain without
     * clearing an array on a wholesale generation change.
     */
    uint32_t tombstone_epoch;
    void *host;
    void *body;
} hl_translation_map_entry;

// The marker above is only free while it fits the padding; if the entry ever grows, every probe pays a
// second cache line and the hot lookup's one-line property is gone.
_Static_assert(sizeof(hl_translation_map_entry) == 32, "translation entry must stay one half cache line");
_Static_assert(_Alignof(hl_translation_map_entry) == 8, "translation entry alignment changed");
_Static_assert(offsetof(hl_translation_map_entry, generation) == 8, "translation generation left the first 16 bytes");
_Static_assert(offsetof(hl_translation_map_entry, tombstone_epoch) == 12,
               "translation tombstone no longer occupies the pointer-alignment padding");
_Static_assert(offsetof(hl_translation_map_entry, host) == 16, "translation host pointer moved");
_Static_assert(offsetof(hl_translation_map_entry, body) == 24, "translation body pointer moved");

// All indexes describing the currently live translation generation share one owner. Keep these arrays
// embedded (rather than separately allocated) so the hot lookup layout and zero-initialized lifetime stay
// byte-for-byte equivalent. The compatibility aliases below intentionally leave the existing inline paths
// unchanged while reset sites can treat this as one coherent translation index.
typedef struct {
    hl_translation_map_entry map[JIT_MAP_N];
    uint64_t pages[TXPG_N];
    uint64_t lines[TXLN_N];
    uint64_t hashes[TXLN_N];
} hl_translation_index;

// Cache-line align the index so map[0] starts on a 64B boundary. With a 32B entry that guarantees every
// entry lies wholly within one cache line (map[j] at base+32*j, base%64==0 -> offset 0 or 32, never
// straddling). Without this the array's natural 8B alignment left ~half the 32B entries straddling two
// lines, doubling the cold-miss traffic the smaller entry was meant to remove.
static _Alignas(64) hl_translation_index g_translation_index;
#define g_map g_translation_index.map
#define g_txpg g_translation_index.pages
#define g_txln g_translation_index.lines
#define g_txlh g_translation_index.hashes

// Clearing the 12 MiB translation hash on every in-place code patch made a correct SMC workload spend
// almost all of its time in memset.  Entries belong to a logical generation instead: advancing the
// generation invalidates the whole table in O(1), while normal lookup still stops at the first slot that
// is empty in the current generation.  A physical clear is needed only after the 32-bit epoch wraps.
static uint32_t g_map_epoch = 1;
static uint64_t g_map_host_generation = 1;
static uint64_t g_cache_gen; /* generation of the current immutable code arena */
static uint32_t g_live_map_indices[JIT_MAP_N];
static uint32_t g_live_map_positions[JIT_MAP_N];
static uint32_t g_live_map_count;
#if HL_NATIVE_TEST_HOOKS
static _Thread_local uint64_t g_map_host_probe_count;
#endif

typedef struct {
    uint64_t gpc;
    uint64_t generation;
    void *host;
} hl_map_host_cache_entry;

static _Thread_local hl_map_host_cache_entry g_map_host_cache[2];

static __attribute__((noinline, noclone)) hl_map_host_cache_entry *map_host_cache_current(void) {
    return g_map_host_cache;
}

static void map_host_cache_invalidate(void) {
    uint64_t next = __atomic_add_fetch(&g_map_host_generation, 1, __ATOMIC_RELEASE);
    if (next == 0) __atomic_store_n(&g_map_host_generation, 1, __ATOMIC_RELEASE);
}

/*
 * A live entry's cold per-slot record: the decoded guest-source interval, and the code-cache generation
 * which owns the entry's immutable host bytes.  Kept beside the hot 32-byte hash entry rather than
 * inside it, because map lookup reads none of these and must keep touching exactly one cache line,
 * while the SMC slow path needs them to retire only translations which consumed a rewritten line.
 *
 * These three fields were three parallel JIT_MAP_N arrays.  map_put writes all three for every newly
 * translated block, and as separate tables those stores landed on three different pages of three
 * multi-megabyte arrays, so a process paid three first-touch -- or, after fork, copy-on-write -- page
 * faults per translated block where one would do.  Measured on an x86_64 Linux host at 641d3f580 with
 * `perf record -e page-faults -c 1`, differencing an 8-spawn run against a 0-spawn one: a guest
 * fork+exec translates ~900 blocks, and the three arrays plus the tombstone array now folded into the
 * entry above cost 2,196 of the spawn's 5,726 page faults.  One record per slot pays that once, and
 * map_source_overlaps' two loads become adjacent where they were certainly two pages apart.
 *
 * The record is 24 bytes and deliberately unpadded: nothing reads it on the hot path, so paying a
 * quarter more pages -- and a quarter more copy-on-write after fork -- to line-align a cold field
 * would spend exactly what this change is here to recover.
 */
typedef struct {
    uint64_t guest_start;
    uint64_t guest_end;
    uint64_t cache_generation;
} hl_translation_map_metadata;

_Static_assert(sizeof(hl_translation_map_metadata) == 24, "cold translation record gained padding");
_Static_assert(_Alignof(hl_translation_map_metadata) == 8, "cold translation record alignment changed");
_Static_assert(offsetof(hl_translation_map_metadata, guest_start) == 0, "cold guest start moved");
_Static_assert(offsetof(hl_translation_map_metadata, guest_end) == 8, "cold guest end is no longer adjacent");
_Static_assert(offsetof(hl_translation_map_metadata, cache_generation) == 16, "cold cache generation moved");

static hl_translation_map_metadata g_map_metadata[JIT_MAP_N];

/*
 * Guest-source page -> translation map slot reverse index.  A block contributes one compact node for
 * every 4 KiB source page it intersects.  Both tables are bounded: unusual giant blocks, a full page
 * table, or node exhaustion latch overflow and retain the full-scan invalidator as the authoritative
 * fallback until map_clear().  Thus resource pressure can cost time, never correctness.
 *
 * Page slots and nodes carry the map epoch implicitly: page slots are tagged, while nodes are reachable
 * only from a tagged slot and the bump allocator is rewound on an epoch change.  Reset is therefore O(1).
 * source_index_put() runs before map_put publishes the map entry's generation, so a quiescent invalidator
 * can never observe a live entry which is absent from the index.  The existing stop-the-world mapping
 * boundary remains the concurrency authority; this index adds no second lock or weaker publication path.
 */
#define JIT_SOURCE_PAGE_N (1u << 18)
#define JIT_SOURCE_NODE_N JIT_MAP_N
#define JIT_SOURCE_PAGE_SHIFT 12u
#define JIT_SOURCE_NONE UINT32_MAX

typedef struct {
    uint64_t page;
    uint32_t head;
    uint32_t epoch;
} jit_source_page_entry;

typedef struct {
    uint32_t map_index;
    uint32_t next;
} jit_source_node;

_Static_assert(sizeof(jit_source_page_entry) == 16, "source page entry gained padding");
_Static_assert(sizeof(jit_source_node) == 8, "source index node must remain compact");

static jit_source_page_entry g_source_pages[JIT_SOURCE_PAGE_N];
static jit_source_node g_source_nodes[JIT_SOURCE_NODE_N];
static uint32_t g_source_node_count;
static int g_source_index_overflow;
#if HL_NATIVE_TEST_HOOKS
static int g_source_index_publish_probe;
static int g_source_index_publish_observed;
#endif

// Bounded instruction provenance shared by diagnostics and synchronous guest-fault delivery. Translation
// records source boundaries; execution performs no checkpoint writes. Epoch publication makes signal-side
// reads coherent, while the circular bound caps metadata at 8 MiB.
#define JIT_INSN_MAP_N (1u << 18)

typedef struct {
    uint64_t host, end, guest;
    uint32_t preserve_registers;
    uint32_t epoch;
} jit_instruction_map_entry;

static jit_instruction_map_entry g_instruction_map[JIT_INSN_MAP_N];
static uint32_t g_instruction_map_next;
static int jit_host_to_rwpc(uint64_t host_pc, uint64_t *rwpc);
static inline __attribute__((always_inline)) int jit_resolve_rw_code(void *rwcode, void **rxcode, uint64_t *generation);
static void ibtc_drop_target(uint64_t target);
static void ibtc_clear_lazy(void);

static void jit_instruction_map_put_preserve(uint64_t host, uint64_t end, uint64_t guest,
                                             uint32_t preserve_registers) {
    if (host >= end) return;
    uint32_t index = __atomic_fetch_add(&g_instruction_map_next, 1u, __ATOMIC_RELAXED) & (JIT_INSN_MAP_N - 1u);
    g_instruction_map[index].host = host;
    g_instruction_map[index].end = end;
    g_instruction_map[index].guest = guest;
    g_instruction_map[index].preserve_registers = preserve_registers;
    __atomic_store_n(&g_instruction_map[index].epoch, g_map_epoch, __ATOMIC_RELEASE);
}

static void jit_instruction_map_put(uint64_t host, uint64_t end, uint64_t guest) {
    jit_instruction_map_put_preserve(host, end, guest, 0);
}

static int jit_instruction_map_lookup(uint64_t rwpc, uint64_t *guest, uint32_t *preserve_registers) {
    uint64_t best = 0, source = 0;
    uint32_t preserve = 0;
    for (uint32_t i = 0; i < JIT_INSN_MAP_N; i++) {
        uint32_t epoch = __atomic_load_n(&g_instruction_map[i].epoch, __ATOMIC_ACQUIRE);
        if (epoch == g_map_epoch && g_instruction_map[i].host <= rwpc && rwpc < g_instruction_map[i].end &&
            g_instruction_map[i].host >= best) {
            best = g_instruction_map[i].host;
            source = g_instruction_map[i].guest;
            preserve = g_instruction_map[i].preserve_registers;
        }
    }
    if (!best) return 0;
    if (guest) *guest = source;
    if (preserve_registers) *preserve_registers = preserve;
    return 1;
}

static int jit_instruction_guest_pc(uint64_t host_pc, uint64_t *guest_pc) {
    uint64_t rwpc;
    if (!jit_host_to_rwpc(host_pc, &rwpc)) return 0;
    return jit_instruction_map_lookup(rwpc, guest_pc, NULL);
}

static int jit_instruction_guest_pc_preserve(uint64_t host_pc, uint64_t *guest_pc, uint32_t *preserve_registers) {
    uint64_t rwpc;
    if (!jit_host_to_rwpc(host_pc, &rwpc)) return 0;
    return jit_instruction_map_lookup(rwpc, guest_pc, preserve_registers);
}

static int map_live(uint32_t index) {
    return g_map[index].generation == g_map_epoch;
}

static int map_tombstone(uint32_t index) {
    return !map_live(index) && g_map[index].tombstone_epoch == g_map_epoch;
}

static uint32_t source_page_hash(uint64_t page) {
    page ^= page >> 33;
    page *= UINT64_C(0xff51afd7ed558ccd);
    page ^= page >> 33;
    return (uint32_t)page & (JIT_SOURCE_PAGE_N - 1u);
}

static jit_source_page_entry *source_page_find(uint64_t page, int create) {
    uint32_t hash = source_page_hash(page);
    for (uint32_t probe = 0; probe < JIT_SOURCE_PAGE_N; probe++) {
        jit_source_page_entry *entry = &g_source_pages[(hash + probe) & (JIT_SOURCE_PAGE_N - 1u)];
        if (entry->epoch != g_map_epoch) {
            if (!create) return NULL;
            entry->page = page;
            entry->head = JIT_SOURCE_NONE;
            entry->epoch = g_map_epoch;
            return entry;
        }
        if (entry->page == page) return entry;
    }
    return NULL;
}

static void source_index_put(uint32_t map_index, uint64_t guest_start, uint64_t guest_end) {
    if (g_source_index_overflow) return;
    uint64_t first = guest_start >> JIT_SOURCE_PAGE_SHIFT;
    uint64_t last = (guest_end - 1u) >> JIT_SOURCE_PAGE_SHIFT;
    uint64_t pages = last - first + 1u;
    if (pages > JIT_SOURCE_NODE_N - g_source_node_count) {
        g_source_index_overflow = 1;
        return;
    }
    for (uint64_t page = first;; page++) {
        jit_source_page_entry *entry = source_page_find(page, 1);
        if (entry == NULL) {
            g_source_index_overflow = 1;
            return;
        }
        uint32_t node = g_source_node_count++;
        g_source_nodes[node] = (jit_source_node){.map_index = map_index, .next = entry->head};
        entry->head = node;
        if (page == last) break;
    }
}

static void map_clear(void) {
    g_live_map_count = 0;
    g_source_node_count = 0;
    g_source_index_overflow = 0;
    g_map_epoch++;
    if (g_map_epoch == 0) {
        // Epoch wrapped (2^32 flushes -- effectively never): no valid entry may carry generation 0, so
        // clear every entry's generation before restarting at 1. Cold path; correctness over speed.
        for (uint32_t i = 0; i < JIT_MAP_N; i++) {
            g_map[i].generation = 0;
            g_map[i].tombstone_epoch = 0;
        }
        for (uint32_t i = 0; i < JIT_INSN_MAP_N; i++)
            __atomic_store_n(&g_instruction_map[i].epoch, 0, __ATOMIC_RELAXED);
        for (uint32_t i = 0; i < JIT_SOURCE_PAGE_N; i++) g_source_pages[i].epoch = 0;
        g_map_epoch = 1;
    }
    map_host_cache_invalidate();
    /* A map epoch change invalidates every cached body. Callers are at a quiescent lifecycle boundary
       (single-threaded, stop-the-world, or fork child), so keep the data-only IBTC in the same lifecycle. */
    ibtc_clear_lazy();
}

// Crash-only reverse lookup: map a host RX pc back to the nearest translated block start.
int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn) {
    uint64_t rwpc;
    if (!jit_host_to_rwpc(hpc, &rwpc)) return 0;
    uint64_t best = 0;
    uint64_t bgpc = 0;
    for (uint32_t i = 0; i < JIT_MAP_N; i++) {
        if (!map_live(i)) continue;
        uint64_t h = (uint64_t)g_map[i].host;
        if (h && h <= rwpc && h >= best) {
            best = h;
            bgpc = g_map[i].gpc;
        }
    }
    if (!best) return 0;
    uint64_t exact = 0;
    if (gpc) *gpc = jit_instruction_map_lookup(rwpc, &exact, NULL) ? exact : bgpc;
    if (off) *off = rwpc - best;
    if (insn) *insn = *(uint32_t *)rwpc;
    return 1;
}

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
// TXPG_N: 256K slots * 8B = 2MB; guest code spans at most a few thousand pages.
// g_txpg values are guest pages (addr>>12); 0 is empty (page 0 never holds guest code).

static void txpg_put(uint64_t p) { // insert one guest page (addr>>12) into the set
    uint32_t h = (uint32_t)(p * 2654435761u) & (TXPG_N - 1);
    for (uint32_t i = 0; i < TXPG_N; i++) {
        uint32_t j = (h + i) & (TXPG_N - 1);
        if (g_txpg[j] == p) break; // already present
        if (g_txpg[j] == 0) {
            g_txpg[j] = p;
            break;
        } // insert into the first empty slot
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
// Cap the open-addressed linear probe. Once this set saturates (a >128MB guest code working set --
// e.g. a large musl binary translates >2M distinct 64B lines during startup), an UNBOUNDED
// probe walks the whole 2M-slot table on every lookup/insert. txln_put() is on translate_block's HOT
// path (via txpg_mark), so a full table turned each block's translation into an O(TXLN_N) scan per 64B
// line -> the guest pinned translate_block at 100% CPU with RSS flat (no progress) forever.
// (This is DISTINCT from the SMC re-translation livelock the content gate below fixes; it is a hash-set
// saturation blowup on the translate path.) Bounding the probe restores O(1) amortized and degrades to
// the conservative fallback the callers already document ("saturated -> assume present -> wholesale
// drop"). Correctness is preserved: txln_put only ever inserts a line within TXLN_PROBE_CAP of its hash,
// and slots are never individually emptied (only txln_clear wholesale-zeroes), so a line that WAS
// inserted is always re-found within the same cap; any probe that exhausts the cap means the line was
// never recorded -> returning "present"/"drop" over-approximates safely (never misses stale code).
#define TXLN_PROBE_CAP 512u
// g_txln values are guest lines (addr>>6); 0 is empty.
// ---- SMC content gate: benign icache-flush detection ----
// A code-generating guest re-flushes ALREADY-TRANSLATED, UNCHANGED code lines constantly at startup (a
// builtin/trampoline flushed as part of a range every call; a block flushing its OWN executing source
// line). smc_icflush() answered EVERY such line-hit with a WHOLESALE drop of the whole translation map,
// re-translating the entire working set -> the guest spun in translate_block at 100% CPU
// forever (RSS flat, no real progress). This parallel array (SAME slot index as g_txln) holds a 64-bit
// content hash of each translated line; the FIRST flush of a line records it (and drops conservatively,
// since we did not capture the pre-flush bytes), and every LATER flush compares -- unchanged bytes
// (benign icache maintenance) SKIP the drop; genuinely-rewritten bytes (soak_smc/smc2, a V8 IC patch)
// still drop + re-record. Cost is on the SMC slow path only (zero translate-path overhead). Cleared in
// lockstep with g_txln (txln_clear) so a slot's hash always matches the line living in that slot.
// g_txlh stores the 64-bit content hash at the SAME slot as g_txln (0 = unrecorded).

// ---- lazy line-set population (translation-throughput) ----
// Populating g_txln during EVERY block's decode is the single largest cold-translation bookkeeping cost
// (~27% of translate_block on sqlite/luajit: ~2 cache-missing inserts into the 16MB line set per block),
// yet the line set is ONLY ever read by txln_flush_class -- i.e. only when the guest issues `ic ivau`
// (self-modifying/JIT code). The overwhelming majority of guests (a normal binary: sqlite, a server, a
// CLI) never self-modify and never read this set at all, so recording it eagerly is pure waste for them.
//
// So defer it: while g_txln_active == 0 the decode loop skips txln_put entirely. Emitted host code is
// byte-identical either way -- g_txln is read ONLY by txln_flush_class and never by any emitter -- so this
// changes only WHEN bookkeeping is recorded, never what is translated.
//
// Activation (txln_activate) is a one-way switch flipped the instant the line set can first be needed.
// Because the blocks translated BEFORE activation have no recorded lines, the FIRST SMC event after the flip
// cannot be classified from the set -- so it takes a conservative WHOLESALE invalidation (g_txln_prime,
// honoured by smc_commit). Re-translation then re-records EXACT lines (stitching is off once g_smc_seen, so
// a block's decoded set is complete), and every later flush is classified byte-for-byte as in the
// always-eager engine. (An earlier variant that instead RECONSTRUCTED the set from live-block [start,end]
// hulls was measured to mis-drop benign flushes for a real guest JIT -- luajit_trace regressed ~20%; the
// single priming invalidation avoids that while still charging a non-self-modifying guest nothing.)
//
// Activation points (all reached while single-threaded, so this races nothing):
//   * first real SMC event (smc_commit / smc_icflush) -- before any txln_flush_class read;
//   * first guest thread spawn -- so a peer that later self-modifies records from a complete set;
//   * pcache warm-load -- restored blocks keep the historical page-fallback path (no prime needed: those
//     blocks were never line-recorded even by the eager engine).
static int g_txln_active;
static int g_txln_prime; // first SMC after activation must invalidate wholesale (no lines recorded yet)

static void txln_activate(void) {
    if (g_txln_active) return;
    g_txln_active = 1;
    g_txln_prime = 1;
}

static void txln_put(uint64_t l) {
    uint32_t h = (uint32_t)(l * 2654435761u) & (TXLN_N - 1);
    for (uint32_t i = 0; i < TXLN_PROBE_CAP; i++) { // bounded probe: see TXLN_PROBE_CAP
        uint32_t j = (h + i) & (TXLN_N - 1);
        if (g_txln[j] == l) break;
        if (g_txln[j] == 0) {
            g_txln[j] = l;
            break;
        }
    }
    // cap exhausted: the line's home cluster is full -> leave it unrecorded. txln_has/txln_flush_class
    // then over-approximate it as present (conservative drop), never miss it. Keeps the hot path O(cap).
}

static void txln_clear(void) {
    memset(g_txln, 0, sizeof g_txln);
    memset(g_txlh, 0, sizeof g_txlh); // keep the content-hash array in lockstep with the line set
}

// NOSMCHASH=1: revert the content gate to the legacy always-drop behaviour (A/B for the SMC-livelock fix).

// FNV-1a over the 64B guest line at `line_base` (64B-aligned). The line was just executed/flushed by the
// guest, so it is in a mapped code page -> the 64-byte read never faults. Guest VA == host VA under the
// JIT, so this reads the guest's own current bytes.
static int line_hash64(uint64_t line_base, uint64_t *hash) {
    uint8_t bytes[64];
    if (hl_guest_fetch_exec(line_base, bytes, sizeof bytes) != 0) return 0;
    uint64_t h = 1469598103934665603ull;
    for (int i = 0; i < 64; i++) {
        h ^= bytes[i];
        h *= 1099511628211ull;
    }
    *hash = h ? h : 1; // never 0 (0 sentinel == "unrecorded")
    return 1;
}

// Classify a guest `ic ivau` of the 64B line containing `addr`:
//   0 = the line is NOT the source of any live translation (nothing stale to drop)
//   1 = translated, and this is its FIRST flush OR its bytes CHANGED -> GENUINE, take the wholesale drop
//   2 = translated but bytes UNCHANGED since the last flush -> BENIGN icache maintenance, SKIP the drop
// Case 2 breaks the re-translation livelock: a hot loop re-flushing its own unchanged
// code no longer nukes the working set. Correct-by-construction: a genuine in-place rewrite changes the
// 64B line -> case 1 -> the block still re-translates (g_smc_seen already latched by the caller).
static int txln_flush_class(uint64_t addr) {
    uint64_t l = addr >> 6;
    uint32_t h = (uint32_t)(l * 2654435761u) & (TXLN_N - 1);
    for (uint32_t i = 0; i < TXLN_PROBE_CAP; i++) { // bounded probe: see TXLN_PROBE_CAP
        uint32_t j = (h + i) & (TXLN_N - 1);
        if (g_txln[j] == l) {
            uint64_t cur;
            // A disappearing/non-executable logical line is conservatively dirty.
            if (!line_hash64(l << 6, &cur)) return 1;
            if (g_txlh[j] == 0) { // first flush: no pre-flush baseline -> drop
                g_txlh[j] = cur;
                return 1;
            }
            if (g_txlh[j] == cur) return 2; // unchanged -> benign, skip the drop
            g_txlh[j] = cur;                // changed -> genuine rewrite, drop + re-record
            return 1;
        }
        if (g_txln[j] == 0) return 0; // empty slot before the line -> not translated
    }
    return 1; // table saturated -> conservative drop
}

static void txpg_mark(uint64_t lo, uint64_t hi) {
    if (hi <= lo) hi = lo + 1;
    for (uint64_t p = lo >> 12; p <= ((hi - 1) >> 12); p++)
        txpg_put(p);
    // The finer 64B line-granular source set (g_txln) is now populated incrementally during the decode
    // loop in translate_block -- marking only the lines actually decoded rather than the whole contiguous
    // [lo,hi) hull, which over-counted the address gaps between opt4-stitched sub-blocks and made each
    // block's translation do ~15x the necessary (cache-missing) txln_put work. txln_flush_class stays
    // correct: the incremental set is still a complete superset of every real source line.
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

static void txpg_clear(void) {
    memset(g_txpg, 0, sizeof g_txpg);
}

static int map_idx(uint64_t gpc) {
    // hash shift is per-arch (frontend/<arch>/abi.h G_GPC_HASH_SHIFT): aarch64 PCs are 4-byte aligned
    // (>>2 spreads), x86 PCs are byte-granular (>>0). Pure tuning constant; aarch64 value is 2 (unchanged).
    uint32_t h = (uint32_t)((gpc >> G_GPC_HASH_SHIFT) * 2654435761u) & (JIT_MAP_N - 1);
    for (int i = 0; i < JIT_MAP_N; i++) {
#if HL_NATIVE_TEST_HOOKS
        g_map_host_probe_count++;
#endif
        uint32_t j = (h + i) & (JIT_MAP_N - 1);
        if (!map_live(j)) {
            if (map_tombstone(j)) continue;
            return -1;
        }
        if (g_map[j].gpc == gpc) return j;
    }
    return -1;
}

static void *map_host_cached(hl_map_host_cache_entry cache[2], uint64_t gpc) {
    uint64_t generation = __atomic_load_n(&g_map_host_generation, __ATOMIC_ACQUIRE);
    if (cache[0].generation == generation && cache[0].gpc == gpc) return cache[0].host;
    if (cache[1].generation == generation && cache[1].gpc == gpc) {
        hl_map_host_cache_entry hit = cache[1];
        cache[1] = cache[0];
        cache[0] = hit;
        return hit.host;
    }
    int i = map_idx(gpc);
    if (i < 0) return NULL;
    cache[1] = cache[0];
    cache[0] = (hl_map_host_cache_entry){gpc, generation, g_map[i].host};
    return g_map[i].host;
}

static void *map_host(uint64_t gpc) { return map_host_cached(g_map_host_cache, gpc); }

#if HL_NATIVE_TEST_HOOKS
static void map_put(uint64_t gpc, uint64_t guest_start, uint64_t guest_end, void *host, void *body);
typedef struct {
    uint64_t guest;
    uint64_t probes;
    _Atomic int ready;
    _Atomic int published;
} hl_map_host_thread_test;

static void *map_host_thread_test(void *opaque) {
    hl_map_host_thread_test *test = opaque;
    g_map_host_probe_count = 0;
    hl_map_host_cache_entry *cache = g_map_host_cache;
    if ((uintptr_t)map_host_cached(cache, test->guest) != UINT64_C(0x111000)) test->probes = UINT64_MAX;
    __atomic_store_n(&test->ready, 1, __ATOMIC_RELEASE);
    while (!__atomic_load_n(&test->published, __ATOMIC_ACQUIRE)) sched_yield();
    if ((uintptr_t)map_host_cached(cache, test->guest) != UINT64_C(0x444000)) test->probes = UINT64_MAX;
    if (test->probes != UINT64_MAX) test->probes = g_map_host_probe_count;
    return NULL;
}

static int map_host_cache_test(uint32_t scenario, uint64_t *probes) {
    if ((scenario < 15 || scenario > 17) && scenario != 25) return -EINVAL;
    if (probes == NULL) return -EINVAL;
    const uint64_t guests[3] = {UINT64_C(0x100000), UINT64_C(0x200001), UINT64_C(0x300002)};
    const uint64_t first = guests[0], second = guests[1], third = guests[2];
    uint32_t indices[3];
    hl_translation_map_entry saved[3];
    hl_translation_map_metadata saved_meta[3];
    for (size_t i = 0; i < 3; i++) {
        indices[i] = (uint32_t)((guests[i] >> G_GPC_HASH_SHIFT) * UINT32_C(2654435761)) & (JIT_MAP_N - 1);
        saved[i] = g_map[indices[i]];
        saved_meta[i] = g_map_metadata[indices[i]];
    }
    uint32_t saved_epoch = g_map_epoch;
    uint32_t saved_live_count = g_live_map_count;
    uint32_t saved_live_indices[3] = {g_live_map_indices[0], g_live_map_indices[1], g_live_map_indices[2]};
    uint64_t saved_probe_count = g_map_host_probe_count;
    uint64_t saved_host_generation = __atomic_load_n(&g_map_host_generation, __ATOMIC_RELAXED);
    hl_map_host_cache_entry saved_host_cache[2] = {g_map_host_cache[0], g_map_host_cache[1]};

    map_clear();
    map_put(first, first, first + 1, (void *)(uintptr_t)UINT64_C(0x111000), (void *)(uintptr_t)UINT64_C(0x111001));
    map_put(second, second, second + 1, (void *)(uintptr_t)UINT64_C(0x222000), (void *)(uintptr_t)UINT64_C(0x222001));
    g_map_host_probe_count = 0;
    int result = 0;
    if (scenario == 15) {
        for (int iteration = 0; iteration < 64; ++iteration) {
            uint64_t guest = (iteration & 1) != 0 ? second : first;
            uintptr_t expected = (iteration & 1) != 0 ? UINT64_C(0x222000) : UINT64_C(0x111000);
            if ((uintptr_t)map_host_cached(g_map_host_cache, guest) != expected) result = -EIO;
        }
        *probes = g_map_host_probe_count;
    } else if (scenario == 16) {
        if ((uintptr_t)map_host_cached(g_map_host_cache, first) != UINT64_C(0x111000)) result = -EIO;
        if ((uintptr_t)map_host_cached(g_map_host_cache, second) != UINT64_C(0x222000)) result = -EIO;
        map_put(third, third, third + 1,
                (void *)(uintptr_t)UINT64_C(0x333000), (void *)(uintptr_t)UINT64_C(0x333001));
        if ((uintptr_t)map_host_cached(g_map_host_cache, first) != UINT64_C(0x111000)) result = -EIO;
        *probes = g_map_host_probe_count;
    } else if (scenario == 17) {
        hl_map_host_thread_test test = {
            .guest = first,
        };
        pthread_t thread;
        if (pthread_create(&thread, NULL, map_host_thread_test, &test) != 0) {
            result = -errno;
        } else {
            while (!__atomic_load_n(&test.ready, __ATOMIC_ACQUIRE)) sched_yield();
            map_clear();
            map_put(first, first, first + 1, (void *)(uintptr_t)UINT64_C(0x444000),
                    (void *)(uintptr_t)UINT64_C(0x444001));
            __atomic_store_n(&test.published, 1, __ATOMIC_RELEASE);
            if (pthread_join(thread, NULL) != 0 || test.probes != 2) result = -EIO;
            *probes = test.probes;
        }
    } else {
        hl_map_host_cache_entry *cache = g_map_host_cache;
        if ((uintptr_t)map_host_cached(cache, first) != UINT64_C(0x111000)) result = -EIO;
        pid_t child = fork();
        if (child < 0) {
            result = -errno;
        } else if (child == 0) {
            map_clear();
            map_put(first, first, first + 1, (void *)(uintptr_t)UINT64_C(0x555000),
                    (void *)(uintptr_t)UINT64_C(0x555001));
            _exit((uintptr_t)map_host_cached(cache, first) == UINT64_C(0x555000) ? 0 : 1);
        } else {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) result = -EIO;
            *probes = g_map_host_probe_count;
        }
    }

    for (size_t i = 0; i < 3; i++) {
        g_map[indices[i]] = saved[i];
        g_map_metadata[indices[i]] = saved_meta[i];
    }
    g_map_epoch = saved_epoch;
    g_live_map_count = saved_live_count;
    memcpy(g_live_map_indices, saved_live_indices, sizeof(saved_live_indices));
    g_map_host_probe_count = saved_probe_count;
    __atomic_store_n(&g_map_host_generation, saved_host_generation, __ATOMIC_RELAXED);
    memcpy(g_map_host_cache, saved_host_cache, sizeof(saved_host_cache));
    return result;
}
#endif

static void *map_body(uint64_t gpc) {
    int i = map_idx(gpc);
    return i < 0 ? NULL : g_map[i].body;
}

static void map_put(uint64_t gpc, uint64_t guest_start, uint64_t guest_end, void *host, void *body) {
    uint32_t h = (uint32_t)((gpc >> G_GPC_HASH_SHIFT) * 2654435761u) & (JIT_MAP_N - 1);
    uint32_t first_tombstone = UINT32_MAX;
    uint32_t destination = UINT32_MAX;
    for (int i = 0; i < JIT_MAP_N; i++) {
        uint32_t j = (h + i) & (JIT_MAP_N - 1);
        if (!map_live(j)) {
            if (map_tombstone(j)) {
                if (first_tombstone == UINT32_MAX) first_tombstone = j;
                continue;
            }
            destination = first_tombstone != UINT32_MAX ? first_tombstone : j;
            break;
        }
    }
    if (destination == UINT32_MAX) destination = first_tombstone;
    if (destination != UINT32_MAX) {
        uint64_t source_end =
            guest_end > guest_start ? guest_end : (guest_start == UINT64_MAX ? UINT64_MAX : guest_start + 1);
        g_map[destination].gpc = gpc;
        g_map[destination].host = host;
        g_map[destination].body = body;
        g_map_metadata[destination].guest_start = guest_start;
        g_map_metadata[destination].guest_end = source_end;
        g_map_metadata[destination].cache_generation = g_cache_gen;
        g_map[destination].tombstone_epoch = 0;
        source_index_put(destination, guest_start, source_end);
        g_live_map_positions[destination] = g_live_map_count;
        g_live_map_indices[g_live_map_count++] = destination;
#if HL_NATIVE_TEST_HOOKS
        if (g_source_index_publish_probe) {
            jit_source_page_entry *page = source_page_find(guest_start >> JIT_SOURCE_PAGE_SHIFT, 0);
            int indexed = 0;
            if (page != NULL)
                for (uint32_t node = page->head; node != JIT_SOURCE_NONE; node = g_source_nodes[node].next)
                    if (g_source_nodes[node].map_index == destination) {
                        indexed = 1;
                        break;
                    }
            g_source_index_publish_observed = indexed && !map_live(destination);
        }
#endif
        /* Publish liveness last: every live entry already has metadata, a live-list position, and either
           complete reverse-index nodes or the overflow latch which forces the authoritative full scan. */
        g_map[destination].generation = g_map_epoch;
        map_host_cache_invalidate();
    }
}

static int map_source_overlaps(uint32_t index, uint64_t lo, uint64_t hi) {
    return g_map_metadata[index].guest_start < hi && lo < g_map_metadata[index].guest_end;
}

static void map_remove_live(uint32_t index) {
    uint32_t position = g_live_map_positions[index];
    uint32_t last = g_live_map_indices[--g_live_map_count];
    g_live_map_indices[position] = last;
    g_live_map_positions[last] = position;
    ibtc_drop_target(g_map[index].gpc);
    g_map[index].generation = 0;
    g_map[index].tombstone_epoch = g_map_epoch;
}

static uint32_t map_invalidate_source_ranges_full(const uint64_t ranges[][2], uint32_t count) {
    uint32_t retained = 0, removed = 0;
    for (uint32_t n = 0; n < g_live_map_count; n++) {
        uint32_t index = g_live_map_indices[n];
        if (!map_live(index)) continue;
        int overlap = 0;
        for (uint32_t r = 0; r < count; r++) {
            if (map_source_overlaps(index, ranges[r][0], ranges[r][1])) {
                overlap = 1;
                break;
            }
        }
        if (!overlap) {
            g_live_map_positions[index] = retained;
            g_live_map_indices[retained++] = index;
            continue;
        }
        ibtc_drop_target(g_map[index].gpc);
        g_map[index].generation = 0;
        g_map[index].tombstone_epoch = g_map_epoch;
        removed++;
    }
    g_live_map_count = retained;
    return removed;
}

/*
 * Remove every live translation whose decoded source overlaps one of the
 * dirty [lo,hi) ranges.  All callers hold a stop-the-world mapping boundary,
 * so map readers cannot observe the mutation.  Host bytes remain immutable in
 * the arena; only future ingress is removed.
 */
static uint32_t map_invalidate_source_ranges(const uint64_t ranges[][2], uint32_t count) {
    uint64_t pages = 0;
    for (uint32_t r = 0; r < count; r++) {
        if (ranges[r][0] >= ranges[r][1]) continue;
        uint64_t first = ranges[r][0] >> JIT_SOURCE_PAGE_SHIFT;
        uint64_t last = (ranges[r][1] - 1u) >> JIT_SOURCE_PAGE_SHIFT;
        uint64_t span = last - first + 1u;
        if (span > JIT_SOURCE_PAGE_N - pages) {
            pages = JIT_SOURCE_PAGE_N + 1u;
            break;
        }
        pages += span;
    }
    uint32_t removed = 0;
    if (g_source_index_overflow || pages > JIT_SOURCE_PAGE_N) {
        removed = map_invalidate_source_ranges_full(ranges, count);
    } else {
        for (uint32_t r = 0; r < count; r++) {
            if (ranges[r][0] >= ranges[r][1]) continue;
            uint64_t page = ranges[r][0] >> JIT_SOURCE_PAGE_SHIFT;
            uint64_t last = (ranges[r][1] - 1u) >> JIT_SOURCE_PAGE_SHIFT;
            for (;;) {
                jit_source_page_entry *entry = source_page_find(page, 0);
                if (entry != NULL) {
                    for (uint32_t node = entry->head; node != JIT_SOURCE_NONE; node = g_source_nodes[node].next) {
                        uint32_t index = g_source_nodes[node].map_index;
                        if (!map_live(index)) continue;
                        int overlap = 0;
                        for (uint32_t candidate = 0; candidate < count; candidate++)
                            if (map_source_overlaps(index, ranges[candidate][0], ranges[candidate][1])) {
                                overlap = 1;
                                break;
                            }
                        if (overlap) {
                            map_remove_live(index);
                            removed++;
                        }
                    }
                }
                if (page == last) break;
                page++;
            }
        }
    }
    if (removed != 0) map_host_cache_invalidate();
    return removed;
}

#if HL_NATIVE_TEST_HOOKS
typedef struct {
    _Atomic int published;
    uint64_t range[1][2];
    uint32_t removed;
} map_source_index_thread_test;

static void *map_source_index_invalidator(void *opaque) {
    map_source_index_thread_test *test = opaque;
    while (!atomic_load_explicit(&test->published, memory_order_acquire)) sched_yield();
    test->removed = map_invalidate_source_ranges(test->range, 1);
    return NULL;
}

static int map_source_index_test(uint32_t scenario, uint64_t *result) {
    const uint64_t first = UINT64_C(0x51000000);
    const uint64_t second = UINT64_C(0x52000000);
    const uint64_t third = UINT64_C(0x53000000);
    int verdict = 0;
    map_clear();
    if (scenario == 26) {
        map_put(first, first + 0x100, first + 0x800, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
        map_put(second, second + 0xf00, second + 0x1100, (void *)(uintptr_t)0x2020,
                (void *)(uintptr_t)0x2021);
        map_put(third, third, third + 0x100, (void *)(uintptr_t)0x3030, (void *)(uintptr_t)0x3031);
        uint64_t dirty[][2] = {{second + 0x1000, second + 0x1001}};
        *result = map_invalidate_source_ranges(dirty, 1);
        if (*result != 1 || map_body(first) == NULL || map_body(second) != NULL || map_body(third) == NULL ||
            g_live_map_count != 2)
            verdict = -EIO;
    } else if (scenario == 27) {
        map_put(first, first, first + 1, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
        uint64_t old_page[][2] = {{first, first + 1}};
        if (map_invalidate_source_ranges(old_page, 1) != 1) verdict = -EIO;
        map_put(first, second, second + 1, (void *)(uintptr_t)0x2020, (void *)(uintptr_t)0x2021);
        if (map_invalidate_source_ranges(old_page, 1) != 0 || map_body(first) == NULL) verdict = -EIO;
        uint64_t new_page[][2] = {{second, second + 1}};
        *result = map_invalidate_source_ranges(new_page, 1);
        if (*result != 1 || map_body(first) != NULL) verdict = -EIO;
    } else if (scenario == 28) {
        map_put(first, first, first + 1, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
        uint32_t epoch = g_map_epoch;
        map_clear();
        if (g_map_epoch == epoch || g_source_node_count != 0 || g_source_index_overflow ||
            source_page_find(first >> JIT_SOURCE_PAGE_SHIFT, 0) != NULL)
            verdict = -EIO;
        map_put(second, second, second + 1, (void *)(uintptr_t)0x2020, (void *)(uintptr_t)0x2021);
        uint64_t dirty[][2] = {{second, second + 1}};
        *result = map_invalidate_source_ranges(dirty, 1);
        if (*result != 1) verdict = -EIO;
    } else if (scenario == 29) {
        map_put(first, first, first + 1, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
        map_put(second, second, second + 1, (void *)(uintptr_t)0x2020, (void *)(uintptr_t)0x2021);
        jit_source_page_entry *missing = source_page_find(second >> JIT_SOURCE_PAGE_SHIFT, 0);
        if (missing == NULL) {
            verdict = -EIO;
        } else {
            missing->epoch = 0;
        }
        g_source_index_overflow = 1;
        uint64_t dirty[][2] = {{second, second + 1}};
        *result = map_invalidate_source_ranges(dirty, 1);
        if (*result != 1 || map_body(first) == NULL || map_body(second) != NULL) verdict = -EIO;
    } else if (scenario == 30) {
        map_put(first, first, first + 1, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
        pid_t child = fork();
        if (child < 0) {
            verdict = -errno;
        } else if (child == 0) {
            uint64_t dirty[][2] = {{first, first + 1}};
            _exit(map_invalidate_source_ranges(dirty, 1) == 1 && map_body(first) == NULL ? 0 : 1);
        } else {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
                map_body(first) == NULL)
                verdict = -EIO;
            *result = 1;
        }
    } else if (scenario == 31) {
        map_source_index_thread_test test = {.range = {{first, first + 1}}};
        pthread_t thread;
        if (pthread_create(&thread, NULL, map_source_index_invalidator, &test) != 0) {
            verdict = -errno;
        } else {
            g_source_index_publish_probe = 1;
            g_source_index_publish_observed = 0;
            map_put(first, first, first + 1, (void *)(uintptr_t)0x1010, (void *)(uintptr_t)0x1011);
            g_source_index_publish_probe = 0;
            atomic_store_explicit(&test.published, 1, memory_order_release);
            if (pthread_join(thread, NULL) != 0 || !g_source_index_publish_observed || test.removed != 1 ||
                map_body(first) != NULL)
                verdict = -EIO;
            *result = test.removed;
        }
    } else {
        verdict = -EINVAL;
    }
    map_clear();
    return verdict;
}
#endif

static uint32_t map_invalidate_cache_generation(uint64_t generation) {
    uint32_t retained = 0, removed = 0;
    for (uint32_t n = 0; n < g_live_map_count; n++) {
        uint32_t index = g_live_map_indices[n];
        if (!map_live(index)) continue;
        if (g_map_metadata[index].cache_generation != generation) {
            g_live_map_positions[index] = retained;
            g_live_map_indices[retained++] = index;
            continue;
        }
        ibtc_drop_target(g_map[index].gpc);
        g_map[index].generation = 0;
        g_map[index].tombstone_epoch = g_map_epoch;
        removed++;
    }
    g_live_map_count = retained;
    if (removed != 0) map_host_cache_invalidate();
    return removed;
}

static int map_has_cache_generation(uint64_t generation) {
    for (uint32_t n = 0; n < g_live_map_count; n++) {
        uint32_t index = g_live_map_indices[n];
        if (map_live(index) && g_map_metadata[index].cache_generation == generation) return 1;
    }
    return 0;
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
// dispatcher. The reader's hash width (guest/aarch64/stubs.c) and both fills (the per-arch
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

/* Page-align (to the largest AArch64 base page, 64 KiB) so the whole table is a
   whole number of pages under any kernel page size.  That lets the fork child
   clear it with a single MADV_DONTNEED covering exactly the table (see
   ibtc_clear_lazy) instead of a COW-faulting memset, while still satisfying the
   16-byte alignment the atomic ldp/stp entry access requires. */
#define IBTC_PAGE_ALIGN 65536u
/* PE/COFF caps the alignment a section may request; the object-file field is a
   4-bit log2 code whose largest value is 8192, so a 64 KiB _Alignas is a hard
   compile error on this object format rather than a wasted-space trade-off.
   Nothing correctness-bearing is lost: the 64 KiB figure buys the whole-pages
   property MADV_DONTNEED wants, and MADV_DONTNEED is a Linux call this host
   does not have -- the fork child on a PE host clears the table by writing it.
   The 16-byte granule the atomic pair publish actually depends on is asserted
   below and is satisfied by either value. */
#if defined(_WIN32) && defined(_MSC_VER)
/* MSVC-ABI Windows caps this a second time, and for a different reason than the
   object-format one above. 8192 IS representable in the section header, and
   ld.lld accepts it by raising the image's section alignment to match; link.exe
   does not -- its default /ALIGN is one 4 KiB page, and an object asking for
   more is the hard error "LNK1164: section alignment greater than /ALIGN
   value", raised in whatever consumer links the archive rather than here.
   Raising /ALIGN instead was rejected: it is a property of every downstream
   image, not of this object, so it would make the engine archive impossible to
   link without a flag no consumer can be expected to pass. The 16-byte granule
   the atomic pair publish depends on is unaffected, and is asserted below. */
#define IBTC_ALIGN 4096u
#elif defined(_WIN32)
#define IBTC_ALIGN 8192u
#else
#define IBTC_ALIGN IBTC_PAGE_ALIGN
#endif
_Alignas(IBTC_ALIGN) static ibtc_ent g_ibtc[IBTC_N];
/* Both publish paths rest on this: AArch64's stp is single-copy atomic only within a 16-byte granule (a
   grown ibtc_ent silently reintroduces torn dispatch), and movdqa #GP-faults if misaligned. */
_Static_assert(sizeof(ibtc_ent) == 16, "ibtc_ent must be one 16-byte granule for the atomic pair publish");
_Static_assert(IBTC_ALIGN % 16u == 0u, "the ibtc table's alignment must keep every entry 16-byte aligned");

/* Wholesale-invalidate the inline-branch cache.  In a fork child the table is
   COW-inherited fully populated, so a memset first faults in every page (~190us
   on aarch64).  On Linux, MADV_DONTNEED on this private/anonymous (BSS) region
   drops the child's page references and restores guaranteed zero-fill-on-fault
   -- the same all-empty result in ~5us, and it never touches the parent's
   pages.  Other kernels (and any failure) fall back to the exact memset. */
static void ibtc_clear_lazy(void) {
#if defined(__linux__)
    if (madvise(g_ibtc, sizeof g_ibtc, MADV_DONTNEED) == 0) return;
#endif
    memset(g_ibtc, 0, sizeof g_ibtc);
}

static inline uint32_t ibtc_index(uint64_t target) {
    return (uint32_t)((target >> 2) & (IBTC_N - 1));
}

static inline void ibtc_publish(ibtc_ent *e, uint64_t target, void *body);

static inline ibtc_ent ibtc_snapshot(const ibtc_ent *e) {
#if defined(HL_HOST_CPU_AARCH64)
    ibtc_ent pair;
    __asm__ volatile("ldp %0, %1, [%2]" : "=r"(pair.target), "=r"(pair.body) : "r"(e) : "memory");
    return pair;
#else
    typedef unsigned long long ibtc_pair __attribute__((vector_size(16)));
    ibtc_pair pair;
    __asm__ volatile("movdqa %1, %0" : "=x"(pair) : "m"(*e) : "memory");
    return (ibtc_ent){pair[0], (void *)(uintptr_t)pair[1]};
#endif
}

static void ibtc_drop_target(uint64_t target) {
    ibtc_ent *entry = &g_ibtc[ibtc_index(target)];
    if (ibtc_snapshot(entry).target != target) return;
    /* Mapping invalidation owns the same STW/quiescent gate as tombstoning. Keep the pair atomic too so a
       future caller cannot accidentally expose target=0 with an old executable body to an emitted reader. */
    ibtc_publish(entry, 0, NULL);
}

// ---- W5C: race-free threaded IBTC fill ----
// g_mtibtc: enable threaded shared-hash IBTC fill (NOMTIBTC=1 disables -> revert to the
// locked-dispatcher path where threaded indirect branches always miss to the C dispatcher).
// g_mtfill: PROF count of threaded shared-hash publishes.
static int g_mtibtc = 1;
static uint64_t g_mtfill;

// Atomic 128-bit RELEASE publish of a {target, body} pair into a 16-byte-aligned IBTC slot.
// Single writer (the dispatcher holds g_jit_lock across every fill); many lock-free readers.
// `dmb ish` orders all prior stores (incl. the body block's translation + its IC IVAU, both
// already DSB-complete before this point) before the pair becomes observable; the `stp` of two
// X regs to a 16-byte-aligned address is single-copy atomic under FEAT_LSE2 (all Apple Silicon),
// so it is mutually atomic with the reader's plain `ldp`. We use explicit asm rather than a
// 16-byte __atomic (which could lower to a lock-based libatomic call that would NOT be atomic
// against the lock-free ldp reader). Layout: target at +0, body at +8 (matches struct ibtc_ent).
// ORDERING is free on x86-TSO: all that survives of `dmb ish` is the "memory" clobber. SINGLE-COPY ATOMICITY
// of the 16-byte access is needed on BOTH sides -- the READER's plain load is equally load-bearing -- hence
// movdqa, not `lock cmpxchg16b`: LOCK cannot make a plain load indivisible, and the reader is emitted
// fast-path code that must not become a locked RMW. Intel and AMD document aligned 16-byte SSE/AVX as atomic
// on AVX-capable parts. A future emit_ibranch MUST use one aligned 16-byte load, not two 8-byte ones.
static inline void ibtc_publish(ibtc_ent *e, uint64_t target, void *body) {
#if defined(HL_HOST_CPU_AARCH64)
    __asm__ volatile("dmb ish\n\t"
                     "stp %1, %2, [%0]\n\t"
                     :
                     : "r"(e), "r"(target), "r"(body)
                     : "memory");
#else
    // The vector type only gives the "x" constraint something to hold; a 16-byte __atomic_store would
    // lower to a lock-taking libatomic call.
    typedef unsigned long long ibtc_pair __attribute__((vector_size(16)));
    ibtc_pair pair = {target, (unsigned long long)(uintptr_t)body}; // target at +0, body at +8: ibtc_ent order
    __asm__ volatile("movdqa %1, %0" : "=m"(*e) : "x"(pair) : "memory");
#endif
}

static uint64_t g_prof_miss, g_prof_sys, g_lse_n;
/* HL_C_DIAGNOSTICS soft-memory attribution. Hot events are sampled at a
 * deterministic 1/64 of guest PCs; dispatcher-side events are exact. */
static uint64_t g_prof_soft_hull_sampled, g_prof_soft_cached_sampled, g_prof_soft_sites_sampled;
static uint64_t g_prof_soft_miss, g_prof_soft_span, g_prof_soft_bounce_prepare, g_prof_soft_bounce_commit;
static uint64_t g_prof_smc_queued, g_prof_smc_commit;
/* Translated bytes the aarch64 soft-memory lowering costs, summed over every
   guarded site, its miss lowering and the shared bodies.  This is the arena
   pressure the guard adds, which is the axis on which the inline and
   shared-resolver lowerings trade against each other; it is exact, not
   sampled.  Defined in this shared unit so the x86 [prof] reporter -- which
   shares linux_abi/syscall/process/identity.c -- links, and stays 0 there. */
static uint64_t g_prof_soft_guard_bytes;
/* How many guarded sites took each lowering.  0 shared sites is the shipped
   default; HL_SOFT_SHARED_RESOLVER moves them.  Exact, not sampled. */
static uint64_t g_prof_soft_shared_sites, g_prof_soft_inline_sites;
// PROF=1: dispatcher crossings / IBTC misses / translations
// A3 §B instrumentation (PROF=1). Runtime: shadow pushes executed, predicted-return FAST hits (host
// ret, RAS), and returns that fell through emit_shadow_ret to the IBTC fallback. Translate-time:
// how many guest `bl` sites the depth-gate steered to §B (shadow push) vs the cheap leaf Stage-B path.
static uint64_t g_prof_shpush, g_prof_shret_hit, g_prof_shret_fb;
static uint64_t g_prof_bl_shadow, g_prof_bl_leaf;

// IBSLIM: indirect-dispatch/call-path slimming (aarch64; defined here -- shared TU --
// used in translate/aarch64). NOIBSLIM=1 reverts every piece (steal-aware emit_set_x30 + the dead
// per-site-IC skip at recognized interpreter-dispatch `br`s) for A/B.
static int g_noibslim; // NOIBSLIM=1

// ARM-B1: recognize a clang jump-table switch dispatch at a guest `br xN`. The compiler emits
//   ldrh wM,[xB,wI,uxtw #1] ; adr xA,. ; add xN,xA,wM,sxth #2 ; br xN
// (an indexed 16-bit offset table). Bit-exact opcode match on the 3 predecessors + Rd==br.Rn.
static int is_jt_dispatch_br(uint64_t gpc) {
    uint32_t a = *(uint32_t *)(gpc - 12), b = *(uint32_t *)(gpc - 8), c = *(uint32_t *)(gpc - 4), br = *(uint32_t *)gpc;
    int brn = (int)((br >> 5) & 31);
    return (a & 0xFFE0FC00u) == 0x78605800u    // ldrh wM,[xB,wI,uxtw #1]
           && (b & 0x9F000000u) == 0x10000000u // adr xA, .
           && (c & 0xFFE0FC00u) == 0x8B20A800u // add xd,xa,wm,sxth #2
           && (int)(c & 31) == brn;            // add Rd feeds the br
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
    return (int)((in >> 12) & 1); // S=1: scaled #3 (an 8-byte pointer table)
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

// ---------------- W4E adaptive tier-2 ----------------
// W4E tier-2: a hot self-loop's in-cache back-edge counter reached threshold -> the dispatcher
// recompiles (promotes) the block with the optimized codegen, then resumes (pc already = block start).
// (The reason code normally lives next to R_BRANCH/R_SYSCALL in guest/aarch64/cpu.h; it is defined here
// because this engine integration is confined to the jit/ + frontend/aarch64/ translate units.)
// W5B: the x86 engine reuses this substrate but its reason-code space already uses 2 for R_CPUID, so it
// pre-defines R_TIER2=7 in guest/x86_64/cpu.h. Guard the aarch64 default so the x86 value wins in the
// x86 unity build; aarch64 (whose cpu.h does not define it) still gets 2. No aarch64 change.
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
static uint64_t g_t2gpc[T2_MAX];   // the loop-start gpc owning each slot (dedup on re-translate)
static int g_t2n;                  // slots allocated
static int g_notier2;              // NOTIER2=1 kill switch (pure tier-1 baseline)
static uint64_t g_t2thresh = 1000; // back-edge iterations before promotion (TIER2_THRESHOLD env)
static uint64_t g_prof_t2;         // PROF: blocks promoted to tier-2
static int g_tier2_build;          // set while recompiling a block as tier-2 (fold, no counter, no map_put)
static void *g_last_body;          // body pointer of the most recent translate_block (for the promoter)

// Kill-switch + threshold env, read ONCE (idempotent static guard; the W4E diff read these in the target
// main(), relocated here to keep the integration inside the allowed jit/ + frontend/aarch64/ units).
static void tier2_env_init(void) {
    g_notier2 = 0;
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
// IRQSLIM (aarch64): when the async-signal poll is emitted as a fixed 2-insn block
// header (ldr+cbnz, see emit_irq_check), a FORWARD direct chain may land at body+8 and skip the
// poll: a cycle of direct branches must contain a backward edge (code addresses strictly increase
// along forward-only paths), and every indirect entry (IBTC/IC/ctx/SDC) still lands on body+0 --
// so every possible in-cache loop keeps polling, while straight-line chains (the common case in
// branchy interpreter code) stop paying a load+branch per block. The production translators initialize
// this layout to 8; keeping the declaration at that effective default also makes pre-initialization
// translator identity checks agree with the engine that produced a checkpoint.
static int g_fwdskip = 8;

static struct {
    uint32_t *slot;
    uint64_t target;
    int is_bl;
    // is_bl: §B host bl, patch as bl
    int fwd; // IRQSLIM: forward direct edge -> patch to body+g_fwdskip (skip the entry poll)
    uint32_t orig;
    uint64_t source_gpc;
} g_pend[1 << 16];

static int g_npend;

// Index the pending back-patch links by target guest-PC. patch_links_to() used to linear-scan the ENTIRE
// g_pend array for every newly-translated block: with thousands of never-resolved links accumulating
// (indirect/data targets that are never reached), that is O(blocks * npend) -- the dominant cold-translation
// cost on branchy workloads (sqlite: ~11M compares, ~1us/block). A per-target bucket chain over the SAME
// compact g_pend array (swap-remove preserved, so pcache serialization and the array layout are unchanged)
// turns each patch into a walk of just the colliding entries. Buckets are epoch-tagged so a wholesale cache
// flush (pend_reset -> g_npend=0) invalidates every stale head in O(1) instead of memset-ing the table.
#define PEND_CAP (1 << 16)
#define PBUCKET_N (1 << 16)
static int32_t g_pnext[PEND_CAP]; // intrusive doubly-linked bucket chain over g_pend indices
static int32_t g_pprev[PEND_CAP];
static int32_t g_pbhead[PBUCKET_N];   // bucket head index (valid only when g_pbepoch == g_pend_epoch)
static uint32_t g_pbepoch[PBUCKET_N]; // zero-init: reads as "empty" against g_pend_epoch (starts at 1)
static uint32_t g_pend_epoch = 1;     // never 0 in use, so zero-init buckets always read empty

static inline uint32_t pbucket_of(uint64_t target) {
    return (uint32_t)((target >> 2) & (PBUCKET_N - 1));
}

static inline int32_t pbucket_head(uint32_t h) {
    return g_pbepoch[h] == g_pend_epoch ? g_pbhead[h] : -1;
}

static inline void pbucket_link(int32_t i, uint64_t target) {
    uint32_t h = pbucket_of(target);
    int32_t head = pbucket_head(h);
    g_pbepoch[h] = g_pend_epoch;
    g_pprev[i] = -1;
    g_pnext[i] = head;
    if (head != -1) g_pprev[head] = i;
    g_pbhead[h] = i;
}

static inline void pbucket_unlink(int32_t i, uint64_t target) {
    if (g_pprev[i] != -1)
        g_pnext[g_pprev[i]] = g_pnext[i];
    else
        g_pbhead[pbucket_of(target)] = g_pnext[i];
    if (g_pnext[i] != -1) g_pprev[g_pnext[i]] = g_pprev[i];
}

static void pend_reset(void) {
    g_npend = 0;
    if (++g_pend_epoch == 0) g_pend_epoch = 1; // skip 0 so zero-init buckets never alias a live epoch
}

static void add_pend3(uint32_t *slot, uint64_t target, int is_bl, int fwd) {
    if (g_npend < PEND_CAP) {
        int32_t i = g_npend++;
        g_pend[i].slot = slot;
        g_pend[i].target = target;
        g_pend[i].is_bl = is_bl;
        g_pend[i].fwd = fwd;
        g_pend[i].orig = 0;
        g_pend[i].source_gpc = 0;
        pbucket_link(i, target);
    }
}

static uint32_t pend_recode_cond(uint32_t in, int64_t d) {
    if ((in & 0xff000010u) == 0x54000000u) return (in & 0xff00001fu) | (((uint32_t)d & 0x7ffffu) << 5);
    if ((in & 0x7e000000u) == 0x34000000u) return (in & 0xff00001fu) | (((uint32_t)d & 0x7ffffu) << 5);
    return (in & 0xfff8001fu) | (((uint32_t)d & 0x3fffu) << 5);
}

static void add_pend_cond(uint32_t *slot, uint64_t target, uint32_t orig, uint64_t source_gpc, int fwd) {
    if (g_npend < PEND_CAP) {
        int32_t i = g_npend++;
        g_pend[i].slot = slot;
        g_pend[i].target = target;
        g_pend[i].is_bl = 2;
        g_pend[i].fwd = fwd;
        g_pend[i].orig = orig;
        g_pend[i].source_gpc = source_gpc;
        pbucket_link(i, target);
    }
}

static void add_pend2(uint32_t *slot, uint64_t target, int is_bl) {
    add_pend3(slot, target, is_bl, 0);
}

static void patch_links_to(uint64_t gpc, void *body) {
    // body == NULL means gpc has no live translation (e.g. map_put silently failed on a full map).
    // Patching `b (body - slot)` would then bake a wild branch; leave the pends unresolved so they keep
    // taking the safe dispatcher round-trip until gpc is (re)registered with a real body.
    if (!body) return;
    uint32_t h = pbucket_of(gpc);
    if (g_pbepoch[h] != g_pend_epoch) return; // no pends hash to this target
    int32_t i = g_pbhead[h];
    while (i != -1) {
        int32_t next = g_pnext[i];
        if (g_pend[i].target == gpc) {
            uint8_t *entry = (uint8_t *)body + (g_pend[i].fwd ? g_fwdskip : 0);
            int64_t d = (entry - (uint8_t *)g_pend[i].slot) / 4;
            if (g_pend[i].is_bl == 2) {
                uint32_t orig = g_pend[i].orig;
                int tb = (orig & 0x7e000000u) == 0x36000000u;
                int64_t lo = tb ? -(INT64_C(1) << 13) : -(INT64_C(1) << 18);
                int64_t hi = tb ? ((INT64_C(1) << 13) - 1) : ((INT64_C(1) << 18) - 1);
                if (d >= lo && d <= hi) {
                    *g_pend[i].slot = pend_recode_cond(orig, d);
                    if (!jit_publish_code(g_pend[i].slot, 4)) return;
                }
                goto cond_remove;
            }
            *g_pend[i].slot =
                // bl / b target.body (+8: forward edge skips the entry poll under IRQSLIM)
                (g_pend[i].is_bl ? 0x94000000u : 0x14000000u) | ((uint32_t)d & 0x3FFFFFFu);
            if (!jit_publish_code(g_pend[i].slot, 4)) return;
        cond_remove:
            // swap-remove keeps g_pend compact (pcache/layout unchanged); fix up the bucket chains.
            pbucket_unlink(i, gpc);
            int32_t last = --g_npend;
            if (i != last) {
                g_pend[i] = g_pend[last];
                int32_t p = g_pprev[last], n = g_pnext[last];
                g_pprev[i] = p;
                g_pnext[i] = n;
                if (p != -1)
                    g_pnext[p] = i;
                else
                    g_pbhead[pbucket_of(g_pend[i].target)] = i;
                if (n != -1) g_pprev[n] = i;
                if (next == last) next = i; // the node we were about to visit was relocated into slot i
            }
        }
        i = next;
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
    struct cpu *cpu;
    _Atomic uint64_t dispatch_ack;
    _Atomic int in_translated;
    _Atomic int departing;
} g_stw_threads[STW_MAXTHREAD];

static pthread_mutex_t g_stw_reg_lock = PTHREAD_MUTEX_INITIALIZER;
/* A checkpoint releases g_jit_lock while its dispatcher gate remains active.
   Serialize every gate epoch so a second publisher cannot replace or clear it. */
static pthread_mutex_t g_quiesce_lock = PTHREAD_MUTEX_INITIALIZER;
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
static __thread _Atomic uint64_t *g_my_exec_gen; // this thread's exec_gen slot (NULL until registered)
static __thread int g_my_stw_slot = -1;
#ifdef G_STW_CPU_SLOT
#define STW_SLOT(cpu) ((cpu)->stw_slot)
#define STW_EXEC_GEN(cpu) (&g_stw_threads[(cpu)->stw_slot].exec_gen)
#else
#define STW_SLOT(cpu) (g_my_stw_slot)
#define STW_EXEC_GEN(cpu) (g_my_exec_gen)
#endif
static __thread unsigned g_mapping_stw_depth;
static _Atomic uint64_t g_dispatch_request;
static _Atomic int g_dispatch_gate;
#define STW_RETIRED_MAX (STW_MAXTHREAD + 8)

static struct {
    hl_host_handle handle;
    uint8_t *rw;     // RW base of the retired mapping
    ptrdiff_t rw2rx; // RX-RW delta (0 for the single-mapping MAP_JIT fallback)
    uint64_t gen;    // generation this cache served
} g_retired[STW_RETIRED_MAX];

static int g_nretired;
static int g_no_stw_reclaim;

static int jit_host_to_rwpc(uint64_t host_pc, uint64_t *rwpc) {
    if (!g_cache) return 0;
    uint64_t lo = (uint64_t)g_cache + g_rw2rx;
    if (host_pc >= lo && host_pc < lo + CACHE_SZ) {
        *rwpc = host_pc - g_rw2rx;
        return 1;
    }
    lo = (uint64_t)g_cache;
    if (host_pc >= lo && host_pc < lo + CACHE_SZ) {
        *rwpc = host_pc;
        return 1;
    }
    for (int i = 0; i < g_nretired; i++) {
        lo = (uint64_t)g_retired[i].rw + g_retired[i].rw2rx;
        if (host_pc >= lo && host_pc < lo + CACHE_SZ) {
            *rwpc = (uint64_t)((intptr_t)host_pc - g_retired[i].rw2rx);
            return 1;
        }
        lo = (uint64_t)g_retired[i].rw;
        if (host_pc >= lo && host_pc < lo + CACHE_SZ) {
            *rwpc = host_pc;
            return 1;
        }
    }
    return 0;
}

// Non-wrapping body ownership for recovery from a wrapped instruction-provenance ring.  Each arena
// generation gets an immutable append-only range vector; it is freed only when that generation's mapping
// is no longer pinned or reachable.  Exhaustion declines the optional emitted body rather than overwriting
// a live owner.
#define JIT_BODY_OWNER_N 1398101u
#define JIT_BODY_OWNER_BLOCK_HEADROOM 217u
// Reserved non-canonical guest PCs for generated shared stubs.  The interrupted
// task's architectural RIP is already in cpu->rip; the second form additionally
// says TL_MM_FLAGS, rather than the transient host flags, is authoritative.
#define JIT_BODY_OWNER_DYNAMIC_RIP UINT64_MAX
#define JIT_BODY_OWNER_DYNAMIC_RIP_FLAGS (UINT64_MAX - 1u)
typedef struct {
    uint32_t rw_start, rw_end;
    uint64_t guest;
} jit_body_owner_entry;
_Static_assert(sizeof(jit_body_owner_entry) == 16, "body owner ABI must stay compact");
typedef uint16_t jit_body_owner_preserve;
typedef struct {
    uint64_t generation;
    uint8_t *rw;
    ptrdiff_t rw2rx;
    _Atomic uint32_t count;
    _Atomic(jit_body_owner_entry *) entry;
} jit_body_owner_set;
_Static_assert(ATOMIC_POINTER_LOCK_FREE == 2, "signal recovery requires lock-free atomic pointers");
_Static_assert(ATOMIC_INT_LOCK_FREE == 2 && sizeof(uint32_t) == sizeof(unsigned int),
               "signal recovery requires a lock-free 32-bit owner count");
static jit_body_owner_set g_body_owners[STW_RETIRED_MAX + 1];
#if defined(HL_NATIVE_TEST_HOOKS)
static _Atomic int g_body_owner_publish_pause;
static _Atomic int g_body_owner_publish_slot;
static _Atomic int g_body_owner_batch_pause;
static uint64_t g_body_owner_low_test_generation;
static uint64_t g_body_owner_low_test_rotations;
static uint64_t g_body_owner_low_test_retranslations;
static int g_body_owner_low_test_seeded;
static int g_body_owner_low_test_rotated;
#endif

static jit_body_owner_set *jit_body_owner_set_for(uint64_t generation, int create) {
    jit_body_owner_set *empty = NULL;
    for (size_t i = 0; i < sizeof(g_body_owners) / sizeof(g_body_owners[0]); i++) {
        jit_body_owner_entry *entries = atomic_load_explicit(&g_body_owners[i].entry, memory_order_acquire);
        if (entries != NULL && g_body_owners[i].generation == generation) return &g_body_owners[i];
        if (empty == NULL && entries == NULL) empty = &g_body_owners[i];
    }
    if (!create || empty == NULL) return NULL;
    // Keep the 16-byte search entry compact: one parallel 16-bit mask per range carries the complete
    // x86 GPR preserve set, in the same allocation and behind the same release publication as the entry.
    jit_body_owner_entry *entries = calloc(JIT_BODY_OWNER_N, sizeof(*entries) + sizeof(jit_body_owner_preserve));
    if (entries == NULL) return NULL;
    empty->generation = generation;
    empty->rw = g_cache;
    empty->rw2rx = g_rw2rx;
    atomic_store_explicit(&empty->count, 0, memory_order_relaxed);
#if defined(HL_NATIVE_TEST_HOOKS)
    if (atomic_load_explicit(&g_body_owner_publish_pause, memory_order_acquire) == 1) {
        atomic_store_explicit(&g_body_owner_publish_slot, (int)(empty - g_body_owners), memory_order_release);
        atomic_store_explicit(&g_body_owner_publish_pause, 2, memory_order_release);
        while (atomic_load_explicit(&g_body_owner_publish_pause, memory_order_acquire) == 2) sched_yield();
    }
#endif
    atomic_store_explicit(&empty->entry, entries, memory_order_release);
    return empty;
}

static int jit_body_owner_reserve(uint64_t generation, uint32_t *token) {
    jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
    if (set == NULL) return 0;
    uint32_t count = atomic_load_explicit(&set->count, memory_order_relaxed);
    if (count >= JIT_BODY_OWNER_N) return 0;
    *token = count;
    return 1;
}

static int jit_body_owner_reserve_n(uint64_t generation, uint32_t wanted, uint32_t *token) {
    jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
    if (set == NULL || wanted == 0) return 0;
    uint32_t count = atomic_load_explicit(&set->count, memory_order_relaxed);
    if (wanted > JIT_BODY_OWNER_N - count) return 0;
    *token = count;
    return 1;
}

// A body may publish at most one owner per decoded instruction plus one terminal tail.  Capacity is a
// cache-rotation condition, not a per-PC refusal: returning NULL here forever would leave most of a 64 MiB
// arena unusable for legal short SSE-heavy bodies.  The shared dispatcher checks this before translation,
// while it still owns the ordinary single-thread/STW rollover decision.
static int jit_body_owner_needs_rotation(uint64_t generation) {
    jit_body_owner_set *set = jit_body_owner_set_for(generation, 0);
    if (set == NULL) return 0;
    uint32_t count = atomic_load_explicit(&set->count, memory_order_relaxed);
    return JIT_BODY_OWNER_BLOCK_HEADROOM > JIT_BODY_OWNER_N - count;
}

// Keep the dispatcher's two independent cache-capacity limits behind one predicate so the exact
// pre-translation decision is directly testable.  In particular, owner exhaustion must rotate even
// while the byte arena still has ample space; otherwise every later translation in that generation
// would refuse permanently.
static int jit_cache_needs_rotation(void) {
    return g_cp + CACHE_EMIT_HEADROOM > g_cache + CACHE_SZ ||
           jit_body_owner_needs_rotation(g_cache_gen);
}

#if defined(HL_NATIVE_TEST_HOOKS)
// Seed the exact single-thread dispatcher condition rather than calling the predicate in isolation.  If
// the owner-low arm is removed from jit_cache_needs_rotation, translation keeps refusing in the old
// generation and the end-to-end hook reports neither a rotation nor a replacement publication.
static void jit_body_owner_low_test_seed(void) {
    const char *enabled = hl_option_get("HL_TRANSLIT_BODY_OWNER_ROTATE_TEST");
    if (g_body_owner_low_test_seeded || enabled == NULL || enabled[0] == '0' || enabled[0] == 0) return;
    jit_body_owner_set *set = jit_body_owner_set_for(g_cache_gen, 1);
    if (set == NULL) return;
    atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - (JIT_BODY_OWNER_BLOCK_HEADROOM - 1),
                          memory_order_relaxed);
    g_body_owner_low_test_generation = g_cache_gen;
    g_body_owner_low_test_seeded = 1;
}

static void jit_body_owner_low_test_after_rotation(void) {
    if (!g_body_owner_low_test_seeded || g_cache_gen == g_body_owner_low_test_generation) return;
    if (jit_body_owner_set_for(g_body_owner_low_test_generation, 0) != NULL) return;
    g_body_owner_low_test_rotations++;
    g_body_owner_low_test_rotated = 1;
}

static void jit_body_owner_low_test_after_translation(void) {
    if (!g_body_owner_low_test_rotated) return;
    g_body_owner_low_test_retranslations++;
    g_body_owner_low_test_rotated = 0;
}
#endif

static jit_body_owner_preserve *jit_body_owner_preserves(jit_body_owner_entry *entries) {
    return (jit_body_owner_preserve *)(void *)(entries + JIT_BODY_OWNER_N);
}

static int jit_body_owner_publish(uint64_t generation, uint32_t token, uint64_t lo, uint64_t hi, uint64_t guest) {
    jit_body_owner_set *set = jit_body_owner_set_for(generation, 0);
    jit_body_owner_entry *entries = set == NULL ? NULL : atomic_load_explicit(&set->entry, memory_order_acquire);
    uintptr_t base = (uintptr_t)(set == NULL ? NULL : set->rw);
    if (set == NULL || hi <= lo || lo < base || hi > base + CACHE_SZ ||
        token != atomic_load_explicit(&set->count, memory_order_relaxed))
        return 0;
    uint32_t start = (uint32_t)(lo - base), end = (uint32_t)(hi - base);
    if (token != 0) {
        jit_body_owner_entry *previous = &entries[token - 1];
        if (previous->rw_end <= previous->rw_start || previous->rw_end > start) return 0;
    }
    entries[token] = (jit_body_owner_entry){start, end, guest};
    jit_body_owner_preserves(entries)[token] = 0;
    atomic_store_explicit(&set->count, token + 1, memory_order_release);
    return 1;
}

typedef struct {
    uint64_t lo, hi, guest;
    uint32_t preserve_registers;
} jit_body_owner_range;

static int jit_body_owner_publish_n(uint64_t generation, uint32_t token,
                                    const jit_body_owner_range *range, uint32_t wanted) {
    jit_body_owner_set *set = jit_body_owner_set_for(generation, 0);
    jit_body_owner_entry *entries = set == NULL ? NULL : atomic_load_explicit(&set->entry, memory_order_acquire);
    uintptr_t base = (uintptr_t)(set == NULL ? NULL : set->rw);
    uint32_t count = set == NULL ? 0 : atomic_load_explicit(&set->count, memory_order_relaxed);
    if (entries == NULL || range == NULL || wanted == 0 || token != count || wanted > JIT_BODY_OWNER_N - token)
        return 0;
    uint32_t previous_end = token == 0 ? 0 : entries[token - 1].rw_end;
    for (uint32_t i = 0; i < wanted; i++) {
        if (range[i].hi <= range[i].lo || range[i].lo < base || range[i].hi > base + CACHE_SZ ||
            range[i].preserve_registers > UINT16_MAX)
            return 0;
        uint32_t start = (uint32_t)(range[i].lo - base), end = (uint32_t)(range[i].hi - base);
        if ((token != 0 || i != 0) && previous_end > start) return 0;
        previous_end = end;
    }
    jit_body_owner_preserve *preserves = jit_body_owner_preserves(entries);
    for (uint32_t i = 0; i < wanted; i++) {
        entries[token + i] = (jit_body_owner_entry){(uint32_t)(range[i].lo - base),
                                                    (uint32_t)(range[i].hi - base), range[i].guest};
        preserves[token + i] = (jit_body_owner_preserve)range[i].preserve_registers;
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    if (atomic_load_explicit(&g_body_owner_batch_pause, memory_order_acquire) == 1) {
        atomic_store_explicit(&g_body_owner_batch_pause, 2, memory_order_release);
        while (atomic_load_explicit(&g_body_owner_batch_pause, memory_order_acquire) == 2) sched_yield();
    }
#endif
    atomic_store_explicit(&set->count, token + wanted, memory_order_release);
    return 1;
}

static int jit_body_owner_lookup_preserve(uint64_t host_pc, uint64_t *guest, uint32_t *preserve_registers) {
    for (size_t i = 0; i < sizeof(g_body_owners) / sizeof(g_body_owners[0]); i++) {
        jit_body_owner_set *set = &g_body_owners[i];
        jit_body_owner_entry *entries = atomic_load_explicit(&set->entry, memory_order_acquire);
        if (entries == NULL) continue;
        uintptr_t rw = (uintptr_t)set->rw;
        uintptr_t rx = (uintptr_t)((intptr_t)set->rw + set->rw2rx);
        uint64_t rwpc;
        if (host_pc >= rw && host_pc < rw + CACHE_SZ)
            rwpc = host_pc - rw;
        else if (host_pc >= rx && host_pc < rx + CACHE_SZ)
            rwpc = host_pc - rx;
        else
            continue;
        uint32_t count = atomic_load_explicit(&set->count, memory_order_acquire);
        // Emission is monotonic inside an arena, so binary search is signal-safe and bounded.
        uint32_t lo = 0, hi = count;
        while (lo < hi) {
            uint32_t mid = lo + (hi - lo) / 2;
            jit_body_owner_entry *entry = &entries[mid];
            if (rwpc < entry->rw_start)
                hi = mid;
            else if (rwpc >= entry->rw_end)
                lo = mid + 1;
            else {
                *guest = entry->guest;
                if (preserve_registers != NULL)
                    *preserve_registers = jit_body_owner_preserves(entries)[mid];
                return 1;
            }
        }
    }
    return 0;
}

static int jit_body_owner_lookup(uint64_t host_pc, uint64_t *guest) {
    return jit_body_owner_lookup_preserve(host_pc, guest, NULL);
}

static void jit_body_owner_drop_generation(uint64_t generation) {
    for (size_t i = 0; i < sizeof(g_body_owners) / sizeof(g_body_owners[0]); i++) {
        jit_body_owner_entry *entries = atomic_load_explicit(&g_body_owners[i].entry, memory_order_acquire);
        if (entries == NULL || g_body_owners[i].generation != generation) continue;
        /* The dispatcher is the sole registry writer.  Production callers reach
           here only after STW has quiesced signal lookup and exec_gen proves no
           generated frame pins this generation.  Detach first so a later lookup
           cannot acquire the allocation; only that quiescent writer may free it. */
        entries = atomic_exchange_explicit(&g_body_owners[i].entry, NULL, memory_order_acq_rel);
        free(entries);
        g_body_owners[i].generation = 0;
        g_body_owners[i].rw = NULL;
        g_body_owners[i].rw2rx = 0;
        atomic_store_explicit(&g_body_owners[i].count, 0, memory_order_relaxed);
        return;
    }
}

/* Rewind the current arena only at a lifecycle boundary where no generated
   frame can still execute in it.  Body-owner ranges are append-only within a
   generation, so retaining them while reusing offsets from the arena base
   would make the replacement ranges overlap stale provenance. */
static void jit_cache_rewind_in_place(void) {
    jit_body_owner_drop_generation(g_cache_gen);
    g_cache_gen++;
    g_cp = g_cache;
}

static void jit_body_owner_clear(void) {
    for (size_t i = 0; i < sizeof(g_body_owners) / sizeof(g_body_owners[0]); i++) {
        /* Called only in the single surviving fork child or under dispatcher
           teardown/STW, when no signal lookup can still hold this pointer. */
        jit_body_owner_entry *entries =
            atomic_exchange_explicit(&g_body_owners[i].entry, NULL, memory_order_acq_rel);
        free(entries);
        g_body_owners[i].generation = 0;
        g_body_owners[i].rw = NULL;
        g_body_owners[i].rw2rx = 0;
        atomic_store_explicit(&g_body_owners[i].count, 0, memory_order_relaxed);
    }
}

static void jit_body_owner_after_fork(int preserve) {
    if (!preserve) jit_body_owner_clear();
}

static int jit_body_owner_reclaim_if_unused(uint64_t generation, int pinned, int mapped) {
    if (pinned || mapped) return 0;
    jit_body_owner_drop_generation(generation);
    return 1;
}

/*
 * Resolve a translation-map value (always an RW-alias address) through the
 * arena which actually owns it.  Retained generations can have a different
 * RX-RW delta from the current arena, and their generation must be published
 * to STW before execution so reclamation cannot unmap them underneath a peer.
 * Caller holds g_jit_lock whenever guest threads can race a rollover.
 */
static inline __attribute__((always_inline)) int jit_resolve_rw_code(void *rwcode, void **rxcode,
                                                                     uint64_t *generation) {
    uintptr_t pc = (uintptr_t)rwcode;
    uintptr_t lo = (uintptr_t)g_cache;
    if (pc >= lo && pc < lo + CACHE_SZ) {
        *rxcode = (void *)((intptr_t)pc + g_rw2rx);
        *generation = g_cache_gen;
        return 1;
    }
    for (int i = 0; i < g_nretired; i++) {
        lo = (uintptr_t)g_retired[i].rw;
        if (pc >= lo && pc < lo + CACHE_SZ) {
            *rxcode = (void *)((intptr_t)pc + g_retired[i].rw2rx);
            *generation = g_retired[i].gen;
            return 1;
        }
    }
    return 0;
}

#if HL_NATIVE_TEST_HOOKS
/* Diagnostic callers can receive either alias: dispatch passes RX to run_block, while the map stores RW.
   Normalize through the owning arena before asking the canonical resolver for its RX address/generation. */
static int jit_resolve_host_rx_code(void *hostcode, void **rxcode, uint64_t *generation) {
    uint64_t rwcode = 0;
    return jit_host_to_rwpc((uint64_t)(uintptr_t)hostcode, &rwcode) &&
           jit_resolve_rw_code((void *)(uintptr_t)rwcode, rxcode, generation);
}
#endif

// Crash diagnostics: keep a bounded tombstone ring of retired caches we have unmapped. If a later crash PC
// falls in one of these ranges, the process resumed through a stale cache pointer after reclamation.
#define STW_FREED_MAX 4096

static struct {
    uint8_t *rw;
    ptrdiff_t rw2rx;
    uint64_t gen;
} g_freed[STW_FREED_MAX];

static uint64_t g_nfreed_total;

static int jit_flush_to_fresh(int retain_map_generations);

int jit_pc_in_retained_cache(uint64_t pc) {
    if (!g_cache) return 0;
    uint64_t lo = (uint64_t)g_cache + g_rw2rx;
    if (pc >= lo && pc < lo + CACHE_SZ) return 1;
    for (int i = 0; i < g_nretired; i++) {
        lo = (uint64_t)g_retired[i].rw + g_retired[i].rw2rx;
        if (pc >= lo && pc < lo + CACHE_SZ) return 1;
    }
    return 0;
}

int jit_hostpc_alias_kind(uint64_t hpc) {
    if (!g_cache) return 0;
    uint64_t lo = (uint64_t)g_cache + g_rw2rx;
    if (hpc >= lo && hpc < lo + CACHE_SZ) return 1; // current RX alias
    lo = (uint64_t)g_cache;
    if (hpc >= lo && hpc < lo + CACHE_SZ) return 2; // current RW alias
    for (int i = 0; i < g_nretired; i++) {
        lo = (uint64_t)g_retired[i].rw + g_retired[i].rw2rx;
        if (hpc >= lo && hpc < lo + CACHE_SZ) return 3; // retained RX alias
        lo = (uint64_t)g_retired[i].rw;
        if (hpc >= lo && hpc < lo + CACHE_SZ) return 4; // retained RW alias
    }
    uint64_t n = g_nfreed_total < STW_FREED_MAX ? g_nfreed_total : STW_FREED_MAX;
    for (uint64_t i = 0; i < n; i++) {
        lo = (uint64_t)g_freed[i].rw + g_freed[i].rw2rx;
        if (hpc >= lo && hpc < lo + CACHE_SZ) return 5; // freed RX alias tombstone
        lo = (uint64_t)g_freed[i].rw;
        if (hpc >= lo && hpc < lo + CACHE_SZ) return 6; // freed RW alias tombstone
    }
    return 0;
}

void jit_cache_diag(uint64_t *gen, uint64_t *flushes, uint32_t *retired, uint32_t *freed) {
    if (gen) *gen = g_cache_gen;
    if (flushes) *flushes = g_stw_flushes;
    if (retired) *retired = (uint32_t)g_nretired;
    if (freed) *freed = (uint32_t)(g_nfreed_total > UINT32_MAX ? UINT32_MAX : g_nfreed_total);
}

// Park safepoint handler -- signal-safe host backoff plus atomics only. A peer caught here is, by
// definition, no longer executing a translated block (it is on its host stack in this handler), so the
// flusher may safely retire the cache while we spin.
static void stw_park_handler(int sig, siginfo_t *si, void *ucv) {
    (void)sig;
    (void)si;
    (void)ucv;
    /* A BUS activation uses this signal only to break a peer out of a host wait.  The
       peer's emitted IRQ poll performs the architectural spill at a real guest
       instruction boundary; redirecting an arbitrary host PC would lose that
       precision.  Ordinary cache rotation still parks here. */
    if (atomic_load_explicit(&g_dispatch_gate, memory_order_acquire)) {
        int slot = g_my_stw_slot;
        if (slot >= 0 && g_stw_threads[slot].cpu) {
            struct cpu *cpu = g_stw_threads[slot].cpu;
            __atomic_store_n(&cpu->irq, 1, __ATOMIC_RELAXED);
            if (!atomic_load_explicit(&g_stw_threads[slot].in_translated, memory_order_acquire) &&
                !__atomic_load_n(&cpu->in_service, __ATOMIC_SEQ_CST)) {
                while (atomic_load_explicit(&g_dispatch_gate, memory_order_acquire)) {
                    uint64_t request = atomic_load_explicit(&g_dispatch_request, memory_order_acquire);
                    atomic_store_explicit(&g_stw_threads[slot].dispatch_ack, request, memory_order_release);
                    jit_backoff_ns(UINT64_C(50000));
                }
            }
        }
        return;
    }
    atomic_fetch_add_explicit(&g_stw_parked, 1, memory_order_seq_cst);
    while (atomic_load_explicit(&g_stw_active, memory_order_seq_cst)) {
        jit_backoff_ns(UINT64_C(200000)); // 0.2ms
    }
    atomic_fetch_sub_explicit(&g_stw_parked, 1, memory_order_seq_cst);
}

static pthread_once_t g_stw_once = PTHREAD_ONCE_INIT;

static void stw_install(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = stw_park_handler;
    sa.sa_flags = SA_RESTART | SA_SIGINFO | SA_ONSTACK; // never place a host frame on the live guest stack
    sigemptyset(&sa.sa_mask);
    sigaction(STW_SIG, &sa, NULL);
}

static void stw_register(struct cpu *cpu) {
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
            g_stw_threads[i].cpu = cpu;
            atomic_store_explicit(&g_stw_threads[i].exec_gen, g_cache_gen, memory_order_relaxed);
            atomic_store_explicit(&g_stw_threads[i].dispatch_ack,
                                  atomic_load_explicit(&g_dispatch_request, memory_order_relaxed),
                                  memory_order_relaxed);
            atomic_store_explicit(&g_stw_threads[i].in_translated, 0, memory_order_relaxed);
            atomic_store_explicit(&g_stw_threads[i].departing, 0, memory_order_relaxed);
#ifdef G_SOFT_TLB_REFRESH
            G_SOFT_TLB_REFRESH(cpu);
#endif
            g_my_exec_gen = &g_stw_threads[i].exec_gen;
            g_my_stw_slot = i;
#ifdef G_STW_CPU_SLOT
            cpu->stw_slot = i;
#endif
            atomic_store_explicit(&g_stw_threads[i].used, 1, memory_order_release);
            break;
        }
    pthread_mutex_unlock(&g_stw_reg_lock);
}

static void stw_unregister(struct cpu *cpu) {
    pthread_t me = pthread_self();
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) &&
            pthread_equal(g_stw_threads[i].th, me)) {
            atomic_store_explicit(&g_stw_threads[i].used, 0, memory_order_release);
            g_stw_threads[i].cpu = NULL;
            g_my_stw_slot = -1;
#ifdef G_STW_CPU_SLOT
            cpu->stw_slot = -1;
#endif
            break;
        }
    pthread_mutex_unlock(&g_stw_reg_lock);
}

/* Publish a precise dispatcher safepoint.  A BUS prepare waits for this
   generation acknowledgement before publishing a shortened file mapping, so
   no peer can enter an old, unguarded translation after the prepare returns. */
static void stw_dispatch_safepoint_slot(int slot) {
    if (slot < 0) return;
    /* Read the gate before its request. Reading request first permits a
       publisher to advance the epoch and raise the gate between the ack and
       this load: the peer then parks with a stale ack forever. Q keeps the
       active request stable until the gate is released. */
    while (atomic_load_explicit(&g_dispatch_gate, memory_order_seq_cst)) {
        uint64_t request = atomic_load_explicit(&g_dispatch_request, memory_order_acquire);
        atomic_store_explicit(&g_stw_threads[slot].dispatch_ack, request, memory_order_release);
        jit_backoff_ns(UINT64_C(50000));
    }
}

static void stw_dispatch_safepoint(void) {
    stw_dispatch_safepoint_slot(g_my_stw_slot);
}

/* A peer may reach cache lookup immediately before a quiescer publishes its
   gate. Blocking on the writer lock there prevents that peer from reaching
   the dispatcher acknowledgement the writer is awaiting. Retry through the
   safepoint so an active epoch can park us without weakening writer exclusion. */
static void jit_dispatch_lock(void) {
    for (;;) {
        int status = pthread_mutex_trylock(&g_jit_lock);
        if (status == 0) return;
        if (status != EBUSY) abort();
        stw_dispatch_safepoint();
        jit_backoff_ns(UINT64_C(50000));
    }
}

/* Every quiescer arrives with JIT writer authority. A checkpoint may keep the
   epoch lock after releasing that authority, so never wait for the epoch while
   retaining g_jit_lock: a running peer must remain able to acknowledge and park. */
static void stw_quiesce_lock(void) {
    for (;;) {
        int status = pthread_mutex_trylock(&g_quiesce_lock);
        if (status == 0) return;
        if (status != EBUSY) abort();
        pthread_mutex_unlock(&g_jit_lock);
        stw_dispatch_safepoint();
        jit_dispatch_lock();
    }
}

static int stw_before_translated(struct cpu *cpu, uint64_t selected_epoch) {
    int slot = STW_SLOT(cpu);
    if (slot < 0) return 1;
    for (;;) {
        stw_dispatch_safepoint_slot(slot);
        if (atomic_load_explicit(&g_dispatch_request, memory_order_acquire) != selected_epoch) return 0;
        /* seq_cst, not release/acquire: this store and the gate load below form a
           StoreLoad handshake with a quiescing peer's store-gate-then-load-
           in_translated.  Under release/acquire BOTH sides may miss -- we enter
           translated code believing there is no gate while the quiesce believes we
           are not translated, so it never interrupts us and waits forever. */
        atomic_store_explicit(&g_stw_threads[slot].in_translated, 1, memory_order_seq_cst);
        /* Close activation's phase-transition race: once the gate is visible we
           withdraw from translated execution and acknowledge at the dispatcher. */
        if (!atomic_load_explicit(&g_dispatch_gate, memory_order_seq_cst) &&
            atomic_load_explicit(&g_dispatch_request, memory_order_acquire) == selected_epoch)
            return 1;
        atomic_store_explicit(&g_stw_threads[slot].in_translated, 0, memory_order_release);
        if (atomic_load_explicit(&g_dispatch_request, memory_order_acquire) != selected_epoch) return 0;
    }
}

static void stw_after_translated(struct cpu *cpu) {
    int slot = STW_SLOT(cpu);
    if (slot >= 0) {
        atomic_store_explicit(&g_stw_threads[slot].in_translated, 0, memory_order_release);
        /* Dispatcher/service state holds no code-cache PC.  Drop the generation
           pin now so repeated BUS activations can reclaim retired arenas. */
        atomic_store_explicit(&g_stw_threads[slot].exec_gen, 0, memory_order_release);
    }
    stw_dispatch_safepoint_slot(slot);
}

/* Wait until every peer still executing translated code has acknowledged `request`
   at a dispatcher boundary.  irq is re-asserted every round, not once by the
   arming scan: a peer that entered translated code after that scan was never
   interrupted, and a guest loop leaves the cache only on an irq poll -- so the
   one-shot form waits forever on it.  No deadline: proceeding while a peer still
   runs a translation this window is about to invalidate is memory corruption. */
static void stw_wait_translated_acks(uint64_t request) {
    for (;;) {
        int pending = 0;
        for (int i = 0; i < STW_MAXTHREAD; i++) {
            if (!atomic_load_explicit(&g_stw_threads[i].used, memory_order_acquire)) continue;
            if (!atomic_load_explicit(&g_stw_threads[i].in_translated, memory_order_seq_cst)) continue;
            if (atomic_load_explicit(&g_stw_threads[i].dispatch_ack, memory_order_acquire) >= request) continue;
            if (g_stw_threads[i].cpu) __atomic_store_n(&g_stw_threads[i].cpu->irq, 1, __ATOMIC_SEQ_CST);
            pending = 1;
        }
        if (!pending) break;
        jit_backoff_ns(UINT64_C(50000));
    }
}

static int stw_checkpoint_member(int slot) {
    return atomic_load_explicit(&g_stw_threads[slot].used, memory_order_acquire) &&
           !atomic_load_explicit(&g_stw_threads[slot].departing, memory_order_seq_cst);
}

static int stw_force_dispatch_flush(void) {
    pthread_t me = pthread_self();
    /* Serialize activation ownership before publishing its epoch/gate.  If two
       callbacks publish first and lock second, the first can clear the second's
       gate; its signal is then mistaken for an ordinary park and its peer never
       acknowledges the newer epoch. */
    jit_dispatch_lock();
    stw_quiesce_lock();
    uint64_t request = atomic_fetch_add_explicit(&g_dispatch_request, 1, memory_order_acq_rel) + 1;
    /* seq_cst: pairs with stw_before_translated's in_translated publication. */
    atomic_store_explicit(&g_dispatch_gate, 1, memory_order_seq_cst);
    /* Preserve the global lock order used by ordinary STW: jit -> registry. */
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++) {
        if (!stw_checkpoint_member(i)) continue;
        if (pthread_equal(g_stw_threads[i].th, me)) {
            atomic_store_explicit(&g_stw_threads[i].dispatch_ack, request, memory_order_release);
        } else if (atomic_load_explicit(&g_stw_threads[i].in_translated, memory_order_seq_cst)) {
            /* The emitted poll observes this aligned word directly.  The
               thread-directed signal path uses the same atomic publication;
               avoiding a host signal also avoids constructing an asynchronous
               frame while guest registers are live in translated code. */
            if (g_stw_threads[i].cpu) __atomic_store_n(&g_stw_threads[i].cpu->irq, 1, __ATOMIC_SEQ_CST);
        }
    }
    stw_wait_translated_acks(request);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_acquire) && g_stw_threads[i].cpu)
            G_ACTIVATION_CLEAR_CPU(g_stw_threads[i].cpu);
    G_ACTIVATION_CLEAR_GLOBAL();
    /* BUS/mapping activation changes translation validity, not cache capacity:
       never carry translations across this arena switch. */
    int ok = jit_flush_to_fresh(0);
    atomic_store_explicit(&g_dispatch_gate, 0, memory_order_release);
    pthread_mutex_unlock(&g_stw_reg_lock);
    pthread_mutex_unlock(&g_quiesce_lock);
    pthread_mutex_unlock(&g_jit_lock);
    return ok;
}

/* Hold every translated peer at a dispatcher boundary while a host mapping
   and its BUS ledger are changed as one transaction.  Unlike cache rotation,
   this preserves the current arena: only the mapping publisher is active
   until stw_mapping_end releases the gate. */
static void stw_mapping_begin_locked(void) {
    if (g_mapping_stw_depth++ != 0) return;
    pthread_t me = pthread_self();
    stw_quiesce_lock();
    uint64_t request = atomic_fetch_add_explicit(&g_dispatch_request, 1, memory_order_acq_rel) + 1;
    /* seq_cst: pairs with stw_before_translated's in_translated publication. */
    atomic_store_explicit(&g_dispatch_gate, 1, memory_order_seq_cst);
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++) {
        if (!stw_checkpoint_member(i)) continue;
        if (pthread_equal(g_stw_threads[i].th, me))
            atomic_store_explicit(&g_stw_threads[i].dispatch_ack, request, memory_order_release);
        else if (atomic_load_explicit(&g_stw_threads[i].in_translated, memory_order_seq_cst) && g_stw_threads[i].cpu)
            __atomic_store_n(&g_stw_threads[i].cpu->irq, 1, __ATOMIC_SEQ_CST);
    }
    stw_wait_translated_acks(request);
    /* Mapping publication may retire canonical soft-page backing as soon as
       this quiescent section ends.  Invalidate every registered per-thread
       translated-data TLB while the registry is pinned and no peer can run. */
#ifdef G_SOFT_TLB_CLEAR
    for (int i = 0; i < STW_MAXTHREAD; ++i)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_acquire) && g_stw_threads[i].cpu != NULL)
            G_SOFT_TLB_CLEAR(g_stw_threads[i].cpu);
#endif
}

static void stw_mapping_begin(void) {
    if (g_mapping_stw_depth != 0) {
        ++g_mapping_stw_depth;
        return;
    }
    jit_dispatch_lock();
    stw_mapping_begin_locked();
}

static void stw_mapping_end(void) {
    if (g_mapping_stw_depth == 0) abort();
    if (--g_mapping_stw_depth != 0) return;
#ifdef G_SOFT_TLB_REFRESH
    /* The logical snapshot is now committed while every peer remains behind
       the mapping gate. Publish each CPU's conservative rejection hull before
       translated execution resumes. */
    for (int i = 0; i < STW_MAXTHREAD; ++i)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_acquire) && g_stw_threads[i].cpu != NULL)
            G_SOFT_TLB_REFRESH(g_stw_threads[i].cpu);
#endif
    atomic_store_explicit(&g_dispatch_gate, 0, memory_order_release);
    pthread_mutex_unlock(&g_stw_reg_lock);
    pthread_mutex_unlock(&g_quiesce_lock);
    pthread_mutex_unlock(&g_jit_lock);
}

/* Arm a checkpoint-grade dispatcher barrier.  Unlike stw_mapping_begin(), the
   caller must interrupt every peer, including threads blocked in host service,
   then call stw_checkpoint_wait().  The registry remains locked throughout so
   the returned inventory cannot gain or lose a thread before release. */
static uint64_t stw_checkpoint_arm(void) {
    pthread_t caller = pthread_self();
    jit_dispatch_lock();
    stw_quiesce_lock();
    uint64_t request = atomic_fetch_add_explicit(&g_dispatch_request, 1, memory_order_acq_rel) + 1;
    /* seq_cst pairs with a thread's departing publication: either this scan
       excludes it, or its final safepoint observes the gate and acknowledges. */
    atomic_store_explicit(&g_dispatch_gate, 1, memory_order_seq_cst);
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; i++) {
        if (!stw_checkpoint_member(i)) continue;
        if (pthread_equal(g_stw_threads[i].th, caller))
            atomic_store_explicit(&g_stw_threads[i].dispatch_ack, request, memory_order_release);
        else if (g_stw_threads[i].cpu)
            __atomic_store_n(&g_stw_threads[i].cpu->irq, 1, __ATOMIC_SEQ_CST);
    }
    // A peer may have passed its top-of-dispatch gate immediately before activation and be waiting for
    // g_jit_lock. Let it drain through cache lookup to stw_before_translated(), where it acknowledges and
    // parks. g_stw_reg_lock remains held, so the checkpoint thread inventory is still immutable.
    pthread_mutex_unlock(&g_jit_lock);
    return request;
}

static int stw_checkpoint_wait(uint64_t request) {
    for (int attempt = 0; attempt < 100000; attempt++) {
        int pending = 0;
        for (int i = 0; i < STW_MAXTHREAD; i++) {
            if (!stw_checkpoint_member(i)) continue;
            if (atomic_load_explicit(&g_stw_threads[i].dispatch_ack, memory_order_acquire) < request) {
                pending = 1;
                break;
            }
        }
        if (!pending) return 0;
        jit_backoff_ns(UINT64_C(50000));
    }
#if HL_ENABLE_LOGGING
    for (int i = 0; i < STW_MAXTHREAD; i++) {
        if (!stw_checkpoint_member(i)) continue;
        uint64_t ack = atomic_load_explicit(&g_stw_threads[i].dispatch_ack, memory_order_acquire);
        if (ack >= request) continue;
        struct cpu *c = g_stw_threads[i].cpu;
        HL_LOGF(&g_jit_log, HL_LOG_TAG_PROCESS,
                "checkpoint thread barrier timeout tid=%d translated=%d service=%llu ack=%llu request=%llu",
                c ? c->tid : -1, atomic_load_explicit(&g_stw_threads[i].in_translated, memory_order_acquire),
                (unsigned long long)(c ? __atomic_load_n(&c->in_service, __ATOMIC_SEQ_CST) : 0),
                (unsigned long long)ack, (unsigned long long)request);
    }
#endif
    return -1;
}

static int stw_checkpoint_cpus(struct cpu **out, int capacity) {
    int count = 0;
    for (int i = 0; i < STW_MAXTHREAD; i++) {
        if (!stw_checkpoint_member(i)) continue;
        /* A used slot with no cpu is not a guest thread and has no state to capture. stw_after_fork() leaves
           exactly that behind whenever a process forks BEFORE its own guest thread registers -- which is the
           shape of every restore refork (ckpt_fork_children runs ahead of run_guest), so the child's later
           stw_register() takes a SECOND slot and the phantom stays. Counting it made a re-capture of a
           restored tree dereference NULL and die silently mid-dump. */
        if (g_stw_threads[i].cpu == NULL) continue;
        if (count < capacity) out[count] = g_stw_threads[i].cpu;
        count++;
    }
    return count;
}

static void stw_checkpoint_end(void) {
    atomic_store_explicit(&g_dispatch_gate, 0, memory_order_release);
    pthread_mutex_unlock(&g_stw_reg_lock);
    pthread_mutex_unlock(&g_quiesce_lock);
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
static void cache_unmap(hl_host_handle handle, uint8_t *rw, ptrdiff_t rw2rx) {
    uint64_t slot = g_nfreed_total++ % STW_FREED_MAX;
    g_freed[slot].rw = rw;
    g_freed[slot].rw2rx = rw2rx;
    g_freed[slot].gen = 0;
    hl_arena_release(&g_jit_services, handle);
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "cache release rw=%p rx=%p", (void *)rw, (void *)(rw + rw2rx));
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
    if (g_no_stw_reclaim) return;
    for (int i = 0; i < g_nretired;) {
        int pinned = gen_in_use(g_retired[i].gen);
        int mapped = map_has_cache_generation(g_retired[i].gen);
        if (!pinned && !mapped) {
            uint64_t gen = g_retired[i].gen;
            (void)jit_body_owner_reclaim_if_unused(gen, pinned, mapped);
            cache_unmap(g_retired[i].handle, g_retired[i].rw, g_retired[i].rw2rx);
            if (g_nfreed_total) g_freed[(g_nfreed_total - 1) % STW_FREED_MAX].gen = gen;
            g_retired[i] = g_retired[--g_nretired]; // swap-remove
        } else
            i++;
    }
}

// Record the CURRENT cache as retired (its blocks may still be reached by parked peers / baked-in chains)
// so a later reclaim_retired() frees it once every thread has drifted off its generation.
static int retire_current(void) {
    if (g_nretired < STW_RETIRED_MAX) {
        /* A fork child NEVER runs in a retired arena: jit_after_fork() cache_unmap()s every entry of
           g_retired on both the preserving and the rebuilding path, before fork_child_hooks returns and
           long before the child's next run_block.  So the child's inherited copy of this arena is pure
           fork cost, and on macOS the executable arena is expensive to inherit in a way plain anonymous
           memory is not (measured: ~+0.58ms of fork() per untouched 64 MiB MAP_JIT arena, charged again
           on EVERY subsequent fork for as long as the arena is retained).  Hand the child a hole
           instead.  Parent-side state is untouched, which is what a peer parked mid-block in this arena
           requires, and the child's later cache_unmap of the hole is a no-op munmap. */
        (void)hl_arena_drop_child_inheritance(g_cache, CACHE_SZ);
        g_retired[g_nretired].handle = g_code_mapping.handle;
        g_retired[g_nretired].rw = g_cache;
        g_retired[g_nretired].rw2rx = g_rw2rx;
        g_retired[g_nretired].gen = g_cache_gen;
        g_nretired++;
        return 1;
    } else {
        return cache_oom_fail();
    }
}

// A fresh cache could not be allocated and the peers are quiesced IN / parked ON the current cache, so
// reusing it in place would corrupt them on resume. Reclamation has already freed everything safe to free,
// so we cannot proceed -- abort cleanly rather than corrupt guest state.
static int cache_oom_fail(void) {
    static const char message[] = "JIT code cache exhausted (out of VA for a fresh cache under threads)";
    return jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
}

// Retire the current cache, switch to a brand-new one, and drop every cross-block link (map / IBTC /
// pending chains). The OLD cache is left mapped and UNMODIFIED (its blocks may still be reached by parked
// peers and by baked-in chains/inline ICs); reclaim_retired() unmaps it once no thread is in its
// generation, so retained VA stays bounded (no per-flush leak). MUST run with all peers quiesced
// (stw_flush) and the dispatcher holding g_jit_lock.
static int jit_flush_to_fresh(int retain_map_generations) {
    hl_host_code_mapping mapping;
#if HL_ENABLE_LOGGING
    size_t old_used = (size_t)(g_cp - g_cache);
    uint32_t old_blocks = g_live_map_count;
#endif
#if G_GPC_HASH_SHIFT != 0
    /*
     * g_smc_seen is an AArch64 frontend latch.  x86 has different SMC
     * bookkeeping and continues to use the established wholesale rollover.
     */
    int retain_generations = retain_map_generations && g_smc_seen;
#else
    int retain_generations = 0;
#endif
    /*
     * After the first SMC prime, every constant inter-block edge probes the
     * shared IBTC instead of baking an arena address.  Keep a bounded sliding
     * window of four immutable 64 MiB arenas in the translation map: capacity
     * rollover then evicts only the oldest quarter of the working set instead
     * of throwing away all hot CoreCLR/runtime blocks.  Clearing the IBTC
     * before reclaim removes the only post-SMC ingress not represented by the
     * map.  Pre-SMC direct chains retain the historical wholesale policy.
     */
#if HL_ENABLE_LOGGING
    uint32_t evicted = retain_generations && g_cache_gen >= 3 ? map_invalidate_cache_generation(g_cache_gen - 3) : 0;
#else
    if (retain_generations && g_cache_gen >= 3) (void)map_invalidate_cache_generation(g_cache_gen - 3);
#endif
    reclaim_retired(); // free retired caches no peer is still in -> bound VA + free space for the new alloc
    if (code_mapping_reserve_preferred(&mapping, g_dualmap) != 0) return cache_oom_fail();
    if (!retire_current()) {
        hl_arena_release(&g_jit_services, mapping.handle);
        return 0;
    }
    hl_arena_bind(&g_emit, &mapping);
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT,
            "cache rotate generation=%llu rw=%p rx=%p used=%zu blocks=%u evicted=%u retained=%u",
            (unsigned long long)(g_cache_gen + 1), (void *)g_cache, J_RX(g_cache), old_used, old_blocks, evicted,
            g_live_map_count);
    g_cache_gen++; // peers still on the just-retired generation pin it until they round-trip
    if (!retain_generations) map_clear();
    if (!retain_generations) memset(g_ibtc, 0, sizeof g_ibtc);
    pend_reset();
    return 1;
}

// Stop-the-world flush. Called from the dispatcher (holding g_jit_lock) when the cache is full and a peer
// guest thread is live: quiesce every peer at the park safepoint, switch to a fresh cache, then release.
static int stw_flush(void) {
    g_stw_flushes++;
    pthread_t me = pthread_self();
    stw_quiesce_lock();
    uint64_t request = atomic_fetch_add_explicit(&g_dispatch_request, 1, memory_order_acq_rel) + 1;
    /*
     * Quiesce at dispatcher boundaries instead of asynchronously parking a
     * peer at an arbitrary host PC. A signaled peer may hold a host-service
     * mutex; parking it there while jit_flush_to_fresh releases an arena
     * through the same service deadlocks permanently. Only translated peers
     * can retain code-cache PCs, so request their emitted IRQ poll and wait
     * for its dispatcher acknowledgement. Service/host threads remain free
     * to release their locks.
     *
     * The caller already holds g_jit_lock. Preserve the global lock order and
     * pin registry slots through the arena switch so a peer cannot unregister
     * between enumeration and acknowledgement.
     */
    atomic_store_explicit(&g_dispatch_gate, 1, memory_order_seq_cst);
    pthread_mutex_lock(&g_stw_reg_lock);
    for (int i = 0; i < STW_MAXTHREAD; ++i) {
        if (!stw_checkpoint_member(i)) continue;
        if (pthread_equal(g_stw_threads[i].th, me)) {
            atomic_store_explicit(&g_stw_threads[i].dispatch_ack, request, memory_order_release);
        } else if (atomic_load_explicit(&g_stw_threads[i].in_translated, memory_order_seq_cst) &&
                   g_stw_threads[i].cpu != NULL) {
            __atomic_store_n(&g_stw_threads[i].cpu->irq, 1, __ATOMIC_SEQ_CST);
        }
    }
    stw_wait_translated_acks(request);
    int ok = jit_flush_to_fresh(1);
    atomic_store_explicit(&g_dispatch_gate, 0, memory_order_release);
    pthread_mutex_unlock(&g_stw_reg_lock);
    pthread_mutex_unlock(&g_quiesce_lock);
    return ok;
}

// SMC coherence: the guest overwrote already-translated code -> drop the cross-block link tables so the
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
    map_clear();
    memset(g_ibtc, 0, sizeof g_ibtc);
    pend_reset();
    txpg_clear();
}

// fork(): drop the inherited (parent-only) thread registry -- host fork() duplicates only the calling
// thread -- so a later flush in the child never signals a dead handle. Re-register the child's own thread.
static void stw_after_fork(void) {
    struct cpu *survivor = g_my_stw_slot >= 0 ? g_stw_threads[g_my_stw_slot].cpu : NULL;
    atomic_store_explicit(&g_stw_active, 0, memory_order_relaxed);
    atomic_store_explicit(&g_stw_parked, 0, memory_order_relaxed);
    /* A sibling may have owned an activation gate at fork.  It does not exist
       in the child, so inheriting its closed gate would deadlock first re-entry. */
    atomic_store_explicit(&g_dispatch_gate, 0, memory_order_relaxed);
    pthread_mutex_init(&g_stw_reg_lock, NULL);
    pthread_mutex_init(&g_quiesce_lock, NULL);
    for (int i = 0; i < STW_MAXTHREAD; i++)
        atomic_store_explicit(&g_stw_threads[i].used, 0, memory_order_relaxed);
    g_stw_threads[0].th = pthread_self();
    g_stw_threads[0].cpu = survivor;
    atomic_store_explicit(&g_stw_threads[0].exec_gen, g_cache_gen, memory_order_relaxed);
    atomic_store_explicit(&g_stw_threads[0].in_translated, 0, memory_order_relaxed);
    atomic_store_explicit(&g_stw_threads[0].departing, 0, memory_order_relaxed);
    atomic_store_explicit(&g_stw_threads[0].used, 1, memory_order_relaxed);
    g_my_exec_gen = &g_stw_threads[0].exec_gen;
    g_my_stw_slot = 0;
#ifdef G_STW_CPU_SLOT
    if (survivor != NULL) survivor->stw_slot = 0;
#endif
}

#if defined(HL_NATIVE_TEST_HOOKS) && defined(G_STW_CPU_SLOT)
/* Exercise the x86 bound-dispatch slot independently of the thread-local copy.
   The signal/unbound entry point intentionally remains stw_dispatch_safepoint(),
   whose only source is g_my_stw_slot. */
static int stw_cpu_slot_lifecycle_test(void) {
    struct cpu cpu = { .stw_slot = -1 };
    stw_register(&cpu);
    int slot = cpu.stw_slot;
    if (slot < 0 || g_my_stw_slot != slot || g_stw_threads[slot].cpu != &cpu) return 30;

    /* A bound dispatcher must not consult TLS: make the two sources disagree. */
    g_my_stw_slot = -1;
    uint64_t epoch = atomic_load_explicit(&g_dispatch_request, memory_order_relaxed);
    if (!stw_before_translated(&cpu, epoch) ||
        !atomic_load_explicit(&g_stw_threads[slot].in_translated, memory_order_relaxed))
        return 31;
    stw_after_translated(&cpu);
    if (atomic_load_explicit(&g_stw_threads[slot].in_translated, memory_order_relaxed)) return 32;

    /* The unbound/async entry point retains TLS and therefore ignores cpu.stw_slot. */
    stw_dispatch_safepoint();
    if (g_my_stw_slot != -1 || cpu.stw_slot != slot) return 33;

    g_my_stw_slot = slot;
    stw_after_fork();
    if (cpu.stw_slot != 0 || g_my_stw_slot != 0 || g_stw_threads[0].cpu != &cpu) return 34;
    stw_unregister(&cpu);
    if (cpu.stw_slot != -1 || g_my_stw_slot != -1 ||
        atomic_load_explicit(&g_stw_threads[0].used, memory_order_relaxed))
        return 35;
    return 0;
}
#endif

// fork() and the dual-mapped cache. Left alone, fork() would COW the RW and RX aliases independently and
// the child's two views of the SAME cache would silently diverge (writes through RW never reach the COW'd
// RX -> the child executes stale/zero pages). The host backend marks the RX alias VM_INHERIT_NONE
// (the child gets a hole at the RX VA), and here -- in the child, before its next run_block -- we re-remap
// a fresh RX alias of the child's OWN COW-inherited RW pages at that SAME VA. That re-couples the aliases
// (child RW writes are visible through child RX again; verified empirically incl. nested forks) at the
// SAME addresses, so EVERY inherited translation, g_map/g_ibtc entry, cross-block chain and IC stays
// valid: a fork child resumes on the parent's warm code with ~zero rebuild cost (was a full 64MB
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

static int jit_after_fork(void) {
    int preserve;
    stw_after_fork(); // single-threaded child: shed the inherited thread registry (also for the MAP_JIT path)
    // fork() only clones the CALLING thread. If a peer M was translating (holding g_jit_lock, and g_cache_lock
    // under it in map_put) at the instant the guest forked, the child inherits those mutexes LOCKED with no
    // owner thread left to release them -- so the child's very first dispatcher iteration deadlocks forever in
    // run_guest's `jit_dispatch_lock()` (0% CPU) while its parent blocks reaping it. This is THE
    // go/npm/cargo build hang: a heavily-threaded driver (Go compiler, node) forks a child while
    // sibling Ms are mid-translate. The child is single-threaded now, so reinitialising both locks to a clean
    // unlocked state is always correct (no surviving peer can hold or want them; the calling thread never holds
    // an engine lock across a guest syscall). Must run before the !g_dualmap early return so the MAP_JIT path
    // is covered too.
    pthread_mutex_init(&g_jit_lock, NULL);
    pthread_mutex_init(&g_cache_lock, NULL);
#if defined(__linux__)
    /* A Linux fork child must not resume the parent's copied translations.  A
       second fork after any completed child otherwise executes a corrupted libc
       return path and trips __stack_chk_fail (or the middle generation dies and
       orphans the grandchild).  On aarch64 the failure persists even with direct
       chaining disabled; on x86 it is carried by the inherited direct block
       chains (a nested fork's parent continuation resolves through a sibling
       generation's baked `b body` edge instead of its own return address), so it
       reproduces on x86 too.  Both arches are cured the same way: start the child
       with a private empty cache and re-translate on demand.  Only the proven
       fixed-address macOS repair path retains warm-cache preservation. */
    preserve = 0;
#if G_GPC_HASH_SHIFT == 0
    /* x86 guest exception: with the persistent translated-code cache active, the
       fresh-arena rebuild leaves a SINGLE-THREADED fork child resuming on incoherent
       guest state -- a re-translated `rep movs` reads a garbage RCX/RSI and runs a
       512 MiB copy off the end of guest memory (guest __stack_chk_fail / SIGSEGV in
       the child only).  The cache forces the image + arena onto fixed bases and
       records baked host-pointer slots, and that bookkeeping does not survive the
       preserve=0 arena move intact.  Restore the proven warm-arena hand-off for this
       path exactly as before the nested-fork fix (single-threaded parent: re-couple
       the dual map's RX alias to the child's COW pages at the same VA).  The
       nested-fork double-fork repro runs WITHOUT the cache and keeps preserve=0, so
       its cure is unaffected.  A threaded parent still rebuilds (a torn arena snapshot
       is never preservable). */
    if (g_pcache && !g_threaded && g_dualmap) preserve = 1;
#endif
#elif defined(_WIN32)
    /* Windows joins the Linux default, and for a reason that is stronger here
       than there.  The dual-alias arena is a pagefile-backed SECTION mapped
       twice, and this host's address-space clone carries a section view as a
       GENUINELY SHARED view -- not a copy-on-write copy.  So a fork child does
       not merely inherit a snapshot of the parent's code cache; it inherits the
       parent's code cache itself, and the first block it translated would be
       written into memory the parent is executing.  The child must therefore get
       a new backing object, and once the backing object is new the inherited
       host addresses are gone -- which is exactly preserve = 0.  The single-
       alias fallback arena is private committed memory and IS copy-on-write, but
       it takes the same branch: with no way to tell a preserved arena from a
       rebuilt one at this level, the conservative answer is the correct one and
       the cost is a re-translate the child would have paid anyway. */
    preserve = 0;
#else
    preserve = !g_threaded || !g_dualmap;
#endif
    /* In a threaded child the fresh dual-map allocation may reuse one of the
       inherited retired RX holes (VM_INHERIT_NONE).  Release retired mappings
       before allocating; releasing them afterward can otherwise unmap the new
       cache through a stale executable address.  The preserving path remaps at
       its fixed current RX address and can retain the former ordering. */
    if (!preserve) {
        for (int i = 0; i < g_nretired; i++) {
            jit_body_owner_drop_generation(g_retired[i].gen);
            cache_unmap(g_retired[i].handle, g_retired[i].rw, g_retired[i].rw2rx);
        }
        g_nretired = 0;
    }
    if (hl_arena_repair(&g_jit_services, &g_emit, preserve) != 0) {
        (void)cache_oom_fail();
        g_fork_preserved = 0;
        return 0;
    }
    if (preserve) {
        for (int i = 0; i < g_nretired; i++) {
            jit_body_owner_drop_generation(g_retired[i].gen);
            cache_unmap(g_retired[i].handle, g_retired[i].rw, g_retired[i].rw2rx);
        }
        g_nretired = 0;
    }
    g_fork_preserved = preserve;
    if (!preserve) {
        jit_body_owner_after_fork(0);
        map_clear();
        /* The child COW-inherited the parent's fully-populated 1 MiB g_ibtc.  A
           memset zeroes it correctly but first faults every COW page in (~190us
           on aarch64 -- ~90% of this hook's cost).  MADV_DONTNEED drops the
           child's private references to the shared parent pages and restores
           zero-fill-on-demand for exactly the same logical result (an all-zero,
           i.e. all-empty, inline-branch cache) in ~5us, without disturbing the
           parent's pages.  Fall back to the memset if the advice is rejected. */
        ibtc_clear_lazy();
        pend_reset();
    }
    HL_LOGF(&g_jit_log, HL_LOG_TAG_PROCESS, "fork cache preserve=%d rw=%p rx=%p", preserve, (void *)g_cache,
            J_RX(g_cache));
    return 1;
}
