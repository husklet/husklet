// translator/guest/aarch64 -- guest CPU state. The cpu struct is the guest register file +
// engine scratch (shadow stack for §B return prediction, mangle scratch). Offsets are baked into
// emitted code. x18/x28/x30 are STOLEN by the engine (x28=cpu ptr, x30=host link, x18=scratch);
// guest values live in cpu->x[].

// ---------------- guest CPU state ----------------
// Offsets are baked into the emitted code below; keep them in sync.
struct cpu {
    // 0x000: x0..x30
    uint64_t x[31];
    // 0x0F8 (248)
    uint64_t sp;
    // 0x100 (256) next guest PC (set on block exit)
    uint64_t pc;
    // 0x108 (264) emulated TPIDR_EL0
    uint64_t tls;
    // 0x110 (272) why the block exited
    uint64_t reason;
    // 0x118 (280)
    uint64_t host_sp;
    // 0x120 (288) host x19..x30
    uint64_t host_save[12];
    // 0x180 (384) guest V0..V31 (128-bit each) -- NEON/FP state
    uint64_t v[64];
    // 0x380 (896) host V8..V15 (callee-saved, preserved by run_block)
    uint64_t host_v[16];
    // 0x400 (1024) guest condition flags
    uint64_t nzcv;
    // 0x408 (1032) IBTC: cache-literal addr to fill after an inline-cache miss
    uint64_t ic_site;
    // 0x410 (1040) async-interrupt poll flag. Set (via g_cpu_key) by the host async-signal handler
    // and the thread-directed tkill/tgkill path when a CAUGHT guest signal becomes pending; polled by a
    // 2-insn (ldr+cbz) check emitted at every block body so a CPU-bound guest loop that makes no syscalls
    // still exits to the dispatcher (maybe_deliver_signal) at a safe block boundary. Placed here -- the LAST
    // baked field is ic_site, so everything below is non-baked C-only state -- to share the hot nzcv cache
    // line the prologue already loads (zero extra cache miss on the fast path). Cleared each dispatcher
    // iteration (a masked-but-pending signal must not bounce the loop). uint64_t so the poll is a clean
    // 64-bit ldr+cbz.
    volatile uint64_t irq;
    int exited;
    int exit_code;
    // execve set pc/sp directly -> dispatcher must NOT advance pc
    int redirect;
    // CLONE_CHILD_CLEARTID addr: cleared + futex-woken on thread exit
    uint64_t ctid;
    // set_robust_list head (per-thread): walked on exit to mark OWNER_DIED + wake robust-mutex waiters
    uint64_t robust_list;
    // per-thread blocked-signal mask
    uint64_t sigmask;
    // §B shadow stack: (guest_ret, host_ret) PAIRS, interleaved (per-thread)
    uint64_t sstk[2048];
    // shadow-stack pointer (counts PAIRS; 64-bit so emitted ldr is simple)
    uint64_t ssp;
    // §B mangle + shadow push/ret scratch spill (per-thread; x28=cpu holds the base)
    uint64_t mscratch[8];
    // §B shadow stack: guest SP captured at each `bl` -- disambiguates frames (recursion/unwind)
    uint64_t gsp[1024];
    uint64_t alt_sp, alt_size;
    // sigaltstack (per-thread)
    uint32_t alt_flags;
    // gettid()/tgkill() identity: the guest thread id this cpu runs as. 0 on the initial (init) thread,
    // which reports container_pid() (==1); each spawned thread gets a unique id. Appended after the
    // baked-offset fields so the emitted-code offsets above are unaffected.
    int tid;
    // Thread-DIRECTED pending signals (1<<signo), the per-thread analogue of the process-wide g_pending.
    // A tgkill()/tkill() to THIS thread sets a bit here so the signal is delivered by this thread alone
    // (a process-directed signal in g_pending may be taken by any thread). Drained by maybe_deliver_signal.
    volatile uint64_t tpending;
    int sync_signal;
    int sync_code;
    uint64_t sync_address;
    // Non-nested signal delivery (matches native serialization): the set of signals that were already
    // pending when the CURRENT handler was entered is deferred until that handler returns, so a batch of
    // signals unblocked together runs one-after-another (priority order) rather than nesting. A signal that
    // becomes pending DURING a handler is not in this set and still nests (raise-in-handler / async fault).
    // sig_defer is the innermost level's deferred set; sig_defer_stack saves the enclosing levels.
    uint64_t sig_defer;
    uint64_t sig_defer_stack[32];
    uint64_t sig_frame_sp[32]; // guest SP of each active handler frame (to detect siglongjmp unwinds)
    int sig_depth;
    // SMC: guest VA of the most recent `ic ivau` (icache invalidate). The emitter spills it here on the
    // R_ICFLUSH exit so smc_icflush() can do PRECISE invalidation -- only drop the translation map + IBTC
    // when that guest page was actually translated, instead of nuking everything on every guest icache
    // flush (V8 issues one per freshly-written line). Appended after the baked-offset fields.
    uint64_t smc_va;
#define SMC_RANGE_CAP 64
    uint64_t smc_ranges[SMC_RANGE_CAP][2];
    uint64_t smc_range_count;
    uint64_t smc_range_overflow;
    // (SIMD-clean syscall exit): RUNTIME "guest vector state may be stale in cpu->V" flag. Set
    // (to the nonzero cpu pointer) by the first vector-writing instruction of a region; the static per-
    // region flag is UNSOUND because blocks CHAIN without spilling (a vectorized region can chain into a
    // statically-clean syscall block), so the decision must be at runtime. Cleared by every FULL spill
    // (which republishes cpu->V). A plain R_SYSCALL exit reads this: 0 -> slim GPR-only spill, else FULL.
    // Appended after the baked-offset fields so the emitted-code offsets above are unaffected.
    uint64_t vdirty;
    /* Synchronous translated-memory SIGBUS handoff; consumed only by dispatcher reason R_BUS. */
    uint64_t fault_addr;
    uint64_t bus_ea;
    /* Runtime-owned monotonic BUS page filter; emitted guards read these pointers. */
    uint64_t bus_filter;
    uint64_t bus_force;
    /*
     * Sparse logical-VMA data TLB.  Mapping mutations happen under the
     * process stop-the-world barrier and invalidate soft_page before retired
     * snapshots/backings are reclaimed.  A translated hit therefore needs
     * only a page comparison, permission test, and host delta add.
     */
    uint64_t soft_page;
    uint64_t soft_limit;
    /* Conservative hull of all sparse logical VMAs. Accesses wholly outside
       this interval bypass the software TLB without a dispatcher crossing. */
    uint64_t soft_filter_first;
    uint64_t soft_filter_last;
    uint64_t soft_delta;
    uint64_t soft_protection;
    uint64_t soft_ea;
    uint64_t soft_bytes;
    uint64_t soft_required;
    uint64_t soft_pc;
    uint64_t soft_span_ea;
    uint64_t soft_span_bytes;
    uint64_t soft_span_delta;
    uint64_t soft_span_protection;
    uint64_t soft_bounce_pending;
    uint64_t soft_bounce_write;
    _Alignas(16) unsigned char soft_bounce_host_mask[128];
    _Alignas(64) unsigned char soft_bounce[64];
    // Published while this thread is inside service(). A directed signal
    // needs a host interrupt only in that window; translated code observes
    // irq itself and dispatcher code is already at a delivery boundary.
    volatile uint64_t in_service;
};

