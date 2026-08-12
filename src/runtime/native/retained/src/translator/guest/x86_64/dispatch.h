#include "../../guest_memory.h" // smc_protect's guest->host resolve, through the seam rather than linux_abi
#include "cpuid.h"
#include "cmpxchg.h"
#include "rotate.h"
#include "rep.h"
#include "x87math.h"
#include "x87state.h"
#include "operand.h"
#include "flags.h"

// translator/guest/x86_64/dispatch.h -- the x86-64 guest's definitions of the shared run_guest()
// dispatch seam (engine-dedup PR3/PR4). Mirror of guest/aarch64/dispatch.h: the shared
// jit/dispatch.c calls these hooks at every guest-architecture seam. Each macro reproduces what the standalone
// frontend/x86_64/dispatch.c did, so swapping the x86 target onto jit/dispatch.c is behavior-preserving.
//
// The macros are EXPANDED at their call sites inside jit/dispatch.c's run_guest() loop (not here), so
// `continue`/`break` reach that loop and the engine globals (g_ibtc/g_xibtc, map_body, g_ibtc_fill,
// do_cpuid/do_repstr/x87_*/tier2_promote, the R_* codes, ...) are in scope there even though this header
// is pulled in early (targets/linux_x86_64.c #includes it right after abi.h). Every name used in a macro
// body is defined earlier in the x86 target TU (declared glue state, cache, emitters and translation) — all
// included before jit/dispatch.c, where the macros expand.
//
// Hooks the shared loop expects (the four PR2 seams + the PR3/PR4 additions for opts committed after the
// design was written -- W6A SMC, opt2 2-way IBTC, the per-block trace dump, the ibtc_base entry setup):
//   G_OWN_TRAMPOLINES   x86 supplies its own run_block/block_return (translate.c) -> suppress the shared
//                       (aarch64) naked trampolines in jit/dispatch.c (different reg model: cpu pinned x28)
//   G_DISPATCH_ENTER    one-time per-thread setup before the loop (x86: publish the 2-way IBTC base)
//   G_DISPATCH_DEBUG    top-of-loop instrumentation (+ the x86 top-of-loop async-signal check)
//   G_SHADOW_CLEAR      wholesale-flush engine reset (x86: drop the 2-way IBTC; aarch64: shadow stack)
//   G_DISPATCH_CHAIN    post-translate chaining (x86: NO-OP -- translate_block already chained)
//   G_AFTER_TRANSLATE   post-translate per-arch step (x86: W6A SMC source-page write-protect)
//   G_TRACE_DUMP        per-block JT trace dump (x86 register/flag layout; the 5th divergence)
//   G_IBTC_FILL         IBTC miss fill (x86: 1-way/2-way, keyed on ic_miss, plain body)
//   G_DISPATCH_REASON   post-run_block reason handling (x86: cpuid/repstr/x87/div/idiv/tier2/syscall)

// ---- x86 dispatch support relocated out of the lifted dispatch.c -------------------------------------
// These DEFINITIONS used to sit at the top of frontend/x86_64/dispatch.c (above run_guest). The swap
// stops #include-ing that file, but linux_abi/x86.c (jit86_lazyguard / jit86_faulth) and the G_AFTER_TRANSLATE /
// G_DISPATCH_DEBUG hooks still need them, so they move here (the x86 dispatch seam). This header is
// #included exactly once in the x86 unity TU -> each is defined once. They reference only libc + the
// extern g_rwx_guest (defined later in os/linux/service.c) -> position-independent here.

// debug: track block transitions for fault diagnosis (used by linux_abi/x86.c).
static uint64_t g_prevpc, g_curpc;

// ---- W6A item 3: SMC (self-modifying code) for in-process JIT guests ----  gate NOSMC=1
// Once a guest takes a PROT_EXEC (RWX) mmap (g_rwx_guest, set in os/linux/service.c), it may overwrite
// code it already executed (and we translated+cached). After translating a block we mprotect its source
// 16KB page READ-ONLY; a guest write then traps in jit86_lazyguard (linux_abi/x86.c) -> smc_on_write() unprotects
// the page + drops the stale translations so the modified bytes re-translate. Entirely inert unless
// g_rwx_guest is set -> zero effect on the normal (non-JIT) matrix.
extern int g_rwx_guest;
// SMC-protected 16 KB code-page table. Sized to cover 64K pages (~1 GB of distinct executed code) so a real
// JIT (V8/PyPy/.NET use tens of MB) never overflows -> no "capacity cliff" stale code in practice; and if a
// pathological guest still exceeds it, smc_protect degrades GRACEFULLY (leaves the overflow page writable
// rather than read-only-but-untracked, which would hang on the un-recognized write). 64K * 8 B = 512 KB BSS.
#define SMC_MAX 65536
static uint64_t g_smc_pg[SMC_MAX];
static int g_smc_n;
static uint64_t g_smc_flushes; // PROF: number of SMC re-translate events

