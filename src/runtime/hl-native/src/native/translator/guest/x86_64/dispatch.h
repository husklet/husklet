#include "../../guest_memory.h" // smc_protect's guest->host resolve, through the seam rather than linux_abi
#include "cpuid.h"
#include "cmpxchg.h"
#include "rotate.h"
#include "rep.h"
#include "x87math.h"
#include "x87state.h"
#include "operand.h"
#include "flags.h"

// x86-64 definitions for the architecture-specific seams in engine/dispatch.c's shared run_guest() loop.
//
// The macros expand at their call sites inside engine/dispatch.c's run_guest() loop, so
// `continue`/`break` reach that loop and the engine globals (g_ibtc/g_xibtc, map_body, g_ibtc_fill,
// do_cpuid/do_repstr/x87_*/tier2_promote, the R_* codes, ...) are in scope there even though this header
// is pulled in by engine/target/x86_64.c before engine/dispatch.c. Every name used in a macro
// body is defined earlier in the x86 target translation unit: glue state, cache, emitters, and translation
// are included before engine/dispatch.c.
//
// Hooks supplied to the shared loop:
//   G_OWN_TRAMPOLINES   x86 supplies its own run_block/block_return (translate.c) -> suppress the shared
//                       AArch64 naked trampolines in engine/dispatch.c (different register model)
//   G_DISPATCH_ENTER    one-time per-thread setup before the loop (x86: publish the 2-way IBTC base)
//   G_DISPATCH_DEBUG    top-of-loop instrumentation (+ the x86 top-of-loop async-signal check)
//   G_SHADOW_CLEAR      wholesale-flush engine reset (x86: drop the 2-way IBTC; aarch64: shadow stack)
//   G_DISPATCH_CHAIN    post-translate chaining (x86: NO-OP -- translate_block already chained)
//   G_AFTER_TRANSLATE   post-translate per-arch step (x86: W6A SMC source-page write-protect)
//   G_TRACE_DUMP        per-block JT trace dump using the x86 register and flag layout
//   G_IBTC_FILL         IBTC miss fill (x86: 1-way/2-way, keyed on ic_miss, plain body)
//   G_DISPATCH_REASON   post-run_block reason handling (x86: cpuid/repstr/x87/div/idiv/tier2/syscall)

// This header is included exactly once in the x86 target translation unit, so its support state has one
// definition. linux_abi/x86.c and the dispatch hooks below share that state.

// debug: track block transitions for fault diagnosis (used by linux_abi/x86.c).
static uint64_t g_prevpc, g_curpc;

// x86 keeps the naked trampolines in translate.c because its register and host-save model differs from
// AArch64. Defining this prevents engine/dispatch.c from emitting the AArch64 trampolines.
#define G_OWN_TRAMPOLINES 1

// One-time entry setup: opt2's emitted indirect hot path loads the 2-way IBTC base from cpu->ibtc_base
// in a single instruction, so publish it once before the dispatcher loop.
#define G_DISPATCH_ENTER(c) ((c)->ibtc_base = (uint64_t)g_xibtc)
#define G_MAP_HOST_CACHE map_host_cache_current()
#define G_MAP_HOST(cache, gpc) map_host_cached((cache), (gpc))

// Top-of-loop instrumentation. x86 checks the async-signal flag at the top of every iteration (the
// shared loop also checks it at the bottom -- the two are the same block boundary, so the top check here
// preserves the required delivery position; maybe_deliver_signal is guarded and idempotent under g_pending,
// so the extra bottom check is a no-op once delivered). Then the fault-diagnosis block: prev/cur pc, the
// diagnostic counters.
#define G_DISPATCH_DEBUG(c)                                                                                            \
    {                                                                                                                  \
        if (signal_deliverable_for_cpu(c)) { maybe_deliver_signal(c); /* deliverable signal -> handler */ }            \
        if (g_dispatch_diagnostics) {                                                                                  \
            g_prevpc = g_curpc;                                                                                        \
            g_curpc = (c)->rip;                                                                                        \
            g_disp_n++;                                                                                                \
        }                                                                                                              \
    }

// §B-equivalent on-flush engine reset. x86 has no shadow stack; instead, on a wholesale cache flush the
// opt2 2-way IBTC bodies point into the cache we just dropped, so zero it. (The shared loop already
// memset()s the 1-way g_ibtc inline; this drops the x86-only g_xibtc.)
#define G_SHADOW_CLEAR(c) memset(g_xibtc, 0, sizeof g_xibtc)