// One coherent description of the AArch64 CPU exposed to the Linux guest.  Every discovery surface
// (auxv, procfs and emulated system registers) must derive from this model; copying an ID-register MRS to
// the host leaks the Apple CPU and can make a guest JIT emit extensions that the engine contract did not
// advertise.  HWCAP_CPUID is deliberately absent, so EL1 ID registers trap at EL0.
struct hl_aarch64_cpu_model {
    uint64_t hwcap;
    uint64_t hwcap2;
    uint64_t ctr_el0;
    uint64_t dczid_el0;
    int user_id_registers;
};

// A bit is set here only when BOTH backends implement the whole feature exactly: the interpreter computes it,
// and the JIT (a same-ISA transliterator) either copies it to silicon that has it or lowers it exactly.
//   HWCAP  bit 20 ASIMDDP  = FEAT_DotProd, mandatory from Armv8.4-A, so the verbatim copy always lands.
//   HWCAP2 bit 13 I8MM / bit 14 BF16 stay CLEAR: Apple Silicon before M4 has neither, so the JIT must use
//     translate.c's baseline lowerings -- which cover only 3 of I8MM's 6 forms and 2 of BF16's 7, and whose
//     BFDOT is not bit-exact (round-to-odd; see the note there). Both are implemented far enough that a guest
//     reaching past the contract still runs, but not far enough to invite one in.
// hwcap2 must exist even at 0: arm64's ARCH_DLINFO always emits AT_HWCAP2, so an absent entry is the one
// shape a real kernel never produces.
static const struct hl_aarch64_cpu_model g_aarch64_cpu_model = {
    .hwcap = 0x1fb | (1u << 20),
    .hwcap2 = 0,
    .ctr_el0 = 0x9444C004,
    .dczid_el0 = 4,
    .user_id_registers = 0,
};

