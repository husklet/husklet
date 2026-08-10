// Engine host<->guest boundary: entry trampoline + run_guest() dispatcher loop.

#include "target/bus.h"

static int bus_activate(void *p) {
    (void)p;
    int ok = stw_force_dispatch_flush();
#ifdef PCACHE_FLUSH_HOOK
    // A guest-bus activation forces a flush-to-fresh (jit_flush_to_fresh): the arena is rotated to a new
    // generation and every prior translation is discarded. The persistent-cache reloc bookkeeping described
    // the OLD arena, so it must be reset in lockstep here -- exactly as the cache-full wholesale flush does
    // in the dispatch loop below. Without this, stale reloc offsets survive into the fresh arena and a later
    // pcache_save() persists offsets outside the live arena, so every warm load fails the bounds check (the
    // feature silently never loads); worse, a passing bounds check would relocate writes over live code.
    if (ok) PCACHE_FLUSH_HOOK;
#endif
    return ok;
}

static void bus_begin(void *p) {
    (void)p;
    stw_mapping_begin();
}

static void bus_end(void *p) {
    (void)p;
    stw_mapping_end();
}

static const hl_guest_bus_ops bus_ops = {bus_activate, bus_begin, bus_end};
static hl_target_bus g_target_bus;
static _Atomic int g_guest_soft_active;

static int jit_guest_soft_activate(void) {
    int expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&g_guest_soft_active, &expected, 1, memory_order_acq_rel,
                                                 memory_order_acquire))
        return 1;
    /* Publish the translation gate before flushing: every block admitted
       after the synchronous rotation contains soft guards. */
    if (bus_activate(NULL)) return 1;
    atomic_store_explicit(&g_guest_soft_active, 0, memory_order_release);
    return 0;
}

/* Restore runs before any saved guest thread can execute and, for the init
 * process, before a dispatcher CPU is registered.  There is therefore no old
 * translation to flush; publish the gate directly so the first restored block
 * is compiled with logical-memory guards. */
static void jit_guest_soft_restore_activate(void) {
    atomic_store_explicit(&g_guest_soft_active, 1, memory_order_release);
}

static void jit_guest_soft_restore_deactivate(void) {
    atomic_store_explicit(&g_guest_soft_active, 0, memory_order_release);
}

static void jit_guest_soft_deactivate(void) {
    int expected = 1;
    if (!atomic_compare_exchange_strong_explicit(&g_guest_soft_active, &expected, 0, memory_order_acq_rel,
                                                 memory_order_acquire))
        return;
    /* Existing guarded blocks remain correct while the flush runs; misses
       resolve identity after the logical snapshot became empty. */
    (void)bus_activate(NULL);
}

static int jit_guest_soft_active(void) {
    return atomic_load_explicit(&g_guest_soft_active, memory_order_acquire);
}

void jit_guest_bus_changed(void *opaque, uint64_t generation, int active) {
    (void)opaque;
    hl_target_bus_changed(&g_target_bus, generation, active);
}

void jit_guest_bus_bind(hl_guest_bus_query query, int active, uint64_t generation) {
    if (g_target_bus.guest.ops == NULL) hl_target_bus_init(&g_target_bus, &bus_ops, NULL);
    hl_target_bus_bind(&g_target_bus, query, active, generation);
}

int jit_guest_bus_active(void) {
    return hl_target_bus_active(&g_target_bus);
}

uint64_t jit_guest_bus_fault(uint64_t address, uint64_t size) {
    return hl_target_bus_fault(&g_target_bus, address, size);
}

void jit_guest_bus_transition_begin(void *opaque) {
    (void)opaque;
    hl_target_bus_begin(&g_target_bus);
}

void jit_guest_bus_transition_end(void *opaque) {
    (void)opaque;
    hl_target_bus_end(&g_target_bus);
}