// The shared dispatcher aligns x86 block entries when forward skipping is enabled. The out-of-line
// per-block poll exit stub shifts downstream block layout, so 16-byte alignment stabilizes hot-loop and
// branch-target-buffer placement; the preceding padding never executes.
#define G_BLOCK_ALIGN (g_fwdskip != 0)

// Post-translate chaining. x86's translate_block() already calls patch_links_to() internally in translate.c
// when !g_threaded, so the dispatcher must not chain again.
#define G_DISPATCH_CHAIN(c) ((void)0)

// After translating a block, write-protect its 16KB source page so a JIT (RWX-mmap) guest's
// later overwrite traps in jit86_lazyguard -> smc_on_write() drops the stale translation. Inert unless
// g_rwx_guest is set (smc_protect returns immediately).
#define G_AFTER_TRANSLATE(c) smc_protect(nonpie_fold((c)->rip))

#define G_TRACE_DUMP(c) ((void)0)

// IBTC miss fill. x86 keys off c->ic_miss (0/1), stores the plain body (no body-8 stub; x16-x21 are
// free scratch, no stash/restore), and is skipped under threads (the indirect probe reads g_ibtc/g_xibtc
// unlocked -> a torn fill would dispatch the wrong body). Use the two-way set-associative g_xibtc insert.
#define G_IBTC_FILL(c)                                                                                                 \
    if ((c)->ic_miss) {                                                                                                \
        if (!g_threaded) {                                                                                             \
            void *body = map_body((c)->rip);                                                                           \
            if (body) {                                                                                                \
                /* The emitted indirect probe (emit_ibranch) branches ABSOLUTELY to slot.body                          \
                 * (ldr x21,[slot+8]; br x21), so under the dual-mapped code cache the stored body must be the         \
                 * EXECUTOR (RX) alias -- map_body returns the RW writer alias. J_RX converts it (a no-op when         \
                 * g_rw2rx==0, i.e. NODUALMAP/single-MAP_JIT fallback -> byte-identical to the prior path). Mirrors    \
                 * the aarch64 IBTC fill's J_RX(bd) in guest/aarch64/dispatch.h. */                                    \
                body = J_RX(body);                                                                                     \
                uint32_t s = (uint32_t)(((c)->rip >> 2) & (XIBTC_SETS - 1));                                           \
                int w0 = s * 2, w1 = s * 2 + 1;                                                                        \
                int w = (!g_xibtc[w0].target || g_xibtc[w0].target == (c)->rip)   ? w0                                 \
                        : (!g_xibtc[w1].target || g_xibtc[w1].target == (c)->rip) ? w1                                 \
                                                                                  : w0;                                \
                g_xibtc[w].target = (c)->rip;                                                                          \
                g_xibtc[w].body = body;                                                                                \
                g_ibtc_fill++;                                                                                         \
            }                                                                                                          \
        }                                                                                                              \
        (c)->ic_miss = 0;                                                                                              \
    }

// Post-run_block reason handling: the unimplemented-opcode abort (99),
// R_TIER2 promotion, R_CPUID, R_REPSTR rep cmps/scas, the x87 m80 fld/fstp, the
// 128/64 div/idiv done in C, and finally R_SYSCALL (x86 pre-advances rip in the emitter, so NO post-
// service pc-advance -- the per-arch syscall tail convention lives here; aarch64 does pc += 4 instead).
// Each non-syscall case `continue`s the shared while-loop (so the shared `if (reason==R_TIER2) ...`
// tail line never re-fires for x86). `break` exits the loop.
#define G_DISPATCH_SOFTSPAN(c)                                                                                         \
    if ((c)->reason == R_SOFTSPAN) {                                                                                   \
        (c)->soft_snapshot = 0;                                                                                        \
        SOFT_TLB_INVALIDATE_ALL(c);                                                                                    \
        (c)->rip = nonpie_unfold((c)->rip);                                                                            \
        (c)->reason = R_BRANCH;                                                                                        \
        continue;                                                                                                      \
    }

#include "xsave.h"

