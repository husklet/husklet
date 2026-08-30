// translator/guest/x86_64/interp.c -- the x86-64 guest backend for hosts that are NOT AArch64: decode and
// execute x86-64 rather than emit ARM64. core/target/x86_64.c forks on HL_HOST_CPU_AARCH64 and textually
// includes this file into that unity TU in place of emit.c + translate.c + cache.c, so it must define
// exactly the names those three defined and no more.
//
// Legal because core/dispatch.c run_guest's whole contract is `code = translate_block(G_PC(c));
// run_block(c, code);`, with c->reason, c->rip and all guest state final in *c on return -- `code` need not
// be machine code. struct cpu and the ARM-NZCV EFLAGS substrate stay as the JIT leaves them; ptrace GETREGS,
// the rt_sigframe builder and the checkpoint image read them.
//
// EXTENSION POINTS: interp_step_one_byte() (one-byte map + group /reg) and interp_step_two_byte() (0F map)
// return STEP_NEXT or STEP_END; add a class there and drop its interp_undefined() route. Host-neutral C
// already covers VEX/EVEX (R_AVX), 0F38/0F3A (R_SSE3B) and x87 m80/transcendental/fxsave (R_X87*, R_FX*),
// and legacy SSE through interp_step_sse() in interp/sse.c, which includes AES-NI and the SSE4.2 string
// engine. A survey of 1,279 real guest programs across glibc and musl rootfs images reached none of the
// residual interp_undefined() routes: on the 0F map the only encodings that get there are the register
// forms of RD/WRFSBASE, which the engine withholds from CPUID leaf 7. Guest memory goes ONLY through
// interp_load/interp_store/interp_locked_rmw.

#include <math.h> // x87 on the double-precision ST stack
#include <setjmp.h>

#if defined(HL_HOST_CPU_X86_64)
// Baseline on x86-64. xmmintrin.h supplies the guest's MXCSR (_mm_getcsr/_mm_setcsr).
#include <emmintrin.h>
#include <xmmintrin.h>
#endif

#include "../../cache_abi.h"
#include "../../digest.h"
#include "../../identity.h"
#include "../../persist.h"
#include "../../../host/native_context.h" // ucontext_t: the fault path restores uc_sigmask by hand
#include "decoder.h"
#include "guest_data.h"
#include "xsave.h"

// ---- The seam: names the JIT files own, which the rest of the TU needs.

// Must match guest/x86_64/cache.c, or switching host CPU silently relocates the guest image.
#define PC_IMG_BASE 0x0000040000000000ull    // 4 TB
#define PC_INTERP_BASE 0x0000048000000000ull // 4.5 TB

// No emitted block to inline clock_gettime into, so the fast-clock lever and cpu->fastclk_* stay off. Must
// still EXIST; core/target/x86_64.c reports them at exit.
static int g_fastsys;
static uint64_t g_fast_count;
static uint64_t x64_pcache_codegen_modes(void);

static void s1_calibrate(void) {
    // Nothing to measure; clock syscalls take the R_SYSCALL exit, as after a failure.
    g_fastsys = 0;
}

// abi.h's G_SMC_UNMAP. No instruction bytes are cached, so a stale DECODE is impossible; dropping the map
// entry only forces a fresh fetch, and a truthful fault if the range is now unmapped.
static void jit86_drop_range_translations(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    uint64_t range[1][2];
    range[0][0] = lo;
    range[0][1] = hi;
    if (map_invalidate_source_ranges((const uint64_t (*)[2])range, 1)) {
        memset(g_ibtc, 0, sizeof g_ibtc);
        memset(g_xibtc, 0, sizeof g_xibtc);
    }
}

// abi.h's G_THREAD_START_FLUSH / G_SHARED_MAP_BARRIERS. No block here has its x86-TSO barriers elided, so
// there is nothing to flush. Must return nonzero -- the clone path reads 0 as a clone failure.
static int hl_x86_flush_for_thread_start(void) {
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc);
    return 1;
}

static int hl_x86_force_barriers_for_shared(void) {
    return 1;
}