// ---------------- host entry trampoline ----------------
// run_block(cpu, code): save host callee-saved into cpu, set env=x28, jump to code.
// The block tail-calls block_return, which restores host state and returns here's
// caller (the dispatcher).
//
// Per-arch trampolines: aarch64 enters block_return with cpu in x0 (all 31 GPRs are guest regs) and
// saves at offsets #288..#376 (q8..q15 @#896, host_sp@#280). x86 has only 16 guest GPRs, pins cpu in
// x28 for the whole block, and saves at different offsets -- so the x86 frontend supplies its OWN
// run_block/block_return (frontend/x86_64/translate.c, included before this file) and defines
// G_OWN_TRAMPOLINES to suppress these aarch64 ones. (engine-dedup §B.1/§B.3: the register model is the
// one irreducible divergence; the shared loop only CALLS run_block, never bakes its offsets.)
//
// Hand-written ARM64: the HOST-CPU axis. HL_HOST_CPU_AARCH64 (src/host/host_cpu.h) decides whether an ARM64
// boundary exists; only inside it does the compiler pick the spelling. One combined `__aarch64__` test handed
// ARM64 mnemonics to the x86 assembler. guest/aarch64/{stubs,cache}.c and guest/x86_64/emit.c must match this
// pair exactly.
#ifndef G_OWN_TRAMPOLINES
#include "../host/host_cpu.h"
#if defined(HL_HOST_CPU_AARCH64)
#if defined(__GNUC__) && !defined(__clang__)
/* GCC has no AArch64 naked-function implementation.  Assembly symbols keep
   the host boundary free of a compiler-generated frame. */
extern void run_block(struct cpu *cpu, void *code) __attribute__((visibility("hidden")));
extern void block_return(void) __attribute__((visibility("hidden")));
__asm__(".pushsection .text\n.p2align 2\n.hidden run_block\n.type run_block,%function\nrun_block:\n"
        "str x19,[x0,#288]\nstr x20,[x0,#296]\nstr x21,[x0,#304]\nstr x22,[x0,#312]\n"
        "str x23,[x0,#320]\nstr x24,[x0,#328]\nstr x25,[x0,#336]\nstr x26,[x0,#344]\n"
        "str x27,[x0,#352]\nstr x28,[x0,#360]\nstr x29,[x0,#368]\nstr x30,[x0,#376]\n"
        "str q8,[x0,#896]\nstr q9,[x0,#912]\nstr q10,[x0,#928]\nstr q11,[x0,#944]\n"
        "str q12,[x0,#960]\nstr q13,[x0,#976]\nstr q14,[x0,#992]\nstr q15,[x0,#1008]\n"
        "mov x9,sp\nstr x9,[x0,#280]\nbr x1\n.size run_block,.-run_block\n"
        ".p2align 2\n.hidden block_return\n.type block_return,%function\nblock_return:\n"
        "ldr x19,[x0,#288]\nldr x20,[x0,#296]\nldr x21,[x0,#304]\nldr x22,[x0,#312]\n"
        "ldr x23,[x0,#320]\nldr x24,[x0,#328]\nldr x25,[x0,#336]\nldr x26,[x0,#344]\n"
        "ldr x27,[x0,#352]\nldr x28,[x0,#360]\nldr x29,[x0,#368]\nldr x30,[x0,#376]\n"
        "ldr q8,[x0,#896]\nldr q9,[x0,#912]\nldr q10,[x0,#928]\nldr q11,[x0,#944]\n"
        "ldr q12,[x0,#960]\nldr q13,[x0,#976]\nldr q14,[x0,#992]\nldr q15,[x0,#1008]\n"
        "ldr x9,[x0,#280]\nmov sp,x9\nret\n.size block_return,.-block_return\n.popsection\n");
