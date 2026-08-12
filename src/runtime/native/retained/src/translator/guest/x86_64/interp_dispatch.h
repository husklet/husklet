#include "cpuid.h"
#include "cmpxchg.h"
#include "rotate.h"
#include "rep.h"
#include "x87math.h"
#include "x87state.h"
#include "operand.h"
#include "flags.h"

// The x86-64 guest's dispatch seam for the INTERPRETER backend (every host CPU that is not AArch64);
// dispatch.h beside it is the same seam for the ARM64 JIT.
//
// Nothing in the shared run_guest() contract requires `code` to be host machine code: interp.c makes it a
// decoded-block DESCRIPTOR and run_block a C decode-and-execute loop, leaving struct cpu, the block cache,
// linux_abi, the checkpoint format and the ARM-NZCV EFLAGS substrate all unchanged.
//
// Hooks that only serve HOST CODE GENERATION are empty here -- except that ibtc_base and g_xibtc are still
// cleared, because they hold HOST pointers carried in the checkpoint image.

// block transitions for fault diagnosis (read by linux_abi/x86.c's fault guards); GUEST PCs.
static uint64_t g_prevpc, g_curpc;

// SMC. The JIT write-protects translated source pages and drops stale HOST CODE on the trap; an interpreter
// has none -- descriptors carry no guest bytes, so every execution re-decodes guest memory. Hence no
// smc_protect and an empty G_AFTER_TRANSLATE. smc_on_write must still EXIST for jit86_lazyguard; 0 lets it
// continue its chain instead of swallowing a guest fault.
extern int g_rwx_guest;

static int smc_on_write(uint64_t address) {
    (void)address;
    return 0;
}

// Asked by the shared G_DISPATCH_CHAIN default and cache.c's arena rollover, both about EMITTED code.
static inline int smc_seen(void) {
    return 0;
}

// interp.c supplies run_block/block_return: core/dispatch.c must not emit its AArch64 trampolines.
#define G_OWN_TRAMPOLINES 1

// Publish the absence of an IBTC base (header note).
#define G_DISPATCH_ENTER(c) ((c)->ibtc_base = 0)

#define G_DISPATCH_DEBUG(c)                                                                                            \
    {                                                                                                                  \
        if (g_pending) maybe_deliver_signal(c); /* redirect to the guest handler */                                    \
        g_prevpc = g_curpc;                                                                                            \
        g_curpc = (c)->rip;                                                                                            \
        g_disp_n++;                                                                                                    \
    }

// On-flush reset; g_xibtc holds HOST code pointers (header note).
#define G_SHADOW_CLEAR(c) memset(g_xibtc, 0, sizeof g_xibtc)

// Compile-time 0 dead-strips the shared loop's emit32() alignment pad.
#define G_BLOCK_ALIGN 0

// No emitted inter-block edges to backpatch.
#define G_DISPATCH_CHAIN(c) ((void)0)

// No source page to write-protect (see SMC above).
#define G_AFTER_TRANSLATE(c) ((void)0)

// MUST be overridden on either backend: the shared default reads cpu->x[]/cpu->sp, which an x86 guest
// lacks. Identical to dispatch.h's; stored C is NOT x86 CF.
#define G_TRACE_DUMP(c) ((void)0)

// No inline branch-target cache: cpu->ic_miss is never set here.
#define G_IBTC_FILL(c) ((void)0)