#define OFF_VDIRTY ((int)offsetof(struct cpu, vdirty))
// OFF_VDIRTY is used in a scaled unsigned ldr/str (F9, imm12*8), so it must be 8-aligned and <= 32760.
_Static_assert(offsetof(struct cpu, vdirty) % 8 == 0 && offsetof(struct cpu, vdirty) <= 32760,
               "OFF_VDIRTY out of ldr/str imm12 range");
#define OFF_SMCVA offsetof(struct cpu, smc_va)
#define OFF_SMC_RANGES ((int)offsetof(struct cpu, smc_ranges))
#define OFF_SMC_RANGE_COUNT ((int)offsetof(struct cpu, smc_range_count))
#define OFF_SMC_RANGE_OVERFLOW ((int)offsetof(struct cpu, smc_range_overflow))
#define G_SMC_QUEUE_RESET(c)                                                                                           \
    do {                                                                                                               \
        (c)->smc_range_count = 0;                                                                                      \
        (c)->smc_range_overflow = 0;                                                                                   \
    } while (0)
#define OFF_V 384
#define OFF_HOSTV 896
#define OFF_NZCV 1024
#define OFF_ICSITE 1032
// async-poll flag offset (non-baked -> use the real offset, computed after the struct shifts).
#define OFF_IRQ ((int)offsetof(struct cpu, irq))
#define OFF_SP 248
#define OFF_PC 256
#define OFF_TLS 264
#define OFF_RSN 272
#define OFF_HSP 280
#define OFF_HSAVE 288
// Offset safety (C3): the baked numeric OFF_* above are duplicated into emitted machine code AND the
// run_block/block_return asm. A struct edit that shifts any of them must fail the BUILD, not corrupt a
// guest at runtime -- so assert each baked offset against the real field. (See REFACTOR.md "Offset safety".)
_Static_assert(offsetof(struct cpu, x) == 0, "OFF x[] base drifted");
_Static_assert(offsetof(struct cpu, sp) == OFF_SP, "OFF_SP drifted");
_Static_assert(offsetof(struct cpu, pc) == OFF_PC, "OFF_PC drifted");
_Static_assert(offsetof(struct cpu, tls) == OFF_TLS, "OFF_TLS drifted");
_Static_assert(offsetof(struct cpu, reason) == OFF_RSN, "OFF_RSN drifted");
_Static_assert(offsetof(struct cpu, host_sp) == OFF_HSP, "OFF_HSP drifted");
_Static_assert(offsetof(struct cpu, host_save) == OFF_HSAVE, "OFF_HSAVE drifted");
_Static_assert(offsetof(struct cpu, v) == OFF_V, "OFF_V drifted");
_Static_assert(offsetof(struct cpu, host_v) == OFF_HOSTV, "OFF_HOSTV drifted");
_Static_assert(offsetof(struct cpu, nzcv) == OFF_NZCV, "OFF_NZCV drifted");
_Static_assert(offsetof(struct cpu, ic_site) == OFF_ICSITE, "OFF_ICSITE drifted");
// §B shadow stack base (pairs)
#define OFF_SSTK offsetof(struct cpu, sstk)
#define OFF_SSP offsetof(struct cpu, ssp)
#define OFF_MSCRATCH offsetof(struct cpu, mscratch)
// §B shadow guest-SP array (1 per frame)
#define OFF_GSP offsetof(struct cpu, gsp)
// §B: real x28 reserved = cpu pointer
#define CPUREG 28
// guest_base bias-fold (non-PIE ET_EXEC only): re-target a guest load/store of a LOW image pointer to the
// HIGH mapping (+g_nonpie_bias) at translate time instead of faulting. The bias is materialized INLINE per
// folded access (no stolen host register) -- stealing a 5th GPR was unsafe: a Linux/Go guest uses every
// GPR (Go reserves R27/R28 on ARM64), so any instruction form not flagged by gpr_field_mask would read the
// stolen reg's engine value instead of the guest's. g_nonpie_lo/g_nonpie_bias are defined (and set by
// load_elf) in os/linux/elf.c + container/vfs.c (compiled LATER in the same unity TU); forward-declared
// static here (tentative -> merges with the single later def). Both 0 for PIE/static-PIE -> guestbase_on()
// is 0 -> the fold is inert and codegen stays byte-identical (the test matrix never sees a non-PIE image).
static uint64_t g_nonpie_lo, g_nonpie_bias;
// master enable (default on); cleared at startup by NOGUESTFOLD=1 for an A/B kill-switch.
static int g_guestfold = 1;