#else
__attribute__((naked)) static void run_block(struct cpu *cpu, void *code) {
    // x0=cpu, x1=code
    __asm__ volatile("str x19, [x0, #288]\n str x20, [x0, #296]\n"
                     "str x21, [x0, #304]\n str x22, [x0, #312]\n"
                     "str x23, [x0, #320]\n str x24, [x0, #328]\n"
                     "str x25, [x0, #336]\n str x26, [x0, #344]\n"
                     "str x27, [x0, #352]\n str x28, [x0, #360]\n"
                     "str x29, [x0, #368]\n str x30, [x0, #376]\n"
                     "str q8, [x0, #896]\n str q9, [x0, #912]\n str q10, [x0, #928]\n str q11, [x0, #944]\n"
                     "str q12, [x0, #960]\n str q13, [x0, #976]\n str q14, [x0, #992]\n str q15, [x0, #1008]\n"
                     // host_sp
                     "mov x9, sp\n str x9, [x0, #280]\n"
                     // x0=cpu -> emitted prologue
                     "br x1\n");
}

__attribute__((naked)) static void block_return(void) {
    // x0 == &cpu
    __asm__ volatile("ldr x19, [x0, #288]\n ldr x20, [x0, #296]\n"
                     "ldr x21, [x0, #304]\n ldr x22, [x0, #312]\n"
                     "ldr x23, [x0, #320]\n ldr x24, [x0, #328]\n"
                     "ldr x25, [x0, #336]\n ldr x26, [x0, #344]\n"
                     "ldr x27, [x0, #352]\n ldr x28, [x0, #360]\n"
                     "ldr x29, [x0, #368]\n ldr x30, [x0, #376]\n"
                     "ldr q8, [x0, #896]\n ldr q9, [x0, #912]\n ldr q10, [x0, #928]\n ldr q11, [x0, #944]\n"
                     "ldr q12, [x0, #960]\n ldr q13, [x0, #976]\n ldr q14, [x0, #992]\n ldr q15, [x0, #1008]\n"
                     // host sp
                     "ldr x9, [x0, #280]\n mov sp, x9\n"
                     "ret\n");
}
#endif
#else
// An unsupported host boundary cannot unwind through generated code.
static void run_block(struct cpu *cpu, void *code) {
    (void)cpu;
    (void)code;
    (void)jit_fail(HL_STATUS_NOT_SUPPORTED, "host execution backend unavailable",
                   sizeof("host execution backend unavailable") - 1u);
    abort();
}

static void block_return(void) {
    (void)jit_fail(HL_STATUS_CORRUPT, "invalid generated-code return", sizeof("invalid generated-code return") - 1u);
    abort();
}
#endif
#endif // G_OWN_TRAMPOLINES

// ---------------- dispatch seam defaults ----------------
// Hooks the aarch64 guest does NOT define in translator/guest/aarch64/dispatch.h (the seams added for
// engine-dedup PR3/PR4 + opts committed after the design). Their #ifndef defaults below reproduce the
// EXACT aarch64-inline behavior, so the aarch64 engine stays bit-identical; the x86 frontend overrides
// each in translator/guest/x86_64/dispatch.h. (The four PR2 seams -- G_DISPATCH_DEBUG / G_SHADOW_CLEAR /
// G_IBTC_FILL / G_DISPATCH_REASON -- are defined by BOTH frontends, so they need no default here.)

// One-time per-thread setup before the loop. aarch64 has none.
#ifndef G_DISPATCH_ENTER
#define G_DISPATCH_ENTER(c) ((void)0)
#endif
// Post-translate chaining. aarch64 chains in the dispatcher (here); x86 chains inside translate_block.
#ifndef G_DISPATCH_CHAIN
#define G_DISPATCH_CHAIN(c)                                                                                            \
    do {                                                                                                               \
        if (!smc_seen()) patch_links_to(G_PC(c), map_body(G_PC(c)));                                                   \
    } while (0)
#endif
// Post-translate per-arch step. aarch64 has none; x86 does W6A SMC source-page write-protect.
#ifndef G_AFTER_TRANSLATE
#define G_AFTER_TRANSLATE(c) ((void)0)
#endif
#ifndef G_TRACE_DUMP
#define G_TRACE_DUMP(c) ((void)(c))
#endif