static uint64_t smc_page_size(void) {
    static uint64_t size;
    if (size == 0) size = (uint64_t)getpagesize();
    return size;
}

static void smc_protect(uint64_t pc) {
    if (!g_rwx_guest) return; // no JIT guest -> inert (matrix bit-exact)
    const void *canonical = NULL;
    size_t contiguous = 0;
    // The last hl_logical_vma_resolve_exec call in the translator (f98ae9cf): same tri-state, same
    // arguments, but reached through the bound seam so translator -> linux_abi stays an arrow DOCS.md 3.3
    // does not draw. core/target/x86_64.c binds the logical-VMA implementation in engine_global_init, which
    // runs long before any block is translated, so the answer is unchanged.
    int resolved = hl_guest_memory_resolve_exec(pc, 1, &canonical, &contiguous);
    if (resolved < 0) return;
    if (resolved > 0) pc = (uint64_t)(uintptr_t)canonical;
    uint64_t size = smc_page_size();
    uint64_t pg = pc & ~(size - 1);
    for (int i = 0; i < g_smc_n; i++)
        if (g_smc_pg[i] == pg) return; // already protected
    // Capacity check BEFORE the mprotect. If the table is full, leave the page WRITABLE: a protected-but-
    // untracked page would trap a later guest write that smc_on_write() cannot recognize (returns 0), so the
    // fault falls through as a real SIGSEGV / hangs on the un-handled write. Not protecting past SMC_MAX only
    // loses SMC coherence for the overflow pages (the separate "SMC capacity cliff" -> stale code, not a hang).
    if (g_smc_n >= SMC_MAX) return;
    if (mprotect((void *)pg, (size_t)size, PROT_READ) != 0) return; // code page -> read-only; writes trap
    g_smc_pg[g_smc_n++] = pg;
}

// If `a` falls in a protected SMC page, unprotect+forget it and return 1 (caller drops translations).
// Re-protected the next time the page is translated. Called from jit86_lazyguard (linux_abi/x86.c).
static int smc_on_write(uint64_t a) {
    if (!g_rwx_guest) return 0;
    uint64_t size = smc_page_size();
    uint64_t pg = a & ~(size - 1);
    for (int i = 0; i < g_smc_n; i++)
        if (g_smc_pg[i] == pg) {
            mprotect((void *)pg, (size_t)size, PROT_READ | PROT_WRITE); // let the guest's write through
            g_smc_pg[i] = g_smc_pg[--g_smc_n];                          // re-protected on next translate
            g_smc_flushes++;
            return 1;
        }
    return 0;
}

// ------------------------------------------------------------------------------------------------------

// x86 keeps its own naked trampolines (frontend/x86_64/translate.c: cpu pinned in x28, 16-GPR model,
// host save offsets #168..#264). Defining this tells jit/dispatch.c NOT to emit the aarch64 ones.
#define G_OWN_TRAMPOLINES 1

// One-time entry setup: opt2's emitted indirect hot path loads the 2-way IBTC base from cpu->ibtc_base
// in a single insn, so publish it once before the dispatcher loop. (Was the line right after
// pthread_setspecific in frontend/x86_64/dispatch.c.)
#define G_DISPATCH_ENTER(c) ((c)->ibtc_base = (uint64_t)g_xibtc)