// Never called (G_BLOCK_ALIGN is a compile-time 0) but must exist for that dead branch to type-check, and
// aborts rather than appending an ARM64 nop into the descriptor arena. Non-static: lower/primitives.h.
void emit32(uint32_t instruction) {
    static const char message[] = "interpreter cannot emit host instructions";
    (void)instruction;
    (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
    abort();
}

// ---- The fault model. *cpu is already authoritative, so capture is trivial; escaping is not -- the
// faulting access is a memcpy several C frames deep and must be ABANDONED, not resumed past. run_block arms
// a sigsetjmp pad and marks every guest access; claiming a fault only when marked is what keeps an
// engine-side null dereference reportable. The pad is armed with savemask=0 and the one route that enters it
// from a signal handler restores the mask itself -- see interp_restore_handler_mask.

/* The pad's TYPE is per host, because the escape is.
 *
 * On a POSIX host it is a sigjmp_buf and the escape is siglongjmp. On Windows
 * neither exists: the CRT has no sigsetjmp, and plain longjmp is implemented
 * over RtlUnwindEx -- taking it out of a vectored exception handler would mean
 * unwinding through the kernel's exception dispatcher while dispatch is still in
 * progress, which is not a supported operation on any Windows build. The host
 * fault primitive supplies the replacement: the same ten words a mask-less
 * sigsetjmp would have stored, restored either into the fault context (the
 * handler route) or into the machine directly (the ordinary-code route). No
 * unwinder is involved on either, which is the entire reason to prefer it. */
#if defined(_WIN32)
#include "../../../host/windows/fault.h"
typedef hl_windows_fault_pad interp_fault_pad;
/* Set by whichever route entered the pad, cleared before every arm. Volatile and
 * thread-local because it is the ONE value that must be read afresh after the
 * resume lands -- the arm macro clobbers every volatile register precisely so
 * the compiler cannot cache something across the landing site. */
static __thread volatile int g_interp_pad_taken;
/* A statement expression, not a comma expression: the arm is inline asm, which
 * is a statement and cannot appear as an operand. */
#define INTERP_PAD_ARM(pad)                                                                                            \
    __extension__({                                                                                                    \
        g_interp_pad_taken = 0;                                                                                        \
        HL_WINDOWS_FAULT_PAD_ARM(&(pad));                                                                              \
        g_interp_pad_taken;                                                                                            \
    })
#else
typedef sigjmp_buf interp_fault_pad;
#define INTERP_PAD_ARM(pad) (sigsetjmp((pad), 0) != 0)
#endif

static __thread interp_fault_pad g_interp_fault_pad;
static __thread int g_interp_pad_armed;             // a run_block landing pad is live
static __thread volatile int g_interp_guest_access; // a guest access is in flight
// The cpu run_block armed the pad for; the ledger check below has no siginfo.
static __thread struct cpu *g_interp_pad_cpu;
static __thread hl_x86_guest_data_pins g_interp_data_pins;
static __thread int g_interp_data_active;
static __thread uint64_t g_dispatch_census_interp_steps;
static __thread unsigned g_dispatch_census_interp_stop;
#if !defined(HL_NATIVE_TEST_HOOKS)
static __thread unsigned g_dispatch_census_open;
static __thread uint64_t g_dispatch_census_pending_form;
static __thread unsigned g_dispatch_census_pending_form_valid;
static int signal_deliverable_for_cpu(const struct cpu *cpu);

static inline void interp_executed_form_commit(void) {
    if (!g_dispatch_census_pending_form_valid) return;
    hl_backend_tree_executed_form(g_dispatch_census_pending_form);
    g_dispatch_census_pending_form_valid = 0;
}
static inline void interp_executed_form_cancel(void) {
    g_dispatch_census_pending_form_valid = 0;
    g_dispatch_census_open = 0;
}
static inline void interp_executed_form_complete_enabled(struct cpu *cpu, unsigned successful_reason) {
    if ((unsigned)cpu->reason != successful_reason || signal_deliverable_for_cpu(cpu) || cpu->irq != 0) {
        interp_executed_form_cancel();
        return;
    }
    interp_executed_form_commit();
    g_dispatch_census_open = 0;
}
#define interp_executed_form_complete(cpu, reason)                                                                    \
    do {                                                                                                               \
        if (g_dispatch_census_open == 2) interp_executed_form_complete_enabled((cpu), (reason));                      \
    } while (0)
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
static inline void interp_executed_form_commit(void) {}
static inline void interp_executed_form_cancel(void) {}
#define interp_executed_form_complete(cpu, reason) ((void)(cpu), (void)(reason))
static __thread unsigned g_backend_shape_open;
static __thread unsigned g_backend_shape_interp_stop;
static __thread uint64_t g_backend_shape_interp_stop_form;
static __thread int g_backend_shape_edge_pending;
static __thread unsigned g_backend_shape_edge_family;
static __thread uint64_t g_backend_shape_edge_target;
static __thread uint64_t g_backend_shape_edge_source_generation;
static __thread int g_backend_shape_edge_source_resolved;
static __thread uintptr_t g_backend_shape_edge_source_host_lo;
static __thread uintptr_t g_backend_shape_edge_source_host_hi;
static __thread int g_backend_shape_edge_same_page;

static void interp_backend_shape_dispatch_enter(struct cpu *cpu) {
    cpu->ibtc_base = (uint64_t)g_ibtc;
    cpu->jcc_ibtc_miss = 0;
    g_backend_shape_open = 0;
    g_backend_shape_edge_pending = 0;
    g_backend_shape_edge_source_resolved = 0;
}

#endif

// ---- The past-EOF SIGBUS ledger. mem.c re-maps the past-EOF tail of a MAP_PRIVATE file mapping as
// anonymous zero, so the host never raises BUS_ADRERR and the translator owes the guest SIGBUS out of
// mem.c's ledger (core/bus.h). Takes the GUEST address -- what the guest handler compares si_addr against --
// and rebases both ways, since the ledger stores HOST addresses. jit_guest_bus_fault returns the FIRST
// past-EOF byte, so an access straddling the boundary reports it as Linux does.
static void interp_bus_ledger_check(uint64_t guest_address, uint64_t length) {
    // Inert when the ledger is empty.
    if (!jit_guest_bus_active()) return;
    uint64_t host = hl_x86_guest_pointer(guest_address);
    uint64_t host_fault = jit_guest_bus_fault(host, length);
    if (host_fault == 0) return;
    struct cpu *cpu = g_interp_pad_cpu;
    if (cpu == NULL || !g_interp_pad_armed) return; // no pad: unreachable from run_block
    uint64_t guest_fault = host_fault - (host - guest_address);
    cpu->fault_addr = guest_fault;
    cpu->bus_ea = guest_fault;
    cpu->soft_guest_ea = guest_fault;
    cpu->reason = R_BUS;
    g_interp_guest_access = 0;
    // The OTHER route into the pad, and the one that owes no mask restore: this runs on the ordinary
    // interpretation path (no signal was raised), so the host mask is already what run_block will resume on.
#if defined(_WIN32)
    g_interp_pad_taken = 1;
    hl_windows_fault_pad_jump(&g_interp_fault_pad);
#else
    siglongjmp(g_interp_fault_pad, 1);
#endif
}

static inline void interp_access_begin(uint64_t guest_address, uint64_t length) {
    interp_bus_ledger_check(guest_address, length);
    g_interp_guest_access = 1;
    __atomic_signal_fence(__ATOMIC_SEQ_CST); // the marker must be visible to a handler BEFORE the access
}

static inline void interp_access_end(void) {
    __atomic_signal_fence(__ATOMIC_SEQ_CST);
    g_interp_guest_access = 0;
}

int interp_signal_capture(struct cpu *cpu, void *native_context);
void interp_signal_resume(struct cpu *cpu, void *native_context);
static int translit_signal_capture(struct cpu *cpu, void *native_context); // translit.inc

// 1 only for a fault the GUEST caused: inside a marked interpreter access, or -- when the transliterator
// is on -- at a host PC inside the code cache, where every access is a guest access by construction and
// *cpu has to be rebuilt from the host register file before it is authoritative.
int interp_signal_capture(struct cpu *cpu, void *native_context) {
    if (cpu == NULL || !g_interp_pad_armed) return 0;
    if (translit_signal_capture(cpu, native_context)) return 1;
    return g_interp_guest_access ? 1 : 0;
}

// WHY THIS EXISTS, AND WHY IT IS NOT sigsetjmp(pad, 1).
//
// Leaving a host handler by long jump instead of returning means the kernel never runs rt_sigreturn, so the
// mask restore rt_sigreturn would have done is OWED BY US. Skip it and the fault signal stays blocked in this
// guest thread forever: the SECOND guest SIGSEGV never arrives, and the failure is silent and looks like a
// hang. The JIT backend has no such debt -- it rewrites the ucontext PC to block_return and RETURNS, so
// sigreturn restores the mask for it. This function is the interpreter's hand-rolled sigreturn.
//
// The mask to install is exactly ucontext->uc_sigmask: the kernel wrote the pre-signal mask there, and it is
// by definition the value sigreturn would restore. That is stronger than snapshotting the run loop's mask,
// because it assumes NO invariance -- it stays correct across a nested guest signal, an SA_ONSTACK handler,
// a guest rt_sigprocmask that mirrored SIGTSTP/SIGTTIN/SIGTTOU onto the real host mask (syscall/signal.c
// case 135), a clone/fork child that inherited a different mask, and a host-service window that blocked
// signals around a write (host/linux/host.c). Restoring BEFORE the jump matches glibc's own siglongjmp
// ordering (mask first, __longjmp second).
//
// The savemask=1 this replaces cost an rt_sigprocmask syscall on EVERY guest basic block -- 271.7 ns, 44% of
// compute CPU and 99.96% of the process's host syscalls -- to
// save a mask that only this rare path ever reads.
static void interp_restore_handler_mask(void *native_context) {
#if defined(_WIN32)
    /* Nothing is owed. This host has no signal mask at all -- no per-thread
     * blocked set, nothing the kernel auto-blocks at handler entry, and no
     * sigreturn whose work could be skipped. The debt this function exists to
     * repay is created by leaving a POSIX handler other than by returning; a
     * vectored exception handler creates none. An empty body here is the honest
     * answer, not a stub: there is no Windows call that would make it fuller. */
    (void)native_context;
#else
    if (native_context != NULL) {
        pthread_sigmask(SIG_SETMASK, &((ucontext_t *)native_context)->uc_sigmask, NULL);
        return;
    }
    // No context (no caller does this today: deliver_guest_fault rejects a NULL ucontext up front).
    // Unblock the classes the kernel could have auto-blocked at handler entry, so a repeat fault is still
    // deliverable -- the failure mode this guards is exactly the silent one described above.
    sigset_t fault;
    sigemptyset(&fault);
    sigaddset(&fault, SIGSEGV);
    sigaddset(&fault, SIGBUS);
    sigaddset(&fault, SIGILL);
    sigaddset(&fault, SIGFPE);
    sigaddset(&fault, SIGTRAP);
    pthread_sigmask(SIG_UNBLOCK, &fault, NULL);
#endif
}

// The delivery path has already queued the signal and set R_BRANCH; just return from run_block.
void interp_signal_resume(struct cpu *cpu, void *native_context) {
    (void)cpu;
    if (!g_interp_pad_armed) return; // not ours; the caller re-raises (and returns, so sigreturn restores)
    interp_restore_handler_mask(native_context);
#if defined(_WIN32)
    // The handler route. Editing the context and returning HL_WINDOWS_FAULT_RESUME is what "return from
    // the handler with a modified ucontext" means on this host -- so unlike the POSIX arm, this function
    // RETURNS, and the resume happens when the dispatcher hands the edited context back to the kernel.
    // A NULL context would mean a caller reached here without a fault, which cannot happen: the only
    // entry is the fault classifier, and it declines an exception whose context it did not receive.
    g_interp_pad_taken = 1;
    if (native_context != NULL) hl_windows_fault_pad_restore((CONTEXT *)native_context, &g_interp_fault_pad);
#else
    siglongjmp(g_interp_fault_pad, 1);
#endif
}

// ---- Guest memory. Once a logical mapping exists, every data access projects through
// hl_guest_memory_pin_data. The common identity/non-PIE case keeps its direct one-copy path. Everything
// goes through memcpy because UNALIGNED ACCESS MUST WORK and a cast through uint64_t* is UB. No
// byte swapping: guest and every host so far are little-endian. Nothing is emitted, so the guest sees the
// host's memory ordering -- x86-64 host TSO IS guest TSO, hence the empty fence below.

static inline void interp_tso_fence(void) {
#if !defined(HL_HOST_CPU_X86_64)
    // Around EVERY access: an interpreter cannot know which one is the synchronising one.
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
#endif
}

// ONE host access per guest access. With a RUNTIME size, memcpy is a CALL into glibc, whose 4..7 and 8..15
// paths copy head and tail separately -- the same address twice at n == 4 and n == 8 -- so a guest store
// landed TWICE and resurrected whatever a peer guest thread had committed to that word in between. Measured
// on the aarch64 guest as a spinlock admitting two holders; ~1e-4 of racing CASes silently undone. Packed
// structs, not casts: a guest access need not be aligned, and this must stay one instruction at every -O
// level, not only where the optimiser inlines a constant-size memcpy.
struct interp_una8 {
    uint8_t v;
} __attribute__((packed));

struct interp_una16 {
    uint16_t v;
} __attribute__((packed));

struct interp_una32 {
    uint32_t v;
} __attribute__((packed));

struct interp_una64 {
    uint64_t v;
} __attribute__((packed));

static void interp_copy_indivisible(void *destination, const void *source, unsigned bytes) {
    switch (bytes) {
    case 1: *(struct interp_una8 *)destination = *(const struct interp_una8 *)source; return;
    case 2: *(struct interp_una16 *)destination = *(const struct interp_una16 *)source; return;
    case 4: *(struct interp_una32 *)destination = *(const struct interp_una32 *)source; return;
    case 8: *(struct interp_una64 *)destination = *(const struct interp_una64 *)source; return;
    default: memcpy(destination, source, bytes); return; // 3/5/6/7: no guest access is this width
    }
}

static _Noreturn void interp_projection_fault(uint64_t guest_address, size_t length, hl_guest_memory_access access) {
    struct cpu *cpu = g_interp_pad_cpu;
    if (cpu == NULL || !g_interp_pad_armed) abort();
    cpu->bus_ea = guest_address;
    cpu->soft_guest_ea = guest_address;
    cpu->soft_width = length;
    cpu->soft_required = access == HL_GUEST_MEMORY_WRITE ? X86_SOFT_WRITE : X86_SOFT_READ;
    cpu->reason = R_SOFTMISS;
    g_interp_guest_access = 0;
#if defined(_WIN32)
    g_interp_pad_taken = 1;
    hl_windows_fault_pad_jump(&g_interp_fault_pad);
    abort();
#else
    siglongjmp(g_interp_fault_pad, 1);
#endif
}

static void interp_data_release_abandoned(void) {
    if (!g_interp_data_active) return;
    hl_x86_guest_data_abandon(&g_interp_data_pins);
    g_interp_data_active = 0;
}

static void interp_data_prepare(uint64_t guest, size_t length, hl_guest_memory_access access) {
    uint64_t fault = guest;
    if (g_interp_data_active) abort();
    if (hl_x86_guest_data_prepare(&g_interp_data_pins, guest, length, access, &fault) != 0)
        interp_projection_fault(fault, guest + length - fault, access);
    g_interp_data_active = 1;
}

static void interp_data_finish(void) {
    hl_x86_guest_data_release(&g_interp_data_pins);
    g_interp_data_active = 0;
}

static void interp_copy_from_guest(uint64_t guest, void *destination, size_t length) {
    if (!hl_guest_memory_indirect()) {
        const void *host = (const void *)(uintptr_t)hl_x86_guest_pointer(guest);
        interp_access_begin(guest, length);
        memcpy(destination, host, length);
        interp_access_end();
        return;
    }
    interp_data_prepare(guest, length, HL_GUEST_MEMORY_READ);
    interp_access_begin(guest, length);
    hl_x86_guest_data_copy_from(&g_interp_data_pins, destination);
    interp_access_end();
    interp_data_finish();
}

static void interp_copy_to_guest(uint64_t guest, const void *source, size_t length) {
    if (!hl_guest_memory_indirect()) {
        void *host = (void *)(uintptr_t)hl_x86_guest_pointer(guest);
        interp_access_begin(guest, length);
        memcpy(host, source, length);
        interp_access_end();
        return;
    }
    interp_data_prepare(guest, length, HL_GUEST_MEMORY_WRITE);
    interp_access_begin(guest, length);
    hl_x86_guest_data_copy_to(&g_interp_data_pins, source);
    interp_access_end();
    hl_guest_memory_store_observe(guest, length);
    interp_data_finish();
}

static uint64_t interp_load(uint64_t guest_address, int width) {
    uint64_t value = 0;
    if (!hl_guest_memory_indirect()) {
        const void *host = (const void *)(uintptr_t)hl_x86_guest_pointer(guest_address);
        interp_access_begin(guest_address, (uint64_t)width);
        interp_copy_indivisible(&value, host, (unsigned)width);
        interp_access_end();
    } else
        interp_copy_from_guest(guest_address, &value, (size_t)width);
    interp_tso_fence();
    return value;
}

static void interp_store(uint64_t guest_address, int width, uint64_t value) {
    interp_tso_fence();
    if (!hl_guest_memory_indirect()) {
        void *host = (void *)(uintptr_t)hl_x86_guest_pointer(guest_address);
        interp_access_begin(guest_address, (uint64_t)width);
        interp_copy_indivisible(host, &value, (unsigned)width);
        interp_access_end();
    } else
        interp_copy_to_guest(guest_address, &value, (size_t)width);
    // Stores into an emulated MAP_SHARED mapping (or an executable alias) must be queued for
    // jit86_smc_commit before a syscall lets a peer observe them.
    if (!hl_guest_memory_indirect() && jit86_store_alias_observation_active())
        jit86_store_alias_changed(guest_address, (uint64_t)width);
}

// Operands wider than a general register. Reads land in caller-owned locals, so a fault in a later projected
// span cannot partially mutate architectural register state. Stores prevalidate every span before committing.
static void interp_load_bytes(uint64_t guest_address, void *destination, unsigned length) {
    interp_copy_from_guest(guest_address, destination, length);
    interp_tso_fence();
}

static void interp_store_bytes(uint64_t guest_address, const void *source, unsigned length) {
    interp_tso_fence();
    interp_copy_to_guest(guest_address, source, length);
    if (!hl_guest_memory_indirect() && jit86_store_alias_observation_active())
        jit86_store_alias_changed(guest_address, (uint64_t)length);
}

// The address CALL pushes is guest-visible: DWARF FDE lookup, dladdr and unwinding need a biased ET_EXEC's
// LINK address; instruction fetch projects it onto storage after RET. Must stay byte-for-byte translate.c's.
#if defined(HL_NATIVE_TEST_HOOKS)
static int interp_backend_shape_rel32_reachable(int source_resolved, uintptr_t source_lo,
                                                uintptr_t source_hi, uintptr_t target);
static void interp_backend_family_completed(const struct cpu *cpu, const struct insn *insn, int step);
#endif
#include "interp/execution.c"

#if defined(HL_NATIVE_TEST_HOOKS)
static int interp_backend_shape_rel32_reachable(int source_resolved, uintptr_t source_lo,
                                                uintptr_t source_hi, uintptr_t target) {
    if (!source_resolved) return 0;
    if (source_hi < source_lo) return 0;
    uint64_t from_lo = source_lo > target ? source_lo - target : target - source_lo;
    uint64_t from_hi = source_hi > target ? source_hi - target : target - source_hi;
    /* Requiring both ends of the immutable emitted span to reach the target is conservative for every
       possible branch site within it.  Same-generation spans are guaranteed by CACHE_SZ's static bound;
       retained/current arena pairs are measured rather than assumed. */
    return from_lo <= INT32_MAX && from_hi <= INT32_MAX;
}

static void *interp_backend_shape_map_host(void *opaque, uint64_t gpc) {
    hl_map_host_cache_entry *cache = opaque;
    void *code = map_host_cached(cache, gpc);
    if (!g_backend_shape_edge_pending) return code;
    unsigned family = g_backend_shape_edge_family;
    if (gpc != g_backend_shape_edge_target) {
        hl_backend_tree_direct_edge_resolution(family, HL_BACKEND_SHAPE_EDGE_INTERRUPTED, 0, 0, 0, 0);
    } else if (code == NULL) {
        hl_backend_tree_direct_edge_resolution(family, HL_BACKEND_SHAPE_EDGE_UNMAPPED, 0, 0, 0, 0);
    } else {
        struct interp_block *target_block = code;
        void *target_rx = NULL;
        uint64_t target_generation = UINT64_MAX;
        int target_resolved = jit_resolve_rw_code(code, &target_rx, &target_generation);
        int target_translated = target_resolved && target_block->host_entry_off != 0;
        int current_generation = target_translated && g_backend_shape_edge_source_resolved &&
                                 g_backend_shape_edge_source_generation == g_cache_gen &&
                                 target_generation == g_cache_gen && target_block->generation == g_cache_gen;
        uintptr_t target_entry = target_translated
                                     ? (uintptr_t)target_rx + target_block->host_entry_off
                                     : 0;
        int rel32_reachable = target_translated &&
                              interp_backend_shape_rel32_reachable(g_backend_shape_edge_source_resolved,
                                                                   g_backend_shape_edge_source_host_lo,
                                                                   g_backend_shape_edge_source_host_hi, target_entry);
        int eligible = family == HL_BACKEND_SHAPE_EDGE_JCC_TAKEN && g_backend_shape_edge_same_page &&
                       target_translated && current_generation && rel32_reachable;
        hl_backend_tree_direct_edge_resolution(family, HL_BACKEND_SHAPE_EDGE_MAPPED, target_translated,
                                               current_generation, rel32_reachable, eligible);
    }
    g_backend_shape_edge_pending = 0;
    return code;
}
#endif

static int interp_step(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    // VEX/EVEX -> avx.c, which advances rip itself, so rip must name THIS instruction on the way out.
    if (insn->vex) {
        // Trap, not gap: EVEX mm==0 and 0x62's legacy BOUND are both #UD here, so SIGILL to the guest.
        if (insn->evex && insn->vex_map == 0) return interp_guest_trap(cpu, pc, 4, 2);
        return interp_exit(cpu, pc, R_AVX);
    }
    if (insn->map3) return interp_exit(cpu, pc, R_SSE3B);
    if (insn->two) return interp_step_two_byte(cpu, insn, pc, next);
    return interp_step_one_byte(cpu, insn, pc, next);
}

static unsigned interp_backend_shape_stop(const struct cpu *cpu, const struct insn *insn, uint64_t next) {
    if (insn->op == 0xFF && insn->has_modrm && insn->is_mem) {
        unsigned operation = (unsigned)insn->reg & 7u;
        if (operation == 4) return HL_BACKEND_SHAPE_S_INDIRECT_BRANCH_MEMORY;
        if (operation == 2) return HL_BACKEND_SHAPE_S_INDIRECT_CALL_MEMORY;
    }
    switch (translit_classify(insn)) {
    case TL_JCC:
        return cpu->rip == next ? HL_BACKEND_SHAPE_S_COND_NOT_TAKEN : HL_BACKEND_SHAPE_S_COND_TAKEN;
    case TL_JMP: return HL_BACKEND_SHAPE_S_DIRECT_JUMP;
    case TL_CALL: return HL_BACKEND_SHAPE_S_DIRECT_CALL;
    case TL_RET: return HL_BACKEND_SHAPE_S_RETURN;
    case TL_JMP_REG:
    case TL_JMP_MEM:
    case TL_JMP_RIP: return HL_BACKEND_SHAPE_S_INDIRECT_BRANCH;
    case TL_CALL_REG:
    case TL_CALL_RIP: return HL_BACKEND_SHAPE_S_INDIRECT_CALL;
    case TL_SYSCALL: return HL_BACKEND_SHAPE_S_SYSCALL;
    default: return cpu->reason == R_BRANCH ? HL_BACKEND_SHAPE_S_OTHER : HL_BACKEND_SHAPE_S_SERVICE;
    }
}

#if defined(HL_NATIVE_TEST_HOOKS)
// Dedicated terminal-family counts are taken only after interp_step has returned. A faulting memory
// operand longjmps before this point, so these are completed instructions rather than decode attempts.
// The 64-bit divide service has a second completion counter in interp_dispatch.h, after RAX/RDX commit.
static void interp_backend_family_completed(const struct cpu *cpu, const struct insn *insn, int step) {
    if (insn->two || insn->map3 || insn->vex || insn->evex || !insn->has_modrm) return;
    unsigned operation = (unsigned)insn->reg & 7u;
    if (insn->op == 0xFF && operation == 4 && insn->is_mem && !insn->rip_rel &&
        step == STEP_END && cpu->reason == R_BRANCH) {
        hl_backend_tree_family_jmem();
        return;
    }
    if (translit_classify(insn) != TL_NO) return;
    if ((insn->op != 0xF6 && insn->op != 0xF7) || (operation != 6 && operation != 7)) return;
    unsigned is_signed = operation == 7;
    unsigned expected_reason = is_signed ? R_IDIV : R_DIV;
    if (step != STEP_END) {
        hl_backend_tree_family_div(is_signed, HL_BACKEND_FAMILY_DIV_INLINE);
    } else if ((unsigned)cpu->reason == expected_reason && cpu->divop != 0 &&
               insn->op == 0xF7 && insn->opsize == 8) {
        hl_backend_tree_family_div(is_signed, HL_BACKEND_FAMILY_DIV_SERVICE64);
    } else if ((unsigned)cpu->reason == expected_reason && cpu->divop == 0) {
        hl_backend_tree_family_div(is_signed, HL_BACKEND_FAMILY_DIV_DE);
    }
}

static unsigned interp_backend_shape_edge_family(unsigned terminator) {
    switch (terminator) {
    case HL_BACKEND_SHAPE_T_FALLTHROUGH: return HL_BACKEND_SHAPE_EDGE_FALLTHROUGH;
    case HL_BACKEND_SHAPE_T_COND_TAKEN: return HL_BACKEND_SHAPE_EDGE_JCC_TAKEN;
    case HL_BACKEND_SHAPE_T_COND_NOT_TAKEN: return HL_BACKEND_SHAPE_EDGE_JCC_NOT_TAKEN;
    case HL_BACKEND_SHAPE_T_DIRECT_JUMP: return HL_BACKEND_SHAPE_EDGE_DIRECT_JUMP;
    case HL_BACKEND_SHAPE_T_DIRECT_CALL: return HL_BACKEND_SHAPE_EDGE_DIRECT_CALL;
    default: return HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT;
    }
}

#endif

// Every interpreted guest control transfer ends the block. Transliterated taken JCCs may cross one
// already-published same-page edge after doing their own spill/IRQ poll; every other transfer returns here.
static void interp_execute(hl_x86_hot_context *context, struct cpu *cpu) {
    g_dispatch_census_interp_steps = 0;
    g_dispatch_census_interp_stop = HL_BACKEND_SHAPE_S_OTHER;
    int census_steps = hl_backend_tree_steps_enabled();
    for (;;) {
        uint64_t pc = cpu->rip; // a fault below reports precisely this PC
        struct insn insn;
        if (hl_x86_decode_context(context, pc, &insn) < 0) {
            // Fetch failed the executable-mapping check: a guest fault, not an engine crash.
            (void)interp_guest_trap(cpu, pc, 11, 2);
#if !defined(HL_NATIVE_TEST_HOOKS)
            g_dispatch_census_pending_form_valid = 0;
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
            g_backend_shape_interp_stop = HL_BACKEND_SHAPE_S_FAULT;
            g_backend_shape_interp_stop_form = 0;
#endif
            return;
        }
        int step = interp_step(cpu, &insn, pc, pc + (uint64_t)insn.len);
        if (census_steps && step != STEP_END) {
            g_dispatch_census_interp_steps++;
#if !defined(HL_NATIVE_TEST_HOOKS)
            hl_backend_tree_executed_form(translit_unsupported_key(&insn));
#endif
        }
#if defined(HL_NATIVE_TEST_HOOKS)
        interp_backend_family_completed(cpu, &insn, step);
        // This is an execution census, not an admission/attempt census. STEP_END hands deferred work to
        // a service which may still fail or trap, so only the in-interpreter committed path is counted.
        if (g_prof) translit_unsupported_record_completed(&insn, pc, step != STEP_END);
#endif
        if (step == STEP_END) {
            if (census_steps)
                g_dispatch_census_interp_stop = interp_backend_shape_stop(cpu, &insn, pc + (uint64_t)insn.len);
#if !defined(HL_NATIVE_TEST_HOOKS)
            if (census_steps && cpu->irq == 0) {
                g_dispatch_census_pending_form = translit_unsupported_key(&insn);
                g_dispatch_census_pending_form_valid = 1;
            }
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
            g_backend_shape_interp_stop = interp_backend_shape_stop(cpu, &insn, pc + (uint64_t)insn.len);
            g_backend_shape_interp_stop_form = translit_unsupported_key(&insn);
#endif
            return;
        }
    }
}

// run_block / block_return -- the symbols core/dispatch.c and the fault path name. On AArch64 they are
// trampolines into emitted code; here run_block IS the interpreter, and block_return stays address-taken
// for sigframe_resume_dispatch but aborts.
//
// STATIC is load-bearing: the dual archive links BOTH target objects and namespace.h does not cover these
// two, so an external definition collides at link time (findings 3.7). Every caller is in this TU.
static void run_block(hl_x86_hot_context *context, struct cpu *cpu, void *code);
static void block_return(void);

static void run_block(hl_x86_hot_context *context, struct cpu *cpu, void *code) {
    struct interp_block *block = (struct interp_block *)code;
    if (block == NULL || block->magic != INTERP_BLOCK_MAGIC) {
        static const char message[] = "interpreter received an invalid block descriptor";
        (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
        cpu->reason = R_BRANCH;
        return;
    }
    // Guest-fault landing pad. savemask=0 -- this is the hottest line in the engine (once per guest block)
    // and savemask=1 makes glibc issue a real rt_sigprocmask here. interp_restore_handler_mask does the
    // restore on the fault path instead, where it is paid once per fault rather than once per block.
    // sigsetjmp/siglongjmp and NOT setjmp/longjmp: on Darwin setjmp/longjmp are the mask-SAVING pair, so
    // sigsetjmp(.,0) is the only portable way to say "this pad does not touch the mask" (same idiom as
    // host_range_mapped's probe pad in linux_abi/thread.c).
    int previous = g_interp_pad_armed;
    struct cpu *previous_cpu = g_interp_pad_cpu;
    if (INTERP_PAD_ARM(g_interp_fault_pad)) {
        // A guest access was abandoned; both routes already set cpu->reason and left cpu->rip on it.
        g_interp_guest_access = 0;
        interp_data_release_abandoned();
#if !defined(HL_NATIVE_TEST_HOOKS)
        g_dispatch_census_pending_form_valid = 0;
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
        if (g_backend_shape_open == 1) {
            unsigned packed = cpu->backend_shape;
            hl_backend_tree_translated_exit(HL_BACKEND_SHAPE_T_FAULT,
                                            (packed >> TL_SHAPE_JMP_SHIFT) & TL_SHAPE_STITCH_MASK,
                                            (packed >> TL_SHAPE_COND_FALL_SHIFT) & TL_SHAPE_STITCH_MASK);
        } else if (g_backend_shape_open == 2) {
            hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_FAULT, 0);
        }
        g_backend_shape_open = 0;
#else
        if (g_dispatch_census_open == 1)
            hl_backend_tree_translated_exit_count(HL_BACKEND_SHAPE_T_FAULT);
        else if (g_dispatch_census_open == 2)
            hl_backend_tree_interpreter_stop(HL_BACKEND_SHAPE_S_FAULT, 0);
        hl_backend_tree_reason((unsigned)cpu->reason);
        g_dispatch_census_open = 0;
#endif
        g_interp_pad_armed = previous;
        g_interp_pad_cpu = previous_cpu;
        return;
    }
    g_interp_pad_armed = 1;
    g_interp_pad_cpu = cpu;
    // A transliterated block, if this one is one and the image still permits it. Both preconditions are
    // re-tested here because they are runtime facts: an image that takes a PROT_EXEC mapping or an emulated
    // MAP_SHARED alias mid-run must stop executing verbatim stores, and the descriptor is still a valid
    // interpreter block, so falling back costs nothing but speed.
    int image_ok = translit_image_ok();
    int translated = block->host_entry_off != 0 && image_ok && translit_bind_cpu(cpu);
    if (translated) {
        if (g_prof) G_MIXED_PROFILE_CLEAR(cpu);
#if defined(HL_NATIVE_TEST_HOOKS)
        cpu->backend_shape = HL_BACKEND_SHAPE_T_OTHER |
                             HL_BACKEND_WOULD_LINK_FAMILY_COUNT << TL_SHAPE_WOULD_FAMILY_SHIFT;
        g_backend_shape_open = 1;
#endif
#if !defined(HL_NATIVE_TEST_HOOKS)
        g_dispatch_census_open = g_prof ? 1u : 0u;
#endif
        hl_backend_tree_run_begin(1, block->profile_insns);
        translit_run(cpu, block);
#if defined(HL_NATIVE_TEST_HOOKS)
        unsigned packed = cpu->backend_shape;
        unsigned terminator = (packed >> TL_SHAPE_KIND_SHIFT) & TL_SHAPE_KIND_MASK;
        /* Read once: an async kick may set irq at any instruction in this accounting sequence.  The
           translated-exit family and its would-link publication disposition must describe the same
           completed terminal, or exact family reconciliation gains a signal-sized race window. */
        int terminal_completed = cpu->irq == 0;
        if (g_prof && terminal_completed) translit_mixed_profile_completed(G_MIXED_PROFILE_VALUE(cpu));
        unsigned exit_kind = terminal_completed ? terminator : HL_BACKEND_SHAPE_T_IRQ;
        hl_backend_tree_translated_exit(exit_kind, (packed >> TL_SHAPE_JMP_SHIFT) & TL_SHAPE_STITCH_MASK,
                                        (packed >> TL_SHAPE_COND_FALL_SHIFT) & TL_SHAPE_STITCH_MASK);
        if (terminal_completed && terminator == HL_BACKEND_SHAPE_T_FALLTHROUGH)
            hl_backend_tree_translated_fall_stop((packed >> TL_SHAPE_FALL_STOP_SHIFT) &
                                                 TL_SHAPE_FALL_STOP_MASK);
        unsigned would_link_family =
            (packed >> TL_SHAPE_WOULD_FAMILY_SHIFT) & TL_SHAPE_WOULD_FAMILY_MASK;
        unsigned would_link_disposition =
            (packed >> TL_SHAPE_WOULD_DISPOSITION_SHIFT) & TL_SHAPE_WOULD_DISPOSITION_MASK;
        if (terminal_completed && would_link_family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT)
            hl_backend_tree_would_link(would_link_family, would_link_disposition);
        unsigned family = interp_backend_shape_edge_family(terminator);
        if (family < HL_BACKEND_SHAPE_EDGE_FAMILY_COUNT) {
            if (g_backend_shape_edge_pending)
                hl_backend_tree_direct_edge_resolution(g_backend_shape_edge_family,
                                                       HL_BACKEND_SHAPE_EDGE_INTERRUPTED, 0, 0, 0, 0);
            uint64_t source_page = (block->gpc & ~UINT64_C(0xfff)) +
                                   ((uint64_t)((packed >> TL_SHAPE_SOURCE_PAGE_SHIFT) &
                                               TL_SHAPE_SOURCE_PAGE_MASK)
                                    << 12);
            int same_page = (source_page >> 12) == (cpu->rip >> 12);
            hl_backend_tree_direct_edge(family, same_page);
            g_backend_shape_edge_pending = 1;
            g_backend_shape_edge_family = family;
            g_backend_shape_edge_target = cpu->rip;
            void *source_rx = NULL;
            uint64_t source_generation = UINT64_MAX;
            int source_resolved = jit_resolve_host_rx_code(block, &source_rx, &source_generation);
            g_backend_shape_edge_source_resolved = source_resolved;
            g_backend_shape_edge_source_generation = source_resolved ? source_generation : UINT64_MAX;
            g_backend_shape_edge_source_host_lo =
                source_resolved ? (uintptr_t)source_rx + block->host_entry_off : 0;
            g_backend_shape_edge_source_host_hi =
                source_resolved ? g_backend_shape_edge_source_host_lo + block->host_len : 0;
            g_backend_shape_edge_same_page = same_page;
        }
        g_backend_shape_open = 0;
#else
        // A fault leaves through the siglongjmp arm above and never reaches this completion seam. IRQ is
        // likewise not a completed terminal. Only a validated marker written by the terminal that actually
        // ran may advance the production execution proof.
        if (g_prof) {
            uint32_t marker = G_MIXED_PROFILE_VALUE(cpu);
            unsigned marker_kind = (marker >> TL_MIXED_PROFILE_KIND_SHIFT) & TL_MIXED_PROFILE_KIND_MASK;
            int marker_valid = (marker & TL_MIXED_PROFILE_MAGIC_MASK) == TL_MIXED_PROFILE_MAGIC;
            if (cpu->irq == 0) {
                translit_mixed_profile_completed(marker);
                if (marker_valid && marker_kind == HL_BACKEND_SHAPE_T_DIRECT_CALL)
                    translit_call_sim_probe(cpu->rip);
            } else if (marker_valid && marker_kind == HL_BACKEND_SHAPE_T_DIRECT_CALL) {
                hl_backend_tree_call_sim_count(HL_BACKEND_CALL_SIM_DECLINE_IRQ);
            }
        }
        g_dispatch_census_open = 0;
#endif
        hl_backend_tree_reason(cpu->reason);
    } else {
        hl_backend_tree_run_begin(0, 0);
#if !defined(HL_NATIVE_TEST_HOOKS)
        g_dispatch_census_open = hl_backend_tree_steps_enabled() ? 2u : 0u;
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
        unsigned fallback = block->host_entry_off == 0
                                ? block->profile_fallback_kind
                                : !image_ok ? HL_BACKEND_SHAPE_I_RUNTIME_IMAGE : HL_BACKEND_SHAPE_I_RUNTIME_BIND;
        hl_backend_tree_interpreter_entry(fallback, block->profile_fallback_form);
        g_backend_shape_interp_stop = HL_BACKEND_SHAPE_S_OTHER;
        g_backend_shape_interp_stop_form = 0;
        g_backend_shape_open = 2;
#endif
        interp_execute(context, cpu);
#if !defined(HL_NATIVE_TEST_HOOKS)
        if (cpu->reason == R_BRANCH) interp_executed_form_complete(cpu, R_BRANCH);
#endif
        hl_backend_tree_interpreted_steps(g_dispatch_census_interp_steps);
#if defined(HL_NATIVE_TEST_HOOKS)
        unsigned stop = cpu->irq != 0 ? HL_BACKEND_SHAPE_S_IRQ : g_backend_shape_interp_stop;
        hl_backend_tree_interpreter_stop(stop, g_backend_shape_interp_stop_form);
        g_backend_shape_open = 0;
#else
        hl_backend_tree_interpreter_stop(g_dispatch_census_interp_stop, 0);
        if (!g_dispatch_census_pending_form_valid) g_dispatch_census_open = 0;
#endif
        hl_backend_tree_reason(cpu->reason);
    }
    g_interp_pad_armed = previous;
    g_interp_pad_cpu = previous_cpu;
}

static void block_return(void) {
    static const char message[] = "interpreter received an invalid generated-code return";
    (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
    abort();
}

// The one-byte opcode map.

// The eight ALU kinds in x86 opcode order.
enum { ALU_ADD, ALU_OR, ALU_ADC, ALU_SBB, ALU_AND, ALU_SUB, ALU_XOR, ALU_CMP };

static const enum interp_rmw_kind g_alu_rmw[8] = {RMW_ADD, RMW_OR,  RMW_ADC, RMW_SBB,
                                                  RMW_AND, RMW_SUB, RMW_XOR, RMW_CMP};

// *store is cleared for CMP, which discards its result.
static uint64_t interp_alu_kind(struct cpu *cpu, int kind, uint64_t a, uint64_t b, int width, int *store) {
    uint64_t result;
    *store = 1;
    switch (kind) {
    case ALU_ADD: return interp_alu_add(cpu, a, b, 0, width);
    case ALU_ADC: return interp_alu_add(cpu, a, b, interp_cf(cpu), width);
    case ALU_SBB: return interp_alu_sub(cpu, a, b, interp_cf(cpu), width);
    case ALU_SUB: return interp_alu_sub(cpu, a, b, 0, width);
    case ALU_CMP: *store = 0; return interp_alu_sub(cpu, a, b, 0, width);
    case ALU_OR: result = (a | b) & interp_mask(width); break;
    case ALU_AND: result = (a & b) & interp_mask(width); break;
    default: result = (a ^ b) & interp_mask(width); break; // ALU_XOR
    }
    interp_flags_logic(cpu, result, width);
    return result;
}

// ALU with an r/m destination; the LOCK path is shared by all five encodings that reach it.
static void interp_alu_to_rm(struct cpu *cpu, struct insn *insn, const interp_operand *operand, int kind, int width,
                             uint64_t source) {
    int store;
    if (insn->lock && operand->is_memory && kind != ALU_CMP) {
        // Flags from the pre-image: a locked op and its unlocked twin must agree.
        unsigned carry = (kind == ALU_ADC || kind == ALU_SBB) ? interp_cf(cpu) : 0u;
        uint64_t old = interp_locked_rmw(operand->address, width, g_alu_rmw[kind], source, carry);
        (void)interp_alu_kind(cpu, kind, old, source, width, &store);
        return;
    }
    uint64_t old = interp_rm_read(cpu, insn, operand, width);
    uint64_t result = interp_alu_kind(cpu, kind, old, source, width, &store);
    if (store) interp_rm_write(cpu, insn, operand, width, result);
}

static int interp_one_byte_alu(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op >= 0x40 || (op & 7) > 5) return -1;
    int kind = op >> 3;
    int form = op & 7;
    int width = (form == 0 || form == 2 || form == 4) ? 1 : insn->opsize;
    if (form == 4 || form == 5) {
        uint64_t old = interp_reg_read(cpu, insn, RAX, width);
        int store;
        uint64_t result = interp_alu_kind(cpu, kind, old, (uint64_t)insn->imm, width, &store);
        if (store) interp_reg_write(cpu, insn, RAX, width, result);
    } else if (form == 2 || form == 3) {
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t source = interp_rm_read(cpu, insn, &operand, width);
        uint64_t old = interp_reg_read(cpu, insn, insn->reg, width);
        int store;
        uint64_t result = interp_alu_kind(cpu, kind, old, source, width, &store);
        if (store) interp_reg_write(cpu, insn, insn->reg, width, result);
    } else {
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_alu_to_rm(cpu, insn, &operand, kind, width, interp_reg_read(cpu, insn, insn->reg, width));
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_stack(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op >= 0x50 && op <= 0x57) {
        int width = interp_stack_width(insn);
        interp_push(cpu, cpu->r[(op & 7) | (insn->rexB << 3)], width);
    } else if (op >= 0x58 && op <= 0x5F) {
        int width = interp_stack_width(insn);
        int number = (op & 7) | (insn->rexB << 3);
        uint64_t value = interp_pop(cpu, width);
        if (width == 2)
            cpu->r[number] = (cpu->r[number] & ~UINT64_C(0xffff)) | (value & 0xffff);
        else
            cpu->r[number] = value;
    } else if (op == 0x68 || op == 0x6A) {
        interp_push(cpu, (uint64_t)insn->imm, interp_stack_width(insn));
    } else if (op == 0x8F) {
        if ((insn->reg & 7) != 0) return interp_undefined(cpu, insn, pc, "group 1A opcode other than POP r/m");
        int width = interp_stack_width(insn);
        uint64_t value = interp_pop(cpu, width);
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_rm_write(cpu, insn, &operand, width, value);
    } else {
        return -1;
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_modrm_transfer(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op < 0x86 || op > 0x8E || op == 0x8F) return -1;
    if (op == 0x86 || op == 0x87) {
        int width = (op & 1) ? insn->opsize : 1;
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t reg_value = interp_reg_read(cpu, insn, insn->reg, width);
        if (operand.is_memory) {
            uint64_t old = interp_locked_rmw(operand.address, width, RMW_XCHG, reg_value, 0);
            interp_reg_write(cpu, insn, insn->reg, width, old);
        } else {
            uint64_t old = interp_reg_read(cpu, insn, operand.number, width);
            interp_reg_write(cpu, insn, operand.number, width, reg_value);
            interp_reg_write(cpu, insn, insn->reg, width, old);
        }
    } else if (op >= 0x88 && op <= 0x8B) {
        int width = (op & 1) ? insn->opsize : 1;
        interp_operand operand = interp_rm(cpu, insn, next);
        if (op <= 0x89)
            interp_rm_write(cpu, insn, &operand, width, interp_reg_read(cpu, insn, insn->reg, width));
        else
            interp_reg_write(cpu, insn, insn->reg, width, interp_rm_read(cpu, insn, &operand, width));
    } else if (op == 0x8C) {
        static const int selector[8] = {0, 0x33, 0x2b, 0, 0, 0, 0, 0};
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_rm_write(cpu, insn, &operand, operand.is_memory ? 2 : insn->opsize, (uint64_t)selector[insn->reg & 7]);
    } else if (op == 0x8D) {
        if (!insn->is_mem) return interp_undefined(cpu, insn, pc, "LEA with a register operand (#UD encoding)");
        interp_reg_write(cpu, insn, insn->reg, insn->opsize, interp_lea_value(cpu, insn, next));
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_relative_control(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op >= 0x70 && op <= 0x7F) {
        cpu->rip = interp_cond(cpu, op & 0xf) ? next + (uint64_t)insn->imm : next;
    } else if (op == 0xE8) {
        interp_push(cpu, interp_call_return_pc(next), interp_stack_width(insn));
        cpu->rip = next + (uint64_t)insn->imm;
    } else if (op == 0xE9 || op == 0xEB) {
        cpu->rip = next + (uint64_t)insn->imm;
    } else {
        return -1;
    }
    cpu->reason = R_BRANCH;
    return STEP_END;
}

static int interp_one_byte_string(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0xA6 || op == 0xA7 || op == 0xAE || op == 0xAF) {
        int width = (op & 1) ? insn->opsize : 1;
        int is_scas = op == 0xAE || op == 0xAF;
        cpu->divop = (uint64_t)width | ((uint64_t)is_scas << 8) | ((uint64_t)(insn->repne != 0) << 9) |
                     ((uint64_t)((insn->rep || insn->repne) != 0) << 10) | ((cpu->df & 1) << 11);
        return interp_exit(cpu, next, R_REPSTR);
    }
    if (op != 0xA4 && op != 0xA5 && op != 0xAA && op != 0xAB && op != 0xAC && op != 0xAD) return -1;
    int width = (op & 1) ? insn->opsize : 1;
    int movs = op == 0xA4 || op == 0xA5;
    int lods = op == 0xAC || op == 0xAD;
    uint64_t step = (cpu->df & 1) ? UINT64_C(0) - (uint64_t)width : (uint64_t)width;
    if (insn->rep && cpu->r[RCX] == 0) {
        cpu->rip = next;
        return STEP_NEXT;
    }
    if (insn->rep) {
        if (movs)
            hl_x86_count_rep_movs();
        else if (!lods)
            hl_x86_count_rep_stos();
    }
    uint64_t iterations = insn->rep ? cpu->r[RCX] : 1;
    while (iterations != 0) {
        if (movs) {
            interp_store(cpu->r[RDI], width, interp_load(cpu->r[RSI], width));
            cpu->r[RSI] += step;
            cpu->r[RDI] += step;
        } else if (lods) {
            interp_reg_write(cpu, insn, RAX, width, interp_load(cpu->r[RSI], width));
            cpu->r[RSI] += step;
        } else {
            interp_store(cpu->r[RDI], width, cpu->r[RAX] & interp_mask(width));
            cpu->r[RDI] += step;
        }
        if (!insn->rep) break;
        iterations = --cpu->r[RCX];
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_flags(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0x9C) {
        interp_push(cpu, interp_read_rflags(cpu), interp_stack_width(insn));
    } else if (op == 0x9D) {
        interp_write_rflags(cpu, interp_pop(cpu, interp_stack_width(insn)));
    } else if (op == 0x9E) {
        uint64_t ah = (cpu->r[RAX] >> 8) & 0xff;
        unsigned of = (unsigned)((cpu->nzcv >> 28) & 1);
        interp_flags_nzcv(cpu, (unsigned)((ah >> 7) & 1), (unsigned)((ah >> 6) & 1), (unsigned)(ah & 1), of);
        cpu->pf = ((ah >> 2) & 1) ^ 1u;
        cpu->af = ((ah >> 4) & 1) << 4;
    } else if (op == 0x9F) {
        uint64_t flags = interp_read_rflags(cpu);
        cpu->r[RAX] = (cpu->r[RAX] & ~UINT64_C(0xff00)) | ((flags & 0xd5) | 0x02) << 8;
    } else if (op == 0xF5) {
        interp_set_cf(cpu, interp_cf(cpu) ^ 1u);
    } else if (op == 0xF8) {
        interp_set_cf(cpu, 0);
    } else if (op == 0xF9) {
        interp_set_cf(cpu, 1);
    } else if (op == 0xFC) {
        cpu->df = 0;
    } else if (op == 0xFD) {
        cpu->df = 1;
    } else {
        return -1;
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_test_immediate(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0x80 || op == 0x81 || op == 0x83) {
        int width = op == 0x80 ? 1 : insn->opsize;
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_alu_to_rm(cpu, insn, &operand, insn->reg & 7, width, (uint64_t)insn->imm);
    } else if (op == 0x84 || op == 0x85) {
        int width = (op & 1) ? insn->opsize : 1;
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t left = interp_rm_read(cpu, insn, &operand, width);
        uint64_t right = interp_reg_read(cpu, insn, insn->reg, width);
        interp_flags_logic(cpu, (left & right) & interp_mask(width), width);
    } else if (op == 0xA8 || op == 0xA9) {
        int width = (op & 1) ? insn->opsize : 1;
        uint64_t value = interp_reg_read(cpu, insn, RAX, width) & (uint64_t)insn->imm & interp_mask(width);
        interp_flags_logic(cpu, value, width);
    } else {
        return -1;
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_integer_convert(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0x63) {
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t source = interp_rm_read(cpu, insn, &operand, 4);
        uint64_t value = insn->opsize == 8 ? (uint64_t)(int64_t)(int32_t)(uint32_t)source : source;
        interp_reg_write(cpu, insn, insn->reg, insn->opsize, value);
    } else if (op == 0x69 || op == 0x6B) {
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t source = interp_rm_read(cpu, insn, &operand, insn->opsize);
        uint64_t value = interp_imul_truncating(cpu, source, (uint64_t)insn->imm, insn->opsize);
        interp_reg_write(cpu, insn, insn->reg, insn->opsize, value);
    } else if (op == 0x98) {
        int width = insn->opsize;
        uint64_t value = width == 2   ? (uint64_t)(int64_t)(int8_t)(uint8_t)cpu->r[RAX]
                         : width == 4 ? (uint64_t)(int64_t)(int16_t)(uint16_t)cpu->r[RAX]
                                      : (uint64_t)(int64_t)(int32_t)(uint32_t)cpu->r[RAX];
        interp_reg_write(cpu, insn, RAX, width, value);
    } else if (op == 0x99) {
        int width = insn->opsize;
        uint64_t sign = interp_msb(cpu->r[RAX] & interp_mask(width), width) ? UINT64_MAX : 0;
        interp_reg_write(cpu, insn, RDX, width, sign);
    } else {
        return -1;
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_one_byte_move_immediate(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op < 0xB0 || op > 0xBF) return -1;
    int width = op < 0xB8 ? 1 : insn->opsize;
    int number = (op & 7) | (insn->rexB << 3);
    interp_reg_write(cpu, insn, number, width, (uint64_t)insn->imm);
    cpu->rip = next;
    return STEP_NEXT;
}

#include "interp_shift.c"
#include "interp_group3.c"

static int interp_step_one_byte(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int delegated = interp_one_byte_alu(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_stack(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_modrm_transfer(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_relative_control(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_string(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_flags(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_test_immediate(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_integer_convert(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_move_immediate(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_shift(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_one_byte_group3(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;

    switch (op) {
    // NOP and XCHG rAX, r
    case 0x90:
    case 0x91:
    case 0x92:
    case 0x93:
    case 0x94:
    case 0x95:
    case 0x96:
    case 0x97: {
        int number = (op & 7) | (insn->rexB << 3);
        // 0x90 without REX.B is XCHG rAX,rAX = NOP, as are 0x66 90 and F3 90 PAUSE.
        if (number != RAX) {
            int width = insn->opsize;
            uint64_t a = interp_reg_read(cpu, insn, RAX, width);
            uint64_t b = interp_reg_read(cpu, insn, number, width);
            interp_reg_write(cpu, insn, RAX, width, b);
            interp_reg_write(cpu, insn, number, width, a);
        }
        cpu->rip = next;
        return STEP_NEXT;
    }

    // FWAIT: no x87 exception is ever pending here
    case 0x9B: cpu->rip = next; return STEP_NEXT;

    // MOV to/from a moffs address
    case 0xA0:
    case 0xA1:
    case 0xA2:
    case 0xA3: {
        int width = (op & 1) ? insn->opsize : 1;
        uint64_t address = (uint64_t)insn->imm;
        if (insn->seg == 1)
            address += cpu->fs_base;
        else if (insn->seg == 2)
            address += cpu->gs_base;
        if (op <= 0xA1)
            interp_reg_write(cpu, insn, RAX, width, interp_load(address, width));
        else
            interp_store(address, width, interp_reg_read(cpu, insn, RAX, width));
        cpu->rip = next;
        return STEP_NEXT;
    }

    // RET and RET imm16
    case 0xC2:
    case 0xC3: {
        int width = interp_stack_width(insn);
        uint64_t target = interp_pop(cpu, width);
        if (op == 0xC2) cpu->r[RSP] += (uint64_t)(uint16_t)insn->imm;
        cpu->dbg_ibsrc = pc; // guest PC of the last indirect branch
        cpu->rip = target;
        cpu->reason = R_BRANCH;
        return STEP_END;
    }

    // MOV r/m, imm
    case 0xC6:
    case 0xC7: {
        if ((insn->reg & 7) != 0) return interp_undefined(cpu, insn, pc, "group 11 opcode other than MOV r/m,imm");
        int width = (op & 1) ? insn->opsize : 1;
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t value = (uint64_t)insn->imm;
        interp_rm_write(cpu, insn, &operand, width, value);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // LEAVE
    case 0xC9: {
        int width = interp_stack_width(insn);
        cpu->r[RSP] = cpu->r[RBP];
        cpu->r[RBP] = interp_pop(cpu, width);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // INT3: #BP -> SIGTRAP at the instruction AFTER the trap
    case 0xCC: return interp_guest_trap(cpu, next, 5 /*SIGTRAP*/, 1 /*TRAP_BRKPT*/);

    // IRETQ (REX.W CF): the long-mode interrupt frame is RIP, CS, RFLAGS, RSP, SS in increasing addresses.
    // At CPL 3 this is a context restore rather than a return from an interrupt; CS/SS are the fixed
    // userspace selectors of case 0x8C, so they are consumed for their stack slots and dropped. The whole
    // frame is read before anything is written: a fault mid-frame must leave RSP and the flags as they
    // were. IRETD/IRETW (no REX.W) keeps reporting.
    case 0xCF: {
        if (!insn->rexW) break;
        uint64_t target = interp_load(cpu->r[RSP] + 0, 8);
        uint64_t flags = interp_load(cpu->r[RSP] + 16, 8);
        uint64_t stack = interp_load(cpu->r[RSP] + 24, 8);
        interp_write_rflags(cpu, flags);
        cpu->r[RSP] = stack;
        cpu->dbg_ibsrc = pc;
        cpu->rip = target;
        cpu->reason = R_BRANCH;
        return STEP_END;
    }

    // XLATB (D7): AL = [(seg:) RBX + zero-extended AL]. The index is the 8-bit AL alone -- bits 63:8 of
    // RAX never take part -- so this cannot ride the ModRM index path.
    case 0xD7: {
        uint64_t address = interp_implicit_address(cpu, insn, cpu->r[RBX] + (cpu->r[RAX] & 0xff));
        interp_reg_write(cpu, insn, RAX, 1, interp_load(address, 1));
        cpu->rip = next;
        return STEP_NEXT;
    }

    // LOOP / LOOPE / LOOPNE and JRCXZ
    case 0xE0:
    case 0xE1:
    case 0xE2: {
        // The counter is RCX, or ECX under a 0x67 address-size override, which also zero-extends.
        uint64_t counter = insn->addr32 ? ((cpu->r[RCX] - 1) & UINT64_C(0xffffffff)) : (cpu->r[RCX] - 1);
        cpu->r[RCX] = counter;
        int zf = (int)((cpu->nzcv >> 30) & 1);
        int take = counter != 0 && (op == 0xE2 || (op == 0xE1 ? zf : !zf));
        cpu->rip = take ? next + (uint64_t)insn->imm : next;
        cpu->reason = R_BRANCH;
        return STEP_END;
    }
    case 0xE3: {
        uint64_t counter = insn->addr32 ? (cpu->r[RCX] & UINT64_C(0xffffffff)) : cpu->r[RCX];
        cpu->rip = counter == 0 ? next + (uint64_t)insn->imm : next;
        cpu->reason = R_BRANCH;
        return STEP_END;
    }

    // Group 4/5
    case 0xFE:
    case 0xFF: {
        int sub = insn->reg & 7;
        interp_operand operand = interp_rm(cpu, insn, next);
        if (sub == 0 || sub == 1) { // INC / DEC -- x86 leaves CF untouched
            int width = (op == 0xFE) ? 1 : insn->opsize;
            uint64_t old;
            if (insn->lock && operand.is_memory) {
                old = interp_locked_rmw(operand.address, width, sub == 0 ? RMW_INC : RMW_DEC, 0, 0);
                (void)interp_alu_incdec(cpu, old, sub == 1, width);
            } else {
                old = interp_rm_read(cpu, insn, &operand, width);
                interp_rm_write(cpu, insn, &operand, width, interp_alu_incdec(cpu, old, sub == 1, width));
            }
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (op == 0xFE) return interp_undefined(cpu, insn, pc, "group 4 opcode other than INC/DEC r/m8");
        int width = interp_stack_width(insn);
        if (sub == 2) { // CALL r/m (near, indirect)
            uint64_t target = interp_rm_read(cpu, insn, &operand, width);
            interp_push(cpu, interp_call_return_pc(next), width);
            cpu->dbg_ibsrc = pc;
            cpu->rip = target;
            cpu->reason = R_BRANCH;
            return STEP_END;
        }
        if (sub == 4) { // JMP r/m (near, indirect)
            uint64_t target = interp_rm_read(cpu, insn, &operand, width);
            cpu->dbg_ibsrc = pc;
            cpu->rip = target;
            cpu->reason = R_BRANCH;
            return STEP_END;
        }
        if (sub == 6) { // PUSH r/m
            interp_push(cpu, interp_rm_read(cpu, insn, &operand, width), width);
            cpu->rip = next;
            return STEP_NEXT;
        }
        // /3 CALLF and /5 JMPF: no segments here, and Linux never issues them.
        return interp_undefined(cpu, insn, pc, "far CALL/JMP through a segment descriptor");
    }

    // HLT: privileged -> #GP, which Linux reports as SIGSEGV/SI_KERNEL with si_addr 0, not SEGV_ACCERR.
    case 0xF4: return interp_guest_trap(cpu, pc, 11 /*SIGSEGV*/, 128 /*SI_KERNEL*/);

    default: break;
    }

    if (op >= 0xD8 && op <= 0xDF) return interp_step_x87(cpu, insn, pc, next);

    // Rest of the map, split trap-versus-gap. These opcodes do not EXIST in 64-bit mode -- the 32-bit
    // segment pushes, BCD adjusts, PUSHA/POPA, the 0x82 group-1 alias, far CALL/JMP, AAM/AAD/SALC and
    // INTO. #UD is the whole implementation; routing them to interp_undefined killed the engine with
    // exit 70 where a guest that branched into data must see SIGILL (272 encodings in the sweep).
    switch (op) {
    case 0x06:
    case 0x07:
    case 0x0E:
    case 0x16:
    case 0x17:
    case 0x1E:
    case 0x1F:
    case 0x27:
    case 0x2F:
    case 0x37:
    case 0x3F:
    case 0x60:
    case 0x61:
    case 0x82:
    case 0x9A:
    case 0xCE:
    case 0xD4:
    case 0xD5:
    case 0xD6:
    case 0xEA: return interp_guest_trap(cpu, pc, 4 /*SIGILL*/, 2 /*ILL_ILLOPN*/);
    default: break;
    }

    // Port I/O at CPL 3 with IOPL 0: always #GP(0), with no implementation to add.
    if (op == 0x6C || op == 0x6D || op == 0x6E || op == 0x6F || op == 0xE4 || op == 0xE5 || op == 0xE6 || op == 0xE7 ||
        op == 0xEC || op == 0xED || op == 0xEE || op == 0xEF)
        return interp_guest_trap(cpu, pc, 11 /*SIGSEGV*/, 128 /*SI_KERNEL*/);

    // INT imm8/IRETD/RETF stay REPORTED: `int $0x80` is Linux's 32-bit syscall gate -- a gap, not a fault.
    if (op == 0xCD || op == 0xCF || op == 0xCA || op == 0xCB)
        return interp_undefined(cpu, insn, pc, "software interrupt / 32-bit IRETD / far return (INT/IRETD/RETF)");
    return interp_undefined(cpu, insn, pc, "one-byte opcode");
}

#include "interp/sse.c"
#include "interp/x87.c"

static int interp_is_legacy_sse(uint8_t op) {
    if (op >= 0x10 && op <= 0x17) return 1;
    if (op >= 0x28 && op <= 0x2F) return 1;
    if (op >= 0x50 && op <= 0x6D) return 1;
    if (op >= 0x6E && op <= 0x7F) return 1;
    if (op >= 0xD0) return 1;
    if (op == 0xC2 || op == 0xC4 || op == 0xC5 || op == 0xC6) return 1;
    return 0;
}

static int interp_two_byte_condition(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op >= 0x80 && op <= 0x8F) {
        cpu->rip = interp_cond(cpu, op & 0xf) ? next + (uint64_t)insn->imm : next;
        cpu->reason = R_BRANCH;
        return STEP_END;
    }

    // SETcc r/m8 (0F 90..9F)
    if (op >= 0x90 && op <= 0x9F) {
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_rm_write(cpu, insn, &operand, 1, (uint64_t)(interp_cond(cpu, op & 0xf) ? 1 : 0));
        cpu->rip = next;
        return STEP_NEXT;
    }
    if (op >= 0x40 && op <= 0x4F) {
        interp_operand operand = interp_rm(cpu, insn, next);
        // Source read and destination written unconditionally; `cmovcc r32` zero-extends even without a move.
        uint64_t source = interp_rm_read(cpu, insn, &operand, insn->opsize);
        interp_reg_write(cpu, insn, insn->reg, insn->opsize,
                         interp_cond(cpu, op & 0xf) ? source : interp_reg_read(cpu, insn, insn->reg, insn->opsize));
        cpu->rip = next;
        return STEP_NEXT;
    }
    return -1;
}

static int interp_two_byte_bswap(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op < 0xC8 || op > 0xCF) return -1;
    int number = (op & 7) | (insn->rexB << 3);
    uint64_t value = cpu->r[number];
    if (insn->opsize == 8)
        value = __builtin_bswap64(value);
    else
        value = __builtin_bswap32((uint32_t)value);
    interp_reg_write(cpu, insn, number, insn->opsize == 8 ? 8 : 4, value);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_system(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0x05) return interp_exit(cpu, next, R_SYSCALL);
    if (op == 0xA2) return interp_exit(cpu, next, R_CPUID);
    if (op == 0x0B || op == 0xB9 || op == 0xFF) return interp_guest_trap(cpu, pc, 4, 2);
    if (op == 0x77 || op == 0x0D || (op >= 0x18 && op <= 0x1F)) {
        cpu->rip = next;
        return STEP_NEXT;
    }
    if (op == 0x31) {
        uint64_t counter = now_ns();
        interp_reg_write(cpu, insn, RAX, 4, counter & UINT64_C(0xffffffff));
        interp_reg_write(cpu, insn, RDX, 4, counter >> 32);
        cpu->rip = next;
        return STEP_NEXT;
    }
    return -1;
}

static int interp_two_byte_extend(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xB6 && op != 0xB7 && op != 0xBE && op != 0xBF) return -1;
    int width = (op & 1) ? 2 : 1;
    interp_operand operand = interp_rm(cpu, insn, next);
    uint64_t value = interp_rm_read(cpu, insn, &operand, width);
    if (op >= 0xBE) {
        unsigned bits = (unsigned)(8 * width);
        value = (uint64_t)((int64_t)(value << (64 - bits)) >> (64 - bits));
    }
    interp_reg_write(cpu, insn, insn->reg, insn->opsize, value);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_shift(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xA4 && op != 0xA5 && op != 0xAC && op != 0xAD) return -1;
    int right = op >= 0xAC;
    int width = insn->opsize;
    unsigned count = (op == 0xA4 || op == 0xAC) ? (unsigned)(insn->imm & 0xff) : (unsigned)(cpu->r[RCX] & 0xff);
    interp_operand operand = interp_rm(cpu, insn, next);
    uint64_t value = interp_rm_read(cpu, insn, &operand, width);
    uint64_t fill = interp_reg_read(cpu, insn, insn->reg, width);
    interp_rm_write(cpu, insn, &operand, width, interp_double_shift(cpu, right, value, fill, count, width));
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_bit_modify(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xA3 && op != 0xAB && op != 0xB3 && op != 0xBB && op != 0xBA) return -1;
    int immediate_form = op == 0xBA;
    int sub = immediate_form ? (insn->reg & 7) : ((op >> 3) & 3) + 4;
    if (immediate_form && sub < 4) return interp_undefined(cpu, insn, pc, "0F BA group with /reg < 4");
    int width = insn->opsize;
    interp_operand operand = interp_rm(cpu, insn, next);
    int64_t index = immediate_form ? (int64_t)(insn->imm & (width == 8 ? 63 : 31))
                                   : (int64_t)interp_reg_read(cpu, insn, insn->reg, width);
    if (!immediate_form && width != 8) index = (int64_t)(int32_t)(uint32_t)(uint64_t)index;
    enum interp_rmw_kind rmw = sub == 5 ? RMW_BTS : sub == 6 ? RMW_BTR : RMW_BTC;
    unsigned bit;
    uint64_t old;
    if (operand.is_memory) {
        uint64_t address = operand.address + (uint64_t)(index >> 3);
        bit = (unsigned)(index & 7);
        if (sub == 4)
            old = interp_load(address, 1);
        else if (insn->lock)
            old = interp_locked_rmw(address, 1, rmw, UINT64_C(1) << bit, 0);
        else {
            old = interp_load(address, 1);
            interp_store(address, 1, interp_rmw_apply(rmw, old, UINT64_C(1) << bit, 0, 1));
        }
    } else {
        bit = (unsigned)((uint64_t)index & (width == 8 ? 63u : (unsigned)(8 * width - 1)));
        old = interp_reg_read(cpu, insn, operand.number, width);
        if (sub != 4)
            interp_reg_write(cpu, insn, operand.number, width,
                             interp_rmw_apply(rmw, old, UINT64_C(1) << bit, 0, width));
    }
    interp_set_cf(cpu, (unsigned)((old >> bit) & 1));
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_bit_count(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0xB8) {
        if (!insn->rep) return interp_undefined(cpu, insn, pc, "0F B8 without the F3 prefix (JMPE)");
        int width = insn->opsize;
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t source = interp_rm_read(cpu, insn, &operand, width) & interp_mask(width);
        uint64_t result = (uint64_t)__builtin_popcountll(source);
        interp_flags_nzcv(cpu, 0, source == 0, 0, 0);
        cpu->pf = 1;
        cpu->af = 0;
        interp_reg_write(cpu, insn, insn->reg, width, result);
        cpu->rip = next;
        return STEP_NEXT;
    }
    if (op != 0xBC && op != 0xBD) return -1;
    int width = insn->opsize;
    unsigned bits = (unsigned)(8 * width);
    interp_operand operand = interp_rm(cpu, insn, next);
    uint64_t source = interp_rm_read(cpu, insn, &operand, width) & interp_mask(width);
    if (insn->rep) {
        uint64_t result = source == 0  ? bits
                          : op == 0xBC ? (uint64_t)__builtin_ctzll(source)
                                       : (uint64_t)(__builtin_clzll(source) - (64 - bits));
        interp_flags_nzcv(cpu, 0, result == 0, source == 0, 0);
        cpu->pf = result & 0xff;
        interp_reg_write(cpu, insn, insn->reg, width, result);
    } else if (source == 0) {
        interp_flags_nzcv(cpu, 0, 1, 0, 0);
        // BSF/BSR leave the destination undefined for a zero source, but a 32-bit
        // destination write still zero-extends its retained low half.
        if (width == 4) interp_reg_write(cpu, insn, insn->reg, width, interp_reg_read(cpu, insn, insn->reg, width));
    } else {
        uint64_t result = op == 0xBC ? (uint64_t)__builtin_ctzll(source) : (uint64_t)(63 - __builtin_clzll(source));
        interp_flags_nzcv(cpu, interp_msb(source, width), 0, 0, 0);
        interp_reg_write(cpu, insn, insn->reg, width, result);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_compare_exchange(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xB0 && op != 0xB1) return -1;
    int width = (op & 1) ? insn->opsize : 1;
    interp_operand operand = interp_rm(cpu, insn, next);
    uint64_t accumulator = interp_reg_read(cpu, insn, RAX, width);
    uint64_t source = interp_reg_read(cpu, insn, insn->reg, width);
    uint64_t observed;
    if (operand.is_memory && insn->lock) {
        uint64_t host_address = hl_x86_guest_pointer(operand.address);
        void *pointer = (void *)(uintptr_t)host_address;
        int swapped = 0;
        interp_access_begin(operand.address, (uint64_t)width);
        if ((host_address & (uint64_t)(width - 1)) == 0) {
            switch (width) {
            case 1: {
                unsigned char expected = (unsigned char)accumulator;
                swapped = __atomic_compare_exchange_n((unsigned char *)pointer, &expected, (unsigned char)source, 0,
                                                      __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                observed = expected;
                break;
            }
            case 2: {
                unsigned short expected = (unsigned short)accumulator;
                swapped = __atomic_compare_exchange_n((unsigned short *)pointer, &expected, (unsigned short)source, 0,
                                                      __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                observed = expected;
                break;
            }
            case 4: {
                uint32_t expected = (uint32_t)accumulator;
                swapped = __atomic_compare_exchange_n((uint32_t *)pointer, &expected, (uint32_t)source, 0,
                                                      __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                observed = expected;
                break;
            }
            default: {
                uint64_t expected = accumulator;
                swapped = __atomic_compare_exchange_n((uint64_t *)pointer, &expected, source, 0, __ATOMIC_SEQ_CST,
                                                      __ATOMIC_SEQ_CST);
                observed = expected;
                break;
            }
            }
        } else {
            unsigned hash = (unsigned)((host_address >> 3) & (INTERP_SPLIT_LOCKS - 1));
            _Atomic unsigned *lock = &g_interp_split_lock[hash];
            while (atomic_exchange_explicit(lock, 1u, memory_order_acquire))
                ;
            observed = 0;
            interp_copy_indivisible(&observed, pointer, (unsigned)width);
            if ((observed & interp_mask(width)) == (accumulator & interp_mask(width))) {
                interp_copy_indivisible(pointer, &source, (unsigned)width);
                swapped = 1;
            }
            atomic_store_explicit(lock, 0u, memory_order_release);
        }
        interp_access_end();
        if (swapped && jit86_store_alias_observation_active())
            jit86_store_alias_changed(operand.address, (uint64_t)width);
    } else {
        observed = interp_rm_read(cpu, insn, &operand, width);
        if ((observed & interp_mask(width)) == (accumulator & interp_mask(width)))
            interp_rm_write(cpu, insn, &operand, width, source);
    }
    (void)interp_alu_sub(cpu, accumulator, observed, 0, width);
    if ((observed & interp_mask(width)) != (accumulator & interp_mask(width)))
        interp_reg_write(cpu, insn, RAX, width, observed);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_exchange(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op == 0xC3) {
        if (!insn->is_mem) return interp_undefined(cpu, insn, pc, "MOVNTI with a register destination (#UD)");
        interp_operand operand = interp_rm(cpu, insn, next);
        interp_store(operand.address, insn->opsize, interp_reg_read(cpu, insn, insn->reg, insn->opsize));
        cpu->rip = next;
        return STEP_NEXT;
    }
    if (op != 0xC0 && op != 0xC1) return -1;
    int width = (op & 1) ? insn->opsize : 1;
    interp_operand operand = interp_rm(cpu, insn, next);
    uint64_t source = interp_reg_read(cpu, insn, insn->reg, width);
    uint64_t old;
    if (operand.is_memory && insn->lock) {
        old = interp_locked_rmw(operand.address, width, RMW_ADD, source, 0);
        (void)interp_alu_add(cpu, old, source, 0, width);
        interp_reg_write(cpu, insn, insn->reg, width, old);
    } else {
        old = interp_rm_read(cpu, insn, &operand, width);
        uint64_t sum = interp_alu_add(cpu, old, source, 0, width);
        interp_reg_write(cpu, insn, insn->reg, width, old);
        interp_rm_write(cpu, insn, &operand, width, sum);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_two_byte_compare_exchange_pair(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    if (insn->op != 0xC7) return -1;
    if ((insn->reg & 7) != 1 || !insn->is_mem)
        return interp_undefined(cpu, insn, pc, "0F C7 group (RDRAND/RDSEED/VMPTRLD)");
    interp_operand operand = interp_rm(cpu, insn, next);
    if (insn->opsize == 8) {
        cpu->x87_ea = hl_x86_guest_pointer(operand.address);
        return interp_exit(cpu, next, R_CMPXCHG16);
    }
    uint64_t expected = ((cpu->r[RDX] & UINT64_C(0xffffffff)) << 32) | (cpu->r[RAX] & UINT64_C(0xffffffff));
    uint64_t desired = ((cpu->r[RCX] & UINT64_C(0xffffffff)) << 32) | (cpu->r[RBX] & UINT64_C(0xffffffff));
    uint64_t observed;
    int equal;
    if (insn->lock) {
        uint64_t host_address = hl_x86_guest_pointer(operand.address);
        uint64_t probe = expected;
        interp_access_begin(operand.address, 8);
        equal = __atomic_compare_exchange_n((uint64_t *)(uintptr_t)host_address, &probe, desired, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
        interp_access_end();
        observed = probe;
        if (equal && jit86_store_alias_observation_active()) jit86_store_alias_changed(operand.address, 8);
    } else {
        observed = interp_load(operand.address, 8);
        equal = observed == expected;
        if (equal) interp_store(operand.address, 8, desired);
    }
    if (!equal) {
        interp_reg_write(cpu, insn, RAX, 4, observed & UINT64_C(0xffffffff));
        interp_reg_write(cpu, insn, RDX, 4, observed >> 32);
    }
    if (equal)
        cpu->nzcv |= NZ_Z;
    else
        cpu->nzcv &= ~NZ_Z;
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_step_two_byte(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int delegated = interp_two_byte_condition(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_bswap(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_system(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_extend(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_shift(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_bit_modify(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_bit_count(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_compare_exchange(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_exchange(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_two_byte_compare_exchange_pair(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;

    switch (op) {
    case 0x01:
        if (insn->has_modrm && insn->modrm == 0xF9) { // RDTSCP: EDX:EAX = counter, ECX = TSC_AUX (0)
            uint64_t counter = now_ns();
            interp_reg_write(cpu, insn, RAX, 4, counter & UINT64_C(0xffffffff));
            interp_reg_write(cpu, insn, RDX, 4, counter >> 32);
            interp_reg_write(cpu, insn, RCX, 4, 0);
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (insn->has_modrm && insn->modrm == 0xD0) { // XGETBV(ecx=0): XCR0 = x87 + SSE, no AVX
            // Must match cpuid.c, which withholds AVX: disagreeing bits pick an unimplemented guest path.
            interp_reg_write(cpu, insn, RAX, 4, 3);
            interp_reg_write(cpu, insn, RDX, 4, 0);
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (insn->has_modrm && insn->modrm == 0xD5) { // XEND: no transaction is ever active -> NOP
            cpu->rip = next;
            return STEP_NEXT;
        }
        return interp_undefined(cpu, insn, pc, "0F 01 system instruction group");

    case 0xAF: {
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t source = interp_rm_read(cpu, insn, &operand, insn->opsize);
        interp_reg_write(
            cpu, insn, insn->reg, insn->opsize,
            interp_imul_truncating(cpu, interp_reg_read(cpu, insn, insn->reg, insn->opsize), source, insn->opsize));
        cpu->rip = next;
        return STEP_NEXT;
    }

    // 0F AE: fences, the FXSAVE/MXCSR group, the cache-line hints and the XSAVE family
    case 0xAE: {
        int sub = insn->reg & 7;
        // PREFIX LEGALITY, measured on silicon: 66 admits only the /6 (CLWB) and /7 (CLFLUSHOPT) memory
        // forms and #UDs every other encoding; every F3/F2 memory form #UDs (F3 /0../3 is the REGISTER-form
        // RD/WRFSBASE, which this engine does not implement either). Executing them as the unprefixed
        // encoding is how `66 0F AE /0` ran FXSAVE and wrote 512 bytes to a guest-chosen address.
        if (insn->p66 && !(insn->is_mem && sub >= 6)) return interp_guest_trap(cpu, pc, 4 /*SIGILL*/, 2);
        if ((insn->rep || insn->repne) && insn->is_mem) return interp_guest_trap(cpu, pc, 4, 2);
        if (sub >= 5 && !insn->is_mem) {
            // LFENCE/MFENCE/SFENCE: no-op under x86-TSO; interp_tso_fence is where a weak host would put
            // a real barrier, hence the call.
            interp_tso_fence();
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (insn->is_mem && (sub == 7 || (sub == 6 && insn->p66))) {
            // CLFLUSH / CLFLUSHOPT / CLWB: nothing to do in a coherent model, but they still fault on an
            // inaccessible line (measured: SIGSEGV), so touch the operand through the ordinary guarded load.
            (void)interp_load(interp_ea(cpu, insn, next), 1);
            cpu->rip = next;
            return STEP_NEXT;
        }
        if ((sub == 0 || sub == 1) && insn->is_mem) {
            // fxsave / fxrstor: the shared helper owns the layout. Arm the tag model here too -- a guest whose
            // FIRST FP instruction is FXSAVE must see an all-empty tag byte, not the disarmed 0xff.
            interp_x87_arm(cpu);
            uint64_t ea = interp_ea(cpu, insn, next);
            if (ea & 15) return interp_guest_trap(cpu, pc, 11 /*#GP*/, 128 /*SI_KERNEL*/); // 16-byte operand
            // x87state.c touches the 512 bytes from the DISPATCH loop, outside this block's fault pad, so
            // an unmapped area killed the ENGINE (measured: 139). Prove it here instead.
            uint64_t area = hl_x86_guest_pointer(ea);
            int ok = sub == 0 ? hl_x86_guest_writable(area, 512) : hl_x86_guest_readable(area, 512);
            if (!ok) return interp_softmiss(cpu, pc, area, 512, sub == 0 ? X86_SOFT_WRITE : X86_SOFT_READ);
            cpu->x87_ea = area;
            return interp_exit(cpu, next, sub == 0 ? R_FXSAVE : R_FXRSTOR);
        }
        if (sub >= 4 && sub <= 6 && insn->is_mem) {
            // XSAVE (/4), XRSTOR (/5), XSAVEOPT (/6). See the layout note above interp_xsave_legacy.
            uint64_t rfbm = (((cpu->r[RDX] & 0xffffffffu) << 32) | (cpu->r[RAX] & 0xffffffffu)) & INTERP_XCR0;
            uint64_t ea = interp_ea(cpu, insn, next);
            if (ea & 63) return interp_guest_trap(cpu, pc, 11 /*#GP*/, 128 /*SI_KERNEL*/); // 64-byte area
            uint8_t image[512];
            if (sub == 5) { // XRSTOR
                uint64_t bv, comp;
                uint8_t reserved[40];
                interp_load_bytes(ea + 512, &bv, 8);
                interp_load_bytes(ea + 520, &comp, 8);
                interp_load_bytes(ea + 528, reserved, 40);
                // #GP on a header this engine cannot honour: a compacted image (XCOMP_BV != 0 -- XSAVEC and
                // XSAVES are not advertised, so no guest can legitimately have produced one), a component
                // outside XCR0, or a nonzero reserved field. Refusing beats restoring a fiction.
                if (comp != 0 || (bv & ~INTERP_XCR0) != 0) return interp_guest_trap(cpu, pc, 11, 128);
                for (unsigned i = 0; i < sizeof reserved; i++)
                    if (reserved[i] != 0) return interp_guest_trap(cpu, pc, 11, 128);
                interp_x87_arm(cpu);
                hl_x86_xsave_legacy(cpu, image); // components outside RFBM keep their current values
                if (rfbm & 1) {
                    if (bv & 1) {
                        interp_load_bytes(ea + 0, image + 0, 24);
                        interp_load_bytes(ea + 32, image + 32, 128);
                    } else {
                        interp_xsave_init(image, 1);
                    }
                }
                if (rfbm & 2) {
                    // MXCSR is loaded from the image whenever SSE is requested, XSTATE_BV[1] or not (SDM).
                    interp_load_bytes(ea + 24, image + 24, 4);
                    if (bv & 2)
                        interp_load_bytes(ea + 160, image + 160, 256);
                    else
                        memset(image + 160, 0, 256);
                }
                uint64_t saved = cpu->x87_ea;
                cpu->x87_ea = (uint64_t)(uintptr_t)image;
                hl_x86_fxrstor(cpu);
                cpu->x87_ea = saved;
                cpu->rip = next;
                return STEP_NEXT;
            }
            // XSAVE and XSAVEOPT. XSAVEOPT's whole point is that it MAY skip a component unmodified since
            // the last XRSTOR, and MAY skip one that is at its initial configuration. Both are permissions,
            // not obligations, and this engine tracks neither modification nor init state per component --
            // so it performs the full save. That is architecturally legal and strictly conservative: it
            // writes MORE than the optimized form ever would, never less, and the XSTATE_BV it leaves is the
            // same one XSAVE leaves, so no consumer can tell the difference except by reading bytes it was
            // already required to treat as undefined. Silently equating the two mnemonics without saying so
            // is the part that would be wrong, not the behaviour.
            interp_x87_arm(cpu);
            hl_x86_xsave_legacy(cpu, image);
            if (rfbm & 1) {
                interp_store_bytes(ea + 0, image + 0, 24);
                interp_store_bytes(ea + 32, image + 32, 128);
            }
            if (rfbm & 2) {
                interp_store_bytes(ea + 24, image + 24, 8);
                interp_store_bytes(ea + 160, image + 160, 256);
            }
            uint64_t bv;
            interp_load_bytes(ea + 512, &bv, 8);
            bv = (bv & ~rfbm) | (rfbm & hl_x86_xsave_xinuse(image));
            interp_store_bytes(ea + 512, &bv, 8);
            cpu->rip = next;
            return STEP_NEXT;
        }
#if defined(HL_HOST_CPU_X86_64)
        if ((sub == 2 || sub == 3) && insn->is_mem) {
            // LDMXCSR (/2) / STMXCSR (/3). The guest MXCSR IS the host MXCSR here, so read memory BEFORE
            // writing it: a faulting operand must leave rounding mode and sticky flags as they were.
            if (sub == 3) {
                uint32_t live = _mm_getcsr();
                interp_store(interp_ea(cpu, insn, next), 4, live);
            } else {
                uint32_t loaded = (uint32_t)interp_load(interp_ea(cpu, insn, next), 4);
                // MASK it: LDMXCSR #GPs outside MXCSR_MASK, and this word is GUEST data -- unmasked it
                // kills the ENGINE where hardware faults the guest. 0xffff keeps every defined field.
                _mm_setcsr(loaded & 0xffffu);
            }
            cpu->rip = next;
            return STEP_NEXT;
        }
#endif
        // XSAVE/XRSTOR (/4,/5): no extended-state layout exists in this model.
        return interp_undefined(cpu, insn, pc, "TODO(amd64-host): XSAVE/XRSTOR (0F AE)");
    }

    default: break;
    }

    // Legacy SSE/SSE2 has no shared C emulator to route to (unlike VEX -> R_AVX, 0F38/0F3A -> R_SSE3B).
    if (interp_is_legacy_sse(op)) {
        int handled = interp_step_sse(cpu, insn, pc, next);
        if (handled != STEP_SSE_UNHANDLED) return handled;
        return interp_undefined(cpu, insn, pc, "TODO(amd64-host): legacy SSE/SSE2 (0F map)");
    }
    if (op == 0x00 || op == 0x02 || op == 0x03 || op == 0x06 || op == 0x08 || op == 0x09 || op == 0x20 || op == 0x21 ||
        op == 0x22 || op == 0x23)
        return interp_undefined(cpu, insn, pc, "privileged system instruction (LGDT/LAR/MOV CRn/MOV DRn)");
    return interp_undefined(cpu, insn, pc, "two-byte (0F) opcode");
}

// The translated-code cache: permanent MISS, no-op save -- it stores HOST CODE and this backend emits
// none. IDENTITY still matters: pcache_engine_id mixes HL_HOST_CPU_ISA, and checkpoint.c validates that
// same id on restore -- without the host-ISA term a JIT-written checkpoint would restore against nothing.

static int g_force_base_failed; // latched by the ELF loader on fixed-VA map fallback

static uint64_t x64_pcache_codegen_modes(void) {
    return (uint64_t)hl_option_flag_value("HL_TRANSLIT_JCC_LINK_DISABLE", 0) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_JCC_IBTC_DISABLE", 0) << 1) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_MIXED_SSE_DISABLE", 0) << 2) |
           ((uint64_t)(g_prof != 0) << 3) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", 1) << 4) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_FS_AUTHORITY_TEST", 0) << 5) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_PROVENANCE_FALLBACK", 0) << 6) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_BODY_OWNER_EXHAUST", 0) << 7) |
           ((uint64_t)hl_option_flag_value("HL_TRANSLIT_RIPREL_READONLY", 0) << 8);
}

static uint64_t pcache_engine_id(void) {
    uint64_t hash = 1469598103934665603ull;
    uint64_t self = hl_identity_source(&g_jit_services, g_self_path);
    for (const char *p = __DATE__ " " __TIME__; *p; p++) {
        hash ^= (uint8_t)*p;
        hash *= 1099511628211ull;
    }
    hash ^= self;
    hash *= 1099511628211ull;
    uint64_t modes = x64_pcache_codegen_modes();
    return hl_identity_configuration(hash, 2, HL_HOST_CPU_ISA, modes);
}

static hl_identity_digest pcache_translator_identity(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    uint64_t modes = x64_pcache_codegen_modes();
    return hl_identity_engine_digest(tag, sizeof tag - 1, HL_PCACHE_ABI_X86_64, 2, HL_HOST_CPU_ISA, modes);
}

static hl_identity_digest pcache_make_id(hl_identity_digest program, hl_identity_digest interpreter,
                                         const char *argv0) {
    return hl_identity_digest_mix(program, interpreter, pcache_translator_identity(), argv0);
}

/* Same-ISA cold-publication format. Warm restore deliberately remains disabled until the relocation and
 * generation reconstruction review is complete. Every address in the payload is an arena-relative offset. */
#define X64_PC_MAGIC UINT64_C(0x3143535034364c48) /* "HL64PSC1" */
#define X64_PC_VERSION UINT64_C(2)

#define X64_PC_ENDIAN UINT64_C(0x0807060504030201)
#define X64_PC_HEADER_SIZE 208u
#define X64_PC_MAP_SIZE 84u
#define X64_PC_OWNER_SIZE 24u
#define X64_PC_RELOC_SIZE 8u
#define X64_PC_CHECKSUM_OFFSET 200u

static void x64_pc_put16(uint8_t **p, uint16_t v) { (*p)[0] = (uint8_t)v; (*p)[1] = (uint8_t)(v >> 8); *p += 2; }
static void x64_pc_put32(uint8_t **p, uint32_t v) { for (unsigned i = 0; i < 4; i++) (*p)[i] = (uint8_t)(v >> (8*i)); *p += 4; }
static void x64_pc_put64(uint8_t **p, uint64_t v) { for (unsigned i = 0; i < 8; i++) (*p)[i] = (uint8_t)(v >> (8*i)); *p += 8; }
static uint32_t x64_pc_get32(const uint8_t *p) { uint32_t v = 0; for (unsigned i = 0; i < 4; i++) v |= (uint32_t)p[i] << (8*i); return v; }
static uint64_t x64_pc_get64(const uint8_t *p) { uint64_t v = 0; for (unsigned i = 0; i < 8; i++) v |= (uint64_t)p[i] << (8*i); return v; }

static hl_persist_directory g_x64_pc_directory;
static char g_x64_pc_directory_path[1024];
static int g_x64_pc_forked;

static uint64_t x64_pc_offset(const void *pointer, uint64_t used) {
    uintptr_t value = (uintptr_t)pointer, base = (uintptr_t)g_cache;
    return pointer != NULL && value >= base && value - base <= used ? (uint64_t)(value - base) : UINT64_MAX;
}

static int x64_pc_file(char *path, size_t size) {
    const char *directory = hl_option_get("HL_PCACHE_DIR");
    if (directory == NULL || directory[0] == 0) return 0;
    if (g_x64_pc_directory.handle != HL_HOST_HANDLE_INVALID && strcmp(g_x64_pc_directory_path, directory) != 0) {
        (void)hl_persist_directory_close(&g_x64_pc_directory);
        g_x64_pc_directory_path[0] = 0;
    }
    if (g_x64_pc_directory.handle == HL_HOST_HANDLE_INVALID &&
        !hl_persist_directory_open(&g_x64_pc_directory, &g_jit_services, directory, 1))
        return 0;
    if (!g_x64_pc_directory_path[0]) {
        int copied = snprintf(g_x64_pc_directory_path, sizeof g_x64_pc_directory_path, "%s", directory);
        if (copied <= 0 || (size_t)copied >= sizeof g_x64_pc_directory_path) return 0;
    }
    if (size < 76) return 0;
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < sizeof g_pc_binid.bytes; i++) {
        path[i * 2] = hex[g_pc_binid.bytes[i] >> 4];
        path[i * 2 + 1] = hex[g_pc_binid.bytes[i] & 15];
    }
    memcpy(path + 64, ".x64pcache", 11);
    return 1;
}

static int pcache_load(uint64_t entry_jump) {
    (void)entry_jump;
    g_pcache_loaded = 0;
    char path[1024];
    void *allocation = NULL;
    size_t size = 0;
    if (!x64_pc_file(path, sizeof path) ||
        !hl_persist_load_at(&g_x64_pc_directory, path, CACHE_SZ + UINT64_C(134217728), &allocation, &size))
        return 0;
    const uint8_t *bytes = allocation;
    unsigned validation = 2; /* structurally invalid/truncated */
    int valid = size >= X64_PC_HEADER_SIZE && x64_pc_get64(bytes) == X64_PC_MAGIC &&
                x64_pc_get64(bytes + 8) == X64_PC_VERSION && x64_pc_get64(bytes + 16) == X64_PC_ENDIAN &&
                x64_pc_get64(bytes + 24) == HL_PCACHE_ABI_X86_64 && x64_pc_get64(bytes + 32) == sizeof(struct cpu) &&
                x64_pc_get64(bytes + 40) == JIT_MAP_N &&
                memcmp(bytes + 48, g_pc_binid.bytes, sizeof g_pc_binid.bytes) == 0 &&
                x64_pc_get64(bytes + 192) == x64_pcache_codegen_modes();
    if (valid) {
        uint64_t arena = x64_pc_get64(bytes + 88), maps = x64_pc_get64(bytes + 96), owners = x64_pc_get64(bytes + 104);
        uint64_t relocs = x64_pc_get64(bytes + 184);
        uint64_t map_bytes = maps <= UINT64_MAX / X64_PC_MAP_SIZE ? maps * X64_PC_MAP_SIZE : UINT64_MAX;
        uint64_t owner_bytes = owners <= UINT64_MAX / X64_PC_OWNER_SIZE ? owners * X64_PC_OWNER_SIZE : UINT64_MAX;
        uint64_t reloc_bytes = relocs <= UINT64_MAX / X64_PC_RELOC_SIZE ? relocs * X64_PC_RELOC_SIZE : UINT64_MAX;
        valid = arena <= CACHE_SZ && maps <= JIT_MAP_N && owners <= JIT_BODY_OWNER_N &&
                relocs <= TL_EXTERNAL_ABSOLUTE_N && map_bytes != UINT64_MAX && reloc_bytes != UINT64_MAX &&
                owner_bytes != UINT64_MAX && X64_PC_HEADER_SIZE + map_bytes <= SIZE_MAX - owner_bytes &&
                X64_PC_HEADER_SIZE + map_bytes + owner_bytes <= SIZE_MAX - reloc_bytes &&
                X64_PC_HEADER_SIZE + map_bytes + owner_bytes + reloc_bytes <= SIZE_MAX - arena &&
                size == X64_PC_HEADER_SIZE + map_bytes + owner_bytes + reloc_bytes + arena;
        for (unsigned offset = 120; valid && offset <= 176; offset += 8) {
            uint64_t helper = x64_pc_get64(bytes + offset);
            valid = helper == UINT64_MAX || helper <= arena;
        }
    }
    if (valid) {
        static const uint8_t zero[8];
        hl_digest digest;
        hl_digest_init(&digest, HL_DIGEST_SEED);
        hl_digest_update(&digest, bytes, X64_PC_CHECKSUM_OFFSET);
        hl_digest_update(&digest, zero, sizeof zero);
        hl_digest_update(&digest, bytes + X64_PC_HEADER_SIZE, size - X64_PC_HEADER_SIZE);
        valid = x64_pc_get64(bytes + X64_PC_CHECKSUM_OFFSET) == hl_digest_value(&digest);
        validation = valid ? 1 : 3; /* authenticated or checksum mismatch */
    }
    if (valid) {
        uint64_t arena = x64_pc_get64(bytes + 88), maps = x64_pc_get64(bytes + 96), owners = x64_pc_get64(bytes + 104);
        uint64_t relocs = x64_pc_get64(bytes + 184);
        const uint8_t *record = bytes + X64_PC_HEADER_SIZE;
        for (uint64_t i = 0; valid && i < maps; i++, record += X64_PC_MAP_SIZE) {
            uint64_t host = x64_pc_get64(record + 24), body = x64_pc_get64(record + 32);
            uint64_t block = x64_pc_get64(record + 40);
            uint32_t entry = x64_pc_get32(record + 72), length = x64_pc_get32(record + 76);
            valid = host <= arena && body <= arena && block == body && entry <= length && entry <= arena && host <= arena - entry &&
                    length <= arena - host;
        }
        record = bytes + X64_PC_HEADER_SIZE + maps * X64_PC_MAP_SIZE;
        for (uint64_t i = 0; valid && i < owners; i++, record += X64_PC_OWNER_SIZE) {
            uint32_t start = x64_pc_get32(record), end = x64_pc_get32(record + 4);
            valid = start <= end && end <= arena;
        }
        record = bytes + X64_PC_HEADER_SIZE + maps * X64_PC_MAP_SIZE + owners * X64_PC_OWNER_SIZE;
        const uint8_t *arena_bytes = record + relocs * X64_PC_RELOC_SIZE;
        uint32_t prior = 0;
        for (uint64_t i = 0; valid && i < relocs; i++, record += X64_PC_RELOC_SIZE) {
            uint32_t offset = x64_pc_get32(record), kind = x64_pc_get32(record + 4);
            valid = offset <= arena && arena - offset >= 8 && translit_external_absolute_kind_valid(kind) &&
                    (i == 0 || offset > prior) && offset >= 2 && arena_bytes[offset - 2] == 0x48 &&
                    arena_bytes[offset - 1] == 0xb8;
            prior = offset;
        }
        if (!valid) validation = 2;
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    char receipt[1024];
    int receipt_length = snprintf(receipt, sizeof receipt, "%s.%s", path,
                                  validation == 1 ? "valid" : validation == 3 ? "checksum-invalid" : "length-invalid");
    if (receipt_length > 0 && (size_t)receipt_length < sizeof receipt)
        (void)hl_persist_store_at(&g_x64_pc_directory, receipt, &validation, sizeof validation);
#endif
    free(allocation);
    (void)valid; /* Validating is deliberately not restoring. */
    return 0; // MISS: the dispatcher translates fresh
}

static void pcache_save(void) {
    if (!g_pcache || g_prof || hl_identity_digest_empty(&g_pc_binid) || g_cp == g_cache || g_force_base_failed ||
        !jit_guest_bus_active())
        return;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_x64_pc_forked) {
        char base[1024], receipt[1024];
        if (x64_pc_file(base, sizeof base)) {
            int length = snprintf(receipt, sizeof receipt, "%s.fork-refused-%lld", base, (long long)getpid());
            static const unsigned refused = 1;
            if (length > 0 && (size_t)length < sizeof receipt)
                (void)hl_persist_store_at(&g_x64_pc_directory, receipt, &refused, sizeof refused);
        }
        return;
    }
#else
    if (g_x64_pc_forked) return;
#endif
    uint64_t used = (uint64_t)(g_cp - g_cache);
    /* Failed speculative builders may have emitted into the tail before rewinding g_cp.
       Compact those dead records; a live unledgered emission still latches refusal above. */
    uint32_t live_relocs = 0;
    for (uint32_t i = 0; i < translit_external_absolute_count; i++) {
        translit_external_absolute relocation = translit_external_absolutes[i];
        if (relocation.offset > used || used - relocation.offset < 8 || relocation.offset < 2 ||
            g_cache[relocation.offset - 2] != 0x48 || g_cache[relocation.offset - 1] != 0xb8)
            continue;
        uint64_t address;
        memcpy(&address, g_cache + relocation.offset, sizeof address);
        if (translit_external_absolute_kind_for((uintptr_t)address) != relocation.kind) continue;
        translit_external_absolutes[live_relocs++] = relocation;
    }
    translit_external_absolute_count = live_relocs;
    translit_external_absolute_emitted = live_relocs;
    for (uint32_t i = 1; i < live_relocs; i++) {
        translit_external_absolute relocation = translit_external_absolutes[i];
        uint32_t j = i;
        while (j != 0 && translit_external_absolutes[j - 1].offset > relocation.offset) {
            translit_external_absolutes[j] = translit_external_absolutes[j - 1];
            j--;
        }
        translit_external_absolutes[j] = relocation;
    }
    if (translit_external_absolute_unclassified ||
        translit_external_absolute_count != translit_external_absolute_emitted) {
        HL_LOGF(&g_jit_log, HL_LOG_TAG_TRANSLATE,
                "pcache=refused reason=external-absolute-census emitted=%u recorded=%u unclassified=%d",
                translit_external_absolute_emitted, translit_external_absolute_count,
                translit_external_absolute_unclassified);
#if defined(HL_NATIVE_TEST_HOOKS)
        char base[1024], receipt[1024];
        if (x64_pc_file(base, sizeof base)) {
            int length = snprintf(receipt, sizeof receipt, "%s.relocation-refused-%lld", base, (long long)getpid());
            static const unsigned refused = 1;
            if (length > 0 && (size_t)length < sizeof receipt)
                (void)hl_persist_store_at(&g_x64_pc_directory, receipt, &refused, sizeof refused);
        }
#endif
        return;
    }
    uint32_t prior_reloc = 0;
    for (uint32_t i = 0; i < translit_external_absolute_count; i++) {
        uint32_t offset = translit_external_absolutes[i].offset;
        uint64_t address = 0;
        if (offset > used || used - offset < sizeof address || offset < 2 ||
            (i != 0 && offset <= prior_reloc) || g_cache[offset - 2] != 0x48 || g_cache[offset - 1] != 0xb8) {
            translit_external_absolute_unclassified = 1;
            return;
        }
        memcpy(&address, g_cache + offset, sizeof address);
        if (translit_external_absolute_kind_for((uintptr_t)address) != translit_external_absolutes[i].kind) {
            translit_external_absolute_unclassified = 1;
            return;
        }
        prior_reloc = offset;
    }
    uint64_t map_count = 0;
    for (uint32_t i = 0; i < JIT_MAP_N; i++)
        if (map_live(i) && g_map_metadata[i].cache_generation == g_cache_gen) map_count++;
    jit_body_owner_set *owners = jit_body_owner_set_for(g_cache_gen, 0);
    jit_body_owner_entry *owner_entries =
        owners == NULL ? NULL : atomic_load_explicit(&owners->entry, memory_order_acquire);
    uint32_t owner_count = owners == NULL ? 0 : atomic_load_explicit(&owners->count, memory_order_acquire);
    if ((owner_count != 0 && owner_entries == NULL) || map_count > SIZE_MAX / X64_PC_MAP_SIZE ||
        owner_count > SIZE_MAX / X64_PC_OWNER_SIZE || translit_external_absolute_count > SIZE_MAX / X64_PC_RELOC_SIZE)
        return;
    size_t map_bytes = (size_t)map_count * X64_PC_MAP_SIZE;
    size_t owner_bytes = (size_t)owner_count * X64_PC_OWNER_SIZE;
    size_t reloc_bytes = (size_t)translit_external_absolute_count * X64_PC_RELOC_SIZE;
    if (used > SIZE_MAX - X64_PC_HEADER_SIZE - map_bytes - owner_bytes - reloc_bytes) return;
    size_t total = X64_PC_HEADER_SIZE + map_bytes + owner_bytes + reloc_bytes + (size_t)used;
    uint8_t *buffer = calloc(1, total);
    if (buffer == NULL) return;
    uint8_t *cursor = buffer;
    x64_pc_put64(&cursor, X64_PC_MAGIC); x64_pc_put64(&cursor, X64_PC_VERSION); x64_pc_put64(&cursor, X64_PC_ENDIAN);
    x64_pc_put64(&cursor, HL_PCACHE_ABI_X86_64); x64_pc_put64(&cursor, sizeof(struct cpu)); x64_pc_put64(&cursor, JIT_MAP_N);
    memcpy(cursor, g_pc_binid.bytes, sizeof g_pc_binid.bytes); cursor += sizeof g_pc_binid.bytes;
    x64_pc_put64(&cursor, g_pc_entry); x64_pc_put64(&cursor, used); x64_pc_put64(&cursor, map_count);
    x64_pc_put64(&cursor, owner_count); x64_pc_put64(&cursor, g_source_index_overflow);
    x64_pc_put64(&cursor, x64_pc_offset(translit_jcc_ibtc_stub_entry, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_jcc_ibtc_stub_rsp_canonical, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_jcc_ibtc_stub_flags_canonical, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_jcc_ibtc_stub_end, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_direct_jmp_ibtc_stub_entry, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_direct_jmp_ibtc_stub_rsp_canonical, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_direct_jmp_ibtc_stub_flags_canonical, used));
    x64_pc_put64(&cursor, x64_pc_offset(translit_direct_jmp_ibtc_stub_end, used));
    x64_pc_put64(&cursor, translit_external_absolute_count);
    x64_pc_put64(&cursor, x64_pcache_codegen_modes());
    x64_pc_put64(&cursor, 0);
    for (uint32_t i = 0; i < JIT_MAP_N; i++) {
        if (!map_live(i) || g_map_metadata[i].cache_generation != g_cache_gen) continue;
        uint64_t host = x64_pc_offset(g_map[i].host, used), body = x64_pc_offset(g_map[i].body, used);
        if (host == UINT64_MAX || body == UINT64_MAX) {
            free(buffer);
            return;
        }
        struct interp_block *block = (struct interp_block *)(g_cache + body);
        x64_pc_put64(&cursor, g_map[i].gpc); x64_pc_put64(&cursor, g_map_metadata[i].guest_start);
        x64_pc_put64(&cursor, g_map_metadata[i].guest_end); x64_pc_put64(&cursor, host); x64_pc_put64(&cursor, body);
        x64_pc_put64(&cursor, body); x64_pc_put64(&cursor, block->magic); x64_pc_put64(&cursor, block->gpc);
        x64_pc_put64(&cursor, block->generation); x64_pc_put32(&cursor, block->host_entry_off);
        x64_pc_put32(&cursor, block->host_len); x64_pc_put16(&cursor, block->profile_insns); x64_pc_put16(&cursor, 0);
    }
    jit_body_owner_preserve *preserves = owner_entries == NULL ? NULL : jit_body_owner_preserves(owner_entries);
    for (uint32_t i = 0; i < owner_count; i++) {
        x64_pc_put32(&cursor, owner_entries[i].rw_start); x64_pc_put32(&cursor, owner_entries[i].rw_end);
        x64_pc_put32(&cursor, preserves[i]); x64_pc_put32(&cursor, 0); x64_pc_put64(&cursor, owner_entries[i].guest);
    }
    for (uint32_t i = 0; i < translit_external_absolute_count; i++) {
        x64_pc_put32(&cursor, translit_external_absolutes[i].offset);
        x64_pc_put32(&cursor, translit_external_absolutes[i].kind);
    }
    memcpy(cursor, g_cache, (size_t)used);
    hl_digest digest;
    hl_digest_init(&digest, HL_DIGEST_SEED);
    hl_digest_update(&digest, buffer, total);
    cursor = buffer + X64_PC_CHECKSUM_OFFSET;
    x64_pc_put64(&cursor, hl_digest_value(&digest));
    char path[1024];
    if (x64_pc_file(path, sizeof path)) {
        int stored = hl_persist_store_at(&g_x64_pc_directory, path, buffer, total);
#if defined(HL_NATIVE_TEST_HOOKS)
        if (stored) {
            char receipt[1024];
            int length = snprintf(receipt, sizeof receipt, "%s.published-%lld", path, (long long)getpid());
            if (length > 0 && (size_t)length < sizeof receipt)
                (void)hl_persist_store_at(&g_x64_pc_directory, receipt, &stored, sizeof stored);
            const char *fresh = hl_option_get("HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST");
            if (fresh != NULL && fresh[0] != '0' && fresh[0] != 0 &&
                translit_test_external_absolute_nonempty_resets != 0 &&
                translit_external_absolute_count == translit_external_absolute_emitted &&
                !translit_external_absolute_unclassified) {
                length = snprintf(receipt, sizeof receipt, "%s.relocation-rollover-exact-%lld", path,
                                  (long long)getpid());
                if (length > 0 && (size_t)length < sizeof receipt)
                    (void)hl_persist_store_at(&g_x64_pc_directory, receipt,
                                              &translit_external_absolute_count,
                                              sizeof translit_external_absolute_count);
            }
        }
#endif
    }
    free(buffer);
}

static void x64_pc_after_fork(void) {
    g_x64_pc_forked = 1;
}

static void pcache_exec_force_main(void) {
    if (g_pcache) g_force_base = PC_IMG_BASE;
}

static void pcache_exec_force_interp(void) {
    if (g_pcache) g_force_base = PC_INTERP_BASE;
}

/* proc.c invokes this only after the exec path has flushed the inherited arena and loaded the new image.
 * That ordering is what makes clearing the fork refusal safe: the new identity cannot describe parent code. */
static void pcache_exec_reload(hl_identity_digest program, hl_identity_digest interpreter, const char *argv0,
                               uint64_t jump) {
    if (!g_pcache) return;
    g_pc_binid = pcache_make_id(program, interpreter, argv0);
    g_pc_entry = jump;
    g_x64_pc_forked = 0;
    g_pcache_loaded = 0;
    (void)pcache_load(jump);
}

#define PCACHE_SAVE_HOOK pcache_save()
#define PCACHE_FORK_HOOK x64_pc_after_fork()
#define PCACHE_EXEC_HOOKS 1

static void pcache_directory_close(void) {
    if (g_x64_pc_directory.handle != HL_HOST_HANDLE_INVALID)
        (void)hl_persist_directory_close(&g_x64_pc_directory);
    g_x64_pc_directory_path[0] = 0;
}

static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    // Nothing is revivable here, so no spans to record.
    (void)base;
    (void)span;
}