// Structurally dispatch.h's, because the reason codes name guest events. No arm advances rip: this
// frontend's convention is that rip is ALREADY past the `0F 05` when the block exits (aarch64, whose PC
// stays on the SVC, does `pc += 4` here). Arms an interpreter cannot reach are handled anyway, so none
// falls through to the R_BRANCH default and resumes at a PC nobody set -- and R_TIER2 must NOT take
// dispatch.h's tier2_promote + `continue`, which without an emitted counter would loop on one block.
#define G_DISPATCH_REASON(c)                                                                                           \
    /* The C instruction emulators come FIRST and do not `continue` unconditionally: they run outside                  \
       run_block, so a guest access they reject leaves a NEW reason (R_SOFTMISS, or R_TRAP for an                      \
       emulated #UD) that the arms below have to see. rip = the insn; the callee advances it. */                       \
    if ((c)->reason == R_AVX) {                                                                                        \
        hl_x86_avx_run(&g_avx_state, (c));                                                                             \
        if ((c)->reason == R_AVX) continue;                                                                            \
    }                                                                                                                  \
    if ((c)->reason == R_SSE3B) {                                                                                      \
        hl_x86_sse_run(&g_avx_state, (c));                                                                             \
        if ((c)->reason == R_SSE3B) continue;                                                                          \
    }                                                                                                                  \
    if ((c)->reason == R_SOFTMISS) {                                                                                   \
        if (soft_tlb_miss(c)) maybe_deliver_signal(c);                                                                 \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_SOFTSPAN) { /* retry the restartable string op */                                             \
        (c)->reason = R_BRANCH;                                                                                        \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_TIER2) { /* see the header note */                                                            \
        (c)->reason = R_BRANCH;                                                                                        \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_CPUID) {                                                                                      \
        hl_x86_cpuid(c);                                                                                               \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_REPSTR) { /* rep cmps/scas */                                                                 \
        hl_x86_rep_compare(c, g_nonpie_lo, g_nonpie_hi, g_nonpie_bias);                                                \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_X87FLD) {                                                                                     \
        hl_x86_x87_load_ext80(c);                                                                                      \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_X87FSTP) {                                                                                    \
        hl_x86_x87_store_ext80_pop(c);                                                                                 \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_X87FUNC) {                                                                                    \
        hl_x86_x87_math(c);                                                                                            \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_RCL) { /* RCL/RCR by CL */                                                                    \
        hl_x86_rotate_carry(c);                                                                                        \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_CMPXCHG16) {                                                                                  \
        hl_x86_cmpxchg16(c);                                                                                           \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_FXSAVE) { /* x87-register DATA + FSW tail */                                                  \
        hl_x86_fxsave(c);                                                                                              \
        continue;                                                                                                      \
    }                                                                                                                  \
    if ((c)->reason == R_FXRSTOR) { /* x87-register DATA + FSW tail */                                                 \
        hl_x86_fxrstor(c);                                                                                             \
        continue;                                                                                                      \
    }                                                                                                                  \
    /* #DE si_code is FPE_INTDIV(1) for a quotient overflow too -- Linux classifies the trap, not its                  \
     * cause (1328eac3, tests/compat/completeness/x86_64/div_overflow.c). interp_divide already rules on               \
     * overflow and reports it as divop == 0, so the arms below are the defensive half; they are kept                  \
     * exact, and they test before dividing so INT128_MIN over -1 is never evaluated (signed-overflow UB). */          \
    if ((c)->reason == R_DIV) { /* 128/64 unsigned div; divop==0 means #DE */                                          \
        uint64_t d = (c)->divop;                                                                                       \
        if (d == 0 || (c)->r[RDX] >= d) { /* /0, or high half >= divisor: both are the same #DE */                     \
            if (raise_guest_de(c, 1 /*FPE_INTDIV*/)) {                                                                 \
                maybe_deliver_signal(c);                                                                               \
                continue;                                                                                              \
            }                                                                                                          \
            break; /* raise_guest_de set exited+exit_code */                                                           \
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
        } else if (d == -1) { /* the only divisor whose quotient overflows __int128 too */                             \
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
    if ((c)->reason == R_TRAP) { /* int3/UD2; cpu->divop = signo|code<<8 */                                            \
        if (raise_guest_trap(c)) {                                                                                     \
            maybe_deliver_signal(c);                                                                                   \
            continue;                                                                                                  \
        }                                                                                                              \
        (c)->exited = 1; /* no handler: default action terminates */                                                   \
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
        if ((c)->redirect) (c)->redirect = 0; /* else rip already = next */                                            \
    }                                                                                                                  \
    /* R_BRANCH: c->rip already holds the target */