// ---------------- dispatcher ----------------
static void run_guest(struct cpu *c) {
    // this thread's cpu, for emitted block exits
    pthread_setspecific(g_cpu_key, c);
    // Join the stop-the-world thread registry so a peer's cache-full flush can quiesce us at a safepoint
    // (and so we are enumerated when WE flush). Unregistered after the loop -> an exited thread is never
    // signalled.
    stw_register(c);
    // Join the tid->thread registry so a tkill()/tgkill() to this thread can find it (thread-directed
    // signal delivery via cpu->tpending); left at loop exit so a dead thread is never targeted.
    thread_register(c);
    // Frontend hook: one-time per-thread entry setup (x86 publishes the 2-way IBTC base; empty on aarch64).
    G_DISPATCH_ENTER(c);
    // a per-thread alternate signal stack so a guest stack overflow's guard fault can be delivered
    // even when the (aarch64) host SP == the exhausted guest SP. No-op reservation on x86 (host SP differs).
    install_host_sigaltstack();
    while (!c->exited) {
        if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
            c->exit_code = 70;
            c->exited = 1;
            break;
        }
        // reset the async-interrupt poll each dispatcher iteration. The emitted body check sets us
        // here when cpu->irq is seen; delivery happens at the bottom of the loop (maybe_deliver_signal).
        // Clearing here is what stops a masked-but-pending signal (which stays in g_pending, undelivered)
        // from bouncing a hot loop out of the code cache every iteration -- a fresh signal simply re-sets
        // irq (host_sigh / thread-directed path) and the next body check catches it.
        c->irq = 0;
        // A checkpoint freezes the registry while holding g_jit_lock. Peers must acknowledge and park at
        // this already-spilled dispatcher boundary BEFORE cache lookup attempts to acquire that same lock.
        // The threaded gate is also the precise boundary needed by mapping activation; single-threaded runs
        // retain their zero-overhead path.
        if (g_threaded) stw_dispatch_safepoint();
#ifdef G_CKPT_POLL
        // Checkpoint safepoint: all guest architectural state is spilled into `c` here, so a pending
        // control-triggered checkpoint (SIGUSR1) writes a coherent snapshot and _exit()s. Defined only by
        // the aarch64 target; the x86 TU never defines it, so its dispatcher compiles byte-identically.
        G_CKPT_POLL(c);
#endif
        if (G_PC(c) == SIGRETURN_PC) {
            sigreturn_frame(c); // do_sigreturn + the non-PIE frame fold (linux_abi/signal.c)
            // A handler just returned: release exactly ITS deferred set (the signals that were pending when it
            // was entered) so they become deliverable again, then immediately deliver the next still-pending
            // signal BEFORE resuming the interrupted context -- a batch of signals unblocked together runs
            // back-to-back in priority order like the kernel drains them at one return point, rather than
            // letting the main code make progress between handlers. (maybe_deliver_signal's SP-unwind check is
            // the backstop for a handler that leaves via siglongjmp instead of rt_sigreturn.)
            if (c->sig_depth > 0) {
                c->sig_depth--;
                c->sig_defer = c->sig_depth > 0 ? c->sig_defer_stack[c->sig_depth] : 0;
            }
            maybe_deliver_signal(c);
            continue;
            // handler returned -> restore context
        }
        // A non-PIE image's un-relocated absolute jump lands on its (unmapped) low link vaddr; redirect it
        // into the biased image so we translate real code instead of faulting on the unmapped low address.
        // (The one place the folded value is deliberately CARRIED rather than used and dropped: the running
        // PC stays high, and pcrel_base/lea un-bias it again whenever an address is materialised.)
        G_PC(c) = nonpie_fold(G_PC(c));
        // Frontend hook: top-of-loop debug instrumentation (x86-only; empty on aarch64).
        G_DISPATCH_DEBUG(c);
        // With threads, the WHOLE cache lookup is under the lock: an unlocked
        // map_host() races map_put() (torn entry) and lacks the acquire barrier
        // that makes a peer thread's freshly-emitted+icache-flushed code visible.
        // Single-threaded skips the lock entirely (g_threaded == 0).
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        void *code = map_host(G_PC(c));
        if (!code) {
            uint64_t _t0 = g_dispatch_profile.enabled ? hl_dispatch_profile_begin(&g_dispatch_profile, now_ns()) : 0;
            // near full -> wholesale flush
            if (g_cp + CACHE_EMIT_HEADROOM > g_cache + CACHE_SZ) {
                if (g_threaded && stw_peers_live()) {
                    // More than one guest thread is live: reusing the arena in place could free code out
                    // from under a peer mid-block. Stop the world and switch to a fresh cache instead
                    // (the old one is retained until peers drift off it). See jit/cache.c.
                    if (!stw_flush()) {
                        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                        c->exit_code = 70;
                        c->exited = 1;
                        continue;
                    }
                } else {
                    // Single-threaded (or every spawned peer has exited): safe wholesale in-place flush.
                    if (!jit_wprot(0)) {
                        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                        c->exit_code = 70;
                        c->exited = 1;
                        continue;
                    }
                    g_cp = g_cache;
                    /* Map visibility is generation-tagged; clearing only the payload leaves
                       old generation slots live and publishes zeroed translation records. */
                    map_clear();
                    pend_reset();
                    // IBTC bodies point into the cache we just dropped
                    memset(g_ibtc, 0, sizeof g_ibtc);
                    // §B: shadow host_rets point into the dropped cache too -> clear (frontend hook)
                    G_SHADOW_CLEAR(c);
                    if (!jit_wprot(1)) {
                        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                        c->exit_code = 70;
                        c->exited = 1;
                        continue;
                    }
                }
#ifdef PCACHE_FLUSH_HOOK
                // the reloc records described the arena we just dropped/renewed; reset so the
                // records stay in lockstep with what is actually emitted (a later save must never
                // relocate offsets into content that no longer matches).
                PCACHE_FLUSH_HOOK;
#endif
            }
            if (!jit_wprot(0)) {
                if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                c->exit_code = 70;
                c->exited = 1;
                continue;
            }
            // A3 §B-off: align each new block ENTRY to 16B. §B-off shrinks the per-bl stubs, which
            // shifts where hot loops land in the cache and can deterministically de-align a NEON loop
            // (e.g. sha256, which has no hot returns yet wobbled ~7%). Padding lives BEFORE the entry
            // (branch/IBTC targets the aligned body), so the nops never execute -> zero runtime cost,
            // just stable layout. Gated on §B-off so NOSHADOWTUNE=1 stays byte-identical to baseline.
            if (G_BLOCK_ALIGN)
                while ((uintptr_t)g_cp & 15)
                    emit32(0xD503201Fu); // nop
            g_emit_start = g_cp;
            code = translate_block(G_PC(c));
            hl_dispatch_profile_translation(&g_dispatch_profile);
            // new block coherent on all cores FIRST (icache is on the RX alias under dual map)
            if (!jit_publish_code(J_RX(g_emit_start), (size_t)(g_cp - g_emit_start))) {
                (void)jit_wprot(1);
                if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                c->exit_code = 70;
                c->exited = 1;
                continue;
            }
            // THEN chain existing blocks to it (still write mode). Frontend hook: aarch64 chains here;
            // x86's translate_block already chained internally, so its hook is a no-op.
            // Debug-only no-chain mode: every block re-enters the dispatcher and the
            // JT trace records every execution (exact per-block PC attribution). Correct but slow.
            if (!g_dbg_nochain) G_DISPATCH_CHAIN(c);
            if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
                (void)jit_wprot(1);
                if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                c->exit_code = 70;
                c->exited = 1;
                continue;
            }
            // back to execute AFTER all cache writes
            if (!jit_wprot(1)) {
                if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
                c->exit_code = 70;
                c->exited = 1;
                continue;
            }
            // Frontend hook: post-translate per-arch step (x86 W6A SMC source-page write-protect; empty aarch64).
            G_AFTER_TRANSLATE(c);
            if (g_dispatch_profile.enabled) hl_dispatch_profile_translation_end(&g_dispatch_profile, _t0, now_ns());
        }
        // IBTC: insert the indirect target that just missed (frontend hook -- per-arch IBTC contract:
        // aarch64 keys on ic_site/body-8/per-site IC literals; x86 will key on ic_miss/plain body).
        G_IBTC_FILL(c);
        if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
            if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
            c->exit_code = 70;
            c->exited = 1;
            continue;
        }
        // Resolve the RX alias to execute through WHILE STILL HOLDING the lock: a concurrent stop-the-world
        // flush may swap g_rw2rx (and g_cache) the instant we drop it, yet `code` is an address in the cache
        // that was current under the lock -- so J_RX(code) must use the matching g_rw2rx. (Single-threaded
        // takes no lock and cannot race a flush; the computation is identical.)
        void *rxcode;
        uint64_t selected_cache_gen;
        if (!jit_resolve_rw_code(code, &rxcode, &selected_cache_gen)) {
            if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
            c->exit_code = 70;
            c->exited = 1;
            continue;
        }
        uint64_t selected_bus_epoch = atomic_load_explicit(&g_dispatch_request, memory_order_acquire);
        // Publish the generation of the cache we are about to execute so a peer's stop-the-world flush can
        // reclaim a retired cache only once no thread is still running in it (see reclaim_retired). Done
        // under g_jit_lock (a flush holds it) so the value is consistent with g_cache_gen; threaded-only,
        // so the single-thread hot path stays zero-overhead.
        if (g_threaded) atomic_store_explicit(g_my_exec_gen, selected_cache_gen, memory_order_relaxed);
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        // Frontend hook: per-block JT trace dump (per-arch register/flag layout). See §A.3 (5th divergence).
        G_TRACE_DUMP(c);
        c->reason = 0;
        hl_dispatch_profile_crossing(&g_dispatch_profile);
        if (!stw_before_translated(selected_bus_epoch)) continue;
        // map_host()/translate_block() return RW-alias addresses; execute via the RX alias.
        run_block(c, rxcode);
        // The translated image is fully spilled at block return. Release STW ownership before reason handling:
        // clone and mapping syscalls may themselves initiate a stop-the-world operation and must not wait on
        // their own caller. Checkpoint capture still occurs at the next loop-top dispatcher safepoint.
        stw_after_translated();
        // Frontend hook: post-run_block reason handling (aarch64: R_SYSCALL service + pc+=4, else R_BRANCH;
        // x86 adds R_CPUID/x87/DIV/IDIV/99). The per-arch syscall pc-advance convention lives in the hook.
        G_DISPATCH_REASON(c);
        // W4E tier-2: a hot self-loop's back-edge counter fired -> recompile+swap it in. pc is already =
        // loop start, so the next iteration of this dispatcher loop runs the folded block. R_TIER2 is
        // disjoint from R_SYSCALL (handled in the reason hook above) so this never double-fires.
        // tier2_promote is a no-op under threads / NOTIER2. NOTE: the x86 frontend's G_DISPATCH_REASON
        // handles R_TIER2 itself (with `continue`), so for the x86 engine this line is never reached;
        // it remains the aarch64 path. Both arches define tier2_promote (per-arch).
        if (c->reason == R_TIER2) tier2_promote(G_PC(c));
        // async signal -> guest handler (process-directed g_pending OR thread-directed cpu->tpending)
        if (g_pending || c->tpending) maybe_deliver_signal(c);
    }
    // Leave the registries: this thread will never execute in the cache again, nor be a signal target.
    thread_unregister(c);
    stw_unregister();
    uninstall_host_sigaltstack(); // release this thread's alternate signal stack
}