// (4) Top-of-loop instrumentation. x86 checks the async-signal flag at the top of every iteration (the
// shared loop also checks it at the bottom -- the two are the same block boundary, so the top check here
// just preserves x86's historical position; maybe_deliver_signal is guarded + idempotent under g_pending,
// so the extra bottom check is a no-op once delivered). Then the fault-diagnosis block: prev/cur pc, the
// trace cap runaway guard, the malloc/avail-mask dumps, and the byte watchpoint. Verbatim from the old
// frontend/x86_64/dispatch.c. A PLAIN brace block (NOT do/while(0)) so the trace-cap `break` reaches the
// shared dispatcher while-loop -- the original broke the loop immediately, not just the macro.
#define G_DISPATCH_DEBUG(c)                                                                                            \
    {                                                                                                                  \
        if (g_pending) {                                                                                               \
            maybe_deliver_signal(c); /* async signal pending -> redirect to guest handler */                           \
            (c)->rip = nonpie_fold((c)->rip);                                                                          \
        }                                                                                                              \
        g_prevpc = g_curpc;                                                                                            \
        g_curpc = (c)->rip;                                                                                            \
        g_disp_n++;                                                                                                    \
        if (g_trace && g_tracecap && g_disp_n > g_tracecap) { /* bound trace output for runaway guests */              \
            fprintf(stderr, "[hl] trace cap %llu blocks reached -> stop\n", (unsigned long long)g_tracecap);           \
            (c)->exited = 1;                                                                                           \
            (c)->exit_code = 99;                                                                                       \
            break;                                                                                                     \
        }                                                                                                              \
        if (g_nochain && g_loadbase && (c)->rip == g_loadbase + 0x2ee0) g_malloc_n++; /* __libc_malloc_impl entries */ \
        if (g_nochain && g_loadbase) {                                                                                 \
            uint64_t po =                                                                                              \
                g_prevpc - g_loadbase; /* malloc first-handout: dump the new group's avail_mask (rbp=meta) */          \
            if (po >= 0x32a0 && po <= 0x3340) {                                                                        \
                uint64_t rbp = (c)->r[5], rax = (c)->r[0];                                                             \
                uint32_t avail = (rbp > 0x10000) ? *(uint32_t *)(rbp + 0x1c) : 0;                                      \
                fprintf(stderr, "[av] blk+%llx handout=%llx meta(rbp)=%llx avail_mask[rbp+1c]=%x freed[rbp+18]=%x\n",  \
                        (unsigned long long)po, (unsigned long long)rax, (unsigned long long)rbp, avail,               \
                        (rbp > 0x10000) ? *(uint32_t *)(rbp + 0x18) : 0);                                              \
            }                                                                                                          \
        }                                                                                                              \
        if (g_w8 && *g_w8 != g_w8v) { /* byte-watchpoint: report the block that just changed it */                     \
            fprintf(stderr, "[w8] @%p %02x -> %02x  by block +%llx  malloc#=%llu  rsi=%llx\n", (void *)g_w8, g_w8v,    \
                    *g_w8, (unsigned long long)(g_prevpc - g_loadbase), (unsigned long long)g_malloc_n,                \
                    (unsigned long long)(c)->r[6]);                                                                    \
            g_w8v = *g_w8;                                                                                             \
        }                                                                                                              \
    }

// §B-equivalent on-flush engine reset. x86 has no shadow stack; instead, on a wholesale cache flush the
// opt2 2-way IBTC bodies point into the cache we just dropped, so zero it. (The shared loop already
// memset()s the 1-way g_ibtc inline; this drops the x86-only g_xibtc.) Was the `memset(g_xibtc, ...)`
// after the flush in frontend/x86_64/dispatch.c.
#define G_SHADOW_CLEAR(c) memset(g_xibtc, 0, sizeof g_xibtc)

// A3 (aarch64-only lever): no §B-off block-entry alignment on x86. Defined so the shared jit/dispatch.c
// compiles; expands to a compile-time 0 -> the alignment `while` is dead-stripped on x86.
// IRQSLIM moved the per-block poll exit stub out of line, which shifts downstream block layout;
// 16-align each block entry (same rationale as the aarch64 A3 alignment: stabilize hot-loop/BTB
// placement; the pad precedes the entry and never executes). Costs only pad bytes.
#define G_BLOCK_ALIGN (g_fwdskip != 0)

// Post-translate chaining. x86's translate_block() already calls patch_links_to() internally (frontend/
// x86_64/translate.c, gated !g_threaded), so the dispatcher must NOT chain again. (aarch64 moved chaining
// to the dispatcher; x86 keeps it in translate_block -- the per-arch placement the shared loop hides here.)
#define G_DISPATCH_CHAIN(c) ((void)0)

// W6A item 3: after translating a block, write-protect its 16KB source page so a JIT (RWX-mmap) guest's
// later overwrite traps in jit86_lazyguard -> smc_on_write() drops the stale translation. Inert unless
// g_rwx_guest is set (smc_protect returns immediately). Was the smc_protect(c->rip) after the translate.
#define G_AFTER_TRANSLATE(c) smc_protect((c)->rip)

// (5) Per-block JT trace dump. x86 register/flag layout (flags derived from cpu->nzcv; stored C = NOT
// x86 CF). Verbatim from frontend/x86_64/dispatch.c.
#define G_TRACE_DUMP(c)                                                                                                \
    if (g_trace) {                                                                                                     \
        unsigned nz = (unsigned)(c)->nzcv;                                                                             \
        int CF = !((nz >> 29) & 1), ZF = (nz >> 30) & 1, SF = (nz >> 31) & 1, OF = (nz >> 28) & 1;                     \
        fprintf(stderr,                                                                                                \
                "[blk] rip=%llx rax=%llx rbx=%llx rcx=%llx rdx=%llx rsi=%llx rdi=%llx rbp=%llx r8=%llx r9=%llx "       \
                "r10=%llx r11=%llx r12=%llx r13=%llx r14=%llx r15=%llx fl=C%dZ%dS%dO%d\n",                             \
                (unsigned long long)(c)->rip, (unsigned long long)(c)->r[RAX], (unsigned long long)(c)->r[3],          \
                (unsigned long long)(c)->r[RCX], (unsigned long long)(c)->r[RDX], (unsigned long long)(c)->r[RSI],     \
                (unsigned long long)(c)->r[RDI], (unsigned long long)(c)->r[RBP], (unsigned long long)(c)->r[8],       \
                (unsigned long long)(c)->r[9], (unsigned long long)(c)->r[10], (unsigned long long)(c)->r[11],         \
                (unsigned long long)(c)->r[12], (unsigned long long)(c)->r[13], (unsigned long long)(c)->r[14],        \
                (unsigned long long)(c)->r[15], CF, ZF, SF, OF);                                                       \
    }