#define G_DISPATCH_REASON(c)                                                                                           \
    /* The C instruction emulators come FIRST and do not `continue` unconditionally: they run outside                  \
       run_block, so a guest access they reject leaves a NEW reason (R_SOFTMISS, or R_TRAP for an                      \
       emulated #UD) that the arms below have to see. rip = the insn; do_avx/do_sse3b advance it. */                   \
    if ((c)->reason == R_AVX) { /* VEX/EVEX AVX insn: emulate in C */                                                  \
        hl_x86_avx_run(&g_avx_state, (c));                                                                             \
        if ((c)->reason == R_AVX) continue;                                                                            \
    }                                                                                                                  \
    if ((c)->reason == R_SSE3B) { /* legacy 0F38/0F3A insn: emulate in C */                                            \
        hl_x86_sse_run(&g_avx_state, (c));                                                                             \
        if ((c)->reason == R_SSE3B) continue;                                                                          \
    }                                                                                                                  \
    if ((c)->reason == R_XSAVE) {                                                                                      \
        uint64_t xsave_fault = (c)->x87_ea;                                                                            \
        int xsave_result = hl_x86_xsave((c), &xsave_fault);                                                            \
        if (xsave_result == 0) {                                                                                       \
            (c)->rip = (c)->divop;                                                                                     \
            (c)->reason = R_BRANCH;                                                                                    \
            continue;                                                                                                  \
        }                                                                                                              \
        if (xsave_result == -2) {                                                                                      \
            (c)->divop = UINT64_C(11) | (UINT64_C(128) << 8);                                                          \
            (c)->reason = R_TRAP;                                                                                      \
        } else {                                                                                                       \
            (c)->bus_ea = xsave_fault;                                                                                 \
            (c)->soft_guest_ea = xsave_fault;                                                                          \
            (c)->soft_width = (c)->x87_ea + HL_X86_XSAVE_SPAN - xsave_fault;                                           \
            (c)->soft_required = X86_SOFT_WRITE;                                                                       \
            (c)->reason = R_SOFTMISS;                                                                                  \
        }                                                                                                              \
    }                                                                                                                  \
    if ((c)->reason == R_SOFTMISS) {                                                                                   \
        if (soft_tlb_miss(c)) maybe_deliver_signal(c);                                                                 \
        continue;                                                                                                      \
    }                                                                                                                  \
    G_DISPATCH_SOFTSPAN(c)                                                                                             \
    if ((c)->reason == 99) {                                                                                           \
        fprintf(stderr, "[hl] aborting at rip marker %llx (unimplemented opcode)\n", (unsigned long long)(c)->rip);    \
        (c)->exited = 1;                                                                                               \
        (c)->exit_code = 70;                                                                                           \
        break;                                                                                                         \
    }                                                                                                                  \
    if ((c)->reason == R_TIER2) { /* W5B: hot self-loop back-edge fired; recompile+swap. rip = loop start */           \
        tier2_promote((c)->rip);                                                                                       \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_CPUID) {                                                                                      \
        hl_x86_cpuid(c);                                                                                               \
        continue;                                                                                                      \
    } /* rip already = next */                                                                                         \
    if ((c)->reason == R_REPSTR) {                                                                                     \
        hl_x86_rep_compare(c, g_nonpie_lo, g_nonpie_hi, g_nonpie_bias);                                                \
        continue;                                                                                                      \
    } /* W4-C rep cmps/scas idiom (rip already = next) */                                                              \
    if ((c)->reason == R_X87FLD) {                                                                                     \
        hl_x86_x87_load_ext80(c);                                                                                      \
        continue;                                                                                                      \
    } /* fld m80 (rip already = next) */                                                                               \
    if ((c)->reason == R_X87FSTP) {                                                                                    \
        hl_x86_x87_store_ext80_pop(c);                                                                                 \
        continue;                                                                                                      \
    } /* fstp m80 */                                                                                                   \
    if ((c)->reason == R_X87FUNC) {                                                                                    \
        hl_x86_x87_math(c);                                                                                            \
        continue;                                                                                                      \
    } /* x87 transcendental (rip already = next) */                                                                    \
    if ((c)->reason == R_RCL) { /* RCL/RCR by CL: rotate-through-carry in C (rip already = next) */                    \
        hl_x86_rotate_carry(c);                                                                                        \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_CMPXCHG16) { /* atomic 128-bit compare-exchange in C (rip already = next) */                  \
        hl_x86_cmpxchg16(c);                                                                                           \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_FXSAVE) { /* fxsave x87-register-DATA + FSW tail (rip already = next) */                      \
        hl_x86_fxsave(c);                                                                                              \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_FXRSTOR) { /* fxrstor x87-register-DATA + FSW tail (rip already = next) */                    \
        hl_x86_fxrstor(c);                                                                                             \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_X87ENV) { /* fnstenv/fldenv m28, fnsave/frstor m108 (rip already = next) */                   \
        hl_x86_x87_environment(c);                                                                                     \
        continue;                                                                                                      \
    }                                                                                                                  \
    /* #DE si_code. Linux/x86 reports FPE_INTDIV(1) for the #DE trap WHATEVER raised it -- a zero divisor              \
     * and a quotient overflow alike; this host's silicon is the oracle and tests/compat/completeness/                 \
     * x86_64/div_overflow.c holds its answer. Queueing FPE_INTOVF(2) here diverged for the JIT only:                  \
     * the interpreter reports overflow by exiting with divop == 0, so it lands in the /0 arm (1328eac3).              \
     * Overflow is also ruled on BEFORE dividing: RDX:RAX == INT128_MIN over -1 is signed-overflow UB, and             \
     * deciding that the GUEST's idiv faults must not depend on how the host's __divti3 answers it. (It does           \
     * NOT trap on either host -- measured on x86-64 and aarch64 at -O0 and -O2 -- so this is UB removal, not          \
     * a crash fix; 1328eac3's message says otherwise and is wrong on that point.) */                                  \
    if ((c)->reason == R_DIV) { /* 128/64 unsigned div (rip already = next) */                                         \
        uint64_t d = (c)->divop;                                                                                       \
        if (d == 0 || (c)->r[RDX] >= d) { /* /0, or quotient overflow (high half >= divisor): both #DE */              \
            if (raise_guest_de(c, 1 /*FPE_INTDIV*/)) {                                                                 \
                maybe_deliver_signal(c);                                                                               \
                continue;                                                                                              \
            }                                                                                                          \
            break; /* raise_guest_de recorded death-by-SIGFPE / set exited+exit_code */                                \
        }                                                                                                              \
        unsigned __int128 num = ((unsigned __int128)(c)->r[RDX] << 64) | (c)->r[RAX];                                  \
        (c)->r[RAX] = (uint64_t)(num / d);                                                                             \
        (c)->r[RDX] = (uint64_t)(num % d);                                                                             \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_IDIV) { /* 128/64 signed idiv */                                                              \
        int64_t d = (int64_t)(c)->divop;                                                                               \
        __int128 num = ((__int128)(int64_t)(c)->r[RDX] << 64) | (c)->r[RAX];                                           \
        int de;                                                                                                        \
        if (d == 0) {                                                                                                  \
            de = 1;                                                                                                    \
        } else if (d == -1) { /* the only divisor whose quotient overflows __int128 too: test, don't divide */         \
            de = num < -(__int128)INT64_MAX || num > -(__int128)INT64_MIN;                                             \
        } else {                                                                                                       \
            __int128 q0 = num / d;                                                                                     \
            de = (__int128)(int64_t)q0 != q0;                                                                          \
        }                                                                                                              \
        if (de) {                                                                                                      \
            if (raise_guest_de(c, 1 /*FPE_INTDIV*/)) {                                                                 \
                maybe_deliver_signal(c);                                                                               \
                continue;                                                                                              \
            }                                                                                                          \
            break;                                                                                                     \
        }                                                                                                              \
        (c)->r[RAX] = (uint64_t)(num / d);                                                                             \
        (c)->r[RDX] = (uint64_t)(num % d);                                                                             \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_TRAP) { /* int3 -> SIGTRAP, UD2 -> SIGILL: deliver from C (cpu->divop = signo|code<<8) */     \
        if (raise_guest_trap(c)) {                                                                                     \
            maybe_deliver_signal(c);                                                                                   \
            continue;                                                                                                  \
        }                                                                                                              \
        (c)->exited = 1; /* no guest handler: default action terminates with the signal */                             \
        (c)->exit_code = 128 + ((int)((c)->divop & 0xff));                                                             \
        break;                                                                                                         \
    }                                                                                                                  \
    if ((c)->reason == R_BUS) {                                                                                        \
        if (raise_guest_bus(c)) {                                                                                      \
            maybe_deliver_signal(c);                                                                                   \
            continue;                                                                                                  \
        }                                                                                                              \
        break;                                                                                                         \
    }                                                                                                                  \
    if ((c)->reason == R_SMC) {                                                                                        \
        jit86_smc_commit(c);                                                                                           \
        (c)->reason = R_BRANCH;                                                                                        \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_SYSCALL) {                                                                                    \
        /* Publish emulated MAP_SHARED stores before a write/socket/futex syscall can notify a peer. */                \
        if ((c)->smc_range_count || (c)->smc_range_overflow) jit86_smc_commit(c);                                      \
        service(c);                                                                                                    \
        if ((c)->exited) break;                                                                                        \
        /* And after: the syscall's own copyout (G_SMC_COPYOUT) may have written an executable alias. */               \
        if ((c)->smc_range_count || (c)->smc_range_overflow) jit86_smc_commit(c);                                      \
        if ((c)->redirect) (c)->redirect = 0; /* else rip already = next (set at exit) */                              \
    }                                                                                                                  \
    /* R_BRANCH: c->rip already holds the target */