// Resolved path of the ENGINE binary itself (set once at entry). Mixed into the persistent-cache key so
// the cache invalidates on any engine relink -- see pcache_engine_id() in cache.c for why the compile
// timestamp of a single translation unit is not sufficient.
static const char *g_self_path = "";

static int guestbase_on(void) {
    return g_guestfold && g_nonpie_lo != 0;
}

// A1: x16/x17 engine-private (IBTC scratch); guest x16/x17 mangled like x18 so they never live in
// the host reg -> the per-indirect-branch red-zone stash/restore of x16/x17 disappears.
// x18 volatile, x28=cpu, x30=host link (§B). NOSTEAL1617=1 reverts to the 3-reg stolen set at startup.
static int g_steal1617 = 1;

static int is_stolen(int r) {
    return r == 18 || r == 28 || r == 30 || (g_steal1617 && (r == 16 || r == 17));
}

#define R_BRANCH 0
#define R_SYSCALL 1
#define R_BUS 5
#define R_FETCHFAULT 7
#define R_SOFTMISS 8
#define R_SOFTSPAN 9
#define R_SOFTCOMMIT 10
#define OFF_FAULT_ADDR ((int)offsetof(struct cpu, fault_addr))
#define OFF_BUS_EA ((int)offsetof(struct cpu, bus_ea))
#define OFF_BUS_FILTER ((int)offsetof(struct cpu, bus_filter))
#define OFF_BUS_FORCE ((int)offsetof(struct cpu, bus_force))
#define OFF_SOFT_PAGE ((int)offsetof(struct cpu, soft_page))
#define OFF_SOFT_LIMIT ((int)offsetof(struct cpu, soft_limit))
#define OFF_SOFT_FILTER_FIRST ((int)offsetof(struct cpu, soft_filter_first))
#define OFF_SOFT_FILTER_LAST ((int)offsetof(struct cpu, soft_filter_last))
#define OFF_SOFT_DELTA ((int)offsetof(struct cpu, soft_delta))
#define OFF_SOFT_PROTECTION ((int)offsetof(struct cpu, soft_protection))
#define OFF_SOFT_EA ((int)offsetof(struct cpu, soft_ea))
#define OFF_SOFT_BYTES ((int)offsetof(struct cpu, soft_bytes))
#define OFF_SOFT_REQUIRED ((int)offsetof(struct cpu, soft_required))
#define OFF_SOFT_PC ((int)offsetof(struct cpu, soft_pc))
#define OFF_SOFT_SPAN_EA ((int)offsetof(struct cpu, soft_span_ea))
#define OFF_SOFT_SPAN_BYTES ((int)offsetof(struct cpu, soft_span_bytes))
#define OFF_SOFT_SPAN_DELTA ((int)offsetof(struct cpu, soft_span_delta))
#define OFF_SOFT_SPAN_PROTECTION ((int)offsetof(struct cpu, soft_span_protection))
#define OFF_SOFT_BOUNCE_PENDING ((int)offsetof(struct cpu, soft_bounce_pending))
#define OFF_SOFT_BOUNCE ((int)offsetof(struct cpu, soft_bounce))
#define G_SOFT_STATE_RESET(c)                                                                                          \
    do {                                                                                                               \
        (c)->soft_page = UINT64_MAX;                                                                                   \
        (c)->soft_limit = 0;                                                                                           \
        (c)->soft_filter_first = UINT64_MAX;                                                                           \
        (c)->soft_filter_last = 0;                                                                                     \
        (c)->soft_delta = 0;                                                                                           \
        (c)->soft_protection = 0;                                                                                      \
        (c)->soft_ea = 0;                                                                                              \
        (c)->soft_bytes = 0;                                                                                           \
        (c)->soft_required = 0;                                                                                        \
        (c)->soft_pc = 0;                                                                                              \
        (c)->soft_span_ea = 0;                                                                                         \
        (c)->soft_span_bytes = 0;                                                                                      \
        (c)->soft_span_delta = 0;                                                                                      \
        (c)->soft_span_protection = 0;                                                                                 \
        (c)->soft_bounce_pending = 0;                                                                                  \
        (c)->soft_bounce_write = 0;                                                                                    \
    } while (0)