// (1) IBTC miss fill. x86 keys off c->ic_miss (0/1), stores the PLAIN body (no body-8 stub; x16-x21 are
// free scratch, no stash/restore), and is skipped under threads (the indirect probe reads g_ibtc/g_xibtc
// unlocked -> a torn fill would dispatch the wrong body). IBTC1WAY=1 restores the old 1-way shared-g_ibtc
// fill; otherwise opt2's 2-way set-associative g_xibtc insert. Verbatim from frontend/x86_64/dispatch.c.
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
                if (ibtc1way()) { /* IBTC1WAY=1: exact prior 1-way shared-g_ibtc fill */                               \
                    uint32_t h = (uint32_t)(((c)->rip >> 2) & (IBTC_N - 1));                                           \
                    g_ibtc[h].target = (c)->rip;                                                                       \
                    g_ibtc[h].body = body;                                                                             \
                } else { /* opt2: 2-way insert -> reuse the way already holding this target, else a free */            \
                    uint32_t s = (uint32_t)(((c)->rip >> 2) & (XIBTC_SETS - 1));                                       \
                    int w0 = s * 2, w1 = s * 2 + 1;                                                                    \
                    int w = (!g_xibtc[w0].target || g_xibtc[w0].target == (c)->rip)   ? w0                             \
                            : (!g_xibtc[w1].target || g_xibtc[w1].target == (c)->rip) ? w1                             \
                                                                                      : w0; /* way, else evict way0 */ \
                    g_xibtc[w].target = (c)->rip;                                                                      \
                    g_xibtc[w].body = body;                                                                            \
                }                                                                                                      \
                g_ibtc_fill++;                                                                                         \
            }                                                                                                          \
        }                                                                                                              \
        (c)->ic_miss = 0;                                                                                              \
    }

// (2) Post-run_block reason handling. The full x86 reason switch: the unimplemented-opcode abort (99),
// the W5-B R_TIER2 promote, R_CPUID, the W4-C R_REPSTR rep cmps/scas idiom, the x87 m80 fld/fstp, the
// 128/64 div/idiv done in C, and finally R_SYSCALL (x86 pre-advances rip in the emitter, so NO post-
// service pc-advance -- the per-arch syscall tail convention lives here; aarch64 does pc += 4 instead).
// Each non-syscall case `continue`s the shared while-loop (so the shared `if (reason==R_TIER2) ...`
// tail line never re-fires for x86). Verbatim from frontend/x86_64/dispatch.c. `break` exits the loop.
#if defined(__APPLE__)
#define G_DISPATCH_SOFTSPAN(c)                                                                                         \
    if ((c)->reason == R_SOFTSPAN) {                                                                                   \
        (c)->soft_snapshot = 0;                                                                                        \
        (c)->rip = nonpie_unfold((c)->rip);                                                                            \
        (c)->reason = R_BRANCH;                                                                                        \
        continue;                                                                                                      \
    }
#else
#define G_DISPATCH_SOFTSPAN(c)
#endif

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
    if ((c)->reason == R_SOFTMISS) {                                                                                   \
        if (soft_tlb_miss(c)) maybe_deliver_signal(c);                                                                 \
        continue;                                                                                                      \
    }                                                                                                                  \
    G_DISPATCH_SOFTSPAN(c)                                                                                             \
    if ((c)->reason == 99) {                                                                                           \
        fprintf(stderr, "[hl] aborting at rip marker %llx (unimplemented opcode)\n", (unsigned long long)(c)->rip);    \
        if (g_trace) {                                                                                                 \
            for (int rr = 0; rr < 16; rr++) { /* dump heap-pointer regs (meta etc.) */                                 \
                uint64_t v = (c)->r[rr];                                                                               \
                if (v > 0x100000000ull && v < 0x200000000ull && (v & 7) == 0) {                                        \
                    fprintf(stderr, "  r%d=%llx:", rr, (unsigned long long)v);                                         \
                    for (int i = 0; i < 5; i++)                                                                        \
                        fprintf(stderr, " %016llx", (unsigned long long)((uint64_t *)v)[i]);                           \
                    fprintf(stderr, "\n");                                                                             \
                }                                                                                                      \
            }                                                                                                          \
        }                                                                                                              \
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
